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
//! **Process-lifecycle plumbing is now shared, not hand-rolled here (board
//! item `01M0TV7ZDS8X4F4TEJPRZB9P6T`).** The spawn, the id-correlated NDJSON
//! round trip, the per-call timeout, and the fail-closed teardown (dead
//! session, malformed frame, `Drop`-time SIGKILL) are ONE implementation,
//! `conway::plugin::ChildSession` -- the SAME primitive
//! `conway-plugin-mcp::session::McpSession` builds on (see that re-export's
//! own doc, and `conway_tools::process::child_session`'s module doc, for the
//! full argument). This module owns only what is genuinely THIS wire
//! dialect's own: the `initialize/1`/`permission.policy/1`/`observe/1`/
//! `status.declare/1`/`tool/1` request shapes, the version-negotiation
//! table, and the per-point participant-vs-observer refuse/degrade rules
//! below.
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
//! **Correlation discipline: JSON-RPC `id` + `ChildSession`'s
//! outstanding-request table.** Each call rides `ChildSession::
//! send_request`/`framed_round_trip` (a monotonic `id` from
//! `ChildSession::next_id`, the framed write, then the correlated read). A
//! line carrying no matching `id` is an inbound NOTIFICATION (board item
//! `01M03VKQ738DTGHHK2C4RWXC0E`): `ChildSession::spawn` is given
//! [`conway::plugin::NotificationRoute::Forward`] for this session, so the
//! reader routes such a line, via a NON-blocking `try_send`, onto the
//! notification channel this module owns; the handler task below (spawned
//! by [`PersistentSession::spawn`]) drains it, parses `op`, and stores a
//! `status/1` line's contribution.
//!
//! **`initialize/1` handshake at session open (board item
//! `01M03VK7MRPSAVWMW7YNYPRPGT`).** Before any `tool/1` call, the host
//! exchanges ONE `initialize/1` request/response with the plugin over
//! [`ChildSession::framed_round_trip`] (NO second reader). The host sends
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
//! [`crate::SubprocessPluginError::MalformedFrame`], not a deadlock. Every
//! one of these four causes is now constructed in exactly one place --
//! `ChildSession` -- via this crate's `impl conway::plugin::
//! ChildSessionError for SubprocessPluginError` (`lib.rs`), a
//! one-line-per-variant mapping onto this crate's own, unchanged, public
//! error enum. `HandshakeRefused`/`HandshakeMalformed` (this crate's OWN
//! version-negotiation outcomes, not shared with `conway-plugin-mcp`'s
//! different handshake shape) stay constructed locally, in this file.
//!
//! **Hazards (now owned by `ChildSession`, disclosed there; one remains
//! local).** `conway_tools::process::child_session`'s own module doc
//! carries the four-way-join-starvation avoidance, the
//! stderr-drain-but-discard disclosure, and the process-group Drop-time
//! kill this module used to disclose locally. Still local to this module:
//! the `observe/1` writer task below deliberately does NOT go through
//! `ChildSession::write_frame` -- that helper kills the WHOLE session on any
//! write failure, but an `observe/1` notification write failing must
//! degrade the OBSERVE forwarding alone, leaving concurrent `tool/1` calls
//! on the same session unaffected (observer-class: an observer cannot fail
//! the run). It takes the shared stdin lock directly via
//! [`conway::plugin::ChildSession::stdin_handle`] instead, and manages its
//! own bounded write deadline and its own `broken` flag.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::time::timeout;

use conway::plugin::{
    ChildSession, Event, EventSink, EventSinkHandle, NotificationRoute, ToolError,
};

use crate::wire::{
    build_observe_notification, parse_persistent_initialize_response,
    parse_persistent_observe_response, parse_persistent_permission_policy_response,
    parse_persistent_status_declare_response, parse_persistent_tool_response,
    parse_status_notification, InitializeParseError, ObserveParseError, PermissionPolicyAnswer,
    PermissionPolicyParseError, PersistentInitializeRequest, PersistentObserveRequest,
    PersistentPermissionPolicyRequest, PersistentStatusDeclareRequest, PersistentToolRequest,
    StatusDeclaration, StatusDeclareParseError, WirePermissionRule, WireStatusContribution,
    WireToolResult, HOST_OBSERVE_VERSION, HOST_PERMISSION_POLICY_VERSION, HOST_STATUS_VERSION,
    HOST_WIRE_MAJOR, HOST_WIRE_MINOR,
};
use crate::{SubprocessPluginError, SubprocessPluginSpec};

/// Bounded capacity of the inbound notification channel (board item
/// `01M03VKQ738DTGHHK2C4RWXC0E`): a plugin pushes `status/1` (and, in future,
/// other one-way no-`id` notifications) onto this channel; the reader does a
/// NON-blocking `try_send`, so a plugin that floods notifications faster than
/// the handler drains hits `Full` and the line is DROPPED with a
/// `tracing::warn!` -- never blocks the host turn, never kills the session
/// (observer-class). Sized to absorb a reasonable burst without dropping under
/// normal pacing; a pathological producer degrades lossy-with-notice, the
/// identical discipline `conway::EventStream` guarantees a slow event consumer.
const NOTIFICATION_CHANNEL_CAPACITY: usize = 256;

