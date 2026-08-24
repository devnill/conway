//! The persistent JSON-RPC 2.0 over stdio transport for `conway-plugin-mcp`
//! (board item `01M03GPNF0KN59FHAEEAEY2JD3`): a long-lived child process -- the
//! external MCP SERVER -- spawned ONCE per plugin at discovery time, kept
//! alive across many `tools/call` invocations, framing requests/responses as
//! newline-delimited JSON-RPC 2.0 objects over the child's stdin/stdout.
//!
//! **A sibling transport to `conway-plugin-subprocess::session::PersistentSession`,
//! NOT a layering on it.** `PersistentSession` speaks conway's OWN wire
//! protocol (`initialize/1`, `tool.spec/1`, `tool/1`, `observe/1`,
//! `permission.policy/1`, `status.declare/1`/`status/1`) -- its parser is
//! conway-wire-specific, not generic JSON-RPC 2.0. MCP speaks a DIFFERENT
//! protocol (JSON-RPC 2.0: `initialize`, `notifications/initialized`,
//! `tools/list`, `tools/call`, capability structs). So this module owns its
//! OWN lightweight `McpSession` that reuses the PATTERN `PersistentSession`
//! proves out -- spawn once + `kill_on_drop(true)` child, a long-lived reader
//! task routing inbound lines by JSON-RPC `id` via a
//! `Mutex<HashMap<u64, oneshot::Sender<Value>>>`, a `framed_round_trip` that
//! writes one line and awaits the matching reply, a stderr drain, and
//! Drop-time group SIGKILL -- but parses JSON-RPC 2.0, not conway wire. This
//! crate does NOT depend on `conway-plugin-subprocess`; the two transports are
//! siblings, not a layering. Read `conway-plugin-subprocess/src/session.rs`
//! for the PATTERN this adapts.
//!
//! **Framing: NDJSON -- one JSON-RPC 2.0 object per line, `\n`-delimited, over
//! the child's stdin/stdout.** The same framing `PersistentSession` uses;
//! JSON-RPC 2.0 over stdio is NDJSON by the spec's own "one JSON object per
//! line, UTF-8, `\n`-delimited" rule.
//!
//! **Correlation: JSON-RPC `id` + an outstanding-request table.** Each
//! `initialize`/`tools/list`/`tools/call` request assigns a monotonic `id`,
//! inserts a `oneshot` sender into a pending table keyed by `id`, writes the
//! framed request line, and awaits the sender (bounded by the per-call
//! timeout). A long-lived reader task reads stdout line-by-line, parses each
//! line, extracts `id`, and routes the value to the matching pending sender.
//! A line with NO `id` is an inbound server-initiated NOTIFICATION (out of
//! scope for this minimal client -- dropped with a `tracing::warn!`, the
//! session is NOT torn down: an MCP server pushing a notification is
//! observer-class, not a malformed frame, the identical rule
//! `PersistentSession`'s reader applies to a no-`id` line).
//!
//! **`initialize` handshake once at session open.** Before any `tools/call`,
//! the client exchanges ONE `initialize` request/response (negotiate the
//! `tools` capability -- a server that does NOT offer `tools` is refused at
//! discover), sends the `notifications/initialized` notification (no `id`,
//! no reply expected), and calls `tools/list` to enumerate the server's
//! tools. The whole handshake rides the SAME id-correlated NDJSON framing
//! `tools/call` uses (via `framed_round_trip`); NO second reader.
//!
//! **Failure handling -- fail-closed uniformly, mirroring
//! `PersistentSession`/`SubprocessPluginError`'s discipline.** A session
//! that dies mid-call (the child exits, or closes stdout) surfaces a typed
//! [`crate::McpPluginError::SessionDied`], never a hang and never a silent
//! retry. **No automatic reconnect** (an MCP server that died has lost
//! whatever session state it had; the death is surfaced and the caller must
//! re-`discover`). Once a session is marked dead, every subsequent call fails
//! fast with `SessionDied` -- the session is NOT re-spawned. A server that
//! never answers a framed response is killed and reported
//! [`crate::McpPluginError::TimedOut`] within `timeout_ms` (the per-call
//! deadline on the framed read, NOT a session-wide idle kill -- a session
//! that legitimately sits idle between calls is left alone). An
//! unterminated/malformed frame (no newline, invalid JSON, a partial line
//! then EOF) is a typed [`crate::McpPluginError::MalformedFrame`], not a
//! deadlock. A JSON-RPC `error` response to `tools/call` (a protocol-level
//! failure) is mapped to `ToolError::Internal`; an MCP `isError: true` RESULT
//! (a tool-level failure) is mapped to a `ToolOutput` with `is_error: true`,
//! NOT an error -- the distinction is load-bearing in MCP.
//!
//! **Hazards (mirroring `PersistentSession`'s own disclosed hazards).**
//!
//! - **No `tokio::join!` joins `child.wait()` against piped stdio.** The reader
//!   is a long-lived `BufRead::read_line` loop in its OWN task, stderr is
//!   drained in its OWN task, and `child.wait()` is NEVER joined concurrently
//!   with either -- it runs only on drop / on a fatal kill, after the
//!   reader/stderr tasks have already torn down. No four-way-join starvation.
//! - **stderr is drained concurrently for the session's lifetime** (a server
//!   that writes to stderr with nobody reading it blocks); the drained bytes
//!   are DISCARDED, mirroring `PersistentSession`'s own "stderr is drained but
//!   discarded" disclosure.
//! - **Process-group kill on drop**: when `McpSession` is dropped, the
//!   process group is killed (best-effort SIGKILL on the group, synchronously
//!   -- `Drop` cannot `await`). `kill_on_drop(true)` is ALSO set on the
//!   `Command` as belt-and-suspenders. A long-lived child is never orphaned.
//! - **`kill_group` is SHARED, not duplicated (board item
//!   `01M0EKVR1BEXXS75NV2JC4HZZ9`)** -- `conway::plugin::kill_group` (the
//!   ONE implementation every crate that needs this now calls, re-exported
//!   from `conway_tools::process::unix::kill_group`) is used for the
//!   graceful timeout kill; the synchronous `Drop`-time SIGKILL below still
//!   uses `nix::sys::signal::kill` directly (`Drop` cannot `await`).

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{oneshot, Mutex as AsyncMutex};
use tokio::time::timeout;

