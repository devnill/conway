//! The persistent NDJSON transport for `conway-plugin-subprocess` (board
//! item `01M03VJHG1WFECFJB4ZH3CKWDX`): a long-lived child process, spawned
//! ONCE per plugin, kept alive across many `tool/1` calls, framing
//! requests/responses as newline-delimited JSON objects over the child's
//! stdin/stdout. A NEW lifecycle alongside the existing one-shot
//! [`crate::spawn_one_shot`] path -- the one-shot path is NOT deleted
//! (discovery `tool.spec/1` stays one-shot by design; see `wire`'s own
//! module doc for why that sidesteps the manifest-`id` / JSON-RPC-`id`
//! collision a persistent envelope would otherwise force).
//!
//! **Framing decision (this item's own, disclosed): NDJSON -- one JSON-RPC
//! object per line, `\n`-delimited, over the child's stdin/stdout.** The
//! spec's title names NDJSON; its primary suggestion is "one
//! `serde_json::Value` per line"; `docs/plugins/subprocess-plugins.md`
//! already says verbatim "persistent NDJSON JSON-RPC connection" and "the
//! long-lived NDJSON JSON-RPC". So the established, consistent framing
//! language is NDJSON. The wire vocabulary is reused from `wire.rs`, not
//! parallel-invented: a persistent `tool/1` request is
//! `{"id":N,"op":"tool/1","tool":...,"call_id":...,"arguments":{...}}\n`
//! (the one-shot [`crate::wire::Request::ToolV1`] fields plus a JSON-RPC
//! `id`), and the response is the one-shot `tool/1` answer fields plus the
//! echoed `id`, one per line (see [`crate::wire::PersistentToolRequest`] /
//! [`crate::wire::PersistentToolResponse`]).
//!
//! **Correlation discipline: JSON-RPC `id` + an outstanding-request table.**
//! Each call assigns a monotonic `id`, inserts a `oneshot` sender into a
//! pending table keyed by `id`, writes the framed request line, and awaits
//! the sender (bounded by the per-call timeout). A long-lived reader task
//! reads stdout line-by-line, parses each line, extracts `id`, and routes
//! the value to the matching pending sender. This shape is built now so a
//! LATER item can add notifications (`observe/1`) alongside requests
//! without redesigning framing -- a line carrying no matching `id` would
//! be a notification; today (no notifications yet) such a line is a
//! malformed frame and fails closed, by design.
//!
//! **Failure handling -- fail-closed uniformly, mirroring
//! [`crate::SubprocessPluginError`]'s discipline.** A session that dies
//! mid-call (the child exits nonzero, or closes stdout) surfaces a typed
//! [`crate::SubprocessPluginError::SessionDied`], never a hang and never a
//! silent retry. **No automatic reconnect** (a plugin that died has lost
//! whatever session state it had; the death is surfaced and the caller
//! must re-`discover`). Once a session is marked dead, every subsequent
//! call fails fast with `SessionDied` -- the session is NOT re-spawned.
//! A plugin that never answers a framed response is killed and reported
//! [`crate::SubprocessPluginError::TimedOut`] within `timeout_ms` (the
//! per-call deadline on the framed read, NOT a session-wide idle kill --
//! a session that legitimately sits idle between calls is left alone).
//! An unterminated/malformed frame (no newline, invalid JSON, a partial
//! line then EOF) is a typed
//! [`crate::SubprocessPluginError::MalformedFrame`], not a deadlock.
//!
//! **Hazards (from this item's own spec).**
//!
//! - **The four-way `tokio::join!` deadlock** (board item
//!   `01M03FNRGWNMMRKXBJKCEE14QJ`): the one-shot path already splits its
//!   join into a three-way then a sequential `wait()`. The persistent path
//!   does NOT re-introduce a join that starves `child.wait()` against
//!   piped stdio: the reader is a long-lived `BufRead::read_line` loop in
//!   its OWN task, stderr is drained in its OWN task, and `child.wait()` is
//!   NEVER joined concurrently with either -- it runs only on drop / on a
//!   fatal kill, after the reader/stderr tasks have already torn down. No
//!   `tokio::join!` here joins `child.wait()` against piped stdio.
//! - **stderr is drained concurrently for the session's lifetime** (a
//!   plugin that writes to stderr with nobody reading it blocks); the
//!   drained bytes are DISCARDED, mirroring the one-shot path's own
//!   "stderr is drained but discarded" disclosure -- no log/event sink is
//!   wired for a subprocess plugin's own diagnostic output.
//! - **Process-group kill on drop**: when [`PersistentSession`] is
//!   dropped, the process group is killed (best-effort SIGKILL on the
//!   group, synchronously -- `Drop` cannot `await` the graceful
//!   SIGTERM-then-SIGKILL `unix::kill_group` uses on the timeout path).
//!   `kill_on_drop(true)` is ALSO set on the `Command` as a
//!   belt-and-suspenders so the leader dies even if our `Drop`'s
//!   `kill(-pgid)` is beaten to it. A long-lived child is never orphaned.
//! - **`kill_group` is DUPLICATED, not shared** -- `crate::unix::kill_group`
//!   (itself already a documented duplicate of
//!   `conway_tools::process::unix::kill_group`) is reused for the
//!   graceful timeout kill; the synchronous `Drop`-time SIGKILL uses
//!   `nix::sys::signal::kill` directly. `conway_tools::process`'s
//!   visibility is NOT widened (out of this item's owned paths).

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{oneshot, Mutex as AsyncMutex};
use tokio::time::timeout;