/// Bounded capacity of the outbound observe channel (board item
/// `01M03VKQ738DTGHHK2C4RWXC0E`): the host fans `Event`s onto this channel via
/// the plugin's `EventSink` (a NON-blocking `try_send`), and a writer task
/// drains it and serializes each `Event` as an `observe/1` notification line
/// onto the plugin's stdin. A slow plugin (one not draining its stdin) makes
/// the writer's `write_all` block; the writer bounds each write by
/// `timeout_ms` and, on a write failure/timeout, stops forwarding and marks
/// the observe path broken -- the channel then fills and the `EventSink`'s own
/// `try_send` drops+warns, so the host turn NEVER blocks on a slow plugin read
/// loop. Lossy-with-notice, mirroring `conway::EventStream`.
const OBSERVE_CHANNEL_CAPACITY: usize = 256;

/// The observe engagement state stored on [`PersistentSession`] (board item
/// `01M03VKQ738DTGHHK2C4RWXC0E`). Held in a `Mutex<Option<Self>>` so
/// [`PersistentSession::observe_sink`] can build an [`ObserveAdapter`] from
/// the sender without re-running the engagement exchange.
struct ObserveState {
    /// Bounded channel the `EventSink` pushes `Event`s onto (`try_send`,
    /// drop+warn on `Full`) and the writer task drains. When the writer task
    /// ends (write failure/timeout, or session drop), the sender half errors
    /// `Closed` and the adapter sets `broken` so it stops enqueuing.
    tx: mpsc::Sender<Event>,
    /// Set by the writer task on a write failure/timeout (the observe path is
    /// broken -- stop forwarding) and by the adapter on a `Closed` send. Once
    /// true, the adapter drops events silently rather than retrying `try_send`
    /// every `emit` -- a single warn at the break site names the degradation.
    broken: Arc<AtomicBool>,
    /// The writer task handle, kept so it is not leaked; it ends on drain
    /// completion (the session is dropped and the adapter stops sending) or a
    /// write failure/timeout.
    _writer: tokio::task::JoinHandle<()>,
}

/// An [`EventSink`] that bridges the host's live `Event` stream onto a
/// subprocess plugin's stdin as `observe/1` notifications (board item
/// `01M03VKQ738DTGHHK2C4RWXC0E`). The host's forwarding task (a subscriber of
/// the runtime's `EventBus`, installed by the `conway` facade) calls
/// [`EventSink::emit`] for each `Envelope`'s `Event`; this adapter does a
/// NON-blocking `try_send` onto a bounded channel the observe writer task
/// drains. Lossy-with-notice by construction: a `Full` channel drops the event
/// with a `tracing::warn!` (never blocks the host turn); a `Closed` channel
/// (writer gone) sets `broken` and drops silently thereafter.
struct ObserveAdapter {
    tx: mpsc::Sender<Event>,
    broken: Arc<AtomicBool>,
    config_id: String,
}

impl EventSink for ObserveAdapter {
    fn emit(&self, event: Event) {
        if self.broken.load(Ordering::Relaxed) {
            // The observe writer already stopped (write failure/timeout, or
            // the channel closed). Drop silently -- the break was warned once
            // at its cause site, so a per-event warn here would only spam.
            return;
        }
        match self.tx.try_send(event) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!(
                    config_id = %self.config_id,
                    "observe/1 notification channel full; dropping an Event \
                     (lossy-with-notice: a slow plugin read loop must not stall \
                     the host turn, per the observe/1 contract)"
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                // The writer task ended (observe path broken). Set the flag so
                // every subsequent `emit` takes the early `broken` return
                // rather than retrying `try_send` (which would keep hitting
                // `Closed`).
                self.broken.store(true, Ordering::Relaxed);
            }
        }
    }
}

