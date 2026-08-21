//! `conway-plugin-subprocess`: the subprocess plugin host (board item
//! `01KZY8PATND84AKY0J376E3DWV`) -- the mechanism that lets a shipped
//! `conway` binary gain a tool it was never compiled with, by naming an
//! external command in `settings.json`.
//!
//! **What this crate proves, and no more.** Board item `01KZYBK2EYPH0MAV8F5251TE6Z`
//! (the decision record settling the shape) suggested decomposing this item
//! and named the de-risking first slice explicitly: *"`tool/1` only -- a
//! process plugin that declares one tool, is spawned by a config entry,
//! answers one call, and is torn down. No backend, no router, no capability
//! negotiation."* This crate is exactly that slice, generalized to any
//! number of declared tools (nothing here assumes exactly one), now with a
//! SECOND lifecycle for `tool/1` alongside the original one-shot exec: a
//! persistent NDJSON JSON-RPC channel (board item
//! `01M03VJHG1WFECFJB4ZH3CKWDX`, see `session`'s own module doc). Still
//! nothing beyond `tool/1` itself, the session-scoped
//! `permission.policy/1` declaration, the one-way `observe/1` observer sink
//! (board item `01M03VKQ738DTGHHK2C4RWXC0E`), and the `status.declare/1` /
//! `status/1` status push (same item): no `context.hook/1`,
//! no capability handshake beyond the `PluginManifest::required_host_caps`
//! the wire now CARRIES (board item `01M03VJXARFHSDAGHFXGCWKJTY`: a
//! subprocess plugin declares its required host caps in
//! `WireManifest::required_host_caps`, mapped into
//! `PluginManifest::required_host_caps` here and consulted by the `conway`
//! builder at registration -- a cap the host lacks refuses the plugin; an
//! unknown cap tag fails closed at parse). The persistent transport DOES
//! now run an `initialize/1` version-negotiation handshake at session open
//! (board item `01M03VK7MRPSAVWMW7YNYPRPGT`): `PersistentSession::
//! initialize` exchanges one `initialize/1` request/response with the
//! plugin BEFORE any `tool/1` call, applies `docs/plugins/compatibility.md`'s
//! version-negotiation table (refuse on major mismatch or unsatisfied
//! `minor_min`; accept otherwise; unknown fields in the plugin's answer
//! ignored-and-counted), and records the plugin's declared per-point
//! versions for the later wire-point items to consult. Immediately AFTER
//! the handshake, `PersistentSession::request_permission_policy` (board
//! item `01M03VKJG7JJ0JEKY265WA7MJ7`) exchanges ONE `permission.policy/1`
//! request/response -- the plugin declares per-tool NARROWING verdicts
//! (`deny`/`prompt`/`abstain`, NO `allow` by type construction: a plugin
//! may only narrow, never widen), which the `conway` facade installs as
//! `PatternOrigin::Plugin` deny/prompt rules in the `PermissionBroker`,
//! advisory-under-enforcement and subordinate to the operator's own
//! `permissions.json`/`PermissionMode`. A plugin declaring the point at an
//! unsupported version is REFUSED at discover (participant rule); a plugin
//! that does not declare it loads normally and contributes no wire policy.
//! One-shot discovery (`tool.spec/1`) stays handshake-free -- the handshake
//! and policy exchange are persistent-transport concerns.
//!
//! **Transport: one-shot exec (default) AND a persistent NDJSON channel.**
//! `docs/plugins/hooks.md`'s own point 9 doc and the decision record both
//! describe the eventual remote transport as a persistent connection. The
//! original slice (board item `01KZY8PATND84AKY0J376E3DWV`) deliberately did
//! NOT build that -- every RPC spawned the command fresh, the identical
//! shape `conway-tools`'s `ProcessHookRunner` already uses, so no process
//! outlives a single request. Board item `01M03VJHG1WFECFJB4ZH3CKWDX` adds
//! the persistent transport as a SECOND lifecycle, opt-in per
//! [`SubprocessPluginSpec::transport`]; the one-shot path is NOT deleted
//! (discovery `tool.spec/1` stays one-shot under both, and one-shot remains
//! the default so existing behavior is unchanged). The persistent path --
//! [`PersistentSession`] -- spawns the configured command ONCE, keeps it
//! alive across many `tool/1` calls, frames requests/responses as
//! newline-delimited JSON objects over the child's stdin/stdout, and tears
//! it down only on drop or fatal error. See `session`'s own module doc for
//! the NDJSON framing decision, the JSON-RPC `id` correlation discipline,
//! and the fail-closed failure handling (dead session, per-call timeout,
//! malformed frame).
//!
//! **The wire vocabulary is real, not invented for this crate.** The two
//! request kinds are named `tool.spec/1` and `tool/1` -- the exact point
//! names `docs/plugins/hooks.md` points 1 and 2 already use for "declared
//! but design-only, wire form". This crate is that wire form's first
//! implementation, disclosed as narrower than the design's own persistent-
//! connection shape (previous paragraph). Tool-result content blocks reuse
//! `conway_core::content::ContentBlock`'s own external `{"type": ...}`
//! tagging verbatim -- the same shape a backend adapter already puts on the
//! wire for model output -- rather than inventing a second content-block
//! JSON vocabulary for this one transport.
//!
//! **What this crate is NOT: a trust mechanism.** A subprocess plugin's
//! `command` executes with the operator's own privileges, unsandboxed --
//! see [`SubprocessPluginSpec`]'s own doc for why this is deliberately on
//! the SAME footing as `[hooks].rules[].command`
//! (`conway_tools::hook_runner::ProcessHookRunner`'s own module doc: "no
//! sandboxing, no allow/deny list, no argument sanitization here...an
//! operator's review of the command...is the control point, not this
//! type"), never a new, wider one. Board item `01KZHVFCN6ZEAXV7K5JHRQN1YB`
//! (a `plugin` trust subject kind keyed on a content digest) is under a
//! STANDING OPERATOR DEFERRAL and is explicitly out of scope here -- this
//! crate does not build it, and does not work around its absence by
//! inventing a parallel trust mechanism of its own. The honest state this
//! leaves: naming a subprocess plugin in `settings.json` is exactly as
//! trusted, and exactly as unaudited, as naming a `[hooks].rules[].command`
//! already is today -- an operator who would not paste an unknown shell
//! command into a hook rule should not paste an unknown command into a
//! subprocess plugin entry either. See `docs/plugins/trust-and-security.md`
//! for where this crate's entry in that page's inventory lives.

