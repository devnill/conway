//! C4: facade-only parity acceptance test (GP-03/P-6) -- the payoff and
//! acceptance gate for the whole facade-parity epic (C1/C2/C3/C5).
//!
//! Proves that conway's own `conway_subagent`/`conway_ask`/`conway_steer`/
//! `conway_await`/`conway_cancel`/`report`/`cd` tools can be RE-AUTHORED
//! using only the public `conway::` plugin surface -- the identical surface
//! a third-party plugin author gets. The in-tree built-ins
//! (`conway-tools`) cannot prove this themselves: `conway` depends on
//! `conway-tools` behind the `builtin-tools` feature, so `conway-tools`
//! importing `conway::plugin` back would be a crate cycle. So parity is
//! proven here instead, by re-implementing each tool's `invoke` logic
//! against `conway::plugin` alone, in a file that lives where a
//! third-party plugin crate would.
//!
//! Read `plugin_surface.rs` first -- this file mirrors its structure, its
//! file-level doc rule, and its idiom throughout.
//!
//! **This file must never import `conway_core`.** That is the break-the-
//! guard property, built in: if the curated export set in
//! `crates/conway/src/lib.rs`'s `pub mod plugin` (or the root re-exports
//! `ForkSpec`/`SpawnSpec`/`AgentResult`/`AskOrigin`/`Budget`/`ToolSelector`/
//! `RoleAlias` these replicas also need) is missing anything one of these
//! seven tools' real logic needs, this file fails to COMPILE -- the test
//! cannot silently pass against a shrunken surface. The negative direction
//! is verified by hand each time this surface changes: remove one
//! re-export and confirm this file stops compiling, then restore it and
//! confirm it compiles again (same discipline `plugin_surface.rs` states
//! for itself).
//!
//! **Compile-only, not behavior-driven.** Every `invoke` body below is
//! real -- the same argument parsing, spec construction, handle call, and
//! error mapping as the built-in it replicates -- but no test here ever
//! calls `invoke`: doing so needs a real `ToolCtx`, which needs
//! `conway-core`'s `fakes` feature, which would reintroduce the exact
//! dependency this file exists to prohibit (and would duplicate coverage
//! C1's/C3's own unit tests already provide for conversion semantics and
//! error mapping -- not this file's job). `cargo test`ing this crate still
//! type-checks every `invoke` body in full regardless (Rust type-checks
//! impl bodies whether or not anything calls them), so the compile guard
//! is live even though no test executes that code path -- the same
//! "compile-only static check" precedent `plugin_surface.rs` already
//! establishes for `EchoTool`. The `#[test]`s below only exercise what
//! needs no `ToolCtx` at all: each replica's own declared
//! `spec()`/`path_args()`/`render_kind()`, mirroring
//! `authored_plugin_and_tool_are_self_consistent`'s division of labor.
//!
//! **`SubagentSpec` is never named, on purpose.** It is deliberately not
//! exported (see `lib.rs`'s `pub mod plugin` doc, the closed list under
//! "RESOLVED"). Every place the real `conway_subagent`/`conway_ask` tools
//! construct one directly, this file instead builds a `conway::ForkSpec`/
//! `conway::SpawnSpec` and calls `.into()`: type inference resolves the
//! public `From<ForkSpec>`/`From<SpawnSpec>` impl from the unnamed
//! `SubagentSpec` parameter type of `SubagentHandle::start`/`::ask` --
//! itself reached only through `ctx.subagents`, a `ToolCtx` field,
//! method-dispatched without ever naming `SubagentHandle` either (the same
//! "field access, not the type name" pattern `plugin_surface.rs` already
//! uses for `ctx.cwd`). Per C5, `SubagentSpec::await_result` no longer
//! exists and nothing here constructs or references it; awaiting is
//! decided entirely by `conway_subagent`'s own `await` argument, exactly as
//! in the real tool.
//!
//! `serde`/`serde_json`/`schemars` are the facade's own declared
//! dependencies (available to every integration test in this crate, the
//! same note `plugin_surface.rs` makes about itself) -- not something this
//! file or F8 curates.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use conway::plugin::{
    Artifact, ArtifactKind, ContentBlock, CwdError, Fact, PathArgs, PermissionClass, RenderKind,
    Tool, ToolCall, ToolCategory, ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec,
    TruncationPolicy,
};
use conway::{
    AgentId, AgentResult, AskOrigin, Budget, ForkSpec, ResultStatus, RoleAlias, SpawnSpec,
    ToolSelector,
};

