//! `conway-plugin-mcp`: an MCP-over-stdio CLIENT plugin (board item
//! `01M03GPNF0KN59FHAEEAEY2JD3`; `PHILOSOPHY.md` §5: "An MCP server is a plugin
//! that brings tools with it"). A [`Plugin`] implementation that speaks
//! JSON-RPC 2.0 as a CLIENT -- it spawns an operator-configured external MCP
//! server as a persistent child process ONCE at discovery time, completes the
//! `initialize`/`notifications/initialized` handshake, calls `tools/list`, and
//! exposes every tool the server declares as an ordinary
//! `conway::plugin::Tool` whose `invoke` calls `tools/call` over the same
//! persistent stdio.
//!
//! **NOT an MCP server.** This crate does NOT expose conway itself over MCP
//! (a separate, lower-priority question). The conway harness is the MCP
//! CLIENT; the spawned child is the MCP SERVER. The plugin is an IN-PROCESS
//! `Arc<dyn Plugin>` attached via `ConwayBuilder::with_plugin`, exactly like
//! `conway-tools`' builtins -- it does NOT itself go through conway's
//! subprocess host.
//!
//! **A sibling transport to `conway-plugin-subprocess`, NOT a layering on it.**
//! `conway-plugin-subprocess`'s `PersistentSession` speaks conway's OWN wire
//! protocol (`initialize/1`, `tool.spec/1`, `tool/1`, ...); MCP speaks a
//! DIFFERENT protocol (JSON-RPC 2.0: `initialize`, `notifications/initialized`,
//! `tools/list`, `tools/call`, capability structs). So this crate owns its
//! OWN lightweight `McpSession` (see `session`) that reuses the PATTERN
//! `PersistentSession` proves out -- spawn once + `kill_on_drop(true)` child,
//! a long-lived reader task routing inbound lines by JSON-RPC `id` via a
//! pending table, a `framed_round_trip` that writes one line and awaits the
//! matching reply, a stderr drain, and Drop-time group SIGKILL -- but parses
//! JSON-RPC 2.0, not conway wire. This crate does NOT depend on
//! `conway-plugin-subprocess`; the two transports are siblings.
//!
//! **NO new dependency.** MCP's wire protocol IS JSON-RPC 2.0 -- hand-rolled
//! with `serde_json` (already in the workspace graph). The official `rmcp` SDK
//! or any MCP client library is RECOMMENDED AGAINST (the spec's hard
//! constraint + the operator's memory): they pull async-runtime/HTTP stacks
//! disproportionate to a stdio JSON-RPC codec, and `cargo deny check` has
//! caught an ungranted licence from exactly this kind of addition before.
//!
//! **What this crate is NOT: a trust mechanism.** An MCP server's `command`
//! executes with the operator's own privileges, unsandboxed -- the SAME
//! footing `[hooks].rules[].command` and `[plugins].subprocess[]` already
//! have (see `conway-plugin-subprocess`'s own crate doc for the full
//! argument). Board item `01KZHVFCN6ZEAXV7K5JHRQN1YB` (a `plugin` trust
//! subject kind keyed on a content digest) was reopened once both
//! out-of-process transports shipped (decision `01M0R4RWCDJJ6RMNVFYCNHW0NK`
//! lifted the 2026-08-12 standing deferral) and worked to a conclusion:
//! DECLINED, for the reasons `conway-plugin-subprocess`'s own crate doc now
//! states in full -- a digest check gated onto only the out-of-process
//! transports, while `[hooks].rules[].command` stays permanently ungated,
//! would assert a distinction (plugins reviewed, hooks not) that the
//! identical unsandboxed, full-privilege execution underneath both does not
//! support. Naming an MCP server in `settings.json` is exactly as trusted,
//! and exactly as unaudited, as naming a `[hooks].rules[].command` already
//! is today.
//!
//! **HTTP+SSE MCP transport is a SEPARATE item -- do NOT fold it in.** This
//! crate is stdio only.
//!
//! **No MCP prompts or resources.** This crate performs `initialize`,
//! `tools/list`, and `tools/call` only -- MCP's `prompts` methods (`list`,
//! `get`) and `resources` methods (`list`, `read`) are never called (see
//! `docs/plugins/mcp.md`'s own "No MCP prompts or resources" bullet for the
//! exact wire-form names and the grep that confirms it). A prompt would map
//! onto a namespaced slash command (`Plugin::commands()`); a resource onto
//! a read-only `Tool` or a `Plugin::instructions()` fragment. Neither is
//! built or scheduled -- forward-declared here, not tracked, since no item
//! exists to build one.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex as StdMutex};

use conway::plugin::{
    async_trait, ChildSessionError, PathArgs, Plugin, PluginManifest, PluginStatusContribution,
    RenderKind, ResultStatus, Tool, ToolCall, ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec,
    TruncationPolicy,
};

mod session;
mod wire;

pub use session::McpSession;
use wire::CallOutcome;

/// Applied when a [`McpPluginSpec`] does not name its own `timeout_ms`.
///
/// **Re-exported, not restated (board item `01M0TV6E2K6QF9VXP6C7TFH06X`).**
/// This used to be its own `pub const` declaring the same 5000ms value,
/// kept "the same" as `conway-plugin-subprocess`'s identical constant by a
/// doc comment alone -- nothing enforced the two literals actually
/// agreeing. The value now lives once, at `conway::plugin::DEFAULT_TIMEOUT_MS` (see
/// that item's own doc for why it is declared directly on the facade rather
/// than routed through `conway-tools` the way [`conway::plugin::kill_group`]
/// is); this is that same constant, re-exported so the old
/// `conway_plugin_mcp::DEFAULT_TIMEOUT_MS` path still resolves.
pub use conway::plugin::DEFAULT_TIMEOUT_MS;

/// The default deadline for an MCP server's OPENING `initialize` handshake:
/// two minutes, against [`DEFAULT_TIMEOUT_MS`]'s five seconds for every
/// request after it.
///
/// These bound genuinely different things. A per-call deadline asks how long
/// an already-running server may take to answer one request, and five seconds
/// is generous for that. The first round trip additionally covers the server
/// becoming able to answer anything -- process spawn, runtime start, and, for
/// a Claude Code plugin, possibly a first-launch BUILD.
///
/// **Operator-reported, 2026-08-30.** Claude Code installs a plugin by
/// cloning it with no build step and bundles no runtime, so a plugin whose
/// server is compiled builds itself on first launch: ideate's
/// `bin/ideate-mcp` runs `npm install && npm run build` before exec'ing Node,
/// which is minutes on a cold cache and cannot fit in five seconds. conway's
/// `claude_compat` has to tolerate that to host the same plugins, and
/// tolerating it is what makes an install work on the operator's machine
/// rather than only on one where the plugin happens to be prebuilt.
///
/// Two minutes is a real bound, not "effectively forever": a server that
/// cannot open a session in two minutes is wedged, and failing then is
/// better than hanging. It is deliberately generous because the cost of
/// being wrong is asymmetric -- too short bricks a legitimate first launch,
/// too long delays a diagnosis the operator can also get from the failing
/// server's own stderr.
pub const DEFAULT_STARTUP_TIMEOUT_MS: u64 = 120_000;