use std::process::Stdio;
use std::sync::Arc;

use conway::plugin::{
    async_trait, kill_group, EventSinkHandle, PathArgs, Plugin, PluginManifest,
    PluginPermissionRule, PluginPermissionVerdict, PluginStatusContribution, RenderKind, Tool,
    ToolCall, ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec, TruncationPolicy,
};

mod session;
mod wire;

pub use session::PersistentSession;
pub use wire::{WireManifest, WireTool, WireToolError, WireToolErrorKind, WireToolResult};

/// Applied when a [`SubprocessPluginSpec`] does not name its own
/// `timeout_ms` -- the SAME 5000ms default `crates/conway/src/config/
/// schema.rs`'s `HookEntry::timeout_ms` uses, for the identical reason
/// (`docs/plugins/hooks.md`'s own note on that field): long enough for a
/// typical local script to finish, short enough that a hung plugin process
/// cannot silently stall an agent turn indefinitely.
pub const DEFAULT_TIMEOUT_MS: u64 = 5000;

/// The transport a [`SubprocessPluginSpec`] uses for `tool/1` calls --
/// one-shot exec (the original slice, every call spawns fresh) or a
/// persistent NDJSON JSON-RPC channel (one long-lived child, board item
/// `01M03VJHG1WFECFJB4ZH3CKWDX`). **Default stays one-shot** so existing
/// behavior is unchanged; a plugin opts IN to persistent by setting
/// [`SubprocessPluginSpec::transport`] to [`Self::Persistent`].
///
/// `tool.spec/1` discovery stays one-shot under BOTH variants -- the
/// persistent channel carries only `tool/1` (see `wire`'s own module doc
/// for why that sidesteps the manifest-`id` / JSON-RPC-`id` collision).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SubprocessTransport {
    /// One-shot exec: every `tool/1` call spawns the command fresh, writes
    /// one JSON request, reads one JSON response, tears the process down.
    /// The original slice (board item `01KZY8PATND84AKY0J376E3DWV`); the
    /// default, so existing behavior is unchanged.
    #[default]
    OneShot,
    /// Persistent NDJSON JSON-RPC: spawn the command ONCE, keep it alive
    /// across many `tool/1` calls, frame requests/responses as one JSON
    /// object per line. See `session`'s own module doc for the framing
    /// decision, the correlation discipline, and the fail-closed failure
    /// handling.
    Persistent,
}

/// One operator-configured subprocess plugin entry: the command to spawn,
/// how long any single spawn (discovery, or one `tool/1` call) is
/// allowed to run before this host kills it, and which transport the
/// `tool/1` calls use.
///
/// **Trust, stated where the capability is defined, not only in the module
/// doc.** `command` is an argv vector (program, then its arguments) --
/// never a single shell string, the identical shape `HookEntry::command`
/// already uses and for the identical reason (no shell-quoting ambiguity
/// between what an operator wrote and what actually gets spawned). Naming a
/// command here is naming code the operator's own machine executes, with
/// the operator's own privileges, unsandboxed -- this type performs no
/// validation of `command` beyond "spawnable", the same posture
/// `ProcessHookRunner` already takes toward `HookInvocation::command`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubprocessPluginSpec {
    /// This plugin's operator-chosen id, used only in error messages this
    /// crate produces (a spawn/timeout/garbage failure names which
    /// configured entry misbehaved) -- NOT trusted as the plugin's own
    /// [`PluginManifest::id`], which the subprocess itself declares via its
    /// `tool.spec/1` answer (see [`SubprocessPlugin::discover`]'s own doc
    /// for why those two ids are allowed to differ and what happens when
    /// they do).
    pub config_id: String,
    /// The command to spawn, argv-shaped (this type's own doc).
    pub command: Vec<String>,
    /// Milliseconds any single spawn -- the one discovery call, or one
    /// `tool/1` call -- is allowed to run before this host kills the
    /// process group and fails closed. Defaults to [`DEFAULT_TIMEOUT_MS`]
    /// when constructed via [`SubprocessPluginSpec::new`]. For the
    /// persistent transport this is a PER-CALL deadline on the framed
    /// read, NOT a session-wide idle kill (a session that sits idle
    /// between calls is left alone).
    pub timeout_ms: u64,
    /// Which transport `tool/1` calls use. Defaults to
    /// [`SubprocessTransport::OneShot`] (existing behavior unchanged); set
    /// to [`SubprocessTransport::Persistent`] for a long-lived NDJSON
    /// JSON-RPC channel.
    pub transport: SubprocessTransport,
}