/// Declared by every replica below, mirroring `conway-tools`' own
/// `subagent/tools.rs::TRUNCATION`: an oversized result keeps its tail.
const TRUNCATION: TruncationPolicy = TruncationPolicy::Tail { max_bytes: 16_384 };

fn parse_agent_id(raw: &str) -> Result<AgentId, ToolError> {
    raw.parse::<AgentId>()
        .map_err(|e| ToolError::InvalidArguments {
            detail: format!("agent_id: {e}"),
        })
}

fn text_output(text: String) -> ToolOutput {
    ToolOutput {
        blocks: vec![ContentBlock::Text { text }],
        is_error: false,
        truncation: TRUNCATION,
        artifacts: Vec::new(),
    }
}

/// `is_error` is `false` only for `ResultStatus::Completed` -- mirrors
/// `conway-tools`' own `agent_result_output`.
fn agent_result_output(result: &AgentResult) -> ToolOutput {
    let is_error = !matches!(result.status, ResultStatus::Completed);
    let text = serde_json::to_string(result).expect("AgentResult is always serializable");
    ToolOutput {
        blocks: vec![ContentBlock::Text { text }],
        is_error,
        truncation: TRUNCATION,
        artifacts: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// `conway_subagent`: SpawnSpec/ForkSpec construction + `.into()`, awaiting
// per the args flag, AgentResult handling.
// ---------------------------------------------------------------------------

fn default_await() -> bool {
    true
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ModeArg {
    Fork,
    Spawn,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BudgetArg {
    max_steps: Option<u32>,
    max_tokens: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SubagentArgs {
    mode: ModeArg,
    prompt: String,
    #[serde(default)]
    agent_def: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    budget: Option<BudgetArg>,
    #[serde(default = "default_await", rename = "await")]
    await_flag: bool,
}

fn resolve_budget(arg: Option<BudgetArg>) -> Budget {
    let mut budget = Budget::default();
    if let Some(arg) = arg {
        if let Some(max_steps) = arg.max_steps {
            budget.max_steps = max_steps;
        }
        budget.max_tokens = arg.max_tokens;
    }
    budget
}

struct SubagentToolReplica;

#[async_trait]
impl Tool for SubagentToolReplica {
    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }

    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("conway_subagent"),
            description: "Fork or spawn a child agent.".into(),
            schema: schemars::schema_for!(SubagentArgs),
            category: ToolCategory::Delegate,
            permission: PermissionClass::Dangerous,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let args: SubagentArgs =
            serde_json::from_value(call.arguments).map_err(|e| ToolError::InvalidArguments {
                detail: e.to_string(),
            })?;

        let budget = resolve_budget(args.budget);
        let tools = args.tools.map(ToolSelector::Only);

        // The `.into()` calls below are the type-inference point this item
        // exists to pin: each resolves the public `From<ForkSpec>`/
        // `From<SpawnSpec>` impl for `SubagentHandle::start`'s unnamed
        // `SubagentSpec` parameter -- that type is never written anywhere
        // in this file.
        let child = match args.mode {
            ModeArg::Fork => {
                let mut spec = ForkSpec::new(args.prompt).budget(budget);
                if let Some(def) = args.agent_def {
                    spec = spec.agent_def(def);
                }
                if let Some(role) = args.role {
                    spec = spec.role(RoleAlias::new(role));
                }
                if let Some(tools) = tools {
                    spec = spec.tools(tools);
                }
                ctx.subagents
                    .start(spec.into())
                    .await
                    .map_err(ToolError::from)?
            }
            ModeArg::Spawn => {
                let mut spec = SpawnSpec::new(args.prompt).budget(budget);
                if let Some(def) = args.agent_def {
                    spec = spec.agent_def(def);
                }
                if let Some(role) = args.role {
                    spec = spec.role(RoleAlias::new(role));
                }
                if let Some(tools) = tools {
                    spec = spec.tools(tools);
                }
                ctx.subagents
                    .start(spec.into())
                    .await
                    .map_err(ToolError::from)?
            }
        };

        if !args.await_flag {
            return Ok(text_output(
                serde_json::json!({ "agent_id": child }).to_string(),
            ));
        }
        // Per C5: `SubagentSpec::await_result` is gone. Whether to block is
        // decided entirely by this tool's own `await` argument above, then
        // carried out through `SubagentHandle::await_result` -- unaffected
        // by that deletion, and never itself constructed/referenced as a
        // spec field anywhere in this file.
        let result = ctx
            .subagents
            .await_result(child)
            .await
            .map_err(ToolError::from)?;
        Ok(agent_result_output(&result))
    }
}

// ---------------------------------------------------------------------------
// `conway_ask`: the ForkSpec ephemeral-ask shape (`ephemeral` +
// `ask_origin`, both added by C3), artifact construction.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AskArgs {
    prompt: String,
    #[serde(default)]
    tools: Option<Vec<String>>,
}

struct AskToolReplica;

#[async_trait]
impl Tool for AskToolReplica {
    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }

    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("conway_ask"),
            description: "Run a prompt in an ephemeral fork of this agent.".into(),
            schema: schemars::schema_for!(AskArgs),
            category: ToolCategory::Delegate,
            permission: PermissionClass::Dangerous,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let args: AskArgs =
            serde_json::from_value(call.arguments).map_err(|e| ToolError::InvalidArguments {
                detail: e.to_string(),
            })?;

        // The ephemeral-ask shape (P-1: `ask` is fork+await-text, not a
        // third primitive) -- fork-only, GP-02: `SpawnSpec` has neither
        // `ephemeral` nor `ask_origin` at all, so there is no way to
        // express this shape with a spawn.
        let mut spec = ForkSpec::new(args.prompt)
            .ephemeral(true)
            .ask_origin(AskOrigin::ToolAsk);
        if let Some(tools) = args.tools {
            spec = spec.tools(ToolSelector::Only(tools));
        }

        let outcome = ctx
            .subagents
            .ask(spec.into())
            .await
            .map_err(ToolError::from)?;

        // P-2: the persisted `ToolOutput` names the child session via an
        // `EphemeralSessionRef` artifact pointing at the child's
        // `transcript_ref`. The model sees only the clean reply text.
        let artifact = Artifact {
            id: outcome.transcript_ref.to_string(),
            kind: ArtifactKind::EphemeralSessionRef,
            path: None,
            media_type: None,
            bytes: None,
            label: "ephemeral_session_ref".to_string(),
        };

        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text { text: outcome.text }],
            is_error: !matches!(outcome.status, ResultStatus::Completed),
            truncation: TRUNCATION,
            artifacts: vec![artifact],
        })
    }
}