/// Applied when a [`McpPluginSpec`] does not name its own
/// `first_call_timeout_ms`: the deadline the FIRST ordinary round trip
/// after `initialize` gets, against [`DEFAULT_TIMEOUT_MS`] for every
/// ordinary round trip after THAT one.
///
/// **Re-exported, not restated -- the [`DEFAULT_TIMEOUT_MS`] pattern
/// (board item `01M0TV6E2K6QF9VXP6C7TFH06X`), applied here for the same
/// reason.** The value lives once, at
/// `conway::plugin::DEFAULT_FIRST_CALL_TIMEOUT_MS` (see that constant's own
/// doc for the full argument -- the third tier between
/// [`DEFAULT_TIMEOUT_MS`] and [`DEFAULT_STARTUP_TIMEOUT_MS`], and the
/// incident it answers), so `crates/conway/src/config/schema.rs`'s
/// `McpPluginEntry::first_call_timeout_ms` default and this crate's own
/// [`McpPluginSpec::new`] draw from the SAME authority rather than two
/// literals that could silently drift apart.
pub use conway::plugin::DEFAULT_FIRST_CALL_TIMEOUT_MS;

/// The maximum number of times a single [`McpPlugin`] will transparently
/// respawn its child after the session a tool call was using turns out to
/// be dead, across the plugin's WHOLE lifetime -- this counter never resets,
/// not even after a long run of healthy calls in between. See
/// [`McpPluginError::SessionDied`]'s own doc for the policy this bound
/// completes, and `SessionSlot::respawn` for where it is spent.
///
/// **Three, argued the way [`DEFAULT_STARTUP_TIMEOUT_MS`] argues 120
/// seconds.** The incident this item answers -- one `tools/call` timing out
/// under CPU contention, then EVERY later call against that plugin failing
/// identically for the rest of the process's life, for hours, requiring a
/// full conway restart -- is exactly the ONE-transient-death case a single
/// respawn already fixes. A second or third death within the same
/// lifetime still plausibly reads as a run of bad luck: a build competing
/// for CPU across several consecutive calls, a scheduler hiccup, a server
/// stumbling again while still warming up. A FOURTH death stops reading as
/// bad luck and starts reading as a server that cannot stay up --
/// respawning it a fourth, fifth, or unbounded number of times would
/// busy-loop spawning a genuinely broken child forever, burning CPU and
/// file descriptors while never telling the operator anything is wrong.
/// Reverting to the pre-this-item fail-closed posture (surface
/// `SessionDied`, require the operator to act) once that is established is
/// the same "stop trusting a dependency that has proven itself unhealthy"
/// instinct [`McpPluginError::SessionDied`]'s own doc already argues for a
/// single death; this bound is where that instinct kicks back in.
///
/// **A flat per-lifetime count, deliberately NOT a time window.** A
/// "N deaths per M minutes" policy auto-heals forever as long as the deaths
/// are spread out -- exactly the blind spot a server that is unhealthy but
/// not obviously crash-looping (one death every ten minutes, indefinitely)
/// would exploit. A flat cap has no such blind spot: three respawns total,
/// ever, no matter how they are spaced in time.
pub const MAX_AUTO_RESPAWNS: u32 = 3;

/// One operator-configured MCP-over-stdio plugin entry: the command to spawn
/// (the external MCP server), how long any single framed JSON-RPC round-trip
/// is allowed to run before this host kills it, and the explicit environment
/// the child runs under (acceptance 3 -- credentials/connection-lifecycle
/// scoping is EXPLICIT, not left to implicit env inheritance).
///
/// **Trust, stated where the capability is defined.** `command` is an argv
/// vector (program, then its arguments) -- never a single shell string, the
/// identical shape `HookEntry::command` / `SubprocessPluginEntry::command`
/// already use and for the identical reason. Naming a command here is naming
/// code the operator's own machine executes with the operator's own
/// privileges, unsandboxed -- this type performs no validation of `command`
/// beyond "spawnable", the same posture `conway-plugin-subprocess` takes
/// toward its own `SubprocessPluginSpec::command`. See this crate's own
/// module doc for the full trust disclosure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpPluginSpec {
    /// This plugin's operator-chosen id, used only in error messages this
    /// crate produces (which configured entry misbehaved) -- NOT trusted as
    /// the plugin's own `PluginManifest::id`, which is derived from the MCP
    /// server's `serverInfo.name` (see [`McpPlugin::discover`]'s own doc for
    /// how the manifest id is built).
    pub config_id: String,
    /// The command to spawn (the external MCP server), argv-shaped (program,
    /// then its arguments) -- never a single shell string.
    pub command: Vec<String>,
    /// Milliseconds any single framed JSON-RPC round-trip (`initialize`,
    /// `tools/list`, or one `tools/call`) is allowed to run before this host
    /// kills the process group and fails closed. Defaults to
    /// [`DEFAULT_TIMEOUT_MS`] when constructed via [`McpPluginSpec::new`]. A
    /// PER-CALL deadline, NOT a session-wide idle kill (a session that sits
    /// idle between calls is left alone).
    pub timeout_ms: u64,
    /// The deadline for the OPENING `initialize` handshake only -- see
    /// [`DEFAULT_STARTUP_TIMEOUT_MS`] for why this is separate from
    /// [`Self::timeout_ms`]. Every later request uses `timeout_ms`.
    pub startup_timeout_ms: u64,
    /// The deadline the FIRST ordinary round trip after `initialize`
    /// (this session's first real `tools/call`) is allowed to run before
    /// this host kills the process group -- see
    /// [`DEFAULT_FIRST_CALL_TIMEOUT_MS`] for the full argument. Every
    /// ordinary round trip AFTER the first uses [`Self::timeout_ms`]; an
    /// operator who tuned `timeout_ms` alone sees no change in what it
    /// governs.
    pub first_call_timeout_ms: u64,
    /// Explicit environment pairs the child inherits IN ADDITION to the
    /// parent process's own env -- the identical shape a `[hooks].rules[]`
    /// entry's env carries, so an operator scopes credentials/connection
    /// lifecycle by naming them here rather than relying on implicit
    /// inheritance (acceptance 3). Empty by default: the child inherits the
    /// parent env unchanged, the same default a hook command has.
    pub env: Vec<(String, String)>,
}

impl McpPluginSpec {
    /// A spec with [`DEFAULT_TIMEOUT_MS`] and an empty `env` (the child
    /// inherits the parent env unchanged). Use the struct literal directly to
    /// override `timeout_ms` or `env`.
    pub fn new(config_id: impl Into<String>, command: Vec<String>) -> Self {
        Self {
            config_id: config_id.into(),
            command,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            startup_timeout_ms: DEFAULT_STARTUP_TIMEOUT_MS,
            first_call_timeout_ms: DEFAULT_FIRST_CALL_TIMEOUT_MS,
            env: Vec::new(),
        }
    }
}