impl SubprocessPluginSpec {
    /// A spec with [`DEFAULT_TIMEOUT_MS`] and the one-shot transport
    /// (existing behavior). Use the struct literal directly to override
    /// `timeout_ms` or `transport`.
    pub fn new(config_id: impl Into<String>, command: Vec<String>) -> Self {
        Self {
            config_id: config_id.into(),
            command,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            transport: SubprocessTransport::default(),
        }
    }
}

/// Every way [`SubprocessPlugin::discover`] or `SubprocessTool::invoke`
/// can fail -- **fail-closed, uniformly**, mirroring
/// `conway_core::error::HookFailure`'s own "every distinct cause lands
/// here, as the ONE way this port reports failure" discipline. Never a
/// silent fallback (an unreachable manifest is not treated as "zero tools",
/// a garbage tool result is not treated as an empty success) -- every
/// variant here is a hard error the caller must act on.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SubprocessPluginError {
    /// The configured command could not even be spawned: not found, not
    /// executable, or any other OS-level spawn failure. Also covers a
    /// malformed spec this host cannot even attempt (an empty `command`).
    #[error("plugin '{config_id}' failed to spawn: {detail}")]
    Spawn { config_id: String, detail: String },
    /// The spawned process did not finish within `timeout_ms` and was
    /// killed (process-group SIGTERM, then SIGKILL after a grace period --
    /// see `conway::plugin::kill_group`).
    #[error("plugin '{config_id}' timed out after {after_ms}ms")]
    TimedOut { config_id: String, after_ms: u64 },
    /// The process ran and exited, but not with status 0. `code` is `None`
    /// when it was killed by a signal rather than exiting normally.
    #[error("plugin '{config_id}' exited nonzero: {code:?}")]
    NonzeroExit {
        config_id: String,
        code: Option<i32>,
    },
    /// Exit 0, but stdout was not valid JSON, or did not match the shape
    /// this call expected (a `tool.spec/1` answer for discovery, a
    /// `tool/1` answer for invocation). Never treated as an empty/default
    /// answer -- unlike `ProcessHookRunner`'s own `HookAnswer`, which
    /// legitimately defaults empty stdout to "no opinion", a `tool.spec/1`
    /// or `tool/1` answer that cannot be parsed carries no default that
    /// is honest to assume.
    #[error("plugin '{config_id}' produced unparseable output: {detail}")]
    UnparseableAnswer { config_id: String, detail: String },
    /// The `tool.spec/1` answer parsed as JSON but failed this host's own
    /// structural checks: an empty `id`, zero declared tools, a duplicate
    /// tool name, or a per-tool `schema` that does not compile as a JSON
    /// Schema document -- mirrors `docs/plugins/hooks.md` point 1's own "a
    /// schema that fails to compile fails registry construction" rule for
    /// an in-process `Plugin`, applied here to a wire-declared one.
    #[error("plugin '{config_id}' declared an invalid manifest: {detail}")]
    InvalidManifest { config_id: String, detail: String },
    /// A persistent session's child process died mid-call: it exited
    /// (nonzero or otherwise), or closed its stdout, before answering the
    /// outstanding `tool/1` request. **Fail-closed, no automatic reconnect**
    /// (board item `01M03VJHG1WFECFJB4ZH3CKWDX`): a plugin that died has
    /// lost whatever session state it had; the death is surfaced and the
    /// caller must re-`discover` to spawn a fresh child. A subsequent call
    /// on the same dead session fails fast with this variant.
    #[error("plugin '{config_id}' session died: {detail}")]
    SessionDied { config_id: String, detail: String },
    /// A persistent session received an unterminated or malformed frame on
    /// stdout: a line that is not valid JSON, a partial line then EOF (no
    /// trailing newline), or a response with no JSON-RPC `id`. A typed
    /// parse error, not a deadlock (acceptance criterion 4). The session is
    /// marked dead after this -- a plugin that garbles its framing cannot be
    /// trusted to recover, fail-closed.
    #[error("plugin '{config_id}' sent a malformed frame: {detail}")]
    MalformedFrame { config_id: String, detail: String },
    /// The `initialize/1` handshake (board item `01M03VK7MRPSAVWMW7YNYPRPGT`)
    /// REFUSED the plugin at session open: the plugin's declared `major` did
    /// not match this host's `wire_major`, or its `minor_min` exceeded this
    /// host's `wire_minor`. `condition` names which row of the compatibility
    /// table failed (`"major mismatch"` or `"minor_min unsatisfied"`); `detail`
    /// names BOTH versions (the host's and the plugin's) so an operator can
    /// see the disagreement. The plugin is refused at `discover` time, BEFORE
    /// any `tool/1` call runs -- a policy that silently never runs is the
    /// worst outcome, so an incompatible plugin fails closed here, not at
    /// first use.
    #[error("plugin '{config_id}' refused by initialize handshake ({condition}): {detail}")]
    HandshakeRefused {
        config_id: String,
        condition: String,
        detail: String,
    },
    /// The `initialize/1` handshake answer was structurally invalid: missing
    /// `ok`, `ok:false` with no `error`, a non-number where a number was
    /// expected, or a `points` entry missing its `name`/`version`. FAILS
    /// CLOSED -- mirroring `MalformedFrame`/`UnparseableAnswer`'s own
    /// "structural malformation fails closed" discipline. Only a KNOWN-shape
    /// answer carrying unknown FIELDS is accepted (ignored-and-counted, see
    /// `wire::parse_persistent_initialize_response`); a structurally-invalid
    /// answer is this variant, not the accept branch.
    #[error("plugin '{config_id}' sent a malformed initialize answer: {detail}")]
    HandshakeMalformed { config_id: String, detail: String },
}