/// A long-lived handle to one persistent subprocess plugin. A thin wrapper
/// over the shared [`ChildSession`] (spawn, id-correlated NDJSON round trip,
/// per-call timeout, fail-closed teardown -- see this module's own doc);
/// this type owns the conway-wire-specific request shapes, the
/// version-negotiation handshake, and the `permission.policy/1`/`observe/1`/
/// `status.declare/1` wire-point exchanges on top of it. Built by
/// `PersistentSession::spawn` (a `pub(crate)` constructor
/// `SubprocessPlugin::discover` calls); used by the `Tool::invoke` impl on
/// `crate::SubprocessTool` when the plugin's
/// [`crate::SubprocessPluginSpec::transport`] is
/// [`crate::SubprocessTransport::Persistent`].
///
/// **Cloning.** A `PersistentSession` is NOT `Clone`; the plugin hands each
/// `SubprocessTool` an `Arc<PersistentSession>`, so every tool on this
/// plugin shares ONE child process (the load-bearing property acceptance
/// criterion 1 asserts: the child PID is identical across two sequential
/// calls).
pub struct PersistentSession {
    inner: ChildSession<SubprocessPluginError>,
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
    /// The status.declare/1 per-key declarations the plugin made at session
    /// open (board item `01M03VKQ738DTGHHK2C4RWXC0E`), recorded ONCE by
    /// [`Self::request_status_declare`]. Empty until the one-time exchange
    /// populates it -- which runs ONLY when the plugin declared the point at
    /// a supported version (an unsupported version DEGRADES -- load without
    /// the point, warn; a plugin that does not declare it contributes no
    /// status and this stays empty). Stored for the facade surface; the
    /// ttl/expiry RENDER path itself stays design-only (point 12).
    status_declarations: Mutex<Vec<StatusDeclaration>>,
    /// The observe engagement state (board item `01M03VKQ738DTGHHK2C4RWXC0E`):
    /// `Some` only when the plugin declared `observe/1` at a supported
    /// version AND the one-time engagement exchange succeeded, holding the
    /// bounded `Event` sender the [`ObserveAdapter`] `EventSink` pushes onto
    /// and the writer task drains, plus the `broken` flag a write
    /// failure/timeout sets so the adapter stops enqueuing. `None` for a
    /// plugin that did not declare the point, declared it at an unsupported
    /// version (DEGRADE), or before the one-time exchange has run.
    observe_state: Mutex<Option<ObserveState>>,
    /// The latest status contribution per `key`, pushed by the notification
    /// handler task from inbound `status/1` no-`id` lines (board item
    /// `01M03VKQ738DTGHHK2C4RWXC0E`). A later line for the same `key`
    /// overwrites an earlier one (per `docs/plugins/hooks.md` point 12's "a
    /// stale value expires at snapshot time" shape -- the ttl/expiry RENDER
    /// path itself stays design-only). Read by
    /// [`PersistentSession::status_contributions`] for the
    /// `Plugin::status_contributions` trait method -- a point-in-time
    /// snapshot, NOT a build-time declaration. Shared (`Arc`) with the
    /// notification handler task, which is the sole writer.
    status: Arc<Mutex<HashMap<String, WireStatusContribution>>>,
    /// The inbound notification handler task (board item
    /// `01M03VKQ738DTGHHK2C4RWXC0E`): drains the notification channel
    /// [`ChildSession`]'s reader forwards a no-`id` line onto (this
    /// session's own [`conway::plugin::NotificationRoute::Forward`]
    /// target), parses `op`, and stores a `status/1` line's contribution in
    /// [`Self::status`]. Kept (never awaited) so it is not leaked; it ends
    /// when the sender half drops (session drop) or the receiver errors.
    _notification_handle: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for PersistentSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistentSession")
            .field("config_id", &self.inner.config_id())
            .finish()
    }
}

impl PersistentSession {
    /// Spawns the configured command once, wires stdin/stdout/stderr, and
    /// starts the long-lived reader + stderr-drain tasks (via
    /// [`ChildSession::spawn`]), plus this crate's OWN inbound-notification
    /// handler task (board item `01M03VKQ738DTGHHK2C4RWXC0E`). Returns a
    /// handle whose child lives until it is dropped or a fatal error kills
    /// it.
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
            // The inbound notification channel (board item
            // `01M03VKQ738DTGHHK2C4RWXC0E`): `ChildSession`'s reader routes
            // no-`id` lines here (`NotificationRoute::Forward`); the handler
            // task drains it and stores `status/1` contributions. Bounded +
            // `try_send` (inside `ChildSession`'s reader) so a flooding
            // plugin degrades lossy-with-notice (drop+warn) rather than
            // blocking the reader or killing the session.
            let (notif_tx, notif_rx) =
                mpsc::channel::<serde_json::Value>(NOTIFICATION_CHANNEL_CAPACITY);

            // This crate has no per-entry `env` field (unlike
            // `conway-plugin-mcp::McpPluginSpec`) -- the child inherits the
            // parent env unchanged, exactly as before this extraction.
            let inner = ChildSession::spawn(
                &spec.config_id,
                &spec.command,
                &[],
                spec.timeout_ms,
                NotificationRoute::Forward(notif_tx),
            )
            .await?;

            // The inbound notification handler task: drains the channel the
            // reader routes no-`id` lines onto, parses `op`, and stores a
            // `status/1` line's contribution in `status` (latest per `key`).
            // An unknown `op` (or a structurally-invalid `status/1` body) is
            // dropped with a `tracing::warn!` -- observer-class, degrade,
            // NEVER fails the session. A separate task from the reader so
            // parsing/storing never blocks stdout reading (the bounded
            // channel + `try_send` in `ChildSession`'s reader enforce that
            // boundary).
            let status: Arc<Mutex<HashMap<String, WireStatusContribution>>> =
                Arc::new(Mutex::new(HashMap::new()));
            let handler_status = status.clone();
            let handler_config_id = spec.config_id.clone();
            let notification_handle = tokio::spawn(async move {
                let mut rx = notif_rx;
                while let Some(value) = rx.recv().await {
                    let op = value.get("op").and_then(|v| v.as_str()).unwrap_or("");
                    if op == "status/1" {
                        match parse_status_notification(&value) {
                            Ok(contrib) => {
                                let mut status =
                                    handler_status.lock().expect("status map poisoned");
                                status.insert(contrib.key.clone(), contrib);
                            }
                            Err(detail) => {
                                tracing::warn!(
                                    config_id = %handler_config_id,
                                    %detail,
                                    "dropping a malformed status/1 notification (observer-class: \
                                     degrade, never fails the session)"
                                );
                            }
                        }
                    } else {
                        tracing::warn!(
                            config_id = %handler_config_id,
                            op = %op,
                            "dropping an unknown no-id notification (observer-class: degrade, \
                             never fails the session)"
                        );
                    }
                }
            });