use conway::plugin::{kill_group, CancellationToken, ToolError};

use crate::wire::{
    initialize_request, initialized_notification, parse_initialize_response,
    parse_tools_list_response, tools_call_request, tools_list_request, CallOutcome,
    InitializeResult,
};
use crate::{McpPluginError, McpPluginSpec};

/// State shared between the `McpSession` handle and the long-lived reader
/// task: the outstanding-request table (keyed by JSON-RPC `id`), the
/// session-dead flag, and -- when dead -- the typed reason the session died.
/// Held in an `Arc` so the reader task can route a response to the waiting
/// call and mark the session dead on EOF/parse error without holding a handle
/// to the `Child` itself. The PATTERN is `PersistentSession::Shared` exactly;
/// the parser behind it is JSON-RPC 2.0, not conway wire.
struct Shared {
    /// Outstanding-request table: `id` -> the `oneshot` sender a waiting
    /// `framed_round_trip` call is awaiting. Inserted by `framed_round_trip`
    /// before it writes the request line; removed by the reader task (on a
    /// routed response), by a [`PendingGuard`] whose `Drop` runs on EVERY
    /// `framed_round_trip` exit path (success, timeout, write-failure, AND a
    /// future dropped mid-await -- the cancel path that the future's own
    /// timeout branch does NOT reach, since the future is dropped before the
    /// timeout fires), or by `kill_all` (on session death, which drops every
    /// pending sender so its waiter sees `Err` and reads the death reason
    /// from `death`). The guard's removal is idempotent -- a no-op if the
    /// reader already routed the entry or `kill_all` already cleared the
    /// table.
    pending: Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>,
    /// Set when the session is torn down (child died, malformed frame, or
    /// explicit kill on timeout). Once true, every subsequent `framed_round_trip`
    /// fails fast -- no re-spawn.
    dead: AtomicBool,
    /// The typed reason the session died, set by `kill_all` under the same
    /// lock that drains `pending`. Read by a `framed_round_trip` whose sender
    /// was dropped (it got `Err` from `rx`) so it can surface the REAL failure
    /// mode rather than a generic "session died".
    death: Mutex<Option<McpPluginError>>,
}