impl SubprocessPluginError {
    /// Maps this host-level error onto the `ToolError` variant the runtime
    /// sees, mirroring the one-shot path's own split: a parse/manifest
    /// failure (`UnparseableAnswer`/`InvalidManifest`/`MalformedFrame`) is
    /// `ToolError::Internal` (an operator-readable "the plugin is broken"),
    /// and every other failure (spawn, timeout, nonzero exit, session
    /// death) is `ToolError::Io` (a transport-level failure), each carrying
    /// this error's own `Display` so an operator can tell a broken plugin
    /// apart from a legitimately-declined call.
    pub(crate) fn into_tool_error(self) -> ToolError {
        match self {
            SubprocessPluginError::UnparseableAnswer { .. }
            | SubprocessPluginError::InvalidManifest { .. }
            | SubprocessPluginError::MalformedFrame { .. }
            | SubprocessPluginError::HandshakeRefused { .. }
            | SubprocessPluginError::HandshakeMalformed { .. } => ToolError::Internal {
                detail: self.to_string(),
            },
            SubprocessPluginError::Spawn { .. }
            | SubprocessPluginError::TimedOut { .. }
            | SubprocessPluginError::NonzeroExit { .. }
            | SubprocessPluginError::SessionDied { .. } => ToolError::Io {
                detail: self.to_string(),
            },
        }
    }
}

// `crate::unix` (this crate's own hand-copied `kill_group`) used to live
// here, `pub(crate)` so `session.rs` (the persistent transport) could reuse
// it. Board item `01M0EKVR1BEXXS75NV2JC4HZZ9` replaced it -- and the
// identical copy `conway-plugin-mcp` carried, and the one `conway-tools`
// kept private -- with a single implementation reached through
// `conway::plugin::kill_group` (see that re-export's own doc in
// `crates/conway/src/lib.rs` for the full argument, and
// `conway_tools::process`'s own doc for the five-way diff). `session.rs`
// imports the SAME re-export, not a second module here.

/// Spawns `spec.command` fresh, writes `payload` to its stdin (closing it
/// afterward so a well-behaved subprocess sees EOF), reads stdout/stderr to
/// completion concurrently with waiting for exit -- the identical shape
/// `conway_tools::hook_runner::ProcessHookRunner`'s own `unix::drive` uses,
/// so a subprocess that never reads stdin or fills an OS pipe buffer before
/// being read cannot deadlock against its own exit -- all bounded by
/// `spec.timeout_ms`. Stderr is drained but discarded, for the identical
/// reason `ProcessHookRunner` discards it: this item wires no log/event
/// sink for a subprocess plugin's own diagnostic output.
async fn spawn_one_shot(
    spec: &SubprocessPluginSpec,
    payload: &[u8],
) -> Result<Vec<u8>, SubprocessPluginError> {
    #[cfg(not(unix))]
    {
        let _ = payload;
        return Err(SubprocessPluginError::Spawn {
            config_id: spec.config_id.clone(),
            detail: "the subprocess plugin host requires a unix host".into(),
        });
    }

    #[cfg(unix)]
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::process::Command;
        use tokio::time::{Duration, Instant};

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

        let deadline = Instant::now() + Duration::from_millis(spec.timeout_ms);

        let drive = async {
            let mut stdin = child.stdin.take().expect("piped stdin");
            let mut stdout_pipe = child.stdout.take().expect("piped stdout");
            let mut stderr_pipe = child.stderr.take().expect("piped stderr");

            let write_fut = async {
                let _ = stdin.write_all(payload).await;
                let _ = stdin.shutdown().await;
                drop(stdin);
            };
            let mut stdout_buf = Vec::new();
            let mut stderr_buf = Vec::new();
            let stdout_fut = stdout_pipe.read_to_end(&mut stdout_buf);
            let stderr_fut = stderr_pipe.read_to_end(&mut stderr_buf);

            // Drains stdin/stdout/stderr CONCURRENTLY, THEN reaps the exit
            // status SEQUENTIALLY afterward -- deliberately NOT
            // `tokio::join!(write_fut, stdout_fut, stderr_fut,
            // child.wait())` in one call, which is the shape
            // `ProcessHookRunner`'s own `unix::drive` uses. That
            // four-way join reproduced a HANG, 100% of the time, under a
            // multi-thread Tokio runtime (`#[tokio::main]`'s default
            // flavor, which `conway-cli`'s own binary uses) -- confirmed
            // by bisection: each two/three-way sub-combination completes
            // in milliseconds, but joining `child.wait()` alongside all
            // three piped-stdio futures in the SAME `join!` starves under
            // multi-thread scheduling. Splitting the four-way join into a
            // three-way join (no `wait`) followed by a sequential
            // `child.wait().await` is safe (draining both pipes to EOF
            // already implies the child has finished writing, so `wait`
            // afterward only reaps a status that is already available or
            // imminent -- no deadlock risk this reordering could
            // introduce) and eliminates the hang; see this crate's own
            // completion report for the disclosure this leaves against
            // `ProcessHookRunner`, which was not touched (out of this
            // item's owned paths) and may carry the identical latent bug.
            let (_, stdout_result, stderr_result) = tokio::join!(write_fut, stdout_fut, stderr_fut);
            let _ = stdout_result;
            let _ = stderr_result;
            let status = child.wait().await;
            status.map(|status| (status, stdout_buf))
        };

        match tokio::time::timeout_at(deadline, drive).await {
            Ok(Ok((status, stdout))) => {
                if !status.success() {
                    return Err(SubprocessPluginError::NonzeroExit {
                        config_id: spec.config_id.clone(),
                        code: status.code(),
                    });
                }
                Ok(stdout)
            }
            Ok(Err(err)) => Err(SubprocessPluginError::Spawn {
                config_id: spec.config_id.clone(),
                detail: format!("failed to wait for plugin process: {err}"),
            }),
            Err(_elapsed) => {
                kill_group(&mut child, pgid).await;
                Err(SubprocessPluginError::TimedOut {
                    config_id: spec.config_id.clone(),
                    after_ms: spec.timeout_ms,
                })
            }
        }
    }
}