/// Every way [`McpPlugin::discover`] or `McpTool::invoke` can fail --
/// **fail-closed, uniformly**, mirroring `conway-plugin-subprocess::
/// SubprocessPluginError`'s own discipline. Never a silent fallback -- every
/// variant here is a hard error the caller must act on.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum McpPluginError {
    /// The configured command could not even be spawned: not found, not
    /// executable, or any other OS-level spawn failure. Also covers a
    /// malformed spec this host cannot even attempt (an empty `command`).
    #[error("MCP plugin '{config_id}' failed to spawn: {detail}")]
    Spawn { config_id: String, detail: String },
    /// The server did not answer a framed request within `timeout_ms` and was
    /// killed (process-group SIGTERM, then SIGKILL after a grace period).
    #[error("MCP plugin '{config_id}' timed out after {after_ms}ms")]
    TimedOut { config_id: String, after_ms: u64 },
    /// The server's child process died mid-call: it exited, or closed its
    /// stdout, before answering the outstanding request.
    ///
    /// **The SESSION never reconnects; the PLUGIN transparently respawns
    /// one, bounded; the CALL that hit the death is re-sent ONLY when its
    /// tool declared itself safe to retry.** Three different claims, and
    /// this variant used to conflate the first two (and state the third as
    /// an unconditional "never"): a died `McpSession` genuinely never comes
    /// back (whatever conversational state that child held is gone, and
    /// nothing resurrects a specific dead session -- see `session::
    /// McpSession`'s own module doc for that half, unchanged). `McpPlugin`
    /// does not stop there: the FIRST tool call that finds its session dead
    /// (or whose own round trip fails with THIS variant) transparently
    /// spawns a fresh child from the same [`McpPluginSpec`] the plugin was
    /// originally discovered with, re-runs the handshake, and checks the
    /// fresh server's `tools/list` against the tool set this plugin
    /// registered with the runtime at `discover` time (a mismatch fails
    /// closed with [`McpPluginError::ToolSetChanged`] instead of silently
    /// swapping in a different plugin) -- so the NEXT tool call already
    /// finds a fresh session in place, no operator action, no conway
    /// restart.
    ///
    /// **Whether the call that discovered the death is itself resent
    /// against that fresh session depends on one thing: did THIS tool
    /// declare `annotations.idempotentHint: true`?** Over stdio, a closed
    /// pipe carries no information about whether the killed process had
    /// already completed the work it was asked to do before it died -- this
    /// host cannot prove the request was never sent (the one condition
    /// gRPC's own transparent-retry rule, proposal A6, requires before it
    /// will resend). MCP itself carries the opt-in that situation needs:
    /// `ToolAnnotations.idempotentHint`, which DEFAULTS to `false` -- the
    /// protocol's own posture is that an unannotated tool is unsafe to
    /// retry, and this host follows that default exactly: absent, `null`,
    /// or an explicit `false` all fail closed, never retried, exactly as
    /// every tool behaved before this exception existed. Only a tool whose
    /// `tools/list` entry declared `idempotentHint: true` -- captured once,
    /// at this plugin's original `discover` time, never updated by a later
    /// respawn's own answer (`SessionSlot::idempotent_tool_names`) -- gets
    /// the one retry, through the SAME call site every other call goes
    /// through (`McpTool::invoke`'s own doc has the mechanics). **That
    /// declaration is the SERVER's, advisory, and unverifiable by this
    /// client** (see `session::extract_idempotent_tool_names`'s own doc
    /// for the full disclosure): a server that declares idempotency it does
    /// not actually have can cause conway to double-execute that tool's
    /// side effect. That is the declaring server's error, not this
    /// client's, but an operator relying on this exception should know
    /// conway is trusting the server's word for it, not checking it. A
    /// caller whose call is NOT eligible for the exception, or whose one
    /// retry attempt itself hits a dead session, fails closed with this
    /// variant exactly as before; a caller that wants the work done issues
    /// a NEW call, which lands on the fresh session the respawn already put
    /// in place. This respawn is bounded by [`MAX_AUTO_RESPAWNS`] (see that
    /// constant's own doc for the argued number, including for a retried
    /// call -- a retry's own respawn spends the SAME bound): once a plugin
    /// has respawned that many times across its whole lifetime, THIS
    /// variant surfaces again exactly as it always did, permanently, for
    /// every later call -- a server dying repeatedly in a short window is
    /// genuinely unhealthy, not transiently unlucky, and the original
    /// fail-closed posture wins once that is established. 2026-09-07's live
    /// incident (a `record_read` timing out once under CPU contention, then
    /// every later call failing identically for hours) is exactly the shape
    /// one bounded auto-respawn now closes.
    #[error("MCP plugin '{config_id}' session died: {detail}")]
    SessionDied { config_id: String, detail: String },
    /// A respawned server's `tools/list` answer no longer matches the tool
    /// set this plugin registered with the runtime at `discover` time:
    /// `added` names a tool the fresh server offers that the original one
    /// did not; `removed` names one the original offered that the fresh one
    /// no longer does. **Never applied silently.** The mismatched fresh
    /// child is killed immediately (never left running, never used for
    /// anything); the plugin's CURRENT session stays whatever it was before
    /// this respawn attempt (the old, dead one); and this typed error
    /// surfaces to the caller naming the exact difference, consuming one of
    /// [`MAX_AUTO_RESPAWNS`]'s bounded attempts. A server whose tool set
    /// changed under the operator mid-session is not the plugin they
    /// installed, even when its command line and config entry are
    /// unchanged -- silently adding or dropping a tool here would be
    /// exactly the "the operator's own review is the only control point"
    /// trust boundary (this crate's own module doc) failing quietly.
    #[error(
        "MCP plugin '{config_id}' respawned but its tool set changed: added {added:?}, removed {removed:?}"
    )]
    ToolSetChanged {
        config_id: String,
        added: Vec<String>,
        removed: Vec<String>,
    },
    /// The server sent an unterminated or malformed frame on stdout: a line
    /// that is not valid JSON, a partial line then EOF, or a response with no
    /// JSON-RPC `id`. A typed parse error, not a deadlock. The session is
    /// marked dead after this -- a server that garbles its framing cannot be
    /// trusted to recover, fail-closed.
    #[error("MCP plugin '{config_id}' sent a malformed frame: {detail}")]
    MalformedFrame { config_id: String, detail: String },
    /// The `initialize` handshake or `tools/list` call failed structurally: a
    /// missing `result`, an `id` mismatch, a server that does not offer the
    /// `tools` capability, a JSON-RPC `error` response, a `tools/list` answer
    /// with a missing/empty/duplicate tool name, or an `inputSchema` that is
    /// not an object. FAILS CLOSED at `discover` time, BEFORE any `tools/call`
    /// runs -- a server that cannot complete the handshake is refused here,
    /// not at first use.
    #[error("MCP plugin '{config_id}' handshake failed: {detail}")]
    HandshakeFailed { config_id: String, detail: String },
}

/// The one-line-per-variant mapping this crate's own error enum needs to
/// consume the shared process-lifecycle layer (board item
/// `01M0TV7ZDS8X4F4TEJPRZB9P6T`): `conway::plugin::ChildSession` constructs
/// its four shared failure causes generically, through this trait, rather
/// than each session type hand-rolling the identical `match`/`kill_all`
/// bookkeeping. `McpPluginError`'s own variants and `Display` text are
/// UNCHANGED by this -- this impl only tells `ChildSession` which variant of
/// THIS enum each cause becomes.
impl ChildSessionError for McpPluginError {
    fn spawn(config_id: &str, detail: String) -> Self {
        McpPluginError::Spawn {
            config_id: config_id.to_string(),
            detail,
        }
    }

