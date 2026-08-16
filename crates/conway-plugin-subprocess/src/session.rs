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
//! **`initialize/1` handshake at session open (board item
//! `01M03VK7MRPSAVWMW7YNYPRPGT`).** Before any `tool/1` call, the host
//! exchanges ONE `initialize/1` request/response with the plugin over the
//! SAME id-correlated NDJSON framing -- the request is a line with its own
//! JSON-RPC `id`, the plugin's answer carries the echoed `id`, and the
//! existing reader task routes it by `id` through the SAME pending table (NO
//! second reader; the handshake reuses `tool_round_trip`'s framing via the
//! shared `PersistentSession::framed_round_trip` helper). The host sends
//! its `wire_major`/`wire_minor` and the points it speaks (today
//! `["tool/1"]`); the plugin answers its own `major`, the minimum `minor` it
//! requires (`minor_min`), and the per-point versions it declares. The host
//! then applies `docs/plugins/compatibility.md`'s version-negotiation table:
//! refuse on `major` mismatch or unsatisfied `minor_min`
//! (`SubprocessPluginError::HandshakeRefused`); accept otherwise; unknown
//! fields in the plugin's answer are IGNORED-AND-COUNTED (the table's accept
//! branch / forward-compat rule), never rejected. A structurally-invalid
//! answer is `SubprocessPluginError::HandshakeMalformed` (fail closed). The
//! handshake runs in `SubprocessPlugin::discover` BEFORE the session is
//! wrapped in `Arc`, so a refusal surfaces at discover time and the
//! just-spawned child is dropped (its `Drop` kills the group), never
//! orphaned. The plugin's declared per-point versions are stored on the
//! session (`PersistentSession::point_version`) for the later wire-point
//! items (permission.policy, observe, status, context.hook) to consult
//! WITHOUT re-negotiating.
//!
//! **`permission.policy/1` declaration immediately after the handshake
//! (board item `01M03VKJG7JJ0JEKY265WA7MJ7`).** Right after `initialize/1`
//! succeeds, `PersistentSession::request_permission_policy` exchanges ONE
//! `permission.policy/1` request/response over the SAME id-correlated
//! NDJSON framing (NO second reader). Version negotiation is against the
//! per-point record `initialize` just produced: a plugin declaring the
//! point at a SUPPORTED version exchanges its per-tool NARROWING policy
//! (`deny`/`prompt`/`abstain` -- NO `allow`, by type construction: a plugin
//! may only narrow, never widen); an UNSUPPORTED version REFUSES the plugin
//! at discover (`HandshakeRefused` naming the mismatch -- the participant
//! rule); a plugin that does NOT declare the point loads NORMALLY and
//! contributes no wire policy (advertising a point means the host speaks
//! it, not that it requires it). The declared rules are stored on the
//! session and surfaced via `SubprocessPlugin::permission_rules` (the
//! `Plugin` trait method the `conway` facade installs as `PatternOrigin::
//! Plugin` deny/prompt rules in the `PermissionBroker`, advisory-under-
//! enforcement and subordinate to the operator's own config). A malformed
//! policy answer is `HandshakeMalformed`, fail-closed (never silently
//! no-op); the just-spawned child is dropped on any failure (its `Drop`
//! kills the group), never orphaned.
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
use crate::wire::{
    parse_persistent_initialize_response, parse_persistent_permission_policy_response,
    parse_persistent_tool_response, InitializeParseError, PermissionPolicyAnswer,
    PermissionPolicyParseError, PersistentInitializeRequest, PersistentPermissionPolicyRequest,
    PersistentToolRequest, WirePermissionRule, WireToolResult, HOST_PERMISSION_POLICY_VERSION,
    HOST_WIRE_MAJOR, HOST_WIRE_MINOR,
};
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
    /// The plugin's declared per-point versions, recorded ONCE by
    /// [`PersistentSession::initialize`] from the `initialize/1` handshake
    /// answer, keyed by point name (e.g. `"tool/1"`). Read by later
    /// wire-point items (permission.policy, observe, status, context.hook)
    /// via [`PersistentSession::point_version`] to decide per-point
    /// refuse-vs-degrade WITHOUT re-negotiating. Empty until a successful
    /// handshake populates it.
    point_versions: Mutex<HashMap<String, u32>>,
    /// The per-tool permission policy the plugin declared over
    /// `permission.policy/1` at session open (board item
    /// `01M03VKJG7JJ0JEKY265WA7MJ7`), recorded ONCE by
    /// [`Self::request_permission_policy`] and read by
    /// [`Self::permission_rules`]. Empty until the one-time
    /// `permission.policy/1` exchange populates it -- which itself runs
    /// ONLY when the plugin declared the point at a supported version (a
    /// plugin that does not declare `permission.policy/1` contributes no
    /// wire policy and this stays empty; see
    /// [`Self::request_permission_policy`]'s own doc for the
    /// version-negotiation behavior).
    permission_policy: Mutex<Vec<WirePermissionRule>>,
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
                point_versions: Mutex::new(HashMap::new()),
                permission_policy: Mutex::new(Vec::new()),
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

    /// The typed death reason (if the session is dead), as a
    /// [`SubprocessPluginError`]. `None` when the session is not dead (or the
    /// death reason was not recorded -- should not happen, but a caller falls
    /// back to a generic `SessionDied` in that case). The handshake's
    /// `initialize` method uses this directly (it returns
    /// `SubprocessPluginError`, not `ToolError`); `tool_round_trip` and the
    /// dead-session fast paths map it onto `ToolError` via
    /// [`SubprocessPluginError::into_tool_error`] through [`Self::death_tool_error`].
    fn death_error(&self) -> Option<SubprocessPluginError> {
        let death = self.shared.death.lock().expect("death lock poisoned");
        // Clone the error: `SubprocessPluginError` is `Clone` (every
        // variant is `String`s).
        death.as_ref().cloned()
    }

    /// The typed death reason (if the session is dead), mapped onto the
    /// `ToolError` variant the runtime sees. `None` when the session is not
    /// dead (or the death reason was not recorded -- should not happen, but
    /// a caller falls back to a generic `SessionDied` in that case).
    fn death_tool_error(&self) -> Option<ToolError> {
        self.death_error().map(|err| err.into_tool_error())
    }

    /// The shared id-correlated NDJSON round-trip both `tool/1` and
    /// `initialize/1` use: registers a oneshot sender in the pending table
    /// under `id` (double-checking dead under the lock so a death between the
    /// caller's `is_dead` check and the insert still fails closed), writes the
    /// already-serialized `\n`-terminated `json` request line under the
    /// per-call write deadline, then awaits the correlated response under the
    /// per-call read deadline. Returns the routed raw [`serde_json::Value`] on
    /// success; the CALLER parses + classifies it (with
    /// [`parse_persistent_tool_response`] or
    /// [`parse_persistent_initialize_response`]) and checks the echoed `id`.
    ///
    /// Fail-closed on every failure mode -- dead session, write failure,
    /// per-call timeout, or the reader dropping the sender (session died
    /// mid-call) -- never a hang and never a silent retry. The write deadline
    /// is the load-bearing property the wedge regression pins: a child that
    /// stops draining stdin while staying alive makes `write_all` block once
    /// the OS pipe buffer fills, and this deadline bounds that block (the
    /// `kill_group_now` SIGKILL unblocks the hung write via the broken pipe).
    /// Returns [`SubprocessPluginError`] (not `ToolError`) so the handshake's
    /// `initialize` can surface it directly; `tool_round_trip` maps it onto
    /// `ToolError` via [`SubprocessPluginError::into_tool_error`].
    async fn framed_round_trip(
        &self,
        id: u64,
        json: Vec<u8>,
    ) -> Result<serde_json::Value, SubprocessPluginError> {
        let (tx, rx) = oneshot::channel::<serde_json::Value>();
        {
            let mut pending = self.shared.pending.lock().expect("pending poisoned");
            // Double-check dead under the lock so a death between the
            // caller's `is_dead` check and the insert still fails closed
            // (the reader would have drained `pending` via `kill_all`).
            if self.is_dead() {
                drop(pending);
                return Err(self.death_error().unwrap_or_else(|| {
                    SubprocessPluginError::SessionDied {
                        config_id: self.config_id.clone(),
                        detail: "session died while this call was being registered".into(),
                    }
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
                return Err(err);
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
                });
            }
        }

        // Await the correlated response, bounded by the per-call timeout.
        match timeout(Duration::from_millis(self.timeout_ms), rx).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_canceled)) => {
                // The reader dropped the sender -- the session died while we
                // were waiting. `kill_all` already recorded the typed death
                // reason; surface THAT, not a generic "session died".
                Err(self
                    .death_error()
                    .unwrap_or_else(|| SubprocessPluginError::SessionDied {
                        config_id: self.config_id.clone(),
                        detail: "the session died before it answered this call".into(),
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
                })
            }
        }
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

        let value = self
            .framed_round_trip(id, json)
            .await
            .map_err(SubprocessPluginError::into_tool_error)?;

        // Parse + classify the response, then correlate the echoed `id`
        // against the request's. A parse error is a malformed frame (the
        // reader already parsed once to route; this second parse is the
        // structural classification + id check).
        let bytes = serde_json::to_vec(&value).map_err(|err| ToolError::Internal {
            detail: format!("failed to re-serialize persistent response: {err}"),
        })?;
        let (resp_id, result) = parse_persistent_tool_response(&bytes).map_err(|detail| {
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

    /// The one-time `initialize/1` version-negotiation handshake (board item
    /// `01M03VK7MRPSAVWMW7YNYPRPGT`), exchanged ONCE at persistent-session
    /// open BEFORE any `tool/1` call. Rides the SAME id-correlated NDJSON
    /// framing `tool_round_trip` uses (via [`Self::framed_round_trip`]); NO second
    /// reader -- the existing reader routes the answer by `id` through the
    /// SAME pending table. Sends this host's `wire_major`/`wire_minor` and
    /// the points it speaks (today `["tool/1"]`), receives the plugin's own
    /// `major`/`minor_min`/per-point versions, then applies
    /// `docs/plugins/compatibility.md`'s version-negotiation table:
    ///
    /// - `plugin.major != HOST_WIRE_MAJOR` -> [`HandshakeRefused`] ("major
    ///   mismatch"), naming both majors.
    /// - `plugin.minor_min > HOST_WIRE_MINOR` -> [`HandshakeRefused`] ("minor_min
    ///   unsatisfied"), naming the required minor and the host's minor.
    /// - else -> accept. Unknown FIELDS in the plugin's answer were already
    ///   ignored-and-counted by [`parse_persistent_initialize_response`] (the
    ///   table's accept branch / forward-compat rule); the plugin's declared
    ///   per-point versions are stored on `self.point_versions` for later
    ///   wire-point items to read via [`Self::point_version`].
    ///
    /// A structurally-invalid answer (missing `ok`, `ok:false` with no error,
    /// a non-number where a number was expected) is [`HandshakeMalformed`],
    /// fail-closed. A plugin that closes stdout without answering surfaces as
    /// [`SessionDied`] (the reader's EOF `kill_all`); a plugin that never
    /// answers within `timeout_ms` surfaces as [`TimedOut`] -- both via
    /// [`Self::framed_round_trip`], never a hang. On any failure the just-spawned
    /// session is dropped by `discover`'s `?`, and its `Drop` kills the
    /// process group, so the child is never orphaned.
    ///
    /// `host.version` is put on the wire for the plugin to read but NEVER
    /// branched on here -- the negotiation compares ONLY `major` and
    /// `minor_min`. A host version bump does not change the negotiation
    /// outcome (see `tests/handshake.rs`'s host-version-is-informational
    /// test).
    ///
    /// [`HandshakeRefused`]: SubprocessPluginError::HandshakeRefused
    /// [`HandshakeMalformed`]: SubprocessPluginError::HandshakeMalformed
    /// [`SessionDied`]: SubprocessPluginError::SessionDied
    /// [`TimedOut`]: SubprocessPluginError::TimedOut
    pub(crate) async fn initialize(&self) -> Result<(), SubprocessPluginError> {
        if self.is_dead() {
            return Err(self
                .death_error()
                .unwrap_or_else(|| SubprocessPluginError::SessionDied {
                    config_id: self.config_id.clone(),
                    detail: "session died before initialize could be sent".into(),
                }));
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = PersistentInitializeRequest::new(id);
        let mut json = serde_json::to_vec(&request).map_err(|err| {
            SubprocessPluginError::HandshakeMalformed {
                config_id: self.config_id.clone(),
                detail: format!("failed to serialize initialize/1 request: {err}"),
            }
        })?;
        json.push(b'\n');

        let value = self.framed_round_trip(id, json).await?;

        let bytes = serde_json::to_vec(&value).map_err(|err| {
            SubprocessPluginError::HandshakeMalformed {
                config_id: self.config_id.clone(),
                detail: format!("failed to re-serialize initialize response: {err}"),
            }
        })?;
        let answer = parse_persistent_initialize_response(&bytes).map_err(|e| match e {
            // A structurally-broken answer: the plugin is broken, not
            // declining. Fail closed as HandshakeMalformed.
            InitializeParseError::Malformed(detail) => SubprocessPluginError::HandshakeMalformed {
                config_id: self.config_id.clone(),
                detail,
            },
            // A deliberate `ok:false` WITH an `error` string: the plugin
            // declined initialize. Surface as HandshakeRefused so the
            // operator-facing variant honestly distinguishes "the plugin
            // declined" from "the plugin is broken" -- the same split the
            // version-mismatch rows below already use HandshakeRefused for.
            InitializeParseError::Refused(detail) => SubprocessPluginError::HandshakeRefused {
                config_id: self.config_id.clone(),
                condition: "ok false".into(),
                detail,
            },
        })?;

        // Correlate the echoed `id` -- a mismatch is a protocol error, fail
        // closed (mirroring `tool_round_trip`'s id check).
        if answer.id != id {
            return Err(SubprocessPluginError::HandshakeMalformed {
                config_id: self.config_id.clone(),
                detail: format!(
                    "initialize response id {} did not match request id {id}",
                    answer.id
                ),
            });
        }

        // Surface the unknown-field count (the compatibility table's accept
        // branch / forward-compat rule) at the session level so the log names
        // WHICH plugin carried the extra fields -- a newer plugin's extra
        // field does not break an older host; the count is auditable
        // out-of-band, never rejected.
        if answer.unknown_field_count > 0 {
            tracing::debug!(
                unknown_field_count = answer.unknown_field_count,
                config_id = %self.config_id,
                "initialize/1 answer carried unknown fields; ignored and counted \
                 (forward-compat: a newer plugin's extra field does not break an older host)"
            );
        }

        // Apply the compatibility table (version-negotiation rows).
        if answer.major != HOST_WIRE_MAJOR {
            return Err(SubprocessPluginError::HandshakeRefused {
                config_id: self.config_id.clone(),
                condition: "major mismatch".into(),
                detail: format!(
                    "host wire_major {HOST_WIRE_MAJOR} != plugin major {} (incompatible frame \
                     vocabulary -- a major bump covers method names / envelope semantics, the \
                     two sides cannot agree on what a frame means)",
                    answer.major
                ),
            });
        }
        if answer.minor_min > HOST_WIRE_MINOR {
            return Err(SubprocessPluginError::HandshakeRefused {
                config_id: self.config_id.clone(),
                condition: "minor_min unsatisfied".into(),
                detail: format!(
                    "plugin requires wire_minor >= {} but host wire_minor is {HOST_WIRE_MINOR} \
                     (plugin needs a feature this host does not have)",
                    answer.minor_min
                ),
            });
        }

        // Accept: record the plugin's declared per-point versions for the
        // later wire-point items (permission.policy, observe, status,
        // context.hook) to consult WITHOUT re-negotiating.
        {
            let mut pv = self.point_versions.lock().expect("point_versions poisoned");
            *pv = answer.points;
        }
        Ok(())
    }

    /// The plugin's declared version for a wire point (e.g. `"tool/1"`),
    /// recorded ONCE by `initialize` from the `initialize/1` handshake
    /// answer. `None` before a successful handshake, or for a point the
    /// plugin did not declare. Later wire-point items
    /// (`permission.policy/1`, `observe/1`, `status/1`, `context.hook/1`)
    /// consult this to decide per-point refuse-vs-degrade per
    /// `docs/plugins/compatibility.md`'s participant-vs-observer table rows,
    /// WITHOUT re-negotiating.
    pub fn point_version(&self, point: &str) -> Option<u32> {
        let pv = self.point_versions.lock().expect("point_versions poisoned");
        pv.get(point).copied()
    }

    /// The per-tool permission policy the plugin declared over
    /// `permission.policy/1` at session open (board item
    /// `01M03VKJG7JJ0JEKY265WA7MJ7`), recorded ONCE by
    /// [`Self::request_permission_policy`]. Empty for a plugin that did not
    /// declare the point (it contributes no wire policy) or before the
    /// one-time exchange has run. `SubprocessPlugin::permission_rules`
    /// delegates here -- the `Plugin` trait method the `conway` facade
    /// consults to install `PatternOrigin::Plugin` deny/prompt rules in the
    /// `PermissionBroker`, advisory-under-enforcement and subordinate to
    /// the operator's own config.
    pub(crate) fn permission_rules(&self) -> Vec<WirePermissionRule> {
        self.permission_policy
            .lock()
            .expect("permission_policy poisoned")
            .clone()
    }

    /// The one-time `permission.policy/1` declaration exchange (board item
    /// `01M03VKJG7JJ0JEKY265WA7MJ7`), run ONCE at persistent-session open,
    /// AFTER [`Self::initialize`] succeeds and BEFORE any `tool/1` call. A
    /// session-scoped static declaration (per-tool narrowing verdicts), not
    /// a per-call evaluation -- the request carries no payload, the plugin's
    /// answer is the policy. Rides the SAME id-correlated NDJSON framing
    /// `initialize/1` and `tool/1` use (via [`Self::framed_round_trip`]); NO
    /// second reader.
    ///
    /// **Version negotiation via [`Self::point_version`]** (the record
    /// `initialize/1` produced), per `docs/plugins/compatibility.md`'s
    /// participant-vs-observer table:
    ///
    /// - Plugin declared `permission.policy/1` at a SUPPORTED version
    ///   (`== [`HOST_PERMISSION_POLICY_VERSION`]`) -> exchange the policy,
    ///   store the rules, enforce as advisory. Unknown FIELDS in the answer
    ///   are ignored-and-counted (the table's accept branch / forward-compat
    ///   rule), surfaced via `tracing::debug!`.
    /// - Plugin declared it at an UNSUPPORTED version -> REFUSE to load
    ///   ([`SubprocessPluginError::HandshakeRefused`]), naming BOTH the
    ///   host's and the plugin's versions -- the participant rule: a plugin
    ///   speaking a point at an incompatible version is refused, never
    ///   silently never-run. Surfaces at `discover` as `ToolError::Internal`
    ///   via [`SubprocessPluginError::into_tool_error`].
    /// - Plugin did NOT declare `permission.policy/1` (`point_version` is
    ///   `None`) -> the plugin contributes no wire policy; LOAD NORMALLY and
    ///   enforce the operator's config alone. **Advertising a point means
    ///   the host speaks it, not that the host requires it**; a plugin
    ///   speaking a subset is fine, and the participant refusal is
    ///   VERSION-gated (both speak the point at incompatible versions), not
    ///   presence-gated.
    ///
    /// A structurally-invalid answer (missing `ok`, `ok:false` with no
    /// `error`, an unknown `verdict` tag, a per-rule entry missing
    /// `tool`/`verdict`) is [`SubprocessPluginError::HandshakeMalformed`],
    /// fail-closed (acceptance criterion 3: never silently no-op). A plugin
    /// that closes stdout without answering surfaces as [`SessionDied`]; a
    /// plugin that never answers within `timeout_ms` surfaces as
    /// [`TimedOut`] -- both via [`Self::framed_round_trip`], never a hang.
    ///
    /// [`SessionDied`]: SubprocessPluginError::SessionDied
    /// [`TimedOut`]: SubprocessPluginError::TimedOut
    pub(crate) async fn request_permission_policy(
        &self,
    ) -> Result<Vec<WirePermissionRule>, SubprocessPluginError> {
        // Version negotiation against the record `initialize/1` produced.
        // `None` (the plugin did not declare the point) is NOT an error: the
        // plugin contributes no wire policy, and the operator's config alone
        // is enforced. This is the "advertising != requiring" rule -- a
        // plugin speaking a subset of the host's points loads normally.
        let version = match self.point_version(PersistentPermissionPolicyRequest::OP) {
            None => return Ok(Vec::new()),
            Some(v) => v,
        };
        if version != HOST_PERMISSION_POLICY_VERSION {
            return Err(SubprocessPluginError::HandshakeRefused {
                config_id: self.config_id.clone(),
                condition: "permission.policy/1 version mismatch".into(),
                detail: format!(
                    "host speaks permission.policy/1 version {HOST_PERMISSION_POLICY_VERSION} but \
                     plugin declared version {version} (participant point: an incompatible version \
                     is refused, never silently never-run)"
                ),
            });
        }

        if self.is_dead() {
            return Err(self
                .death_error()
                .unwrap_or_else(|| SubprocessPluginError::SessionDied {
                    config_id: self.config_id.clone(),
                    detail: "session died before permission.policy/1 could be sent".into(),
                }));
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = PersistentPermissionPolicyRequest::new(id);
        let mut json = serde_json::to_vec(&request).map_err(|err| {
            SubprocessPluginError::HandshakeMalformed {
                config_id: self.config_id.clone(),
                detail: format!("failed to serialize permission.policy/1 request: {err}"),
            }
        })?;
        json.push(b'\n');

        let value = self.framed_round_trip(id, json).await?;

        let bytes = serde_json::to_vec(&value).map_err(|err| {
            SubprocessPluginError::HandshakeMalformed {
                config_id: self.config_id.clone(),
                detail: format!("failed to re-serialize permission.policy response: {err}"),
            }
        })?;
        let answer: PermissionPolicyAnswer = parse_persistent_permission_policy_response(&bytes)
            .map_err(|e| match e {
                PermissionPolicyParseError::Malformed(detail) => {
                    SubprocessPluginError::HandshakeMalformed {
                        config_id: self.config_id.clone(),
                        detail,
                    }
                }
                PermissionPolicyParseError::Refused(detail) => {
                    SubprocessPluginError::HandshakeRefused {
                        config_id: self.config_id.clone(),
                        condition: "ok false".into(),
                        detail,
                    }
                }
            })?;

        // Correlate the echoed `id` -- a mismatch is a protocol error, fail
        // closed (mirroring `initialize`'s id check).
        if answer.id != id {
            return Err(SubprocessPluginError::HandshakeMalformed {
                config_id: self.config_id.clone(),
                detail: format!(
                    "permission.policy response id {} did not match request id {id}",
                    answer.id
                ),
            });
        }

        if answer.unknown_field_count > 0 {
            tracing::debug!(
                unknown_field_count = answer.unknown_field_count,
                config_id = %self.config_id,
                "permission.policy/1 answer carried unknown fields; ignored and counted \
                 (forward-compat: a newer plugin's extra field does not break an older host)"
            );
        }

        // Store the declared policy for the session's lifetime.
        {
            let mut policy = self
                .permission_policy
                .lock()
                .expect("permission_policy poisoned");
            *policy = answer.rules.clone();
        }
        Ok(answer.rules)
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