            Ok(Self {
                inner,
                point_versions: Mutex::new(HashMap::new()),
                permission_policy: Mutex::new(Vec::new()),
                status_declarations: Mutex::new(Vec::new()),
                observe_state: Mutex::new(None),
                status,
                _notification_handle: notification_handle,
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
        self.inner.pgid() as u32
    }

    /// True once the session has been torn down (child died, malformed
    /// frame, or explicit kill). A subsequent `round_trip` fails fast.
    fn is_dead(&self) -> bool {
        self.inner.is_dead()
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
        self.inner.death_error()
    }

    /// The typed death reason (if the session is dead), mapped onto the
    /// `ToolError` variant the runtime sees. `None` when the session is not
    /// dead (or the death reason was not recorded -- should not happen, but
    /// a caller falls back to a generic `SessionDied` in that case).
    fn death_tool_error(&self) -> Option<ToolError> {
        self.death_error().map(|err| err.into_tool_error())
    }

    /// One `tool/1` round-trip over the persistent channel: assigns a
    /// JSON-RPC `id`, writes the framed request line, and awaits the
    /// correlated response, bounded by `spec.timeout_ms` (a per-call
    /// deadline, NOT a session-wide idle kill), via
    /// [`ChildSession::framed_round_trip`]. Returns the classified
    /// [`WireToolResult`]. Fail-closed on every failure mode -- dead
    /// session, write failure, timeout, malformed frame, or an `id`
    /// mismatch -- never a hang and never a silent retry.
    ///
    /// **No cancellation race** (a real, disclosed divergence from
    /// `conway-plugin-mcp::session::McpSession::tools_call`, which races its
    /// own read against the caller's `CancellationToken`): this call is
    /// fully awaited to completion by `SubprocessTool::invoke`, which only
    /// checks `ctx.cancel` BEFORE dispatching, not during. See this item's
    /// own completion report for the full divergence list.
    pub(crate) async fn tool_round_trip(
        &self,
        tool: String,
        call_id: String,
        arguments: serde_json::Value,
    ) -> Result<WireToolResult, ToolError> {
        if self.is_dead() {
            return Err(self.death_tool_error().unwrap_or_else(|| {
                SubprocessPluginError::SessionDied {
                    config_id: self.inner.config_id().to_string(),
                    detail: "session is no longer alive (re-discover to spawn a fresh one)".into(),
                }
                .into_tool_error()
            }));
        }

        let id = self.inner.next_id();
        let request = PersistentToolRequest::tool_v1(id, tool, call_id, arguments);
        let mut json = serde_json::to_vec(&request).map_err(|err| ToolError::Internal {
            detail: format!("failed to serialize persistent tool/1 request: {err}"),
        })?;
        json.push(b'\n');

        let value = self
            .inner
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
                config_id: self.inner.config_id().to_string(),
                detail,
            };
            self.inner.kill_all(err.clone());
            err.into_tool_error()
        })?;
        if resp_id != id {
            let err = SubprocessPluginError::SessionDied {
                config_id: self.inner.config_id().to_string(),
                detail: format!("response id {resp_id} did not match request id {id}"),
            };
            self.inner.kill_all(err.clone());
            return Err(err.into_tool_error());
        }
        Ok(result)
    }

    /// The one-time `initialize/1` version-negotiation handshake (board item
    /// `01M03VK7MRPSAVWMW7YNYPRPGT`), exchanged ONCE at persistent-session
    /// open BEFORE any `tool/1` call. Rides
    /// [`ChildSession::framed_round_trip`] -- the SAME id-correlated NDJSON
    /// framing `tool_round_trip` uses; NO second reader. Sends this host's
    /// `wire_major`/`wire_minor` and the points it speaks (today
    /// `["tool/1"]`), receives the plugin's own `major`/`minor_min`/per-point
    /// versions, then applies `docs/plugins/compatibility.md`'s
    /// version-negotiation table:
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
    /// [`ChildSession::framed_round_trip`], never a hang. On any failure the
    /// just-spawned session is dropped by `discover`'s `?`, and its `Drop`
    /// kills the process group, so the child is never orphaned.
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
                    config_id: self.inner.config_id().to_string(),
                    detail: "session died before initialize could be sent".into(),
                }));
        }

        let id = self.inner.next_id();
        let request = PersistentInitializeRequest::new(id);
        let mut json = serde_json::to_vec(&request).map_err(|err| {
            SubprocessPluginError::HandshakeMalformed {
                config_id: self.inner.config_id().to_string(),
                detail: format!("failed to serialize initialize/1 request: {err}"),
            }
        })?;
        json.push(b'\n');

        let value = self.inner.framed_round_trip(id, json).await?;

        let bytes = serde_json::to_vec(&value).map_err(|err| {
            SubprocessPluginError::HandshakeMalformed {
                config_id: self.inner.config_id().to_string(),
                detail: format!("failed to re-serialize initialize response: {err}"),
            }
        })?;
        let answer = parse_persistent_initialize_response(&bytes).map_err(|e| match e {
            // A structurally-broken answer: the plugin is broken, not
            // declining. Fail closed as HandshakeMalformed.
            InitializeParseError::Malformed(detail) => SubprocessPluginError::HandshakeMalformed {
                config_id: self.inner.config_id().to_string(),
                detail,
            },
            // A deliberate `ok:false` WITH an `error` string: the plugin
            // declined initialize. Surface as HandshakeRefused so the
            // operator-facing variant honestly distinguishes "the plugin
            // declined" from "the plugin is broken" -- the same split the
            // version-mismatch rows below already use HandshakeRefused for.
            InitializeParseError::Refused(detail) => SubprocessPluginError::HandshakeRefused {
                config_id: self.inner.config_id().to_string(),
                condition: "ok false".into(),
                detail,
            },
        })?;

        // Correlate the echoed `id` -- a mismatch is a protocol error, fail
        // closed (mirroring `tool_round_trip`'s id check).
        if answer.id != id {
            return Err(SubprocessPluginError::HandshakeMalformed {
                config_id: self.inner.config_id().to_string(),
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
                config_id = %self.inner.config_id(),
                "initialize/1 answer carried unknown fields; ignored and counted \
                 (forward-compat: a newer plugin's extra field does not break an older host)"
            );
        }

        // Apply the compatibility table (version-negotiation rows).
        if answer.major != HOST_WIRE_MAJOR {
            return Err(SubprocessPluginError::HandshakeRefused {
                config_id: self.inner.config_id().to_string(),
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
                config_id: self.inner.config_id().to_string(),
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
    /// answer is the policy. Rides
    /// [`ChildSession::framed_round_trip`] -- the SAME id-correlated NDJSON
    /// framing `initialize/1` and `tool/1` use; NO second reader.
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
    /// [`TimedOut`] -- both via [`ChildSession::framed_round_trip`], never a
    /// hang.
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
                config_id: self.inner.config_id().to_string(),
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
                    config_id: self.inner.config_id().to_string(),
                    detail: "session died before permission.policy/1 could be sent".into(),
                }));
        }

        let id = self.inner.next_id();
        let request = PersistentPermissionPolicyRequest::new(id);
        let mut json = serde_json::to_vec(&request).map_err(|err| {
            SubprocessPluginError::HandshakeMalformed {
                config_id: self.inner.config_id().to_string(),
                detail: format!("failed to serialize permission.policy/1 request: {err}"),
            }
        })?;
        json.push(b'\n');

        let value = self.inner.framed_round_trip(id, json).await?;

        let bytes = serde_json::to_vec(&value).map_err(|err| {
            SubprocessPluginError::HandshakeMalformed {
                config_id: self.inner.config_id().to_string(),
                detail: format!("failed to re-serialize permission.policy response: {err}"),
            }
        })?;
        let answer: PermissionPolicyAnswer = parse_persistent_permission_policy_response(&bytes)
            .map_err(|e| match e {
                PermissionPolicyParseError::Malformed(detail) => {
                    SubprocessPluginError::HandshakeMalformed {
                        config_id: self.inner.config_id().to_string(),
                        detail,
                    }
                }
                PermissionPolicyParseError::Refused(detail) => {
                    SubprocessPluginError::HandshakeRefused {
                        config_id: self.inner.config_id().to_string(),
                        condition: "ok false".into(),
                        detail,
                    }
                }
            })?;

        // Correlate the echoed `id` -- a mismatch is a protocol error, fail
        // closed (mirroring `initialize`'s id check).
        if answer.id != id {
            return Err(SubprocessPluginError::HandshakeMalformed {
                config_id: self.inner.config_id().to_string(),
                detail: format!(
                    "permission.policy response id {} did not match request id {id}",
                    answer.id
                ),
            });
        }

        if answer.unknown_field_count > 0 {
            tracing::debug!(
                unknown_field_count = answer.unknown_field_count,
                config_id = %self.inner.config_id(),
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

    /// The one-time `observe/1` engagement exchange (board item
    /// `01M03VKQ738DTGHHK2C4RWXC0E`), run ONCE at persistent-session open,
    /// AFTER [`Self::request_permission_policy`] succeeds. An OBSERVER point:
    /// the engagement asks the plugin "what do you want to observe?" and the
    /// plugin's answer is its SELECTOR (`["*"]` or a list of `Event` tags).
    /// Rides [`ChildSession::framed_round_trip`] -- the SAME id-correlated
    /// NDJSON framing `initialize/1` and `tool/1` use; NO second reader. The
    /// one-way `observe/1` NOTIFICATIONS themselves (host -> plugin, no `id`)
    /// ride the RAW writer -- a writer task spawned here serializes each
    /// matching `Event` as `{"op":"observe/1",...}\n` onto the plugin's
    /// stdin under the SAME shared stdin lock (via
    /// [`ChildSession::stdin_handle`], deliberately NOT
    /// [`ChildSession::write_frame`] -- see this module's own doc for why:
    /// a write failure here must degrade only this observe path, not tear
    /// down `tool/1` calls sharing the session).
    ///
    /// **Version negotiation via [`Self::point_version`]** (the record
    /// `initialize/1` produced), per `docs/plugins/compatibility.md`'s
    /// observer-vs-participant table -- the OPPOSITE of
    /// `permission.policy/1`'s participant refusal:
    ///
    /// - Plugin did NOT declare `observe/1` (`point_version` is `None`) ->
    ///   load NORMALLY, contribute no observe sink (advertising != requiring).
    ///   Returns `Ok(())` with no state installed.
    /// - Plugin declared it at an UNSUPPORTED version (`!= [
    ///   HOST_OBSERVE_VERSION]`) -> DEGRADE: `tracing::warn!` naming BOTH
    ///   versions, load WITHOUT the point, return `Ok(())`. NEVER
    ///   `HandshakeRefused` -- an observer cannot fail the run by
    ///   construction, so the host loads the plugin regardless and simply
    ///   does not engage the point.
    /// - Plugin declared it at the SUPPORTED version -> exchange the
    ///   engagement request/response, store the selector, spawn the writer
    ///   task, and install the [`ObserveState`] the [`ObserveAdapter`]
    ///   `EventSink` reads.
    ///
    /// A structurally-invalid answer (missing `ok`, `ok:false` with no
    /// `error`, a non-array `events`, a non-string entry) and a deliberate
    /// `ok:false`-with-error BOTH DEGRADE: `tracing::warn!`, return `Ok(())`,
    /// load WITHOUT the point -- observer-class, never fail the session. An
    /// `id` mismatch on the engagement response ALSO degrades (warn, no
    /// engage) rather than failing closed: the response is still
    /// observer-class even though it rode an id-correlated frame. ONLY a
    /// TRANSPORT-level failure during the engagement
    /// ([`SubprocessPluginError::SessionDied`]/[`TimedOut`] from
    /// [`ChildSession::framed_round_trip`]) propagates as `Err` -- a dead/stuck
    /// session is a transport failure, not an observer degrade, and the
    /// caller (`discover`) surfaces it (the just-spawned child is dropped,
    /// its `Drop` kills the group, never orphaned).
    ///
    /// [`SessionDied`]: SubprocessPluginError::SessionDied
    /// [`TimedOut`]: SubprocessPluginError::TimedOut
    pub(crate) async fn request_observe(&self) -> Result<(), SubprocessPluginError> {
        // Version negotiation -- observer rule: None -> no observe; an
        // unsupported version -> DEGRADE (warn, return Ok), the OPPOSITE of
        // permission.policy/1's REFUSE.
        let version = match self.point_version(PersistentObserveRequest::OP) {
            None => return Ok(()),
            Some(v) => v,
        };
        if version != HOST_OBSERVE_VERSION {
            tracing::warn!(
                config_id = %self.inner.config_id(),
                point = "observe/1",
                host_version = HOST_OBSERVE_VERSION,
                plugin_version = version,
                "plugin declared observe/1 at an unsupported version; degrading -- loading \
                 WITHOUT the observe point (observer rule: degrade, not refuse -- the OPPOSITE \
                 of permission.policy/1's participant refusal)"
            );
            return Ok(());
        }

        if self.is_dead() {
            return Err(self
                .death_error()
                .unwrap_or_else(|| SubprocessPluginError::SessionDied {
                    config_id: self.inner.config_id().to_string(),
                    detail: "session died before observe/1 could be sent".into(),
                }));
        }

        let id = self.inner.next_id();
        let request = PersistentObserveRequest::new(id);
        let mut json = serde_json::to_vec(&request).map_err(|err| {
            SubprocessPluginError::HandshakeMalformed {
                config_id: self.inner.config_id().to_string(),
                detail: format!("failed to serialize observe/1 request: {err}"),
            }
        })?;
        json.push(b'\n');

        // A transport-level failure (SessionDied/TimedOut) propagates as Err;
        // the caller surfaces it and the child is dropped, never orphaned.
        let value = self.inner.framed_round_trip(id, json).await?;

        // Parse + classify the engagement response. EVERY parse failure
        // DEGRADES (observer-class): warn, return Ok, load WITHOUT the point.
        let bytes = serde_json::to_vec(&value).map_err(|err| {
            SubprocessPluginError::HandshakeMalformed {
                config_id: self.inner.config_id().to_string(),
                detail: format!("failed to re-serialize observe/1 response: {err}"),
            }
        })?;
        let (selector, unknown_field_count) = match parse_persistent_observe_response(&bytes) {
            Ok(t) => t,
            Err(ObserveParseError::Malformed(detail)) => {
                tracing::warn!(
                    config_id = %self.inner.config_id(),
                    point = "observe/1",
                    %detail,
                    "plugin sent a malformed observe/1 answer; degrading -- loading WITHOUT \
                     the observe point (observer-class: an observer cannot fail the run)"
                );
                return Ok(());
            }
            Err(ObserveParseError::Refused(detail)) => {
                tracing::warn!(
                    config_id = %self.inner.config_id(),
                    point = "observe/1",
                    %detail,
                    "plugin declined observe/1 (ok:false); degrading -- loading WITHOUT the \
                     observe point (observer-class: a declined observer is not a session failure)"
                );
                return Ok(());
            }
        };

        // Correlate the echoed `id` -- a mismatch degrades (warn, no engage)
        // rather than failing closed: the response is observer-class even
        // though it rode an id-correlated frame.
        let resp_id = value.get("id").and_then(|v| v.as_u64());
        if resp_id != Some(id) {
            tracing::warn!(
                config_id = %self.inner.config_id(),
                point = "observe/1",
                expected_id = id,
                observed_id = ?resp_id,
                "observe/1 response id did not match the request; degrading -- loading WITHOUT \
                 the observe point (observer-class: an id mismatch on an observer engagement \
                 degrades, not fails closed)"
            );
            return Ok(());
        }

        if unknown_field_count > 0 {
            tracing::debug!(
                unknown_field_count = unknown_field_count,
                config_id = %self.inner.config_id(),
                "observe/1 answer carried unknown fields; ignored and counted (forward-compat)"
            );
        }

        // Spawn the observe writer task: drains the bounded `Event` channel
        // the `ObserveAdapter` pushes onto, filters each `Event` by the
        // declared selector, and serializes the survivor as an `observe/1`
        // notification line onto the plugin's stdin under the SHARED stdin
        // lock (so observe and `tool/1` writes never interleave). Each write
        // is bounded by `timeout_ms`; on a write failure/timeout the writer
        // stops forwarding and sets `broken` (lossy-with-notice: the channel
        // then fills and the adapter drops+warns, never blocking the host
        // turn). A pathological write holding the stdin lock past `timeout_ms`
        // is bounded by `ChildSession`'s OWN write deadline (on a `tool/1`
        // round trip sharing this lock), which kills the group on timeout --
        // the shared lock cannot hang a `tool/1` call.
        let (ev_tx, ev_rx) = mpsc::channel::<Event>(OBSERVE_CHANNEL_CAPACITY);
        let broken = Arc::new(AtomicBool::new(false));
        let writer_stdin = self.inner.stdin_handle();
        let writer_timeout = self.inner.timeout_ms();
        let writer_config_id = self.inner.config_id().to_string();
        let writer_broken = broken.clone();
        let writer_handle = tokio::spawn(async move {
            let mut rx = ev_rx;
            while let Some(event) = rx.recv().await {
                let line = match build_observe_notification(&event, &selector) {
                    Some(line) => line,
                    None => continue, // filtered out by the selector, or serialize failure
                };
                let write_result = timeout(Duration::from_millis(writer_timeout), async {
                    let mut stdin = writer_stdin.lock().await;
                    stdin.write_all(&line).await?;
                    stdin.flush().await?;
                    Ok::<(), std::io::Error>(())
                })
                .await;
                match write_result {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => {
                        writer_broken.store(true, Ordering::Relaxed);
                        tracing::warn!(
                            config_id = %writer_config_id,
                            error = %err,
                            "observe/1 notification write failed; stopping the observe forwarding \
                             task (lossy-with-notice: the host turn is unaffected; the session's \
                             own tool/1 write deadline handles a genuinely dead stdin)"
                        );
                        return;
                    }
                    Err(_elapsed) => {
                        writer_broken.store(true, Ordering::Relaxed);
                        tracing::warn!(
                            config_id = %writer_config_id,
                            after_ms = writer_timeout,
                            "observe/1 notification write did not complete within the per-write \
                             deadline; stopping the observe forwarding task (lossy-with-notice: \
                             a slow plugin read loop must not stall the host turn)"
                        );
                        return;
                    }
                }
            }
        });

        {
            let mut obs = self.observe_state.lock().expect("observe_state poisoned");
            *obs = Some(ObserveState {
                tx: ev_tx,
                broken,
                _writer: writer_handle,
            });
        }
        Ok(())
    }

    /// The one-time `status.declare/1` engagement exchange (board item
    /// `01M03VKQ738DTGHHK2C4RWXC0E`), run ONCE at persistent-session open,
    /// AFTER [`Self::request_observe`]. An OBSERVER point: the engagement asks
    /// the plugin "what status keys will you push?" and the plugin's answer is
    /// its per-key declaration metadata (`{ key, max_len?, ttl_ms? }`). Rides
    /// [`ChildSession::framed_round_trip`] -- the SAME id-correlated NDJSON
    /// framing; NO second reader. The one-way `status/1` NOTIFICATIONS
    /// themselves (plugin -> host, no `id`) ride the RAW reader -- the
    /// existing `ChildSession` reader routes no-`id` lines to this session's
    /// own notification channel, and the handler task stores the latest
    /// contribution per `key` in [`Self::status`].
    ///
    /// **Version negotiation** -- identical observer rule to
    /// [`Self::request_observe`]: `None` -> load normally, no status surface;
    /// an unsupported version -> DEGRADE (warn, load without the point); the
    /// supported version -> exchange, store the declarations. A malformed or
    /// refused answer, and an `id` mismatch, ALL degrade (warn, return `Ok`)
    /// -- observer-class, never fail the session. ONLY a transport-level
    /// failure ([`SubprocessPluginError::SessionDied`]/[`TimedOut`]) from
    /// [`ChildSession::framed_round_trip`] propagates as `Err`.
    ///
    /// The declarations are stored on `self.status_declarations` for the
    /// facade surface; the ttl/expiry RENDER path itself stays design-only
    /// (`docs/plugins/hooks.md` point 12).
    ///
    /// [`SessionDied`]: SubprocessPluginError::SessionDied
    /// [`TimedOut`]: SubprocessPluginError::TimedOut
    pub(crate) async fn request_status_declare(&self) -> Result<(), SubprocessPluginError> {
        // Version negotiation -- observer rule (same as request_observe).
        let version = match self.point_version(PersistentStatusDeclareRequest::OP) {
            None => return Ok(()),
            Some(v) => v,
        };
        if version != HOST_STATUS_VERSION {
            tracing::warn!(
                config_id = %self.inner.config_id(),
                point = "status.declare/1",
                host_version = HOST_STATUS_VERSION,
                plugin_version = version,
                "plugin declared status.declare/1 at an unsupported version; degrading -- \
                 loading WITHOUT the status point (observer rule: degrade, not refuse)"
            );
            return Ok(());
        }

        if self.is_dead() {
            return Err(self
                .death_error()
                .unwrap_or_else(|| SubprocessPluginError::SessionDied {
                    config_id: self.inner.config_id().to_string(),
                    detail: "session died before status.declare/1 could be sent".into(),
                }));
        }

        let id = self.inner.next_id();
        let request = PersistentStatusDeclareRequest::new(id);
        let mut json = serde_json::to_vec(&request).map_err(|err| {
            SubprocessPluginError::HandshakeMalformed {
                config_id: self.inner.config_id().to_string(),
                detail: format!("failed to serialize status.declare/1 request: {err}"),
            }
        })?;
        json.push(b'\n');

        let value = self.inner.framed_round_trip(id, json).await?;

        let bytes = serde_json::to_vec(&value).map_err(|err| {
            SubprocessPluginError::HandshakeMalformed {
                config_id: self.inner.config_id().to_string(),
                detail: format!("failed to re-serialize status.declare/1 response: {err}"),
            }
        })?;
        let (decls, unknown_field_count) = match parse_persistent_status_declare_response(&bytes) {
            Ok(t) => t,
            Err(StatusDeclareParseError::Malformed(detail)) => {
                tracing::warn!(
                    config_id = %self.inner.config_id(),
                    point = "status.declare/1",
                    %detail,
                    "plugin sent a malformed status.declare/1 answer; degrading -- loading \
                     WITHOUT the status point (observer-class)"
                );
                return Ok(());
            }
            Err(StatusDeclareParseError::Refused(detail)) => {
                tracing::warn!(
                    config_id = %self.inner.config_id(),
                    point = "status.declare/1",
                    %detail,
                    "plugin declined status.declare/1 (ok:false); degrading -- loading WITHOUT \
                     the status point (observer-class)"
                );
                return Ok(());
            }
        };

        let resp_id = value.get("id").and_then(|v| v.as_u64());
        if resp_id != Some(id) {
            tracing::warn!(
                config_id = %self.inner.config_id(),
                point = "status.declare/1",
                expected_id = id,
                observed_id = ?resp_id,
                "status.declare/1 response id did not match the request; degrading -- loading \
                 WITHOUT the status point (observer-class: an id mismatch degrades, not fails \
                 closed)"
            );
            return Ok(());
        }

        if unknown_field_count > 0 {
            tracing::debug!(
                unknown_field_count = unknown_field_count,
                config_id = %self.inner.config_id(),
                "status.declare/1 answer carried unknown fields; ignored and counted (forward-compat)"
            );
        }

        {
            let mut decl = self
                .status_declarations
                .lock()
                .expect("status_declarations poisoned");
            *decl = decls;
        }
        Ok(())
    }

    /// An [`EventSinkHandle`] the host fans the runtime's live `Event` stream
    /// onto so this plugin can OBSERVE host events over its session -- the
    /// host-side half of the `observe/1` wire point (board item
    /// `01M03VKQ738DTGHHK2C4RWXC0E`). `None` when the plugin did not declare
    /// `observe/1`, declared it at an unsupported version (DEGRADE), or before
    /// [`Self::request_observe`] has run. The sink pushes to a bounded channel
    /// the observe writer task drains (lossy-with-notice, never blocks the
    /// host turn); see [`ObserveAdapter`]'s own doc.
    pub(crate) fn observe_sink(&self) -> Option<EventSinkHandle> {
        let obs = self.observe_state.lock().expect("observe_state poisoned");
        obs.as_ref().map(|state| {
            Arc::new(ObserveAdapter {
                tx: state.tx.clone(),
                broken: state.broken.clone(),
                config_id: self.inner.config_id().to_string(),
            }) as EventSinkHandle
        })
    }

    /// A point-in-time snapshot of the status contributions this plugin is
    /// CURRENTLY pushing -- the host-side half of the `status.declare/1` /
    /// `status/1` wire point (board item `01M03VKQ738DTGHHK2C4RWXC0E`). Reads
    /// [`Self::status`], which the notification handler task updates from
    /// inbound no-`id` `status/1` lines (latest per `key`). Empty for a plugin
    /// that did not declare the point, declared it at an unsupported version
    /// (DEGRADE), or has not yet pushed any `status/1` notifications. NOT a
    /// build-time declaration -- a polled snapshot of an asynchronous push.
    pub(crate) fn status_contributions(&self) -> Vec<WireStatusContribution> {
        self.status
            .lock()
            .expect("status map poisoned")
            .values()
            .cloned()
            .collect()
    }
}