    fn timed_out(config_id: &str, after_ms: u64) -> Self {
        McpPluginError::TimedOut {
            config_id: config_id.to_string(),
            after_ms,
        }
    }

    fn session_died(config_id: &str, detail: String) -> Self {
        McpPluginError::SessionDied {
            config_id: config_id.to_string(),
            detail,
        }
    }

    fn malformed_frame(config_id: &str, detail: String) -> Self {
        McpPluginError::MalformedFrame {
            config_id: config_id.to_string(),
            detail,
        }
    }
}

impl McpPluginError {
    /// Maps this host-level error onto the `ToolError` variant the runtime
    /// sees, mirroring `conway-plugin-subprocess::SubprocessPluginError::
    /// into_tool_error`'s own split: a parse/manifest/handshake failure
    /// (`MalformedFrame`/`HandshakeFailed`) is `ToolError::Internal` (an
    /// operator-readable "the server is broken"); every transport-level
    /// failure (`Spawn`/`TimedOut`/`SessionDied`) is `ToolError::Io`, each
    /// carrying this error's own `Display` so an operator can tell a broken
    /// server apart from a legitimately-declined call.
    pub(crate) fn into_tool_error(self) -> ToolError {
        match self {
            McpPluginError::HandshakeFailed { .. }
            | McpPluginError::MalformedFrame { .. }
            | McpPluginError::ToolSetChanged { .. } => ToolError::Internal {
                detail: self.to_string(),
            },
            McpPluginError::Spawn { .. }
            | McpPluginError::TimedOut { .. }
            | McpPluginError::SessionDied { .. } => ToolError::Io {
                detail: self.to_string(),
            },
        }
    }
}

// `crate::unix` (this crate's own hand-copied `kill_group`, the third
// standalone copy of the identical sequence `conway-plugin-subprocess` and
// `conway-tools` each carried) used to live here. Board item
// `01M0EKVR1BEXXS75NV2JC4HZZ9` replaced all three with a single
// implementation reached through `conway::plugin::kill_group` (see that
// re-export's own doc in `crates/conway/src/lib.rs`, and
// `conway_tools::process`'s own doc for the five-way diff this
// consolidation is built on). `session.rs` imports the same re-export.

/// A [`Plugin`] backed by an MCP-over-stdio server: the server is spawned at
/// [`McpPlugin::discover`], the `initialize`/`notifications/initialized`/
/// `tools/list` handshake runs there, and each declared MCP tool becomes an
/// `McpTool` whose `invoke` calls `tools/call` over the plugin's CURRENT
/// session. `manifest`/`tools` are answered from that first handshake, never
/// re-queried per call -- a respawn (see this crate's own `SessionSlot`, a
/// private type not part of this crate's public surface) replaces the
/// SESSION underneath a tool, never the tool's own name/schema/description;
/// a fresh server whose declared tools differ from `manifest.tools` fails the
/// respawn instead ([`McpPluginError::ToolSetChanged`]).
///
/// **Not "ONCE" anymore, deliberately.** Before this crate's auto-respawn
/// item, the server WAS spawned exactly once for the plugin's whole life and
/// a dead session was permanent. That is still true of any ONE `McpSession`
/// (see `session`'s own module doc); it is no longer true of the plugin as a
/// whole -- see this crate's own `SessionSlot` and
/// [`McpPluginError::SessionDied`]'s own doc for the bounded respawn this
/// type now performs transparently.
pub struct McpPlugin {
    manifest: PluginManifest,
    tools: Vec<Arc<dyn Tool>>,
    /// The swappable current session, shared with every `McpTool` this
    /// plugin built. Held here for the SAME "explicit owner, load-bearing
    /// lifetime" reason the old fixed `session: Arc<McpSession>` field
    /// carried -- the child (whichever one is current right now) must
    /// outlive every tool call -- plus the NEW reason a fixed field never
    /// needed: [`Plugin::status_contributions`] reads this to report a
    /// respawn, so it is a genuinely read field now, not merely held alive.
    slot: Arc<SessionSlot>,
}

impl std::fmt::Debug for McpPlugin {
    // Manual impl: `dyn Tool` carries no `Debug` bound, matching
    // `conway-plugin-subprocess::SubprocessPlugin`'s own manual impl.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpPlugin")
            .field("manifest", &self.manifest)
            .field("tool_count", &self.tools.len())
            .finish()
    }
}