use conway::plugin::ToolError;

use crate::unix::kill_group;
use crate::wire::{parse_persistent_tool_response, PersistentToolRequest, WireToolResult};
use crate::{SubprocessPluginError, SubprocessPluginSpec};

/// State shared between the [`PersistentSession`] handle and the long-lived
/// reader task: the outstanding-request table (keyed by JSON-RPC `id`), the
/// session-dead flag, and -- when dead -- the typed reason the session died.
/// Held in an `Arc` so the reader task can route a response to the waiting
/// call and mark the session dead on EOF/parse error without holding a
/// handle to the `Child` itself.
struct Shared {
    /// Outstanding-request table: `id` -> the `oneshot` sender a waiting
    /// `round_trip` call is awaiting. Inserted by `round_trip` before it
    /// writes the request line; removed by the reader task (on a routed
    /// response), by `round_trip` itself (on timeout), or by `kill_all`
    /// (on session death, which drops every pending sender so its waiter
    /// sees `Err` and reads the death reason from [`Shared::death`]).
    pending: Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>,
    /// Set when the session is torn down (child died, malformed frame, or
    /// explicit kill on timeout). Once true, every subsequent `round_trip`
    /// fails fast -- no re-spawn.
    dead: AtomicBool,
    /// The typed reason the session died, set by `kill_all` under the same
    /// lock that drains `pending`. Read by a `round_trip` whose sender was
    /// dropped (it got `Err` from `rx`) so it can surface the REAL failure
    /// mode -- a typed `SubprocessPluginError::SessionDied` or
    /// `MalformedFrame` -- rather than a generic "session died".
    death: Mutex<Option<SubprocessPluginError>>,
}

impl Shared {
    /// Marks the session dead, records the typed death reason, and drops
    /// every pending sender so its waiter wakes with `Err` and surfaces
    /// `reason`. Called by the reader task on EOF / malformed frame, and by
    /// `round_trip` on a write failure.
    fn kill_all(&self, reason: SubprocessPluginError) {
        self.dead.store(true, Ordering::Release);
        let mut death = self.death.lock().expect("death lock poisoned");
        if death.is_none() {
            *death = Some(reason);
        }
        let mut pending = self.pending.lock().expect("pending table poisoned");
        // Dropping each sender (rather than sending a placeholder) makes the
        // waiter's `rx` resolve to `Err` -- the signal that the session died
        // and its reason is in `self.death`.
        pending.clear();
    }
}