/// A [`Plugin`] backed by a subprocess: `manifest`/`tools` are answered
/// from a `tool.spec/1` discovery call made ONCE, at
/// [`SubprocessPlugin::discover`] -- never re-queried per call, mirroring
/// `docs/plugins/hooks.md` point 1's own "consulted once, at registry
/// construction" contract for an in-process `Plugin`. Each declared tool is
/// a `SubprocessTool` that re-spawns the SAME command per `tool/1` call
/// (module doc: "no process outlives a single request").
pub struct SubprocessPlugin {
    manifest: PluginManifest,
    tools: Vec<Arc<dyn Tool>>,
    /// The persistent session, when [`SubprocessPluginSpec::transport`] is
    /// [`SubprocessTransport::Persistent`]; `None` for one-shot. Held here so
    /// the per-point version records the `initialize/1` handshake produced
    /// (board item `01M03VK7MRPSAVWMW7YNYPRPGT`) are reachable through the
    /// plugin via [`SubprocessPlugin::point_version`] WITHOUT re-negotiating
    /// -- the shape later wire-point items (permission.policy, observe,
    /// status, context.hook) consult to decide per-point refuse-vs-degrade.
    /// Every `SubprocessTool` on this plugin shares the SAME `Arc` (and thus
    /// the same child process); this is a second `Arc` clone, not a second
    /// session.
    session: Option<Arc<PersistentSession>>,
}

impl std::fmt::Debug for SubprocessPlugin {
    // Manual impl: `dyn Tool` carries no `Debug` bound (matching every
    // other `Arc<dyn Trait>`-holding type in this workspace, e.g.
    // `conway_core::ports::plugin::ToolCtx`'s own manual impl), so `tools`
    // is summarized by count rather than derived field-by-field.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubprocessPlugin")
            .field("manifest", &self.manifest)
            .field("tool_count", &self.tools.len())
            .field("persistent", &self.session.is_some())
            .finish()
    }
}

