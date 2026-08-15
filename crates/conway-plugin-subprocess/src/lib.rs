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
//! number of declared tools (nothing here assumes exactly one) but nothing
//! beyond it: no `permission.policy/1`, no `context.hook/1`, no
//! `observe/1`, no persistent connection, no capability handshake against
//! `PluginManifest::required_host_caps` (still declared, still consulted by
//! nobody, unchanged from `crates/conway-core/src/ports/plugin.rs`'s own
//! disclosure).
//!
//! **Transport: one-shot exec, not the long-lived NDJSON JSON-RPC design.**
//! `docs/plugins/hooks.md`'s own point 9 doc and the decision record both
//! describe the eventual remote transport as a persistent connection. This
//! crate deliberately does NOT build that. The hard rule governing this
//! item says plainly: "the existing one-shot hook runner
//! (`conway-tools`'s `ProcessHookRunner`) already solves exactly this
//! problem -- read it and reuse its shape rather than inventing a second
//! process lifecycle." This crate follows that instruction literally: every
//! RPC this host makes -- one for `tool.spec/1` (manifest discovery, once,
//! at [`SubprocessPlugin::discover`]) and one per `tool/1` call (at
//! `SubprocessTool::invoke`) -- spawns the configured command FRESH,
//! writes one JSON request object to its stdin, reads one JSON response
//! object from its stdout, and tears the process down. No process outlives
//! a single request. This is a genuine, disclosed narrowing of the
//! decision record's own described transport, not an oversight: it is
//! simpler, it cannot leak a wedged long-lived child, and it costs an
//! author nothing a one-shot script cannot already pay (`docs/plugins/
//! concepts.md`'s own "Language choice" section already prices a one-shot
//! spawn at 10-400ms, and a `tool/1` call is not typically issued once per
//! tool-call-per-token the way `pre_tool_use` is).
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
    async_trait, PathArgs, Plugin, PluginManifest, RenderKind, Tool, ToolCall, ToolCtx, ToolError,
    ToolName, ToolOutput, ToolSpec, TruncationPolicy,
};

mod wire;

pub use wire::{WireManifest, WireTool, WireToolError, WireToolErrorKind, WireToolResult};

/// Applied when a [`SubprocessPluginSpec`] does not name its own
/// `timeout_ms` -- the SAME 5000ms default `crates/conway/src/config/
/// schema.rs`'s `HookEntry::timeout_ms` uses, for the identical reason
/// (`docs/plugins/hooks.md`'s own note on that field): long enough for a
/// typical local script to finish, short enough that a hung plugin process
/// cannot silently stall an agent turn indefinitely.
pub const DEFAULT_TIMEOUT_MS: u64 = 5000;

/// One operator-configured subprocess plugin entry: the command to spawn,
/// and how long any single spawn (discovery, or one `tool/1` call) is
/// allowed to run before this host kills it.
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
    /// when constructed via [`SubprocessPluginSpec::new`].
    pub timeout_ms: u64,
}

impl SubprocessPluginSpec {
    /// A spec with [`DEFAULT_TIMEOUT_MS`]. Use the struct literal directly
    /// to override `timeout_ms`.
    pub fn new(config_id: impl Into<String>, command: Vec<String>) -> Self {
        Self {
            config_id: config_id.into(),
            command,
            timeout_ms: DEFAULT_TIMEOUT_MS,
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
    /// see `unix::kill_group`).
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
}

#[cfg(unix)]
mod unix {
    //! **Deliberately duplicated, not reused, from `conway-tools`'s
    //! `crate::process::unix::kill_group`.** That module is `mod process;`
    //! (private) in `crates/conway-tools/src/lib.rs`, unreachable from
    //! outside the crate, and `conway-tools` is not among this item's
    //! owned paths -- widening its visibility is exactly the kind of edit
    //! this item's own instructions ask to flag rather than make silently.
    //! This is the identical ~15-line SIGTERM-then-SIGKILL sequence, cited
    //! by name rather than silently reinvented; see this item's completion
    //! report for the follow-up this leaves ("expose `conway_tools::
    //! process` publicly so a third reuse doesn't have to duplicate again
    //! either").

    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    use tokio::process::Child;
    use tokio::time::Duration;

    pub const TERM_GRACE: Duration = Duration::from_secs(2);

    pub async fn kill_group(child: &mut Child, pgid: i32) {
        let _ = kill(Pid::from_raw(-pgid), Signal::SIGTERM);
        if tokio::time::timeout(TERM_GRACE, child.wait())
            .await
            .is_err()
        {
            let _ = kill(Pid::from_raw(-pgid), Signal::SIGKILL);
            let _ = child.wait().await;
        }
    }
}

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
                unix::kill_group(&mut child, pgid).await;
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
            required_host_caps: Vec::new(),
        };

        let spec = Arc::new(spec);
        let tools: Vec<Arc<dyn Tool>> = specs
            .into_iter()
            .map(|tool_spec| {
                Arc::new(SubprocessTool {
                    spec: tool_spec,
                    process_spec: spec.clone(),
                }) as Arc<dyn Tool>
            })
            .collect();

        Ok(Self {
            manifest: plugin_manifest,
            tools,
        })
    }
}

impl Plugin for SubprocessPlugin {
    fn manifest(&self) -> PluginManifest {
        self.manifest.clone()
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}

/// One tool a [`SubprocessPlugin`] declared, answered by re-spawning
/// `process_spec.command` fresh for every [`Tool::invoke`] call -- module
/// doc's "no process outlives a single request".
struct SubprocessTool {
    spec: ToolSpec,
    process_spec: Arc<SubprocessPluginSpec>,
}

#[async_trait]
impl Tool for SubprocessTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    /// PRE (from the trait): `call.arguments` is already schema-validated.
    /// Checks `ctx.cancel` before spawning at all (mirrors
    /// `conway_tools::common::check_cancel`, unreachable from here for the
    /// same private-module reason `unix::kill_group`'s own doc names) --
    /// a call already cancelled by the time it reaches this tool never
    /// spawns a process it would only have to kill moments later.
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
    /// instead, see below).
    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }

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

        let result =
            wire::parse_tool_result(stdout.trim_ascii()).map_err(|detail| ToolError::Internal {
                detail: format!(
                    "plugin '{}' produced an unparseable tool/1 answer: {detail}",
                    self.process_spec.config_id
                ),
            })?;

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