/// A long-lived handle to one persistent subprocess plugin: owns the
/// `tokio::process::Child`, its stdin write half, and a long-lived reader
/// task that frames NDJSON responses off stdout and routes them by JSON-RPC
/// `id` to the waiting call. Built by `PersistentSession::spawn` (a
/// `pub(crate)` constructor `SubprocessPlugin::discover` calls); used by the
/// `Tool::invoke` impl on `crate::SubprocessTool` when the plugin's
/// [`crate::SubprocessPluginSpec::transport`] is
/// [`crate::SubprocessTransport::Persistent`].
///
/// **Cloning.** A `PersistentSession` is NOT `Clone`; the plugin hands each
/// `SubprocessTool` an `Arc<PersistentSession>`, so every tool on this
/// plugin shares ONE child process (the load-bearing property acceptance
/// criterion 1 asserts: the child PID is identical across two sequential
/// calls).
pub struct PersistentSession {
    config_id: String,
    pgid: i32,
    timeout_ms: u64,
    /// The child, held for the session's lifetime and killed on drop.
    /// Behind an async mutex so the timeout path can `kill_group` it
    /// while a `round_trip` write may be in flight.
    child: AsyncMutex<Option<Child>>,
    stdin: AsyncMutex<ChildStdin>,
    next_id: AtomicU64,
    shared: Arc<Shared>,
    /// Kept (never awaited) so the tasks are not leaked: they end on
    /// stdout/stderr EOF or when the session is killed.
    _reader_handle: tokio::task::JoinHandle<()>,
    _stderr_handle: tokio::task::JoinHandle<()>,
}

impl PersistentSession {
    /// Spawns the configured command once, wires stdin/stdout/stderr, and
    /// starts the long-lived reader + stderr-drain tasks. Returns a handle
    /// whose child lives until it is dropped or a fatal error kills it.
    pub(crate) async fn spawn(spec: &SubprocessPluginSpec) -> Result<Self, SubprocessPluginError> {
        #[cfg(not(unix))]
        {
            return Err(SubprocessPluginError::Spawn {
                config_id: spec.config_id.clone(),
                detail: "the subprocess plugin host requires a unix host".into(),
            });
        }

        #[cfg(unix)]
        {
            use tokio::process::Command;

            let (program, args) =
                spec.command
                    .split_first()
                    .ok_or_else(|| SubprocessPluginError::Spawn {
                        config_id: spec.config_id.clone(),
                        detail: "plugin command is empty".into(),
                    })?;

            let mut command = Command::new(program);
            command
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                // Belt-and-suspenders for the `Drop`-time group kill: even
                // if our `Drop`'s `kill(-pgid)` is beaten to it, the leader
                // dies when the `Child` handle drops.
                .kill_on_drop(true)
                .process_group(0);

            let mut child = command
                .spawn()
                .map_err(|err| SubprocessPluginError::Spawn {
                    config_id: spec.config_id.clone(),
                    detail: format!("failed to spawn '{program}': {err}"),
                })?;

            let pgid = child.id().ok_or_else(|| SubprocessPluginError::Spawn {
                config_id: spec.config_id.clone(),
                detail: "spawned plugin process exited before its pid could be read".into(),
            })? as i32;

            let stdin = child.stdin.take().expect("piped stdin");
            let stdout = child.stdout.take().expect("piped stdout");
            let stderr = child.stderr.take().expect("piped stderr");

            let shared = Arc::new(Shared {
                pending: Mutex::new(HashMap::new()),
                dead: AtomicBool::new(false),
                death: Mutex::new(None),
            });

            // The long-lived NDJSON reader: reads stdout line-by-line and
            // routes each line to the waiting `round_trip` by JSON-RPC `id`.
            // This is a SEPARATE task from stderr and from `child.wait()` --
            // no `tokio::join!` here joins `child.wait()` against piped
            // stdio (the four-way-join starvation hazard this item's spec
            // names). The reader ends on stdout EOF or a malformed frame.
            let reader_shared = shared.clone();
            let reader_config_id = spec.config_id.clone();
            let reader_handle = tokio::spawn(async move {
                let mut reader = BufReader::new(stdout);
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line).await {
                        Ok(0) => {
                            // EOF with nothing read: a clean session end. (A
                            // trailing partial line with no terminating `\n`
                            // is returned as `Ok(n > 0)`, NOT `Ok(0)`, so it is
                            // handled in the `Ok(_)` arm below -- where it fails
                            // JSON parsing and is classified as a
                            // `MalformedFrame`. It never reaches this arm, so
                            // a bare EOF here is a plain session death.)
                            reader_shared.kill_all(SubprocessPluginError::SessionDied {
                                config_id: reader_config_id.clone(),
                                detail: "closed stdout (EOF) mid-session".into(),
                            });
                            return;
                        }
                        Ok(_) => {
                            // A full line (read_line includes the trailing
                            // `\n`). Parse it as JSON and route by `id`.
                            let bytes = line.trim_end().as_bytes();
                            if bytes.is_empty() {
                                // An empty line is not a valid JSON-RPC
                                // response; fail closed.
                                reader_shared.kill_all(SubprocessPluginError::MalformedFrame {
                                    config_id: reader_config_id.clone(),
                                    detail: "wrote an empty line to stdout".into(),
                                });
                                return;
                            }
                            let value: serde_json::Value = match serde_json::from_slice(bytes) {
                                Ok(v) => v,
                                Err(err) => {
                                    reader_shared.kill_all(SubprocessPluginError::MalformedFrame {
                                        config_id: reader_config_id.clone(),
                                        detail: format!(
                                            "wrote a line that is not valid JSON: {err}"
                                        ),
                                    });
                                    return;
                                }
                            };
                            let id = value.get("id").and_then(|v| v.as_u64());
                            let id = match id {
                                Some(id) => id,
                                None => {
                                    // No correlation `id` on the wire. Today
                                    // there are no notifications, so this is
                                    // a malformed frame, not a notification.
                                    reader_shared.kill_all(SubprocessPluginError::MalformedFrame {
                                        config_id: reader_config_id.clone(),
                                        detail: format!(
                                            "wrote a response with no JSON-RPC `id` field: {value}"
                                        ),
                                    });
                                    return;
                                }
                            };
                            let tx = {
                                let mut pending =
                                    reader_shared.pending.lock().expect("pending poisoned");
                                pending.remove(&id)
                            };
                            match tx {
                                Some(tx) => {
                                    // The waiting `round_trip` parses + classifies the value.
                                    let _ = tx.send(value);
                                }
                                None => {
                                    // No outstanding request for this id
                                    // (duplicate, or a late response after
                                    // timeout). Drop it -- the call already
                                    // failed closed via its own timeout path.
                                }
                            }
                        }
                        Err(err) => {
                            reader_shared.kill_all(SubprocessPluginError::SessionDied {
                                config_id: reader_config_id.clone(),
                                detail: format!("stdout read failed: {err}"),
                            });
                            return;
                        }
                    }
                }
            });

