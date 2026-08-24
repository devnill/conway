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

use std::sync::Arc;

use conway::plugin::{
    async_trait, ChildSessionError, PathArgs, Plugin, PluginManifest, RenderKind, Tool, ToolCall,
    ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec, TruncationPolicy,
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
    /// stdout, before answering the outstanding request. **Fail-closed, no
    /// automatic reconnect**: a server that died has lost whatever session
    /// state it had; the death is surfaced and the caller must re-`discover`
    /// to spawn a fresh child. A subsequent call on the same dead session
    /// fails fast with this variant.
    #[error("MCP plugin '{config_id}' session died: {detail}")]
    SessionDied { config_id: String, detail: String },
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
            McpPluginError::HandshakeFailed { .. } | McpPluginError::MalformedFrame { .. } => {
                ToolError::Internal {
                    detail: self.to_string(),
                }
            }
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

/// A [`Plugin`] backed by an MCP-over-stdio server: the server is spawned
/// ONCE at [`McpPlugin::discover`], the `initialize`/`notifications/
/// initialized`/`tools/list` handshake runs ONCE there, and each declared MCP
/// tool becomes an `McpTool` whose `invoke` calls `tools/call` over the SAME
/// persistent stdio. `manifest`/`tools` are answered from the handshake, never
/// re-queried per call.
pub struct McpPlugin {
    manifest: PluginManifest,
    tools: Vec<Arc<dyn Tool>>,
    /// The persistent session -- held here so the child process lives as long
    /// as the plugin itself, even though every `McpTool` ALSO holds an
    /// `Arc<McpSession>` clone. A defensive explicit owner: the lifetime is
    /// load-bearing (the child must outlive every tool call), and relying
    /// solely on the tools' clones would make that lifetime an emergent
    /// property of `tools`'s contents rather than a stated field. NOT read
    /// after `discover` -- the lint is silenced because keeping the child
    /// alive IS the read.
    #[allow(dead_code)]
    session: Arc<McpSession>,
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
        let session = McpSession::spawn(&spec).await?;
        let (init, tools) = session.handshake().await?;
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
        };

        let session = Arc::new(session);
        let tools: Vec<Arc<dyn Tool>> = specs
            .into_iter()
            .map(|tool_spec| {
                Arc::new(McpTool {
                    spec: tool_spec,
                    session: session.clone(),
                }) as Arc<dyn Tool>
            })
            .collect();

        Ok(Self {
            manifest: plugin_manifest,
            tools,
            session,
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
}

/// One tool an MCP server declared in its `tools/list` answer. `tools/call`
/// calls dispatch over the shared `session`'s long-lived NDJSON channel -- the
/// SAME `Arc<McpSession>` every tool on this plugin shares (and thus the same
/// child process).
struct McpTool {
    spec: ToolSpec,
    session: Arc<McpSession>,
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
    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }

        let name = self.spec.name.to_string();
        let arguments = call.arguments;

        // The cancel race lives in `tools_call` now, racing ONLY the read
        // (the write completes uncancellably first, so a cancel can never
        // leave a partial request line in the pipe). Hand the token through;
        // a cancel during the read returns `ToolError::Cancelled`, drops the
        // `PendingGuard`, and leaves the session alive.
        let outcome = self
            .session
            .tools_call(name, arguments, ctx.cancel.clone())
            .await?;

        let result = match outcome {
            CallOutcome::Ok(result) => result,
            CallOutcome::JsonRpcError { code, message } => {
                return Err(wire::jsonrpc_error_to_tool_error(code, message));
            }
            CallOutcome::Malformed(detail) => {
                // A malformed frame kills the session, fail-closed (mirrors
                // `PersistentSession::tool_round_trip`'s malformed-frame path).
                let err = McpPluginError::MalformedFrame {
                    config_id: self.session.config_id().to_string(),
                    detail,
                };
                self.session.shared_kill_all(err.clone());
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