impl Shared {
    /// Marks the session dead, records the typed death reason, and drops
    /// every pending sender so its waiter wakes with `Err` and surfaces
    /// `reason`. Called by the reader task on EOF / malformed frame, and by
    /// `framed_round_trip` on a write failure.
    fn kill_all(&self, reason: McpPluginError) {
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

    /// Removes the pending entry for `id`, if any. Idempotent -- a no-op when
    /// the reader already routed it, `kill_all` already cleared the table, or
    /// this id was never registered. Called by [`PendingGuard::drop`] so a
    /// `framed_round_trip` future dropped mid-await (a `tools/call` cancelled
    /// in flight) does not orphan its `oneshot::Sender` in the table.
    fn remove_pending(&self, id: u64) {
        let mut pending = self.pending.lock().expect("pending table poisoned");
        pending.remove(&id);
    }
}

/// RAII removal of a `framed_round_trip` pending-table entry. Created right
/// after `pending.insert(id, tx)`; its `Drop` runs on EVERY exit from
/// `framed_round_trip` -- the ordinary return paths (success, timeout,
/// write-failure) AND the cancel path, where the `tools_call` future is
/// dropped mid-await by `McpTool::invoke`'s `select!` taking the cancel arm.
/// That cancel path is the load-bearing one: the future's OWN timeout branch
/// never fires there (the future is dropped before the timeout future does),
/// so without this guard the `oneshot::Sender` would stay orphaned in the
/// pending table until the server answered or the session died. With the
/// guard, a late server response for a cancelled `id` finds no pending entry
/// and is dropped harmlessly by the reader's `None` arm. Idempotent via
/// [`Shared::remove_pending`], so the success path (where the reader already
/// removed the entry) is a no-op.
struct PendingGuard {
    shared: Arc<Shared>,
    id: u64,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.shared.remove_pending(self.id);
    }
}

/// A long-lived handle to one MCP server child process: owns the
/// `tokio::process::Child`, its stdin write half, and a long-lived reader
/// task that frames NDJSON responses off stdout and routes them by JSON-RPC
/// `id` to the waiting call. Built by `McpSession::spawn` (a `pub(crate)`
/// constructor `McpPlugin::discover` calls); used by the `Tool::invoke` impl
/// on `crate::McpTool`.
///
/// **Cloning.** An `McpSession` is NOT `Clone`; the plugin hands each `McpTool`
/// an `Arc<McpSession>`, so every tool on this plugin shares ONE child process
/// (the load-bearing property acceptance criterion 1 asserts for the
/// subprocess transport; an MCP server's tools share one server process too).
pub struct McpSession {
    config_id: String,
    pgid: i32,
    timeout_ms: u64,
    /// The child, held for the session's lifetime and killed on drop. Behind
    /// an async mutex so the timeout path can `kill_group` it while a
    /// `framed_round_trip` write may be in flight.
    child: AsyncMutex<Option<Child>>,
    /// The child's stdin write half, shared between `framed_round_trip`
    /// (id-correlated requests). Behind an `Arc<AsyncMutex>` so writes
    /// serialize -- two concurrent calls never corrupt each other's framing.
    stdin: Arc<AsyncMutex<ChildStdin>>,
    next_id: AtomicU64,
    shared: Arc<Shared>,
    /// Kept (never awaited) so the tasks are not leaked: they end on
    /// stdout/stderr EOF or when the session is killed.
    _reader_handle: tokio::task::JoinHandle<()>,
    _stderr_handle: tokio::task::JoinHandle<()>,
}