impl McpPlugin {
    /// Spawns `spec.command` once (the external MCP server), runs the
    /// one-time `initialize`/`notifications/initialized`/`tools/list`
    /// handshake, and builds an `McpPlugin` from the answer. Each declared
    /// MCP tool becomes an `McpTool` sharing the SAME `Arc<McpSession>` (and
    /// thus the same child process).
    ///
    /// **Why this is an async associated function, not something
    /// `Plugin::manifest`/`Plugin::tools` do lazily.** Same reason
    /// `conway-plugin-subprocess::SubprocessPlugin::discover` is: those trait
    /// methods are synchronous, and a fallible, I/O-performing handshake needs
    /// a point where its failure means something. This associated function IS
    /// that constructor, and its `Result` is the failure surfacing. A caller
    /// (`conway-cli`'s own MCP-plugin loader) awaits this before ever handing
    /// the result to `ConwayBuilder::with_plugin`.
    ///
    /// **Manifest id derivation.** The MCP server's `serverInfo.name` is the
    /// natural plugin id -- it is what the server calls itself. To keep the
    /// plugin id in conway's namespace shape (`PluginManifest::id` is a
    /// `String`, conventionally dotted like `acme.greet`), the manifest id is
    /// `mcp.<server_info_name>` when the server name is non-empty, or
    /// `mcp.<config_id>` as a fallback (so a server with a missing/empty
    /// `serverInfo.name` still gets a stable, non-empty id). This keeps an MCP
    /// plugin's tools distinguishable from conway's own built-ins and from
    /// subprocess plugins in tool-name collision errors, WITHOUT trusting the
    /// server name as a security boundary (it is a display id, not a trust
    /// claim -- see this crate's module doc).
    pub async fn discover(spec: McpPluginSpec) -> Result<Self, McpPluginError> {
        // Retained past this call for `SessionSlot::respawn`, which needs to
        // spawn an IDENTICAL fresh child later -- same command, same env,
        // same timeouts -- without the operator re-authoring anything.
        // `McpPluginSpec` is cheaply `Clone` (a handful of `String`s and a
        // `Vec`).
        let respawn_spec = spec.clone();
        let session = McpSession::spawn(&spec).await?;
        let (init, tools, idempotent_tool_names) = session.handshake().await?;
        tracing::debug!(
            config_id = %spec.config_id,
            protocol_version = %init.protocol_version,
            server_name = %init.server_name,
            server_version = %init.server_version,
            tool_count = tools.len(),
            "MCP initialize handshake succeeded; proceeding to register tools"
        );

        let server_name = if init.server_name.is_empty() {
            spec.config_id.clone()
        } else {
            init.server_name.clone()
        };
        let plugin_id = format!("mcp.{server_name}");
        let plugin_version = init.server_version.clone();

        let mut tool_names = std::collections::HashSet::new();
        let mut specs = Vec::with_capacity(tools.len());
        for listed in &tools {
            if !tool_names.insert(listed.name.clone()) {
                // `parse_tools_list_response` already rejects duplicates, so
                // this is a defensive double-check, not a reachable path.
                let err = McpPluginError::HandshakeFailed {
                    config_id: spec.config_id.clone(),
                    detail: format!("declared tool name '{}' is duplicated", listed.name),
                };
                session.shared_kill_all(err.clone());
                return Err(err);
            }
            // Compile the MCP `inputSchema` (a JSON Schema) into a
            // `schemars::schema::RootSchema` the SAME way
            // `conway-plugin-subprocess` compiles a wire-declared schema, so
            // the runtime's schema validator works identically.
            let schema: schemars::schema::RootSchema =
                serde_json::from_value(listed.input_schema.clone()).map_err(|err| {
                    let e = McpPluginError::HandshakeFailed {
                        config_id: spec.config_id.clone(),
                        detail: format!("tool '{}' has an invalid JSON Schema: {err}", listed.name),
                    };
                    session.shared_kill_all(e.clone());
                    e
                })?;
            specs.push(ToolSpec {
                name: ToolName::new(listed.name.clone()),
                description: listed.description.clone(),
                schema,
                // An MCP tool is OPAQUE to conway -- MCP carries no
                // category/permission field, so the conservative default is
                // the MOST RESTRICTIVE pair (Execute / Dangerous), mirroring
                // `conway-plugin-subprocess`'s unknown-tag degradation. See
                // `wire::DEFAULT_CATEGORY`/`DEFAULT_PERMISSION`'s own doc.
                category: wire::DEFAULT_CATEGORY,
                permission: wire::DEFAULT_PERMISSION,
            });
        }

        let plugin_manifest = PluginManifest {
            id: plugin_id,
            version: plugin_version,
            tools: specs.iter().map(|s| s.name.clone()).collect(),
            // The MCP client needs NO conway host cap -- it has its own
            // transport (the spawned child). It does not require
            // `PersistentTransport` (that cap is for conway's own subprocess
            // wire plugin, offered iff a `[plugins].subprocess[]` entry is
            // configured persistent).
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        };

        // `tool_names` holds exactly the same set of names as `specs`/
        // `plugin_manifest.tools` (built in lockstep in the loop above) --
        // this is "the set registered with the runtime at build time" a
        // respawn's fresh `tools/list` is compared against, so it is
        // captured from THIS discovery, never re-derived from whatever a
        // later respawned server happens to report.
        let registered_tool_names: BTreeSet<String> = tool_names.into_iter().collect();
        // `idempotent_tool_names` came back from THIS SAME `handshake()` call
        // above, captured by `McpSession::handshake` from the identical
        // `tools/list` answer `tools` (and `registered_tool_names`, above)
        // were built from -- see `SessionSlot::idempotent_tool_names`'s own
        // doc for what this set is used for and its own declaration-honesty
        // disclosure.
        let slot = Arc::new(SessionSlot::new(
            respawn_spec,
            session,
            plugin_manifest.id.clone(),
            registered_tool_names,
            idempotent_tool_names,
        ));
        let tools: Vec<Arc<dyn Tool>> = specs
            .into_iter()
            .map(|tool_spec| {
                Arc::new(McpTool {
                    spec: tool_spec,
                    slot: slot.clone(),
                }) as Arc<dyn Tool>
            })
            .collect();

        Ok(Self {
            manifest: plugin_manifest,
            tools,
            slot,
        })
    }
}

impl Plugin for McpPlugin {
    fn manifest(&self) -> PluginManifest {
        self.manifest.clone()
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }

    /// Makes an auto-respawn visible on the status line / `/context`, not
    /// merely a `tracing::warn!` an operator has to go looking for --
    /// acceptance criterion 6. Empty for a plugin that never respawned
    /// (matching every other zero-cost-default `Plugin` method); once a
    /// respawn has been attempted, this reports the MOST RECENT attempt's
    /// outcome under this plugin's own manifest id -- `ResultStatus::
    /// Completed` for a successful respawn-and-retry, `ResultStatus::Failed`
    /// for a respawn that itself failed (spawn/handshake error, a tool-set
    /// mismatch, or the bound being exhausted) -- read by
    /// `Conway::poll_plugin_status_contributions` on the SAME polled-
    /// snapshot cadence every other plugin's contributions already use.
    fn status_contributions(&self) -> Vec<PluginStatusContribution> {
        self.slot.status_contributions()
    }
}

/// The swappable current session behind an [`McpPlugin`], shared by every
/// `McpTool` it built. Before this crate's auto-respawn item, each `McpTool`
/// held its own `Arc<McpSession>` clone captured once at construction --
/// permanently fixed, so a tool could never observe a replacement session
/// even if one existed. This type is what makes a respawn visible to every
/// tool's NEXT call: `McpTool::invoke` reads [`Self::current`] fresh at the
/// START of every call, never caching it past one call, so a respawn that
/// happens between two calls on the SAME tool -- or on a SIBLING tool
/// sharing this same plugin -- is picked up immediately, without
/// reconstructing the plugin or any tool on it.
///
/// **A `std::sync::Mutex<Arc<McpSession>>`, not `arc_swap`.** The bound
/// C-04 (dependency minimalism) sets: a call site here is one `tools/call`
/// invocation, never latency-sensitive at the microsecond scale `arc_swap`
/// exists for, and every lock hold here is a plain pointer clone/swap with
/// no `.await` inside the critical section (so it can never itself become a
/// contention point across an in-flight I/O wait). A `std::sync::Mutex`
/// says everything this needs to say with zero new dependency-graph surface.
struct SessionSlot {
    /// The CURRENT session every `McpTool::invoke` calls through. Swapped,
    /// never mutated -- a call already holding an `Arc` clone (mid-flight
    /// when a respawn elsewhere swaps this) keeps running against the
    /// session it started with to completion; only the NEXT `current()`
    /// read observes the new one.
    current: StdMutex<Arc<McpSession>>,
    /// The originating spec -- see [`McpPlugin::discover`]'s own doc for why
    /// this is captured there rather than derived from `current` (a dead
    /// session cannot answer "what command built you").
    spec: McpPluginSpec,
    /// This plugin's own manifest id (`McpPlugin::manifest().id`), copied
    /// here so a respawn's status contribution can be filed under it without
    /// `SessionSlot` reaching back into `McpPlugin`.
    plugin_id: String,
    /// The tool NAMES this plugin registered with the runtime at `discover`
    /// time (acceptance criterion 3: "the registered tool set is the
    /// contract"). Every respawn's fresh `tools/list` is compared against
    /// this SAME, never-updated set -- never re-derived from whatever the
    /// fresh server happens to report, or a server whose tool set drifted
    /// under the operator could ratchet the comparison to match itself.
    registered_tool_names: BTreeSet<String>,
    /// The NAMES of tools THIS discovery's `tools/list` answer declared
    /// `annotations.idempotentHint: true` for -- captured once, at the same
    /// `discover` time as [`Self::registered_tool_names`], and never updated
    /// by a later respawn's own `tools/list` answer (the identical
    /// never-re-derived treatment `registered_tool_names` already gets, for
    /// the identical reason: the contract this slot enforces is what THIS
    /// discovery registered, not whatever a later server generation happens
    /// to report).
    ///
    /// **What this set is for.** `McpTool::invoke`'s retry decision -- see
    /// that method's own doc -- reads [`Self::is_idempotent`] to decide
    /// whether a call that just found its session dead mid-flight may be
    /// resent against the fresh session a respawn puts in place. This is the
    /// ONE place that decision is made; nothing else in this crate checks
    /// `idempotentHint`.
    ///
    /// **Advisory and server-self-declared -- conway cannot verify this.**
    /// See `session::extract_idempotent_tool_names`'s own doc for the
    /// full disclosure: MCP's `ToolAnnotations` are a risk vocabulary the
    /// SERVER volunteers, not a property this client can check. Retrying a
    /// tool on the strength of a `false` claim of `true` here would
    /// double-execute whatever that tool's side effect was; that is the
    /// declaring server's error, but it is this host that would have acted
    /// on the lie, so the operator deserves to know conway trusts it.
    idempotent_tool_names: BTreeSet<String>,
    /// Respawn bookkeeping: how many respawns this plugin has performed
    /// (bounded by [`MAX_AUTO_RESPAWNS`]) and the status contribution the
    /// most recent respawn ATTEMPT produced (whether it succeeded or not) --
    /// read by [`McpPlugin::status_contributions`].
    respawn: StdMutex<RespawnBookkeeping>,
}