            // The concurrent stderr drain: discards stderr to EOF so a
            // plugin that writes to stderr with nobody reading it cannot
            // block. Drained bytes are DISCARDED -- no log/event sink is
            // wired (mirroring the one-shot path's own disclosure).
            let stderr_handle = tokio::spawn(async move {
                let mut reader = BufReader::new(stderr);
                let mut sink = tokio::io::sink();
                let _ = tokio::io::copy(&mut reader, &mut sink).await;
            });

            Ok(Self {
                config_id: spec.config_id.clone(),
                pgid,
                timeout_ms: spec.timeout_ms,
                child: AsyncMutex::new(Some(child)),
                stdin: AsyncMutex::new(stdin),
                next_id: AtomicU64::new(1),
                shared,
                _reader_handle: reader_handle,
                _stderr_handle: stderr_handle,
            })
        }
    }

    /// The OS pid of the spawned child -- used by acceptance criterion 1's
    /// test to assert the SAME process answers two sequential calls. Not
    /// used internally; kept as a `pub` affordance so a future diagnostics
    /// surface can name which child a session owns. (Criterion 1's
    /// load-bearing test instead uses a fixture plugin that reports its
    /// OWN `os.getpid()` over `tool/1`, so the assertion is end-to-end
    /// through the wire, not a host-internal read.)
    #[cfg(unix)]
    pub fn pid(&self) -> u32 {
        self.pgid as u32
    }

    /// True once the session has been torn down (child died, malformed
    /// frame, or explicit kill). A subsequent `round_trip` fails fast.
    fn is_dead(&self) -> bool {
        self.shared.dead.load(Ordering::Acquire)
    }

    /// The typed death reason (if the session is dead), mapped onto the
    /// `ToolError` variant the runtime sees. `None` when the session is not
    /// dead (or the death reason was not recorded -- should not happen, but
    /// a caller falls back to a generic `SessionDied` in that case).
    fn death_tool_error(&self) -> Option<ToolError> {
        let death = self.shared.death.lock().expect("death lock poisoned");
        death.as_ref().map(|err| {
            // Clone the error: `SubprocessPluginError` is `Clone` (every
            // variant is `String`s).
            err.clone().into_tool_error()
        })
    }

    /// One `tool/1` round-trip over the persistent channel: assigns a
    /// JSON-RPC `id`, writes the framed request line, and awaits the
    /// correlated response, bounded by `spec.timeout_ms` (a per-call
    /// deadline, NOT a session-wide idle kill). Returns the classified
    /// [`WireToolResult`]. Fail-closed on every failure mode -- dead
    /// session, write failure, timeout, malformed frame, or an `id`
    /// mismatch -- never a hang and never a silent retry.
    pub(crate) async fn tool_round_trip(
        &self,
        tool: String,
        call_id: String,
        arguments: serde_json::Value,
    ) -> Result<WireToolResult, ToolError> {
        if self.is_dead() {
            return Err(self.death_tool_error().unwrap_or_else(|| {
                SubprocessPluginError::SessionDied {
                    config_id: self.config_id.clone(),
                    detail: "session is no longer alive (re-discover to spawn a fresh one)".into(),
                }
                .into_tool_error()
            }));
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = PersistentToolRequest::tool_v1(id, tool, call_id, arguments);
        let mut json = serde_json::to_vec(&request).map_err(|err| ToolError::Internal {
            detail: format!("failed to serialize persistent tool/1 request: {err}"),
        })?;
        json.push(b'\n');

        let (tx, rx) = oneshot::channel::<serde_json::Value>();
        {
            let mut pending = self.shared.pending.lock().expect("pending poisoned");
            // Double-check dead under the lock so a death between the
            // `is_dead` check above and the insert still fails closed
            // (the reader would have drained `pending` via `kill_all`).
            if self.is_dead() {
                drop(pending);
                return Err(self.death_tool_error().unwrap_or_else(|| {
                    SubprocessPluginError::SessionDied {
                        config_id: self.config_id.clone(),
                        detail: "session died while this call was being registered".into(),
                    }
                    .into_tool_error()
                }));
            }
            pending.insert(id, tx);
        }

        // Write the framed request line, bounded by the SAME per-call
        // deadline as the read below. A write left unbounded would hang if the
        // child stops draining stdin while staying alive (the OS pipe buffer
        // fills, `write_all`/`flush` block for space that never comes) -- the
        // one-shot path bounds its whole `drive` under one `timeout_at`, and
        // this restores that parity for the persistent write. The `stdin` lock
        // is acquired INSIDE the timed future: a concurrent call blocked on
        // the shared lock is bounded by ITS own timeout, and its
        // `kill_group_now` SIGKILLs the child, unblocking the hung write via
        // the broken pipe -- so no call, and no lock waiter, can hang.
        match timeout(Duration::from_millis(self.timeout_ms), async {
            let mut stdin = self.stdin.lock().await;
            if let Err(err) = stdin.write_all(&json).await {
                return Err(format!("write to plugin stdin failed: {err}"));
            }
            if let Err(err) = stdin.flush().await {
                return Err(format!("flush of plugin stdin failed: {err}"));
            }
            Ok::<(), String>(())
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(detail)) => {
                // A write/flush failure (broken pipe -- the child died) is a
                // `SessionDied`, fail-closed.
                self.remove_pending(id);
                let err = SubprocessPluginError::SessionDied {
                    config_id: self.config_id.clone(),
                    detail,
                };
                self.shared.kill_all(err.clone());
                return Err(err.into_tool_error());
            }
            Err(_elapsed) => {
                // The write did not complete within the per-call deadline:
                // remove the pending entry, kill the process group (the
                // SIGKILL unblocks any hung `write_all` via the broken pipe),
                // and report `TimedOut` -- never a hang.
                self.remove_pending(id);
                self.kill_group_now().await;
                return Err(SubprocessPluginError::TimedOut {
                    config_id: self.config_id.clone(),
                    after_ms: self.timeout_ms,
                }
                .into_tool_error());
            }
        }

        // Await the correlated response, bounded by the per-call timeout.
        match timeout(Duration::from_millis(self.timeout_ms), rx).await {
            Ok(Ok(value)) => {
                // Parse + classify the response, then correlate the echoed
                // `id` against the request's. A parse error is a malformed
                // frame (the reader already parsed once to route; this
                // second parse is the structural classification + id check).
                let bytes = serde_json::to_vec(&value).map_err(|err| ToolError::Internal {
                    detail: format!("failed to re-serialize persistent response: {err}"),
                })?;
                let (resp_id, result) =
                    parse_persistent_tool_response(&bytes).map_err(|detail| {
                        // A malformed response frame kills the session, fail-closed.
                        let err = SubprocessPluginError::MalformedFrame {
                            config_id: self.config_id.clone(),
                            detail,
                        };
                        self.shared.kill_all(err.clone());
                        err.into_tool_error()
                    })?;
                if resp_id != id {
                    let err = SubprocessPluginError::SessionDied {
                        config_id: self.config_id.clone(),
                        detail: format!("response id {resp_id} did not match request id {id}"),
                    };
                    self.shared.kill_all(err.clone());
                    return Err(err.into_tool_error());
                }
                Ok(result)
            }
            Ok(Err(_canceled)) => {
                // The reader dropped the sender -- the session died while we
                // were waiting. `kill_all` already recorded the typed death
                // reason; surface THAT, not a generic "session died".
                Err(self.death_tool_error().unwrap_or_else(|| {
                    SubprocessPluginError::SessionDied {
                        config_id: self.config_id.clone(),
                        detail: "the session died before it answered this call".into(),
                    }
                    .into_tool_error()
                }))
            }
            Err(_elapsed) => {
                // Per-call timeout: remove the pending entry, kill the
                // process group (graceful SIGTERM-then-SIGKILL), mark dead.
                self.remove_pending(id);
                self.kill_group_now().await;
                Err(SubprocessPluginError::TimedOut {
                    config_id: self.config_id.clone(),
                    after_ms: self.timeout_ms,
                }
                .into_tool_error())
            }
        }
    }

    fn remove_pending(&self, id: u64) {
        let mut pending = self.shared.pending.lock().expect("pending poisoned");
        pending.remove(&id);
    }

    /// Kills the process group with the graceful SIGTERM-then-SIGKILL
    /// sequence (reusing `crate::unix::kill_group`, the documented
    /// duplicate of `conway_tools::process::unix::kill_group`) and marks
    /// the session dead. Used on the per-call timeout path. `kill_group`
    /// reaps the child itself (it `wait`s for exit under `TERM_GRACE`, then
    /// again after the SIGKILL fallback), so no separate reap is needed here.
    async fn kill_group_now(&self) {
        self.shared.dead.store(true, Ordering::Release);
        let mut child_guard = self.child.lock().await;
        if let Some(child) = child_guard.as_mut() {
            kill_group(child, self.pgid).await;
        }
    }
}

impl Drop for PersistentSession {
    fn drop(&mut self) {
        // `Drop` cannot `await` the graceful `kill_group`, so the
        // process-group SIGKILL is sent synchronously here (best-effort --
        // `kill_on_drop(true)` on the `Command` is the belt-and-suspenders
        // that kills the leader even if this `kill` is beaten to it). A
        // long-lived child is never orphaned: either this SIGKILL reaches
        // the group, or `kill_on_drop` reaches the leader.
        #[cfg(unix)]
        {
            use nix::sys::signal::{kill, Signal};
            use nix::unistd::Pid;
            let _ = kill(Pid::from_raw(-self.pgid), Signal::SIGKILL);
        }
        // `child` (still present unless a timeout already reaped it) is
        // dropped here; `kill_on_drop(true)` ensures the leader is killed.
        // The reader/stderr tasks end on the resulting stdout/stderr EOF.
    }
}