// ---------------------------------------------------------------------------
// `report`: `Fact` literals (exported by C3) and the v1 report envelope the
// runtime lifts into the terminal result.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FactArg {
    key: String,
    value: String,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReportArgs {
    summary: String,
    #[serde(default)]
    facts: Vec<FactArg>,
    #[serde(default)]
    structured: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct ReportEnvelope {
    conway_report: ReportBody,
}

#[derive(Debug, Serialize)]
struct ReportBody {
    version: u32,
    summary: String,
    facts: Vec<Fact>,
    structured: Option<serde_json::Value>,
}

struct ReportToolReplica;

#[async_trait]
impl Tool for ReportToolReplica {
    // NOT `None`: the real `report` tool's own doc explains why -- nested
    // per-artifact paths this vocabulary cannot express -- and `report`
    // (unlike this replica's simplified `ReportArgs`) also takes
    // `artifacts`. `Unconfinable` with nothing checkable is the honest
    // answer either way.
    fn path_args(&self) -> PathArgs {
        PathArgs::Unconfinable { checkable: &[] }
    }

    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("report"),
            description: "Declare this agent's terminal result.".into(),
            schema: schemars::schema_for!(ReportArgs),
            category: ToolCategory::Think,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let args: ReportArgs =
            serde_json::from_value(call.arguments).map_err(|e| ToolError::InvalidArguments {
                detail: e.to_string(),
            })?;

        // `Fact` (exported by C3) constructed directly, the report tool's
        // own typed-fact output shape.
        let facts: Vec<Fact> = args
            .facts
            .into_iter()
            .map(|arg| Fact {
                key: arg.key,
                value: serde_json::Value::String(arg.value),
                source: arg.source,
            })
            .collect();

        // The v1 report envelope the runtime recognizes by the `report`
        // tool's name and lifts into the terminal result. That lift itself
        // stays inside `conway-runtime` (architecture boundary:
        // `conway-tools` performs no result finalization) -- a facade-only
        // `report`-alike need only emit this exact shape, which it can, in
        // full, from here.
        let envelope = ReportEnvelope {
            conway_report: ReportBody {
                version: 1,
                summary: args.summary,
                facts,
                structured: args.structured,
            },
        };
        let text =
            serde_json::to_string(&envelope).expect("report envelope is always serializable");

        Ok(text_output(text))
    }
}