/// [`SessionSlot`]'s own respawn counter plus the latest status
/// contribution a respawn attempt produced. A plain struct behind one lock
/// rather than two: the count and the contribution describing its most
/// recent change are updated together, atomically, by every respawn
/// attempt -- two separate locks could observe a torn state (a count bumped
/// but the OLD contribution still visible, or vice versa) under concurrent
/// tool calls on sibling tools sharing this same plugin.
#[derive(Default)]
struct RespawnBookkeeping {
    count: u32,
    last_contribution: Option<PluginStatusContribution>,
}

impl SessionSlot {
    fn new(
        spec: McpPluginSpec,
        initial: McpSession,
        plugin_id: String,
        registered_tool_names: BTreeSet<String>,
        idempotent_tool_names: BTreeSet<String>,
    ) -> Self {
        Self {
            current: StdMutex::new(Arc::new(initial)),
            spec,
            plugin_id,
            registered_tool_names,
            idempotent_tool_names,
            respawn: StdMutex::new(RespawnBookkeeping::default()),
        }
    }

    /// Whether the tool named `name` declared `annotations.idempotentHint:
    /// true` in the `tools/list` answer THIS plugin was originally
    /// discovered with -- see [`Self::idempotent_tool_names`]'s own doc for
    /// what this governs and its declaration-honesty disclosure. The ONE
    /// place `McpTool::invoke`'s retry-after-respawn decision is made;
    /// nothing else in this crate consults `idempotentHint`.
    fn is_idempotent(&self, name: &str) -> bool {
        self.idempotent_tool_names.contains(name)
    }

    /// The plugin's session RIGHT NOW -- read fresh by every
    /// `McpTool::invoke`, never cached across calls (see this type's own
    /// doc for why that is what makes a respawn visible to every tool).
    fn current(&self) -> Arc<McpSession> {
        self.current.lock().expect("session slot poisoned").clone()
    }

    fn config_id(&self) -> &str {
        &self.spec.config_id
    }

    fn status_contributions(&self) -> Vec<PluginStatusContribution> {
        self.respawn
            .lock()
            .expect("session slot poisoned")
            .last_contribution
            .clone()
            .into_iter()
            .collect()
    }

    fn record_contribution(&self, status: ResultStatus, value: String) {
        let mut state = self.respawn.lock().expect("session slot poisoned");
        state.last_contribution = Some(PluginStatusContribution {
            key: self.plugin_id.clone(),
            status,
            value,
        });
    }

    /// Reserves one respawn attempt against [`MAX_AUTO_RESPAWNS`], returning
    /// the 1-based attempt number on success or `None` once the bound is
    /// already exhausted. A `Mutex<u32>` increment-under-lock, not an
    /// `AtomicU32::fetch_add` + separate compare -- the reservation and the
    /// bound check must be one atomic step, or two tool calls racing past
    /// the bound simultaneously could both reserve the SAME last slot.
    fn reserve_respawn(&self) -> Option<u32> {
        let mut state = self.respawn.lock().expect("session slot poisoned");
        if state.count >= MAX_AUTO_RESPAWNS {
            return None;
        }
        state.count += 1;
        Some(state.count)
    }

