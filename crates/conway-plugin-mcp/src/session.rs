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
//! OWN lightweight `McpSession` -- but the process-lifecycle plumbing
//! underneath both (spawn, an id-correlated NDJSON round trip, a per-call
//! timeout, fail-closed teardown) is now ONE shared implementation,
//! `conway::plugin::ChildSession` (board item `01M0TV7ZDS8X4F4TEJPRZB9P6T`;
//! see that re-export's own doc and `conway_tools::process::child_session`'s
//! module doc for the full argument). This module owns only what is
//! genuinely MCP-specific on top: the `initialize`/`notifications/
//! initialized`/`tools/list`/`tools/call` request shapes, JSON-RPC `error`
//! classification, and the cancellation race `tools_call` runs against the
//! read. This crate does NOT depend on `conway-plugin-subprocess`; the two
//! transports are siblings, not a layering. Read
//! `conway-plugin-subprocess/src/session.rs` for the sibling built on the
//! SAME shared primitive.
//!
//! **Framing: NDJSON -- one JSON-RPC 2.0 object per line, `\n`-delimited, over
//! the child's stdin/stdout.** JSON-RPC 2.0 over stdio is NDJSON by the
//! spec's own "one JSON object per line, UTF-8, `\n`-delimited" rule; this is
//! the SAME framing `PersistentSession` uses, and the SAME framing
//! `ChildSession`'s reader implements once for both.
//!
//! **Correlation: JSON-RPC `id` + `ChildSession`'s outstanding-request
//! table.** Each `initialize`/`tools/list`/`tools/call` request assigns a
//! monotonic `id` (`ChildSession::next_id`) and rides `ChildSession::
//! send_request`/`framed_round_trip`. A line with NO `id` is an inbound
//! server-initiated NOTIFICATION (out of scope for this minimal client -- MCP
//! server->client requests like `sampling`/`roots/list` are a later item):
//! `ChildSession::spawn` is given [`conway::plugin::NotificationRoute::
//! WarnAndDrop`] for this session, so the reader drops such a line with a
//! `tracing::warn!` rather than tearing the session down -- a notification is
//! observer-class, not a malformed frame.
//!
//! **`initialize` handshake once at session open.** Before any `tools/call`,
//! the client exchanges ONE `initialize` request/response (negotiate the
//! `tools` capability -- a server that does NOT offer `tools` is refused at
//! discover), sends the `notifications/initialized` notification (no `id`,
//! no reply expected), and calls `tools/list` to enumerate the server's
//! tools. The whole handshake rides `ChildSession::framed_round_trip`; NO
//! second reader.
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
//! NOT an error -- the distinction is load-bearing in MCP. Every one of
//! these four causes (Spawn/TimedOut/SessionDied/MalformedFrame) is now
//! constructed in exactly one place -- `ChildSession` -- via this crate's
//! `impl conway::plugin::ChildSessionError for McpPluginError` (`lib.rs`), a
//! one-line-per-variant mapping onto this crate's own, unchanged, public
//! error enum.
//!
//! **Hazards (now owned by `ChildSession`, disclosed there).**
//! `conway_tools::process::child_session`'s own module doc carries the
//! four-way-join-starvation avoidance, the stderr-drain-but-discard
//! disclosure, and the process-group Drop-time kill this module used to
//! disclose locally.

use std::time::Duration;

use tokio::time::timeout;

use conway::plugin::{CancellationToken, ChildSession, NotificationRoute, ToolError};

use crate::wire::{
    initialize_request, initialized_notification, parse_initialize_response,
    parse_tools_list_response, tools_call_request, tools_list_request, CallOutcome,
    InitializeResult,
};
use crate::{McpPluginError, McpPluginSpec};

/// A long-lived handle to one MCP server child process. A thin wrapper over
/// the shared [`ChildSession`] (spawn, id-correlated NDJSON round trip,
/// per-call timeout, fail-closed teardown -- see this module's own doc);
/// this type owns only the MCP-specific request shapes and the
/// `tools_call` cancellation race on top of it. Built by `McpSession::spawn`
/// (a `pub(crate)` constructor `McpPlugin::discover` calls); used by the
/// `Tool::invoke` impl on `crate::McpTool`.
///
/// **Cloning.** An `McpSession` is NOT `Clone`; the plugin hands each `McpTool`
/// an `Arc<McpSession>`, so every tool on this plugin shares ONE child process
/// (the load-bearing property acceptance criterion 1 asserts for the
/// subprocess transport; an MCP server's tools share one server process too).
pub struct McpSession {
    inner: ChildSession<McpPluginError>,
}

