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
//! be a notification. Board item `01M03VKQ738DTGHHK2C4RWXC0E` wires exactly
//! that: a no-`id` line is now an inbound NOTIFICATION routed to a bounded
//! channel (drop+warn on overflow, never kills the session -- observer-class),
//! not a malformed frame. The `status/1` notification is the first occupant;
//! the handler task parses `op` and stores the latest contribution per `key`.
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
//!   SIGTERM-then-SIGKILL `conway::plugin::kill_group` uses on the timeout
//!   path). `kill_on_drop(true)` is ALSO set on the `Command` as a
//!   belt-and-suspenders so the leader dies even if our `Drop`'s
//!   `kill(-pgid)` is beaten to it. A long-lived child is never orphaned.
//! - **`kill_group` is SHARED, not duplicated (board item
//!   `01M0EKVR1BEXXS75NV2JC4HZZ9`)** -- `conway::plugin::kill_group` (the
//!   ONE implementation every crate that needs this now calls, re-exported
//!   from `conway_tools::process::unix::kill_group`) is reused for the
//!   graceful timeout kill; the synchronous `Drop`-time SIGKILL still uses
//!   `nix::sys::signal::kill` directly (`Drop` cannot `await`).

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};
use tokio::time::timeout;

use conway::plugin::{kill_group, Event, EventSink, EventSinkHandle, ToolError};

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
    /// Inbound notification channel (board item `01M03VKQ738DTGHHK2C4RWXC0E`):
    /// the reader task routes any no-`id` stdout line here via a NON-blocking
    /// `try_send` (drop+warn on `Full`, never blocks the host turn, never
    /// kills the session -- observer-class). The notification handler task
    /// drains the receiver, parses `op`, and stores a `status/1` line's
    /// contribution in [`Shared::status`]; an unknown `op` is dropped with a
    /// `tracing::warn!`. This is NOT touched by `kill_all` -- a notification
    /// is an observer-class line and must not tear down the session even when
    /// the session dies for an unrelated reason (the handler task simply ends
    /// when the sender half drops on session drop).
    notifications: mpsc::Sender<serde_json::Value>,
    /// The latest status contribution per `key`, pushed by the notification
    /// handler task from inbound `status/1` no-`id` lines (board item
    /// `01M03VKQ738DTGHHK2C4RWXC0E`). A later line for the same `key`
    /// overwrites an earlier one (per `docs/plugins/hooks.md` point 12's "a
    /// stale value expires at snapshot time" shape -- the ttl/expiry RENDER
    /// path itself stays design-only). Read by [`PersistentSession::
    /// status_contributions`] for the `Plugin::status_contributions` trait
    /// method -- a point-in-time snapshot, NOT a build-time declaration.
    status: Mutex<HashMap<String, WireStatusContribution>>,
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
    /// The child's stdin write half, shared between `framed_round_trip`
    /// (id-correlated `tool/1`/`initialize/1`/point-engagement requests) and
    /// the observe writer task (one-way `observe/1` notifications, board item
    /// `01M03VKQ738DTGHHK2C4RWXC0E`). Behind an `Arc<AsyncMutex>` so the
    /// observe writer (a separate task) can share it with `framed_round_trip`
    /// WITHOUT interleaving lines -- the mutex serializes every write, so an
    /// observe notification and a `tool/1` request never corrupt each other's
    /// framing. A pathological observe write that holds the lock past
    /// `timeout_ms` is bounded by `framed_round_trip`'s OWN write deadline
    /// (which kills the group on timeout), so the shared lock cannot hang a
    /// `tool/1` call indefinitely.
    stdin: Arc<AsyncMutex<ChildStdin>>,
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
    /// Kept (never awaited) so the tasks are not leaked: they end on
    /// stdout/stderr EOF or when the session is killed.
    _reader_handle: tokio::task::JoinHandle<()>,
    _stderr_handle: tokio::task::JoinHandle<()>,
    /// The inbound notification handler task (board item
    /// `01M03VKQ738DTGHHK2C4RWXC0E`): drains [`Shared::notifications`],
    /// parses `op`, and stores `status/1` contributions in
    /// [`Shared::status`]. Kept (never awaited) so it is not leaked; it ends
    /// when the sender half drops (session drop) or the receiver errors.
    _notification_handle: tokio::task::JoinHandle<()>,
}

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

            // The inbound notification channel (board item
            // `01M03VKQ738DTGHHK2C4RWXC0E`): the reader routes no-`id` lines
            // here; the handler task drains it and stores `status/1`
            // contributions. Bounded + `try_send` so a flooding plugin degrades
            // lossy-with-notice (drop+warn) rather than blocking the reader or
            // killing the session.
            let (notif_tx, notif_rx) =
                mpsc::channel::<serde_json::Value>(NOTIFICATION_CHANNEL_CAPACITY);

            let shared = Arc::new(Shared {
                pending: Mutex::new(HashMap::new()),
                dead: AtomicBool::new(false),
                death: Mutex::new(None),
                notifications: notif_tx,
                status: Mutex::new(HashMap::new()),
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
                                    // No correlation `id` on the wire: an
                                    // inbound one-way NOTIFICATION (board item
                                    // `01M03VKQ738DTGHHK2C4RWXC0E` -- the
                                    // `status/1` push is the first occupant).
                                    // Route to the bounded notification channel
                                    // via a NON-blocking `try_send`: on `Full`,
                                    // DROP the line with a `tracing::warn!`
                                    // (lossy-with-notice -- a flooding plugin
                                    // must not stall the host turn); on
                                    // `Closed`, the handler task has ended
                                    // (session dropping) -- drop silently.
                                    // NEVER `kill_all`: an observer-class line
                                    // must not tear down the session, the
                                    // OPPOSITE of the old malformed-frame
                                    // behavior. Keep reading the next line.
                                    match reader_shared.notifications.try_send(value) {
                                        Ok(()) => {}
                                        Err(mpsc::error::TrySendError::Full(_)) => {
                                            tracing::warn!(
                                                config_id = %reader_config_id,
                                                "inbound notification channel full; dropping a \
                                                 no-id line (lossy-with-notice: a flooding plugin \
                                                 must not stall the host turn, per the observer rule)"
                                            );
                                        }
                                        Err(mpsc::error::TrySendError::Closed(_)) => {
                                            // Handler task gone (session
                                            // dropping) -- drop silently and
                                            // keep reading until EOF.
                                        }
                                    }
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

            // The inbound notification handler task (board item
            // `01M03VKQ738DTGHHK2C4RWXC0E`): drains the notification channel
            // the reader routes no-`id` lines onto, parses `op`, and stores a
            // `status/1` line's contribution in `Shared::status` (latest per
            // `key`). An unknown `op` (or a structurally-invalid `status/1`
            // body) is dropped with a `tracing::warn!` -- observer-class,
            // degrade, NEVER fails the session. A separate task from the
            // reader so parsing/storing never blocks stdout reading (the
            // bounded channel + `try_send` enforce that boundary).
            let handler_shared = shared.clone();
            let handler_config_id = spec.config_id.clone();
            let notification_handle = tokio::spawn(async move {
                let mut rx = notif_rx;
                while let Some(value) = rx.recv().await {
                    let op = value.get("op").and_then(|v| v.as_str()).unwrap_or("");
                    if op == "status/1" {
                        match parse_status_notification(&value) {
                            Ok(contrib) => {
                                let mut status =
                                    handler_shared.status.lock().expect("status map poisoned");
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
                config_id: spec.config_id.clone(),
                pgid,
                timeout_ms: spec.timeout_ms,
                child: AsyncMutex::new(Some(child)),
                stdin: Arc::new(AsyncMutex::new(stdin)),
                next_id: AtomicU64::new(1),
                shared,
                point_versions: Mutex::new(HashMap::new()),
                permission_policy: Mutex::new(Vec::new()),
                status_declarations: Mutex::new(Vec::new()),
                observe_state: Mutex::new(None),
                _reader_handle: reader_handle,
                _stderr_handle: stderr_handle,
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

    /// The one-time `observe/1` engagement exchange (board item
    /// `01M03VKQ738DTGHHK2C4RWXC0E`), run ONCE at persistent-session open,
    /// AFTER [`Self::request_permission_policy`] succeeds. An OBSERVER point:
    /// the engagement asks the plugin "what do you want to observe?" and the
    /// plugin's answer is its SELECTOR (`["*"]` or a list of `Event` tags).
    /// Rides the SAME id-correlated NDJSON framing `initialize/1` and
    /// `tool/1` use (via [`Self::framed_round_trip`]); NO second reader. The
    /// one-way `observe/1` NOTIFICATIONS themselves (host -> plugin, no `id`)
    /// ride the RAW writer -- a writer task spawned here serializes each
    /// matching `Event` as `{"op":"observe/1",...}\n` onto the plugin's stdin
    /// under the shared stdin lock.
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
    /// [`Self::framed_round_trip`]) propagates as `Err` -- a dead/stuck
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
                config_id = %self.config_id,
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
                    config_id: self.config_id.clone(),
                    detail: "session died before observe/1 could be sent".into(),
                }));
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = PersistentObserveRequest::new(id);
        let mut json = serde_json::to_vec(&request).map_err(|err| {
            SubprocessPluginError::HandshakeMalformed {
                config_id: self.config_id.clone(),
                detail: format!("failed to serialize observe/1 request: {err}"),
            }
        })?;
        json.push(b'\n');

        // A transport-level failure (SessionDied/TimedOut) propagates as Err;
        // the caller surfaces it and the child is dropped, never orphaned.
        let value = self.framed_round_trip(id, json).await?;

        // Parse + classify the engagement response. EVERY parse failure
        // DEGRADES (observer-class): warn, return Ok, load WITHOUT the point.
        let bytes = serde_json::to_vec(&value).map_err(|err| {
            SubprocessPluginError::HandshakeMalformed {
                config_id: self.config_id.clone(),
                detail: format!("failed to re-serialize observe/1 response: {err}"),
            }
        })?;
        let (selector, unknown_field_count) = match parse_persistent_observe_response(&bytes) {
            Ok(t) => t,
            Err(ObserveParseError::Malformed(detail)) => {
                tracing::warn!(
                    config_id = %self.config_id,
                    point = "observe/1",
                    %detail,
                    "plugin sent a malformed observe/1 answer; degrading -- loading WITHOUT \
                     the observe point (observer-class: an observer cannot fail the run)"
                );
                return Ok(());
            }
            Err(ObserveParseError::Refused(detail)) => {
                tracing::warn!(
                    config_id = %self.config_id,
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
                config_id = %self.config_id,
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
                config_id = %self.config_id,
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
        // is bounded by `framed_round_trip`'s OWN write deadline, which kills
        // the group on timeout -- the shared lock cannot hang a `tool/1` call.
        let (ev_tx, ev_rx) = mpsc::channel::<Event>(OBSERVE_CHANNEL_CAPACITY);
        let broken = Arc::new(AtomicBool::new(false));
        let writer_stdin = self.stdin.clone();
        let writer_timeout = self.timeout_ms;
        let writer_config_id = self.config_id.clone();
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
    /// the SAME id-correlated NDJSON framing; NO second reader. The one-way
    /// `status/1` NOTIFICATIONS themselves (plugin -> host, no `id`) ride the
    /// RAW reader -- the existing reader task routes no-`id` lines to the
    /// notification channel, and the handler task stores the latest
    /// contribution per `key` in [`Shared::status`].
    ///
    /// **Version negotiation** -- identical observer rule to
    /// [`Self::request_observe`]: `None` -> load normally, no status surface;
    /// an unsupported version -> DEGRADE (warn, load without the point); the
    /// supported version -> exchange, store the declarations. A malformed or
    /// refused answer, and an `id` mismatch, ALL degrade (warn, return `Ok`)
    /// -- observer-class, never fail the session. ONLY a transport-level
    /// failure ([`SubprocessPluginError::SessionDied`]/[`TimedOut`]) from
    /// [`Self::framed_round_trip`] propagates as `Err`.
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
                config_id = %self.config_id,
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
                    config_id: self.config_id.clone(),
                    detail: "session died before status.declare/1 could be sent".into(),
                }));
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = PersistentStatusDeclareRequest::new(id);
        let mut json = serde_json::to_vec(&request).map_err(|err| {
            SubprocessPluginError::HandshakeMalformed {
                config_id: self.config_id.clone(),
                detail: format!("failed to serialize status.declare/1 request: {err}"),
            }
        })?;
        json.push(b'\n');

        let value = self.framed_round_trip(id, json).await?;

        let bytes = serde_json::to_vec(&value).map_err(|err| {
            SubprocessPluginError::HandshakeMalformed {
                config_id: self.config_id.clone(),
                detail: format!("failed to re-serialize status.declare/1 response: {err}"),
            }
        })?;
        let (decls, unknown_field_count) = match parse_persistent_status_declare_response(&bytes) {
            Ok(t) => t,
            Err(StatusDeclareParseError::Malformed(detail)) => {
                tracing::warn!(
                    config_id = %self.config_id,
                    point = "status.declare/1",
                    %detail,
                    "plugin sent a malformed status.declare/1 answer; degrading -- loading \
                     WITHOUT the status point (observer-class)"
                );
                return Ok(());
            }
            Err(StatusDeclareParseError::Refused(detail)) => {
                tracing::warn!(
                    config_id = %self.config_id,
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
                config_id = %self.config_id,
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
                config_id = %self.config_id,
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
                config_id: self.config_id.clone(),
            }) as EventSinkHandle
        })
    }

    /// A point-in-time snapshot of the status contributions this plugin is
    /// CURRENTLY pushing -- the host-side half of the `status.declare/1` /
    /// `status/1` wire point (board item `01M03VKQ738DTGHHK2C4RWXC0E`). Reads
    /// [`Shared::status`], which the notification handler task updates from
    /// inbound no-`id` `status/1` lines (latest per `key`). Empty for a plugin
    /// that did not declare the point, declared it at an unsupported version
    /// (DEGRADE), or has not yet pushed any `status/1` notifications. NOT a
    /// build-time declaration -- a polled snapshot of an asynchronous push.
    pub(crate) fn status_contributions(&self) -> Vec<WireStatusContribution> {
        self.shared
            .status
            .lock()
            .expect("status map poisoned")
            .values()
            .cloned()
            .collect()
    }

    fn remove_pending(&self, id: u64) {
        let mut pending = self.shared.pending.lock().expect("pending poisoned");
        pending.remove(&id);
    }

    /// Kills the process group with the graceful SIGTERM-then-SIGKILL
    /// sequence (`conway::plugin::kill_group`, the one shared
    /// implementation -- board item `01M0EKVR1BEXXS75NV2JC4HZZ9`) and marks
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