// ---------------------------------------------------------------------------
// `cd`: the `CwdError` match, including the `Poisoned` arm AND the
// `#[non_exhaustive]` wildcard arm.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CdArgs {
    path: String,
}

struct CdToolReplica;

#[async_trait]
impl Tool for CdToolReplica {
    fn path_args(&self) -> PathArgs {
        PathArgs::Named(&["path"])
    }

    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("cd"),
            description: "Change the working directory for subsequent tool calls.".into(),
            schema: schemars::schema_for!(CdArgs),
            category: ToolCategory::Move,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let args: CdArgs =
            serde_json::from_value(call.arguments).map_err(|e| ToolError::InvalidArguments {
                detail: e.to_string(),
            })?;

        // The real `cd` tool resolves through `conway-tools`' own
        // internal `resolve_path` helper -- not part of this facade
        // surface. This replica's join-onto-`ctx.cwd` fallback is the
        // externally-visible equivalent (`ctx.cwd` is a plain, exported
        // `PathBuf` field); what this replica exists to pin is the
        // `CwdError` match immediately below, not path resolution.
        let candidate = std::path::Path::new(&args.path);
        let path = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            ctx.cwd.join(candidate)
        };

        // `ctx.chdir` is a `CwdHandle` -- deliberately unexported (see
        // `lib.rs`'s `pub mod plugin` doc's "Deliberately NOT here" list):
        // dispatched by field access alone, exactly like `ctx.cwd`/
        // `ctx.subagents` above, never named.
        ctx.chdir.set(path.clone()).map_err(|err| match err {
            CwdError::Poisoned => ToolError::Internal {
                detail: format!("cwd handle poisoned: {err}"),
            },
            // `CwdError` is `#[non_exhaustive]`: a future variant must map
            // to a typed `ToolError` deliberately, here too, not fall
            // through to a panic (P-10) -- the real `cd` tool's own
            // discipline, reproduced verbatim.
            other => ToolError::Internal {
                detail: format!("cwd handle set failed: {other}"),
            },
        })?;

        Ok(text_output(format!(
            "cwd is now {} (takes effect next batch)",
            path.display()
        )))
    }
}

// ---------------------------------------------------------------------------
// The control trio: `conway_steer`, `conway_await`, `conway_cancel`. These
// shared `host_error` before C2 deleted it, and became facade-only nearly
// free afterward -- included so the family is not left half-migrated.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SteerArgs {
    agent_id: String,
    text: String,
}

struct SteerToolReplica;

#[async_trait]
impl Tool for SteerToolReplica {
    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }

    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("conway_steer"),
            description: "Send a steering message to a running child agent".into(),
            schema: schemars::schema_for!(SteerArgs),
            category: ToolCategory::Delegate,
            permission: PermissionClass::RequiresApproval,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let args: SteerArgs =
            serde_json::from_value(call.arguments).map_err(|e| ToolError::InvalidArguments {
                detail: e.to_string(),
            })?;
        let target = parse_agent_id(&args.agent_id)?;
        ctx.subagents
            .steer(target, args.text)
            .await
            .map_err(ToolError::from)?;
        Ok(text_output(format!("steered agent {target}")))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AwaitArgs {
    agent_id: String,
}