    /// Attempts exactly ONE respawn for a session a tool call found dead:
    /// spawns a fresh child from `self.spec`, runs the handshake, checks
    /// the fresh server's tool set against [`Self::registered_tool_names`],
    /// and swaps the fresh session in as [`Self::current`] on a match --
    /// so the NEXT call through this slot finds a healthy session waiting.
    ///
    /// **Respawn-only, deliberately NOT respawn-and-retry -- even now that
    /// a caller CAN choose to retry.** This method itself never resends the
    /// caller's `tools/call`, for any caller, unconditionally: it spawns the
    /// fresh child, runs the handshake, checks the tool-set guard, and
    /// swaps [`Self::current`] -- full stop. `McpTool::invoke` (`lib.rs`,
    /// its own doc has the mechanics) is what decides, AFTER this method
    /// returns `Ok`, whether to resend -- and only for a tool whose
    /// `tools/list` entry declared `annotations.idempotentHint: true` at
    /// this plugin's original `discover` time
    /// (`Self::idempotent_tool_names`). Keeping that decision entirely out
    /// of this method is deliberate: respawning and retrying are separate
    /// questions (this is why this method is named `respawn`, not
    /// `respawn_and_retry`), and folding the retry in here would make the
    /// safety-critical "may I resend this specific request" check one of
    /// two places instead of one. For the DEFAULT case -- no annotation, or
    /// an explicit `false` -- the reasoning is unchanged: over stdio, a
    /// closed pipe after a kill carries no information about whether the
    /// killed process had already completed the work it was asked to do --
    /// this host can never prove the request was never sent (the one
    /// condition gRPC's own transparent-retry rule, proposal A6, requires
    /// before it will resend), and MCP's own `ToolAnnotations.idempotentHint`
    /// defaults to `false` for exactly this reason. A doubled
    /// `work_complete`/`record_append` over this transport would be durable
    /// and wrong, so the call that found the session dead is the caller's
    /// problem to resend (or not) -- never this method's. This is why the
    /// return type is `Result<(), ToolError>` ("the slot now holds a fresh
    /// session" / "respawn failed or was refused"), not a `CallOutcome` --
    /// there is no retried call HERE to report the outcome of, regardless
    /// of what the caller goes on to do with the fresh session.
    ///
    /// **Never loops.** A failure at ANY step -- the bound already
    /// exhausted, the fresh spawn failing, the fresh handshake failing, or
    /// a tool-set mismatch -- returns that failure directly to the caller;
    /// none of those triggers a second attempt within this same call. See
    /// [`McpPluginError::SessionDied`]'s own doc for the policy this method
    /// enforces and [`McpPluginError::ToolSetChanged`]'s for the mismatch
    /// case.
    async fn respawn(&self) -> Result<(), ToolError> {
        let config_id = self.config_id().to_string();

        let Some(attempt) = self.reserve_respawn() else {
            // Bound exhausted: revert to the pre-this-item posture exactly
            // -- surface the CURRENT session's own typed death reason (or a
            // generic SessionDied if it somehow recorded none) and attempt
            // no further spawn, permanently, for every later call.
            let err = self
                .current()
                .death_error()
                .unwrap_or_else(|| McpPluginError::SessionDied {
                    config_id: config_id.clone(),
                    detail: format!(
                        "session died and the auto-respawn bound \
                         ({MAX_AUTO_RESPAWNS}) is already exhausted"
                    ),
                });
            tracing::warn!(
                config_id = %config_id,
                max = MAX_AUTO_RESPAWNS,
                "MCP plugin session died but the auto-respawn bound is exhausted; \
                 failing closed"
            );
            self.record_contribution(
                ResultStatus::Failed {
                    error: err.to_string(),
                },
                format!("gave up auto-respawning after {MAX_AUTO_RESPAWNS} attempts"),
            );
            return Err(err.into_tool_error());
        };

        tracing::warn!(
            config_id = %config_id,
            attempt,
            max = MAX_AUTO_RESPAWNS,
            "MCP plugin session died; respawning a fresh child"
        );

        let fresh = match McpSession::spawn(&self.spec).await {
            Ok(session) => session,
            Err(err) => {
                self.record_contribution(
                    ResultStatus::Failed {
                        error: err.to_string(),
                    },
                    format!("respawn {attempt}/{MAX_AUTO_RESPAWNS} failed to spawn: {err}"),
                );
                return Err(err.into_tool_error());
            }
        };
        // The fresh handshake's own `idempotent_tool_names` is deliberately
        // discarded here (`_`): `Self::idempotent_tool_names`, like
        // `Self::registered_tool_names`, is captured ONCE at this plugin's
        // ORIGINAL `discover` time and never updated by a later respawn's
        // own answer -- see that field's own doc for why.
        let (_init, tools, _idempotent_tool_names) = match fresh.handshake().await {
            Ok(handshake) => handshake,
            Err(err) => {
                self.record_contribution(
                    ResultStatus::Failed {
                        error: err.to_string(),
                    },
                    format!("respawn {attempt}/{MAX_AUTO_RESPAWNS} failed its handshake: {err}"),
                );
                return Err(err.into_tool_error());
            }
        };

        // Acceptance criterion 5: the registered tool set is the contract.
        // Compare NAMES only (what "the set" names) against the set THIS
        // discovery originally registered -- never against whatever a
        // PREVIOUS respawn reported, which `self.registered_tool_names`
        // never updates to.
        let fresh_names: BTreeSet<String> = tools.iter().map(|t| t.name.clone()).collect();
        if fresh_names != self.registered_tool_names {
            let added: Vec<String> = fresh_names
                .difference(&self.registered_tool_names)
                .cloned()
                .collect();
            let removed: Vec<String> = self
                .registered_tool_names
                .difference(&fresh_names)
                .cloned()
                .collect();
            let err = McpPluginError::ToolSetChanged {
                config_id: config_id.clone(),
                added,
                removed,
            };
            // The mismatched child is killed immediately -- never left
            // running, never used for anything -- and `self.current` is
            // left untouched (still whatever it was before this attempt).
            fresh.shared_kill_all(err.clone());
            self.record_contribution(
                ResultStatus::Failed {
                    error: err.to_string(),
                },
                format!("respawn {attempt}/{MAX_AUTO_RESPAWNS}: {err}"),
            );
            return Err(err.into_tool_error());
        }

        let fresh = Arc::new(fresh);
        {
            let mut current = self.current.lock().expect("session slot poisoned");
            *current = fresh;
        }

        // No retry: the fresh session is now in place for the NEXT call.
        // The call that discovered the old session dead is the caller's to
        // fail closed with -- see this method's own doc.
        self.record_contribution(
            ResultStatus::Completed,
            format!(
                "respawned a fresh session (attempt {attempt}/{MAX_AUTO_RESPAWNS}); the call \
                 that found the old session dead is not retried"
            ),
        );
        Ok(())
    }
}

/// One tool an MCP server declared in its `tools/list` answer. `tools/call`
/// calls dispatch over `slot`'s CURRENT session's long-lived NDJSON channel
/// -- read fresh via [`SessionSlot::current`] at the start of every
/// `invoke`, never a fixed `Arc<McpSession>` captured once at construction
/// (see [`SessionSlot`]'s own doc for why that distinction is load-bearing).
struct McpTool {
    spec: ToolSpec,
    slot: Arc<SessionSlot>,
}