impl SubprocessPlugin {
    /// Spawns `spec.command` once, sends `{"op":"tool.spec/1"}`, and
    /// builds a [`SubprocessPlugin`] from the answer.
    ///
    /// **Why this is an async associated function, not something
    /// [`Plugin::manifest`]/[`Plugin::tools`] do lazily.** Those two
    /// methods are synchronous or fallible (`crates/conway-core/src/ports/
    /// plugin.rs`'s own "There is no initialization hook, deliberately"
    /// doc: `PluginRegistry::from_plugins` is synchronous and eager, so
    /// there is no point in the trait itself where a fallible, I/O-
    /// performing discovery call could run and have its failure mean
    /// anything). This mirrors that doc's own prescribed answer exactly:
    /// "a plugin needing setup does it in its own constructor, before
    /// `ConwayBuilder::with_plugin`, where errors surface to the embedder
    /// directly" -- this associated function IS that constructor, and its
    /// `Result` is the failure surfacing this doc asks for. A caller
    /// (`conway-cli`'s own subprocess-plugin loader) awaits this before
    /// ever handing the result to `ConwayBuilder::with_plugin`.
    ///
    /// **Tool name collision across the discovered set is refused here,
    /// fail-closed** (`SubprocessPluginError::InvalidManifest`) -- a wire
    /// manifest declaring two tools of the same name can never be resolved
    /// to one `ToolName` correctly, and letting the LAST one silently win
    /// would hide a real authoring bug in whatever produced the manifest.
    /// (Collision against a DIFFERENT plugin's tool name is a separate,
    /// pre-existing check `ConwayBuilder::build` already performs across
    /// every registered plugin -- untouched by this crate.)
    pub async fn discover(spec: SubprocessPluginSpec) -> Result<Self, SubprocessPluginError> {
        let request = wire::Request::ToolSpecV1;
        let payload = serde_json::to_vec(&request).expect("Request::ToolSpecV1 always serializes");
        let stdout = spawn_one_shot(&spec, &payload).await?;

        let manifest: WireManifest =
            serde_json::from_slice(stdout.trim_ascii()).map_err(|err| {
                SubprocessPluginError::UnparseableAnswer {
                    config_id: spec.config_id.clone(),
                    detail: err.to_string(),
                }
            })?;

        if manifest.id.is_empty() {
            return Err(SubprocessPluginError::InvalidManifest {
                config_id: spec.config_id.clone(),
                detail: "declared manifest id is empty".into(),
            });
        }
        if manifest.tools.is_empty() {
            return Err(SubprocessPluginError::InvalidManifest {
                config_id: spec.config_id.clone(),
                detail: "declared manifest has zero tools".into(),
            });
        }

        let mut tool_names = std::collections::HashSet::new();
        let mut specs = Vec::with_capacity(manifest.tools.len());
        for wire_tool in &manifest.tools {
            if wire_tool.name.is_empty() {
                return Err(SubprocessPluginError::InvalidManifest {
                    config_id: spec.config_id.clone(),
                    detail: "declared tool has an empty name".into(),
                });
            }
            if !tool_names.insert(wire_tool.name.clone()) {
                return Err(SubprocessPluginError::InvalidManifest {
                    config_id: spec.config_id.clone(),
                    detail: format!("declared tool name '{}' is duplicated", wire_tool.name),
                });
            }
            let schema: schemars::schema::RootSchema =
                serde_json::from_value(wire_tool.schema.clone()).map_err(|err| {
                    SubprocessPluginError::InvalidManifest {
                        config_id: spec.config_id.clone(),
                        detail: format!(
                            "tool '{}' has an invalid JSON Schema: {err}",
                            wire_tool.name
                        ),
                    }
                })?;
            specs.push(ToolSpec {
                name: ToolName::new(wire_tool.name.clone()),
                description: wire_tool.description.clone(),
                schema,
                category: wire_tool.category,
                permission: wire_tool.permission,
            });
        }

        let plugin_manifest = PluginManifest {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            tools: specs.iter().map(|s| s.name.clone()).collect(),
            required_host_caps: manifest.required_host_caps.clone(),
        };

        let spec = Arc::new(spec);
        // For the persistent transport, spawn the long-lived child ONCE
        // here and share it across every tool on this plugin. Discovery
        // itself stays one-shot (the `tool.spec/1` call above) -- the
        // persistent channel carries only `tool/1` (see `wire`'s own module
        // doc for why that sidesteps the manifest-`id` / JSON-RPC-`id`
        // collision). A spawn failure here fails the WHOLE build, the same
        // posture a discovery spawn failure already has.
        let session: Option<Arc<PersistentSession>> = match spec.transport {
            SubprocessTransport::OneShot => None,
            SubprocessTransport::Persistent => {
                // Spawn the long-lived child, THEN run the one-time
                // `initialize/1` handshake BEFORE wrapping in `Arc` and
                // before any `tool/1` call can run (board item
                // `01M03VK7MRPSAVWMW7YNYPRPGT`). `spawn` stays focused on
                // process spawning + task wiring; `initialize` does the
                // one-time version-negotiation round-trip over the SAME
                // id-correlated NDJSON framing `tool/1` uses. A handshake
                // refusal (`HandshakeRefused`/`HandshakeMalformed`) or a
                // transport-level death during the handshake (`SessionDied`/
                // `TimedOut`) surfaces here from `discover`, so an
                // incompatible plugin is refused at discover time, not at
                // first use -- and the just-spawned child is dropped (its
                // `Drop` kills the process group), never orphaned.
                let session = PersistentSession::spawn(&spec).await?;
                session.initialize().await?;
                // The one-time `permission.policy/1` declaration exchange
                // (board item `01M03VKJG7JJ0JEKY265WA7MJ7`), AFTER
                // `initialize/1` succeeds and BEFORE the session is wrapped
                // in `Arc` / any `tool/1` call can run. Version negotiation
                // is against the per-point record `initialize` just
                // produced: a plugin declaring `permission.policy/1` at an
                // unsupported version is REFUSED here (participant rule,
                // `HandshakeRefused` naming the version mismatch); a plugin
                // that does not declare the point loads normally and
                // contributes no wire policy. A malformed policy answer is
                // `HandshakeMalformed`, fail-closed (never silently no-op).
                // On any failure the just-spawned child is dropped (its
                // `Drop` kills the process group), never orphaned.
                session.request_permission_policy().await?;
                // The one-time `observe/1` engagement (board item
                // `01M03VKQ738DTGHHK2C4RWXC0E`), AFTER
                // `permission.policy/1`. An OBSERVER point: a plugin
                // declaring `observe/1` at a SUPPORTED version exchanges its
                // selector and the host spawns a writer task that forwards
                // matching `Event`s as no-`id` notifications on the plugin's
                // stdin; an UNSUPPORTED version DEGRADES (warn, load without
                // the point) -- the observer rule, the OPPOSITE of
                // `permission.policy/1`'s participant refusal; a plugin that
                // does not declare the point loads normally and contributes
                // no observe sink. A malformed/refused engagement answer ALSO
                // degrades (observer-class, never fails the session). ONLY a
                // transport-level death during the engagement propagates as
                // `Err` (the just-spawned child is dropped, never orphaned).
                session.request_observe().await?;
                // The one-time `status.declare/1` engagement (board item
                // `01M03VKQ738DTGHHK2C4RWXC0E`), AFTER `observe/1`. Same
                // observer rule: a SUPPORTED version exchanges the plugin's
                // per-key declarations and the host's reader routes inbound
                // no-`id` `status/1` lines to a bounded notification channel
                // (drop+warn, never blocks the host turn); an UNSUPPORTED
                // version DEGRADES; a plugin that does not declare the point
                // loads normally and contributes no status. A malformed/
                // refused answer degrades; only a transport-level death
                // propagates as `Err`.
                session.request_status_declare().await?;
                Some(Arc::new(session))
            }
        };
        let tools: Vec<Arc<dyn Tool>> = specs
            .into_iter()
            .map(|tool_spec| {
                Arc::new(SubprocessTool {
                    spec: tool_spec,
                    process_spec: spec.clone(),
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

    /// The plugin's declared version for a wire point (e.g. `"tool/1"`),
    /// recorded ONCE by the `initialize/1` handshake at persistent-session
    /// open (board item `01M03VK7MRPSAVWMW7YNYPRPGT`). `None` for the
    /// one-shot transport (no handshake), before a successful handshake, or
    /// for a point the plugin did not declare. Later wire-point items
    /// (`permission.policy/1`, `observe/1`, `status/1`, `context.hook/1`)
    /// consult this to decide per-point refuse-vs-degrade per
    /// `docs/plugins/compatibility.md`'s participant-vs-observer table rows,
    /// WITHOUT re-negotiating -- the records are produced once, here, and
    /// held for the session's lifetime.
    pub fn point_version(&self, point: &str) -> Option<u32> {
        self.session.as_ref().and_then(|s| s.point_version(point))
    }

    /// The per-tool permission policy this plugin declared over
    /// `permission.policy/1` at persistent-session open (board item
    /// `01M03VKJG7JJ0JEKY265WA7MJ7`), as the host-side
    /// [`PluginPermissionRule`]s the `conway` facade installs in the
    /// `PermissionBroker` as `PatternOrigin::Plugin` deny/prompt rules.
    /// Empty for the one-shot transport (no handshake, no policy exchange),
    /// for a persistent plugin that did not declare the point (it contributes
    /// no wire policy), or before `discover` has run the one-time exchange.
    ///
    /// **Narrowing-only by construction.** The wire shape has no `allow`
    /// verdict (a plugin may `deny`/`prompt`/`abstain`, never widen), so the
    /// [`PluginPermissionVerdict`] this returns never carries an `Allow` --
    /// the operator's own `permissions.json`/`PermissionMode` STILL wins over
    /// any plugin declaration. See `Plugin::permission_rules`'s own trait
    /// doc for the subordination boundary.
    pub fn permission_rules(&self) -> Vec<PluginPermissionRule> {
        self.session
            .as_ref()
            .map(|s| s.permission_rules())
            .unwrap_or_default()
            .into_iter()
            .map(|r| PluginPermissionRule {
                tool: r.tool,
                verdict: match r.verdict {
                    wire::WirePermissionVerdict::Deny => PluginPermissionVerdict::Deny,
                    wire::WirePermissionVerdict::Prompt => PluginPermissionVerdict::Prompt,
                    wire::WirePermissionVerdict::Abstain => PluginPermissionVerdict::Abstain,
                },
                reason: r.reason,
            })
            .collect()
    }

    /// An [`EventSinkHandle`] the host fans the runtime's live `Event` stream
    /// onto so this plugin can OBSERVE host events over its persistent session
    /// -- the host-side half of the `observe/1` wire point (board item
    /// `01M03VKQ738DTGHHK2C4RWXC0E`, see `docs/plugins/hooks.md` point 11).
    /// `None` for the one-shot transport (no handshake, no observe
    /// engagement), a persistent plugin that did not declare the point, a
    /// persistent plugin that declared it at an unsupported version (DEGRADE --
    /// loaded without the point), or before `discover` has run the one-time
    /// engagement. The sink is lossy-with-notice (bounded channel, drop+warn
    /// on overflow, never blocks the host turn); see `session::ObserveAdapter`'s
    /// own doc for the discipline and the `Event::Lagged` passthrough.
    pub fn observe_sink(&self) -> Option<EventSinkHandle> {
        self.session.as_ref().and_then(|s| s.observe_sink())
    }

    /// A point-in-time snapshot of the status contributions this plugin is
    /// CURRENTLY pushing -- the host-side half of the `status.declare/1` /
    /// `status/1` wire point (board item `01M03VKQ738DTGHHK2C4RWXC0E`, see
    /// `docs/plugins/hooks.md` point 12). Reads the session's per-key status
    /// store, which the notification handler task updates from inbound no-`id`
    /// `status/1` lines. Empty for the one-shot transport, a plugin that did
    /// not declare the point, a plugin that declared it at an unsupported
    /// version (DEGRADE), or before any `status/1` notifications have arrived.
    /// An unknown `ResultStatus` wire tag the plugin pushes was already
    /// degraded to `ResultStatus::Failed` at parse time (the compatibility
    /// table's `ResultStatus` row, never `Completed`); see
    /// `wire::parse_status_notification`'s own doc.
    pub fn status_contributions(&self) -> Vec<PluginStatusContribution> {
        self.session
            .as_ref()
            .map(|s| s.status_contributions())
            .unwrap_or_default()
            .into_iter()
            .map(|c| PluginStatusContribution {
                key: c.key,
                status: c.status,
                value: c.value,
            })
            .collect()
    }
}

impl Plugin for SubprocessPlugin {
    fn manifest(&self) -> PluginManifest {
        self.manifest.clone()
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }

    /// The per-tool NARROWING permission rules this subprocess plugin
    /// declared over `permission.policy/1` at persistent-session open (board
    /// item `01M03VKJG7JJ0JEKY265WA7MJ7`). See [`Self::permission_rules`]'s
    /// own doc for the wire→host projection and the subordination boundary
    /// (operator wins; wire policy is advisory-under-enforcement, narrowing
    /// only). The default `Vec::new()` is what every other `Plugin`
    /// implementor still returns.
    fn permission_rules(&self) -> Vec<PluginPermissionRule> {
        SubprocessPlugin::permission_rules(self)
    }

    /// The `observe/1` sink this persistent plugin engaged at session open
    /// (board item `01M03VKQ738DTGHHK2C4RWXC0E`). See
    /// [`Self::observe_sink`]'s own doc for the degrade boundary (unsupported
    /// version -> `None`, load without the point) and the one-way,
    /// lossy-with-notice discipline. The default `None` is what every other
    /// `Plugin` implementor still returns.
    fn observe_sink(&self) -> Option<EventSinkHandle> {
        SubprocessPlugin::observe_sink(self)
    }

    /// The `status/1` contributions this persistent plugin is currently
    /// pushing (board item `01M03VKQ738DTGHHK2C4RWXC0E`). See
    /// [`Self::status_contributions`]'s own doc for the polled-snapshot
    /// discipline and the unknown-tag-degrades-to-`Failed` rule. The default
    /// empty `Vec` is what every other `Plugin` implementor still returns.
    fn status_contributions(&self) -> Vec<PluginStatusContribution> {
        SubprocessPlugin::status_contributions(self)
    }
}

/// One tool a [`SubprocessPlugin`] declared. `tool/1` calls are answered
/// one of two ways, selected per-[`SubprocessPluginSpec`] (default
/// one-shot): by re-spawning `process_spec.command` fresh for every call
/// (one-shot, module doc's "no process outlives a single request"), OR by
/// dispatching over the shared `session`'s long-lived NDJSON channel
/// (persistent, board item `01M03VJHG1WFECFJB4ZH3CKWDX`).
struct SubprocessTool {
    spec: ToolSpec,
    process_spec: Arc<SubprocessPluginSpec>,
    /// `Some` only when [`SubprocessPluginSpec::transport`] is
    /// [`SubprocessTransport::Persistent`]; every tool on this plugin
    /// shares the SAME `Arc<PersistentSession>` (and thus the same child
    /// process -- the load-bearing property acceptance criterion 1
    /// asserts).
    session: Option<Arc<PersistentSession>>,
}

#[async_trait]
impl Tool for SubprocessTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    /// PRE (from the trait): `call.arguments` is already schema-validated.
    /// Checks `ctx.cancel` before spawning at all (mirrors
    /// `conway_tools::common::check_cancel`, unreachable from here because
    /// this crate may not depend on `conway-tools` directly -- unlike
    /// `conway_tools::process::unix::kill_group`, `check_cancel` has no
    /// facade re-export, since nothing outside `conway-tools` has needed
    /// one yet) -- a call already cancelled by the time it reaches this
    /// tool never spawns a process it would only have to kill moments
    /// later.
    ///
    /// **Every distinct subprocess failure mode maps to a typed
    /// `ToolError`, never a hang and never a panic** -- the guarantee this
    /// item's own hard rules ask for, argued from the same precedent
    /// `docs/plugins/concepts.md`'s "Fork vs spawn" section cites for a
    /// different mechanism: "a parent awaiting a child cannot hang"
    /// (`PHILOSOPHY.md` §1). Spawn failure and timeout become
    /// `ToolError::Io`; a nonzero exit or unparseable/malformed stdout
    /// becomes `ToolError::Internal`, naming the underlying
    /// [`SubprocessPluginError`] in both cases so an operator can tell a
    /// broken plugin apart from a legitimately-declined call
    /// (`WireToolResult::Err` maps to a specific `ToolError` variant
    /// instead, see below). The persistent path adds `SessionDied` and
    /// `MalformedFrame` to that set, all surfaced through `ToolError::Io`
    /// /`ToolError::Internal` so a dead/misbehaving session is told apart
    /// from a legitimately-declined call the same way.
    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }

        // Dispatch over the persistent NDJSON channel when the spec
        // declared it; otherwise the original one-shot exec path. The
        // `WireToolResult` classification is IDENTICAL on both paths (the
        // persistent channel reuses the one-shot `tool/1` answer shape,
        // see `wire`'s own module doc) -- only the transport differs.
        let result = if let Some(session) = &self.session {
            session
                .tool_round_trip(
                    self.spec.name.to_string(),
                    call.call_id.clone(),
                    call.arguments,
                )
                .await?
        } else {
            let request = wire::Request::ToolV1 {
                tool: self.spec.name.to_string(),
                call_id: call.call_id.clone(),
                arguments: call.arguments,
            };
            let payload = serde_json::to_vec(&request).map_err(|err| ToolError::Internal {
                detail: format!("failed to serialize tool/1 request: {err}"),
            })?;

            let stdout = spawn_one_shot(&self.process_spec, &payload)
                .await
                .map_err(|err| ToolError::Io {
                    detail: err.to_string(),
                })?;

            if ctx.cancel.is_cancelled() {
                // The subprocess answered, but the caller no longer wants the
                // result -- report cancellation, not a stale success, matching
                // `Tool::invoke`'s own POST condition ("honors ctx.cancel").
                return Err(ToolError::Cancelled);
            }

            wire::parse_tool_result(stdout.trim_ascii()).map_err(|detail| ToolError::Internal {
                detail: format!(
                    "plugin '{}' produced an unparseable tool/1 answer: {detail}",
                    self.process_spec.config_id
                ),
            })?
        };

        match result {
            WireToolResult::Ok {
                blocks,
                is_error,
                artifacts,
            } => Ok(ToolOutput {
                blocks,
                is_error,
                truncation: TruncationPolicy::None,
                artifacts,
            }),
            WireToolResult::Err(WireToolError {
                kind,
                detail,
                after_secs,
            }) => Err(kind.into_tool_error(detail, after_secs)),
        }
    }

    /// Every declared field name in `call.arguments` is opaque to this
    /// host (a subprocess plugin's schema is arbitrary JSON Schema this
    /// host never introspects beyond compiling it) -- conservative default
    /// applies unchanged, matching `Tool::path_args`'s own trait-level
    /// default and rationale.
    fn path_args(&self) -> PathArgs {
        PathArgs::default()
    }

    /// Conservative default -- this host has no basis to know whether a
    /// subprocess tool's `render` output (the trait default: a
    /// `name(args)` debug dump) is shell-interpretable, so it stays gated
    /// exactly as every tool is before overriding this method
    /// (`Tool::render_kind`'s own doc).
    fn render_kind(&self) -> RenderKind {
        RenderKind::default()
    }
}