struct AwaitToolReplica;

#[async_trait]
impl Tool for AwaitToolReplica {
    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }

    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("conway_await"),
            description: "Block for a child agent's terminal result".into(),
            schema: schemars::schema_for!(AwaitArgs),
            category: ToolCategory::Delegate,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let args: AwaitArgs =
            serde_json::from_value(call.arguments).map_err(|e| ToolError::InvalidArguments {
                detail: e.to_string(),
            })?;
        let target = parse_agent_id(&args.agent_id)?;
        let result = ctx
            .subagents
            .await_result(target)
            .await
            .map_err(ToolError::from)?;
        Ok(agent_result_output(&result))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CancelArgs {
    agent_id: String,
    #[serde(default)]
    reason: Option<String>,
}

struct CancelToolReplica;

#[async_trait]
impl Tool for CancelToolReplica {
    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }

    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("conway_cancel"),
            description: "Cancel a running child agent".into(),
            schema: schemars::schema_for!(CancelArgs),
            category: ToolCategory::Delegate,
            permission: PermissionClass::RequiresApproval,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let args: CancelArgs =
            serde_json::from_value(call.arguments).map_err(|e| ToolError::InvalidArguments {
                detail: e.to_string(),
            })?;
        let target = parse_agent_id(&args.agent_id)?;
        let reason = args
            .reason
            .unwrap_or_else(|| "cancelled by parent agent".to_string());
        ctx.subagents
            .cancel(target, reason.clone())
            .await
            .map_err(ToolError::from)?;
        Ok(text_output(format!("cancelled agent {target}: {reason}")))
    }
}

// ---------------------------------------------------------------------------
// The acceptance criterion: every replica's own declared spec/path-args/
// render-kind, exercised directly -- no `ToolCtx` required (see this
// file's own header doc for why `invoke` itself is never called here).
// ---------------------------------------------------------------------------

#[test]
fn every_replica_tool_declares_its_own_name_category_and_permission() {
    let subagent = SubagentToolReplica.spec();
    assert_eq!(subagent.name.as_str(), "conway_subagent");
    assert_eq!(subagent.category, ToolCategory::Delegate);
    assert_eq!(subagent.permission, PermissionClass::Dangerous);
    assert_eq!(SubagentToolReplica.path_args(), PathArgs::None);
    assert_eq!(SubagentToolReplica.render_kind(), RenderKind::Structured);

    let ask = AskToolReplica.spec();
    assert_eq!(ask.name.as_str(), "conway_ask");
    assert_eq!(ask.category, ToolCategory::Delegate);
    assert_eq!(ask.permission, PermissionClass::Dangerous);
    assert_eq!(AskToolReplica.path_args(), PathArgs::None);

    let report = ReportToolReplica.spec();
    assert_eq!(report.name.as_str(), "report");
    assert_eq!(report.category, ToolCategory::Think);
    assert_eq!(report.permission, PermissionClass::Safe);
    assert_eq!(
        ReportToolReplica.path_args(),
        PathArgs::Unconfinable { checkable: &[] }
    );

    let cd = CdToolReplica.spec();
    assert_eq!(cd.name.as_str(), "cd");
    assert_eq!(cd.category, ToolCategory::Move);
    assert_eq!(cd.permission, PermissionClass::Safe);
    assert_eq!(CdToolReplica.path_args(), PathArgs::Named(&["path"]));

    let steer = SteerToolReplica.spec();
    assert_eq!(steer.name.as_str(), "conway_steer");
    assert_eq!(steer.category, ToolCategory::Delegate);
    assert_eq!(steer.permission, PermissionClass::RequiresApproval);

    let await_tool = AwaitToolReplica.spec();
    assert_eq!(await_tool.name.as_str(), "conway_await");
    assert_eq!(await_tool.category, ToolCategory::Delegate);
    assert_eq!(await_tool.permission, PermissionClass::Safe);

    let cancel = CancelToolReplica.spec();
    assert_eq!(cancel.name.as_str(), "conway_cancel");
    assert_eq!(cancel.category, ToolCategory::Delegate);
    assert_eq!(cancel.permission, PermissionClass::RequiresApproval);
}