#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    /// PRE (from the trait): `call.arguments` is already schema-validated
    /// against `self.spec().schema` (the MCP server's `inputSchema`). Checks
    /// `ctx.cancel` before sending at all -- a call already cancelled by the
    /// time it reaches this tool never sends a request it would only have to
    /// abandon moments later.
    ///
    /// **Cancellation in flight.** A `tools/call` in flight when cancelled
    /// returns `Err(ToolError::Cancelled)`: the cancel token is handed to
    /// `McpSession::tools_call`, which writes the framed request
    /// UNCANCELLABLY (bounded by its own `timeout_ms` write deadline -- a
    /// cancel during the write would drop the future mid-`write_all` and leave
    /// a partial newline-less request line in the pipe, corrupting the NDJSON
    /// framing for every tool sharing the session) and then races ONLY the read
    /// against a watcher that polls the token on a short interval. The
    /// per-call `timeout_ms` read deadline is the ultimate fail-closed bound
    /// (mirroring `PersistentSession`); the cancel watcher is the polite
    /// early-out. On cancellation the `PendingGuard`'s `Drop` removes the
    /// pending entry (so a late server response finds no entry and is dropped
    /// harmlessly) and the SESSION STAYS ALIVE -- cancellation is a caller
    /// preference, not a session failure.
    ///
    /// **Every distinct transport failure mode maps to a typed `ToolError`,
    /// never a hang and never a panic** -- the same guarantee
    /// `conway-plugin-subprocess::SubprocessTool::invoke` makes. A
    /// dead/timeout/session-died transport failure becomes `ToolError::Io`; a
    /// malformed frame or JSON-RPC `error` response becomes `ToolError::
    /// Internal`, naming the underlying `McpPluginError`/JSON-RPC error so an
    /// operator can tell a broken server apart from a legitimately-declined
    /// call. An MCP `isError: true` RESULT is NOT an error -- it is a
    /// `ToolOutput` with `is_error: true` (the distinction is load-bearing in
    /// MCP: `isError` is a tool-level failure the caller reads, a JSON-RPC
    /// `error` is a protocol-level failure the transport fails closed on).
    ///
    /// **Transparent respawn on a dead session -- the call itself is
    /// retried ONLY for a tool that declared itself safe to.** Two
    /// triggers, deliberately different in scope (see
    /// [`McpPluginError::SessionDied`]'s own doc for the full policy): (1)
    /// the session `self.slot` currently holds is ALREADY dead when this
    /// call starts, for ANY recorded reason -- go straight to
    /// [`SessionSlot::respawn`] rather than pay a doomed round trip first,
    /// and fail this call closed (nothing was ever sent for it, so there is
    /// nothing to retry -- `idempotentHint` answers "may a request that
    /// might already have run be sent again," a question this trigger never
    /// raises); (2) a call that was genuinely live fails mid-flight and its
    /// session's own recorded death reason is SPECIFICALLY
    /// [`McpPluginError::SessionDied`] -- respawn on the strength of THAT
    /// death, THEN check `self.slot.is_idempotent(&name)` (read once, before
    /// either trigger runs, from [`SessionSlot::idempotent_tool_names`] --
    /// the `tools/list` answer this plugin was originally discovered with,
    /// never a later respawn's own answer). Trigger (2) is where the two
    /// outcomes diverge: for a tool that did NOT declare `idempotentHint:
    /// true` (absent, `null`, or explicit `false` -- MCP's own default),
    /// this call still fails closed with the death it hit, exactly as
    /// trigger (1) does; for a tool that DID, and only on the FIRST such
    /// death this call has hit, the IDENTICAL `name`/`arguments` is resent
    /// against the fresh session the respawn just put in place, through
    /// the SAME `tools_call` call site (never a second one -- see this
    /// method's own body for why that is load-bearing). If the respawn
    /// itself failed instead -- the bound spent, a spawn/handshake failure,
    /// a tool-set mismatch -- THAT failure surfaces instead of the
    /// original death, since it is the more specific thing this caller
    /// needs to know, and no retry is attempted either way. **The retry
    /// exception trusts the SERVER's own declaration; conway cannot verify
    /// it** -- see `session::extract_idempotent_tool_names`'s own doc
    /// for the full disclosure. A call that fails with `TimedOut` does NOT
    /// respawn (or retry) here even though the timeout kill also leaves the
    /// session dead afterward -- a slow-but-still-running server is a
    /// different, more ambiguous failure than one that is confirmed gone,
    /// and surfaces `TimedOut` directly to THIS caller; the NEXT call on
    /// this tool (or a sibling tool sharing this plugin) still finds the
    /// session dead via trigger (1) and gets its own chance to recover.
    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }

        let name = self.spec.name.to_string();
        let arguments = call.arguments;
        // Read ONCE, before any attempt: whether `name` declared
        // `annotations.idempotentHint: true` in the `tools/list` answer this
        // plugin was originally discovered with (`SessionSlot::
        // idempotent_tool_names`'s own doc has the full disclosure -- this
        // is a SERVER's advisory, unverifiable claim, not a property conway
        // checks). Fixed for the whole call: the captured set never changes
        // mid-call, so there is nothing to re-read on a retry.
        let retryable = self.slot.is_idempotent(&name);
        // Sticky once this call has spent its one permitted retry, so a
        // session that dies AGAIN on the retried attempt fails closed
        // immediately rather than looping a second time.
        let mut retried = false;

        // The cancel race lives in `tools_call` now, racing ONLY the read
        // (the write completes uncancellably first, so a cancel can never
        // leave a partial request line in the pipe). Hand the token through;
        // a cancel during the read returns `ToolError::Cancelled`, drops the
        // `PendingGuard`, and leaves the session alive.
        //
        // **Looped so the one retry this method ever permits runs through
        // the SAME `tools_call` call site as the original attempt, never a
        // second one.** A second call site was the exact compositional
        // defect an earlier version of this crate carried -- a timed-out
        // request re-sent through a SECOND `tools_call` call. `retried`
        // bounds this to at most two iterations, ever: the original
        // attempt, and (only for a tool that declared `idempotentHint:
        // true`, and only when THAT attempt's own session died mid-flight)
        // one resend against the fresh session the resulting respawn put in
        // place.
        let outcome = loop {
            let session = self.slot.current();
            if session.is_dead() {
                // Trigger (1): already dead. Nothing was sent for THIS
                // attempt yet -- fail it closed with the death already on
                // record, after triggering a respawn so the NEXT call finds
                // a fresh session. Unchanged by this item: nothing was ever
                // "in flight" here for `idempotentHint` to make safe to
                // resend -- this attempt simply has not started yet.
                let death_err =
                    session
                        .death_error()
                        .unwrap_or_else(|| McpPluginError::SessionDied {
                            config_id: session.config_id().to_string(),
                            detail: "session is no longer alive (re-discover to spawn a fresh one)"
                                .into(),
                        });
                self.slot.respawn().await?;
                return Err(death_err.into_tool_error());
            }

            match session
                .tools_call(name.clone(), arguments.clone(), ctx.cancel.clone())
                .await
            {
                Ok(outcome) => break outcome,
                Err(err) => {
                    let is_session_died = matches!(
                        session.death_error(),
                        Some(McpPluginError::SessionDied { .. })
                    );
                    if !is_session_died {
                        return Err(err);
                    }
                    // Trigger (2): this attempt itself just found the
                    // session dead mid-flight. `err` is already the
                    // correctly-mapped failure for THIS attempt -- respawn
                    // so the NEXT call (or this one's own retry, below)
                    // finds a fresh session.
                    self.slot.respawn().await?;
                    if retryable && !retried {
                        // `name` declared itself safe to retry, and this is
                        // the FIRST time this call has hit a mid-flight
                        // death -- resend the IDENTICAL `name`/`arguments`
                        // against the fresh session the respawn just put in
                        // place, through the SAME call site above. Every
                        // other call keeps the ruled default: fail closed
                        // with `err`, never resend.
                        retried = true;
                        continue;
                    }
                    return Err(err);
                }
            }
        };

        let result = match outcome {
            CallOutcome::Ok(result) => result,
            CallOutcome::JsonRpcError { code, message } => {
                return Err(wire::jsonrpc_error_to_tool_error(code, message));
            }
            CallOutcome::Malformed(detail) => {
                // A malformed frame kills the session, fail-closed (mirrors
                // `PersistentSession::tool_round_trip`'s malformed-frame
                // path). Re-reads `self.slot.current()` rather than reusing
                // the `session` captured above: if a respawn ran, the
                // malformed frame came from the FRESH session, not the old
                // one -- killing the stale handle would leave the actual
                // offending session marked alive.
                let current = self.slot.current();
                let err = McpPluginError::MalformedFrame {
                    config_id: current.config_id().to_string(),
                    detail,
                };
                current.shared_kill_all(err.clone());
                return Err(err.into_tool_error());
            }
        };

        Ok(ToolOutput {
            blocks: result.blocks,
            is_error: result.is_error,
            truncation: TruncationPolicy::None,
            artifacts: result.artifacts,
        })
    }

    /// Every declared field name in `call.arguments` is opaque to this host
    /// (an MCP tool's `inputSchema` is arbitrary JSON Schema this host never
    /// introspects beyond compiling it) -- conservative default applies
    /// unchanged, matching `conway-plugin-subprocess::SubprocessTool`'s own
    /// `path_args` and the trait-level default.
    fn path_args(&self) -> PathArgs {
        PathArgs::default()
    }

    /// Conservative default -- this host has no basis to know whether an MCP
    /// tool's `render` output (the trait default: a `name(args)` debug dump)
    /// is shell-interpretable, so it stays gated exactly as every tool is
    /// before overriding this method.
    fn render_kind(&self) -> RenderKind {
        RenderKind::default()
    }
}