impl McpSession {
    /// Spawns the configured command once, wires stdin/stdout/stderr, and
    /// starts the long-lived reader + stderr-drain tasks. Returns a handle
    /// whose child lives until it is dropped or a fatal error kills it.
    pub(crate) async fn spawn(spec: &McpPluginSpec) -> Result<Self, McpPluginError> {
        #[cfg(not(unix))]
        {
            return Err(McpPluginError::Spawn {
                config_id: spec.config_id.clone(),
                detail: "the MCP-over-stdio client requires a unix host".into(),
            });
        }

        #[cfg(unix)]
        {
            use tokio::process::Command;

            let (program, args) =
                spec.command
                    .split_first()
                    .ok_or_else(|| McpPluginError::Spawn {
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

            // Credentials/connection-lifecycle scoping (acceptance 3): the
            // child inherits the PARENT env PLUS the entry's explicit `env`
            // pairs -- the identical shape `HookEntry` uses, so an operator
            // scopes credentials by naming them here rather than relying on
            // implicit inheritance. Explicit, not implicit.
            for (k, v) in &spec.env {
                command.env(k, v);
            }

            let mut child = command.spawn().map_err(|err| McpPluginError::Spawn {
                config_id: spec.config_id.clone(),
                detail: format!("failed to spawn '{program}': {err}"),
            })?;

            let pgid = child.id().ok_or_else(|| McpPluginError::Spawn {
                config_id: spec.config_id.clone(),
                detail: "spawned MCP server process exited before its pid could be read".into(),
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
            // routes each line to the waiting `framed_round_trip` by JSON-RPC
            // `id`. A SEPARATE task from stderr and from `child.wait()` -- no
            // `tokio::join!` here joins `child.wait()` against piped stdio.
            // The reader ends on stdout EOF or a malformed frame.
            let reader_shared = shared.clone();
            let reader_config_id = spec.config_id.clone();
            let reader_handle = tokio::spawn(async move {
                let mut reader = BufReader::new(stdout);
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line).await {
                        Ok(0) => {
                            // EOF: a clean session end. A trailing partial
                            // line with no terminating `\n` is returned as
                            // `Ok(n > 0)`, NOT `Ok(0)`, so it is handled in the
                            // `Ok(_)` arm below (JSON parse failure ->
                            // `MalformedFrame`). A bare EOF here is a plain
                            // session death.
                            reader_shared.kill_all(McpPluginError::SessionDied {
                                config_id: reader_config_id.clone(),
                                detail: "closed stdout (EOF) mid-session".into(),
                            });
                            return;
                        }
                        Ok(_) => {
                            let bytes = line.trim_end().as_bytes();
                            if bytes.is_empty() {
                                reader_shared.kill_all(McpPluginError::MalformedFrame {
                                    config_id: reader_config_id.clone(),
                                    detail: "wrote an empty line to stdout".into(),
                                });
                                return;
                            }
                            let value: serde_json::Value = match serde_json::from_slice(bytes) {
                                Ok(v) => v,
                                Err(err) => {
                                    reader_shared.kill_all(McpPluginError::MalformedFrame {
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
                                    // No correlation `id`: an inbound
                                    // server-initiated NOTIFICATION (out of
                                    // scope for this minimal client -- MCP
                                    // server->client requests like
                                    // `sampling`/`roots/list` are a later
                                    // item). Drop with a `tracing::warn!`,
                                    // NEVER `kill_all`: a notification is
                                    // observer-class, not a malformed frame.
                                    let method = value
                                        .get("method")
                                        .and_then(|m| m.as_str())
                                        .unwrap_or("<missing method>");
                                    tracing::warn!(
                                        config_id = %reader_config_id,
                                        %method,
                                        "dropping an inbound MCP notification with no id \
                                         (observer-class: a server-initiated notification does \
                                         not tear down the session)"
                                    );
                                    continue;
                                }
                            };
                            let tx = {
                                let mut pending =
                                    reader_shared.pending.lock().expect("pending poisoned");
                                pending.remove(&id)
                            };
                            match tx {
                                Some(tx) => {
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
                            reader_shared.kill_all(McpPluginError::SessionDied {
                                config_id: reader_config_id.clone(),
                                detail: format!("stdout read failed: {err}"),
                            });
                            return;
                        }
                    }
                }
            });

            // The concurrent stderr drain: discards stderr to EOF so a server
            // that writes to stderr with nobody reading it cannot block.
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
                stdin: Arc::new(AsyncMutex::new(stdin)),
                next_id: AtomicU64::new(1),
                shared,
                _reader_handle: reader_handle,
                _stderr_handle: stderr_handle,
            })
        }
    }

    /// True once the session has been torn down. A subsequent
    /// `framed_round_trip` fails fast.
    fn is_dead(&self) -> bool {
        self.shared.dead.load(Ordering::Acquire)
    }

    /// The typed death reason (if the session is dead). The handshake uses
    /// this directly (it returns `McpPluginError`); `tools_call` maps it onto
    /// `ToolError` via `Self::death_tool_error`.
    fn death_error(&self) -> Option<McpPluginError> {
        let death = self.shared.death.lock().expect("death lock poisoned");
        death.as_ref().cloned()
    }

    /// The typed death reason mapped onto the `ToolError` variant the runtime
    /// sees. `None` when the session is not dead.
    fn death_tool_error(&self) -> Option<ToolError> {
        self.death_error().map(|err| err.into_tool_error())
    }

    /// Writes a one-way JSON-RPC 2.0 NOTIFICATION line (no `id`, no reply
    /// expected) under the shared stdin lock, bounded by the per-call write
    /// deadline. Used for `notifications/initialized`. Fail-closed on a write
    /// failure/timeout (the session dies -- a server that cannot accept a
    /// notification line is not going to answer a request either).
    async fn write_notification(&self, value: &serde_json::Value) -> Result<(), McpPluginError> {
        let mut json =
            serde_json::to_vec(value).map_err(|err| McpPluginError::HandshakeFailed {
                config_id: self.config_id.clone(),
                detail: format!("failed to serialize notification: {err}"),
            })?;
        json.push(b'\n');
        match timeout(Duration::from_millis(self.timeout_ms), async {
            let mut stdin = self.stdin.lock().await;
            if let Err(err) = stdin.write_all(&json).await {
                return Err(format!("write to MCP server stdin failed: {err}"));
            }
            if let Err(err) = stdin.flush().await {
                return Err(format!("flush of MCP server stdin failed: {err}"));
            }
            Ok::<(), String>(())
        })
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(detail)) => {
                let err = McpPluginError::SessionDied {
                    config_id: self.config_id.clone(),
                    detail,
                };
                self.shared.kill_all(err.clone());
                Err(err)
            }
            Err(_elapsed) => {
                // A notification write that does not complete within the
                // deadline: the server is not draining stdin. Kill the group
                // and report TimedOut.
                self.kill_group_now().await;
                Err(McpPluginError::TimedOut {
                    config_id: self.config_id.clone(),
                    after_ms: self.timeout_ms,
                })
            }
        }
    }

    /// Registers a oneshot sender for `id` in the pending table (double-checking
    /// dead under the lock) and writes the already-serialized `\n`-terminated
    /// `json` request line under the per-call write deadline. Returns the
    /// response receiver and a [`PendingGuard`] whose `Drop` removes the
    /// pending entry on any exit. **The WRITE runs uncancellable** -- bounded
    /// only by its own `timeout_ms` -- so a cancel can never drop the write
    /// mid-flight and leave a partial (newline-less) request line in the pipe.
    /// That matters: a partial line would corrupt the NDJSON framing for EVERY
    /// tool sharing this session (the server's `read_line` would concatenate
    /// the partial prefix onto the next call's full line and parse one garbage
    /// object), turning a single caller's cancellation into a whole-plugin
    /// outage. A write failure/timeout kills the group and returns `Err`
    /// BEFORE the caller can race the read. Shared by the handshake
    /// ([`framed_round_trip`]) and [`tools_call`].
    async fn send_request(
        &self,
        id: u64,
        json: Vec<u8>,
    ) -> Result<(oneshot::Receiver<serde_json::Value>, PendingGuard), McpPluginError> {
        let (tx, rx) = oneshot::channel::<serde_json::Value>();
        {
            let mut pending = self.shared.pending.lock().expect("pending poisoned");
            if self.is_dead() {
                drop(pending);
                return Err(self
                    .death_error()
                    .unwrap_or_else(|| McpPluginError::SessionDied {
                        config_id: self.config_id.clone(),
                        detail: "session died while this call was being registered".into(),
                    }));
            }
            pending.insert(id, tx);
        }

        // RAII cleanup: removes the pending entry on EVERY exit path from here
        // on -- success, per-call read timeout, write-failure, AND the
        // `tools_call` future dropped mid-read-await (a `tools/call` cancelled
        // in flight, which drops this future before its own read-timeout branch
        // can fire). Idempotent: a no-op if the reader already routed the entry
        // or `kill_all` cleared the table.
        let guard = PendingGuard {
            shared: self.shared.clone(),
            id,
        };

        // Write the framed request line, bounded by the SAME per-call deadline
        // as the read. A write left unbounded would hang if the server stops
        // draining stdin while staying alive (the OS pipe buffer fills). This
        // write is NOT raced against cancellation -- see the method doc.
        match timeout(Duration::from_millis(self.timeout_ms), async {
            let mut stdin = self.stdin.lock().await;
            if let Err(err) = stdin.write_all(&json).await {
                return Err(format!("write to MCP server stdin failed: {err}"));
            }
            if let Err(err) = stdin.flush().await {
                return Err(format!("flush of MCP server stdin failed: {err}"));
            }
            Ok::<(), String>(())
        })
        .await
        {
            Ok(Ok(())) => Ok((rx, guard)),
            Ok(Err(detail)) => {
                let err = McpPluginError::SessionDied {
                    config_id: self.config_id.clone(),
                    detail,
                };
                self.shared.kill_all(err.clone());
                Err(err)
            }
            Err(_elapsed) => {
                // The write did not complete within the deadline: the server is
                // not draining stdin. Kill the group and report TimedOut. The
                // `guard` drops here and removes the pending entry (no-op, since
                // `kill_group_now` marks dead but does not clear `pending`;
                // `kill_all` will clear it on the reader's impending EOF).
                self.kill_group_now().await;
                Err(McpPluginError::TimedOut {
                    config_id: self.config_id.clone(),
                    after_ms: self.timeout_ms,
                })
            }
        }
    }

    /// The shared id-correlated NDJSON round-trip the HANDSHAKE uses
    /// (`initialize`/`tools/list`): [`send_request`] writes the framed request
    /// (uncancellable, bounded by the per-call write deadline), then this
    /// awaits the correlated response under the per-call read deadline. No
    /// cancellation -- the handshake owns no `CancellationToken`. Returns the
    /// routed raw `serde_json::Value`; the CALLER parses + classifies it.
    /// Fail-closed on every failure mode -- dead session, write failure,
    /// per-call timeout, or the reader dropping the sender (session died
    /// mid-call) -- never a hang and never a silent retry. Returns
    /// `McpPluginError` (not `ToolError`) so the handshake surfaces it
    /// directly; `tools_call` is the cancellable sibling (it races the read).
    async fn framed_round_trip(
        &self,
        id: u64,
        json: Vec<u8>,
    ) -> Result<serde_json::Value, McpPluginError> {
        let (rx, _guard) = self.send_request(id, json).await?;

        // Await the correlated response, bounded by the per-call timeout.
        match timeout(Duration::from_millis(self.timeout_ms), rx).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_canceled)) => {
                // The reader dropped the sender -- the session died while we
                // were waiting. `kill_all` already recorded the typed death
                // reason; surface THAT, not a generic "session died".
                Err(self
                    .death_error()
                    .unwrap_or_else(|| McpPluginError::SessionDied {
                        config_id: self.config_id.clone(),
                        detail: "the session died before it answered this call".into(),
                    }))
            }
            Err(_elapsed) => {
                self.kill_group_now().await;
                Err(McpPluginError::TimedOut {
                    config_id: self.config_id.clone(),
                    after_ms: self.timeout_ms,
                })
            }
        }
    }

    /// The one-time `initialize` handshake: sends `initialize`, awaits the
    /// response, verifies the server offers `tools`, sends the
    /// `notifications/initialized` notification (no reply), then calls
    /// `tools/list` and returns the server's declared tools. Fail-closed on
    /// every structural problem (a missing `result`, an `id` mismatch, a
    /// server that does not offer `tools`, a `tools/list` JSON-RPC `error`,
    /// or a malformed `tools/list` answer). A transport-level death during
    /// the handshake surfaces as `SessionDied`/`TimedOut`; the just-spawned
    /// child is dropped by `discover`'s `?`, and its `Drop` kills the group,
    /// so the child is never orphaned.
    pub(crate) async fn handshake(
        &self,
    ) -> Result<(InitializeResult, Vec<crate::wire::ListedTool>), McpPluginError> {
        // initialize
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = initialize_request(id);
        let mut json = serde_json::to_vec(&req).map_err(|err| McpPluginError::HandshakeFailed {
            config_id: self.config_id.clone(),
            detail: format!("failed to serialize initialize request: {err}"),
        })?;
        json.push(b'\n');
        let value = self.framed_round_trip(id, json).await?;
        let init = parse_initialize_response(&value, id).map_err(|detail| {
            // A malformed initialize answer fails the whole session.
            let err = McpPluginError::HandshakeFailed {
                config_id: self.config_id.clone(),
                detail,
            };
            self.shared.kill_all(err.clone());
            err
        })?;
        if !init.offers_tools {
            let err = McpPluginError::HandshakeFailed {
                config_id: self.config_id.clone(),
                detail: "MCP server's initialize result does not offer the `tools` capability \
                          (this client requires `tools` to call tools/list and tools/call)"
                    .into(),
            };
            self.shared.kill_all(err.clone());
            return Err(err);
        }

        // notifications/initialized -- a one-way NOTIFICATION (no id, no
        // reply). The server must not answer; if it does, the reader drops
        // the stray line as a no-id notification. A write failure here fails
        // the session (the server is not draining stdin).
        self.write_notification(&initialized_notification()).await?;

        // tools/list
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = tools_list_request(id);
        let mut json = serde_json::to_vec(&req).map_err(|err| McpPluginError::HandshakeFailed {
            config_id: self.config_id.clone(),
            detail: format!("failed to serialize tools/list request: {err}"),
        })?;
        json.push(b'\n');
        let value = self.framed_round_trip(id, json).await?;
        let tools = parse_tools_list_response(&value, id).map_err(|detail| {
            let err = McpPluginError::HandshakeFailed {
                config_id: self.config_id.clone(),
                detail,
            };
            self.shared.kill_all(err.clone());
            err
        })?;

        Ok((init, tools))
    }

    /// One `tools/call` round-trip over the persistent channel: assigns a
    /// JSON-RPC `id`, writes the framed request line (UNCANCELLABLE -- see
    /// [`send_request`]'s doc for why a cancel-during-write would corrupt the
    /// NDJSON framing), then awaits the correlated response under `timeout_ms`
    /// (a per-call deadline, NOT a session-wide idle kill) RACED against the
    /// caller's `CancellationToken`. Returns the classified `CallOutcome`.
    /// Fail-closed on every transport failure mode -- dead session, write
    /// failure, read timeout, or the reader dropping the sender -- never a hang
    /// and never a silent retry. A JSON-RPC `error` response (a protocol-level
    /// failure) is `CallOutcome::JsonRpcError`; an MCP `isError: true` RESULT
    /// (a tool-level failure) is `CallOutcome::Ok` with `is_error: true`.
    ///
    /// **Cancellation is raced ONLY against the read.** The write completes
    /// (or kills the session on its own write-timeout) before the cancel
    /// watcher starts, so a cancel can never leave a partial request line in
    /// the pipe. A cancel during the read drops the `tools_call` future,
    /// dropping the [`PendingGuard`] (which removes the pending entry so a late
    /// server response finds no entry and is dropped harmlessly) and returns
    /// [`ToolError::Cancelled`]; the SESSION STAYS ALIVE -- cancellation is a
    /// caller preference, not a session failure. The per-call `timeout_ms` on
    /// the read remains the ultimate fail-closed bound.
    pub(crate) async fn tools_call(
        &self,
        name: String,
        arguments: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<CallOutcome, ToolError> {
        if self.is_dead() {
            return Err(self.death_tool_error().unwrap_or_else(|| {
                McpPluginError::SessionDied {
                    config_id: self.config_id.clone(),
                    detail: "session is no longer alive (re-discover to spawn a fresh one)".into(),
                }
                .into_tool_error()
            }));
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = tools_call_request(id, &name, &arguments);
        let mut json = serde_json::to_vec(&req).map_err(|err| ToolError::Internal {
            detail: format!("failed to serialize tools/call request: {err}"),
        })?;
        json.push(b'\n');

        // The WRITE runs uncancellable -- `send_request` bounds it by its own
        // `timeout_ms` and fails closed on a write failure/timeout. Racing the
        // write against cancel would drop the future mid-`write_all` and leave a
        // partial newline-less request line in the pipe, corrupting the NDJSON
        // framing for every tool sharing this session. Only the READ is raced.
        let (rx, _guard) = self
            .send_request(id, json)
            .await
            .map_err(McpPluginError::into_tool_error)?;

        // Race ONLY the read against cancellation. A cancel here drops
        // `_guard`, removing the pending entry; the session stays alive. The
        // `timeout_ms` read deadline is the ultimate fail-closed bound.
        let value = tokio::select! {
            res = timeout(Duration::from_millis(self.timeout_ms), rx) => match res {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(_canceled)) => {
                    // The reader dropped the sender -- the session died while we
                    // were waiting. `kill_all` already recorded the typed death
                    // reason; surface THAT, not a generic "session died".
                    Err(self.death_tool_error().unwrap_or_else(|| {
                        McpPluginError::SessionDied {
                            config_id: self.config_id.clone(),
                            detail: "the session died before it answered this call".into(),
                        }
                        .into_tool_error()
                    }))
                }
                Err(_elapsed) => {
                    self.kill_group_now().await;
                    Err(McpPluginError::TimedOut {
                        config_id: self.config_id.clone(),
                        after_ms: self.timeout_ms,
                    }
                    .into_tool_error())
                }
            },
            _ = cancel_watched(cancel) => Err(ToolError::Cancelled),
        }?;

        Ok(CallOutcome::from_value(&value, id))
    }

    /// The configured id of this session's plugin entry -- `pub(crate)` so
    /// `McpTool::invoke` can name the entry in a malformed-frame error and
    /// `McpPlugin::discover` can name it in a handshake-failure error.
    pub(crate) fn config_id(&self) -> &str {
        &self.config_id
    }

    /// Marks the session dead with `reason` and drops every pending sender.
    /// `pub(crate)` so `McpPlugin::discover` and `McpTool::invoke` can fail
    /// closed on a malformed frame / a duplicate tool name without reaching
    /// into `Shared` directly.
    pub(crate) fn shared_kill_all(&self, reason: McpPluginError) {
        self.shared.kill_all(reason);
    }

    /// Kills the process group with the graceful SIGTERM-then-SIGKILL
    /// sequence and marks the session dead. Used on the per-call timeout
    /// path. `kill_group` reaps the child itself, so no separate reap is
    /// needed here.
    async fn kill_group_now(&self) {
        self.shared.dead.store(true, Ordering::Release);
        let mut child_guard = self.child.lock().await;
        if let Some(child) = child_guard.as_mut() {
            kill_group(child, self.pgid).await;
        }
    }
}

/// The interval the cancel watcher polls `token.is_cancelled()`. The
/// `CancellationToken` (an `Arc<AtomicBool>`, not an async signal) has no
/// `await`-able cancellation, so the watcher polls on a short interval and
/// `tokio::select!` races it against the read. Short enough that cancellation
/// is observed promptly, long enough that the polling overhead is negligible
/// against a real `tools/call` (which takes at least milliseconds).
const CANCEL_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

/// A future that completes when `token` is cancelled. Used by `tools_call`'s
/// `select!` to race the READ of a `tools/call` against cancellation. Polls
/// `is_cancelled()` on a short interval -- the `CancellationToken` has no native
/// async wait, so this is the bridge the `conway-core::ports::CancellationToken`
/// doc itself prescribes ("Downstream crates that need an async cancellation
/// *await* ... bridge this token to `tokio_util`'s token themselves" -- this
/// crate uses a polling watcher rather than pulling in `tokio_util` for one
/// select).
async fn cancel_watched(token: CancellationToken) {
    while !token.is_cancelled() {
        tokio::time::sleep(CANCEL_POLL_INTERVAL).await;
    }
}

impl Drop for McpSession {
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
    }
}