impl McpSession {
    /// Spawns the configured command once, wires stdin/stdout/stderr, and
    /// starts the long-lived reader + stderr-drain tasks (via
    /// [`ChildSession::spawn`]). Returns a handle whose child lives until it
    /// is dropped or a fatal error kills it.
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
            // Credentials/connection-lifecycle scoping (acceptance 3): the
            // child inherits the PARENT env PLUS the entry's explicit `env`
            // pairs -- the identical shape `HookEntry` uses, so an operator
            // scopes credentials by naming them here rather than relying on
            // implicit inheritance. Explicit, not implicit.
            //
            // `NotificationRoute::WarnAndDrop`: an inbound no-`id` line is a
            // server-initiated notification, out of scope for this minimal
            // client -- see this module's own doc.
            let inner = ChildSession::spawn(
                &spec.config_id,
                &spec.command,
                &spec.env,
                spec.timeout_ms,
                NotificationRoute::WarnAndDrop,
            )
            .await?;
            Ok(Self { inner })
        }
    }

    /// True once the session has been torn down. A subsequent
    /// `framed_round_trip` fails fast.
    fn is_dead(&self) -> bool {
        self.inner.is_dead()
    }

    /// The typed death reason (if the session is dead). The handshake uses
    /// this directly (it returns `McpPluginError`); `tools_call` maps it onto
    /// `ToolError` via `Self::death_tool_error`.
    fn death_error(&self) -> Option<McpPluginError> {
        self.inner.death_error()
    }

    /// The typed death reason mapped onto the `ToolError` variant the runtime
    /// sees. `None` when the session is not dead.
    fn death_tool_error(&self) -> Option<ToolError> {
        self.death_error().map(|err| err.into_tool_error())
    }

    /// Writes a one-way JSON-RPC 2.0 NOTIFICATION line (no `id`, no reply
    /// expected) via [`ChildSession::write_frame`], bounded by the per-call
    /// write deadline. Used for `notifications/initialized`. Fail-closed on a
    /// write failure/timeout (the session dies -- a server that cannot
    /// accept a notification line is not going to answer a request either).
    async fn write_notification(&self, value: &serde_json::Value) -> Result<(), McpPluginError> {
        let mut json =
            serde_json::to_vec(value).map_err(|err| McpPluginError::HandshakeFailed {
                config_id: self.inner.config_id().to_string(),
                detail: format!("failed to serialize notification: {err}"),
            })?;
        json.push(b'\n');
        self.inner.write_frame(json).await
    }

    /// The one-time `initialize` handshake: sends `initialize`, awaits the
    /// response, verifies the server offers `tools`, sends the
    /// `notifications/initialized` notification (no reply), then calls
    /// `tools/list` and returns the server's declared tools. Fail-closed on
    /// every structural problem (a missing `result`, an `id` mismatch, a
    /// server that does not offer `tools`, a `tools/list` JSON-RPC `error`,
    /// or a malformed `tools/list` answer). A transport-level death during
    /// the handshake surfaces as `SessionDied`/`TimedOut`; the just-spawned
    /// child is dropped by `discover`'s `?`, and its `Drop` kills the
    /// group, so the child is never orphaned.
    pub(crate) async fn handshake(
        &self,
    ) -> Result<(InitializeResult, Vec<crate::wire::ListedTool>), McpPluginError> {
        // initialize
        let id = self.inner.next_id();
        let req = initialize_request(id);
        let mut json = serde_json::to_vec(&req).map_err(|err| McpPluginError::HandshakeFailed {
            config_id: self.inner.config_id().to_string(),
            detail: format!("failed to serialize initialize request: {err}"),
        })?;
        json.push(b'\n');
        let value = self.inner.framed_round_trip(id, json).await?;
        let init = parse_initialize_response(&value, id).map_err(|detail| {
            // A malformed initialize answer fails the whole session.
            let err = McpPluginError::HandshakeFailed {
                config_id: self.inner.config_id().to_string(),
                detail,
            };
            self.inner.kill_all(err.clone());
            err
        })?;
        if !init.offers_tools {
            let err = McpPluginError::HandshakeFailed {
                config_id: self.inner.config_id().to_string(),
                detail: "MCP server's initialize result does not offer the `tools` capability \
                          (this client requires `tools` to call tools/list and tools/call)"
                    .into(),
            };
            self.inner.kill_all(err.clone());
            return Err(err);
        }

        // notifications/initialized -- a one-way NOTIFICATION (no id, no
        // reply). The server must not answer; if it does, the reader drops
        // the stray line as a no-id notification. A write failure here fails
        // the session (the server is not draining stdin).
        self.write_notification(&initialized_notification()).await?;

        // tools/list
        let id = self.inner.next_id();
        let req = tools_list_request(id);
        let mut json = serde_json::to_vec(&req).map_err(|err| McpPluginError::HandshakeFailed {
            config_id: self.inner.config_id().to_string(),
            detail: format!("failed to serialize tools/list request: {err}"),
        })?;
        json.push(b'\n');
        let value = self.inner.framed_round_trip(id, json).await?;
        let tools = parse_tools_list_response(&value, id).map_err(|detail| {
            let err = McpPluginError::HandshakeFailed {
                config_id: self.inner.config_id().to_string(),
                detail,
            };
            self.inner.kill_all(err.clone());
            err
        })?;

        Ok((init, tools))
    }

    /// One `tools/call` round-trip over the persistent channel: assigns a
    /// JSON-RPC `id`, writes the framed request line (UNCANCELLABLE -- see
    /// [`ChildSession::send_request`]'s doc for why a cancel-during-write
    /// would corrupt the NDJSON framing), then awaits the correlated
    /// response under `timeout_ms` (a per-call deadline, NOT a session-wide
    /// idle kill) RACED against the caller's `CancellationToken`. Returns the
    /// classified `CallOutcome`. Fail-closed on every transport failure mode
    /// -- dead session, write failure, read timeout, or the reader dropping
    /// the sender -- never a hang and never a silent retry. A JSON-RPC
    /// `error` response (a protocol-level failure) is
    /// `CallOutcome::JsonRpcError`; an MCP `isError: true` RESULT (a
    /// tool-level failure) is `CallOutcome::Ok` with `is_error: true`.
    ///
    /// **Cancellation is raced ONLY against the read.** The write completes
    /// (or kills the session on its own write-timeout) before the cancel
    /// watcher starts, so a cancel can never leave a partial request line in
    /// the pipe. A cancel during the read drops the `tools_call` future,
    /// dropping the [`conway::plugin::PendingGuard`] (which removes the
    /// pending entry so a late server response finds no entry and is
    /// dropped harmlessly) and returns [`ToolError::Cancelled`]; the SESSION
    /// STAYS ALIVE -- cancellation is a caller preference, not a session
    /// failure. The per-call `timeout_ms` on the read remains the ultimate
    /// fail-closed bound. This cancellation race is the one piece of
    /// process-lifecycle machinery `conway-plugin-subprocess`'s own
    /// persistent `tool/1` path does NOT have (a real, disclosed divergence
    /// -- see this item's own completion report).
    pub(crate) async fn tools_call(
        &self,
        name: String,
        arguments: serde_json::Value,
        cancel: CancellationToken,
    ) -> Result<CallOutcome, ToolError> {
        if self.is_dead() {
            return Err(self.death_tool_error().unwrap_or_else(|| {
                McpPluginError::SessionDied {
                    config_id: self.inner.config_id().to_string(),
                    detail: "session is no longer alive (re-discover to spawn a fresh one)".into(),
                }
                .into_tool_error()
            }));
        }

        let id = self.inner.next_id();
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
            .inner
            .send_request(id, json)
            .await
            .map_err(McpPluginError::into_tool_error)?;

        // Race ONLY the read against cancellation. A cancel here drops
        // `_guard`, removing the pending entry; the session stays alive. The
        // `timeout_ms` read deadline is the ultimate fail-closed bound.
        let value = tokio::select! {
            res = timeout(Duration::from_millis(self.inner.timeout_ms()), rx) => match res {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(_canceled)) => {
                    // The reader dropped the sender -- the session died while we
                    // were waiting. `kill_all` already recorded the typed death
                    // reason; surface THAT, not a generic "session died".
                    Err(self.death_tool_error().unwrap_or_else(|| {
                        McpPluginError::SessionDied {
                            config_id: self.inner.config_id().to_string(),
                            detail: "the session died before it answered this call".into(),
                        }
                        .into_tool_error()
                    }))
                }
                Err(_elapsed) => {
                    self.inner.kill_group_now().await;
                    Err(McpPluginError::TimedOut {
                        config_id: self.inner.config_id().to_string(),
                        after_ms: self.inner.timeout_ms(),
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
        self.inner.config_id()
    }

    /// Marks the session dead with `reason` and drops every pending sender.
    /// `pub(crate)` so `McpPlugin::discover` and `McpTool::invoke` can fail
    /// closed on a malformed frame / a duplicate tool name without reaching
    /// into `ChildSession` directly.
    pub(crate) fn shared_kill_all(&self, reason: McpPluginError) {
        self.inner.kill_all(reason);
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
