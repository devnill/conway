//! `conway_fork` and `conway_spawn`: pure wrappers over `ToolCtx::subagents`
//! (split into two tools by).
//! Zero delegation logic: argument parsing, one `ToolCtx::subagents`
//! (`SubagentHandle`) call -- the exact surface a third-party tool gets,
//! nothing more -- and result shaping.
//!
//! The two tools share every field except `prompt`'s meaning: `conway_fork`
//! sends a directive to a child that already holds this agent's context;
//! `conway_spawn` sends a complete statement of a task to a child that has
//! none. Splitting the single former tool -- one `mode: fork | spawn`
//! argument choosing between them -- in two settles that choice by picking a
//! name rather than by filling in a field -- see `PHILOSOPHY.md`'s "Choosing
//! between them". `SubagentSpec` (the port) keeps its own `mode` field; only
//! the tool surface changed.
//!
//! The small delegation tools `conway_steer`/`conway_await`/`conway_cancel`
//! live in `control.rs`; `conway_ask` lives in `ask.rs`. This file holds the
//! shared helpers those siblings need (`parse_agent_id`, `wait_for_result`,
//! `BudgetArg`, the config helpers, `TRUNCATION`) plus `ForkTool`/`SpawnTool`
//! themselves.
//!
//! Every fallible `ctx.subagents` call site maps its `SubagentError` straight
//! to `ToolError` via `.map_err(ToolError::from)` -- `conway-core`'s own
//! `From<SubagentError> for ToolError` (the ONE place that mapping is
//! implemented once) -- rather than through a crate-local forwarding
//! function (C2 deleted this module's own such function; its
//! flatten-everything-to-`Internal` policy predates `SubagentError` and is
//! superseded by that per-variant `From`).

use std::time::Duration;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use conway_core::agent::{
    AgentDefRef, AgentResult, Budget, ResultStatus, SubagentSpec, ToolSelector,
};
use conway_core::content::{
    ContentBlock, PermissionClass, ToolCall, ToolCategory, ToolSpec, TruncationPolicy,
};
use conway_core::error::ToolError;
use conway_core::ids::{AgentId, RoleAlias, ToolName};
use conway_core::log::SubagentMode;
use conway_core::ports::{PathArgs, PluginConfig, RenderKind, Tool, ToolCtx, ToolOutput};

use crate::common::{check_cancel, parse_args, text_output};

/// Declared by every subagent tool: an oversized result keeps its tail
/// (summary/facts/status), the part that must survive.
pub(super) const TRUNCATION: TruncationPolicy = TruncationPolicy::Tail { max_bytes: 16_384 };

const DEFAULT_MAX_STEPS: u32 = 40;
/// Default `Budget::deadline`, in seconds from now, absent an override.
const DEFAULT_DEADLINE_SECS: u64 = 600;
/// Wait-loop re-poll interval for `ctx.cancel`, which is poll-based (no
/// async `.cancelled()` future — `shell::bash` uses the same pattern).
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(20);

fn default_await() -> bool {
    true
}

/// A resource ceiling passed to a subagent call. Shared by `conway_fork`,
/// `conway_spawn`, and `conway_ask`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct BudgetArg {
    #[schemars(range(min = 1))]
    pub(super) max_steps: Option<u32>,
    #[schemars(range(min = 1))]
    pub(super) deadline_secs: Option<u64>,
    #[schemars(range(min = 1))]
    pub(super) max_tokens: Option<u32>,
    /// Ceiling on tool calls dispatched. Absent means no ceiling unless
    /// `subagent.max_tool_calls` is configured.
    #[schemars(range(min = 1))]
    pub(super) max_tool_calls: Option<u32>,
}

/// `conway_fork`'s arguments: the child inherits this agent's full context,
/// so `prompt` is a directive, not a task restated from nothing.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ForkArgs {
    /// Directive for a child that already holds this agent's full context.
    /// Not a task description -- the child needs no briefing, only what to
    /// do next.
    prompt: String,
    /// Agent definition name. Optional: omitting it means the child
    /// inherits this agent's own role/model.
    #[serde(default)]
    agent_def: Option<String>,
    /// Role alias for routing
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    budget: Option<BudgetArg>,
    /// Select the tools announced to the child by name
    #[serde(default)]
    tools: Option<Vec<String>>,
    /// JSON Schema the child's structured result must satisfy
    #[serde(default)]
    result_contract: Option<serde_json::Value>,
    /// False returns the agent_id immediately for fan-out
    #[serde(default = "default_await", rename = "await")]
    await_flag: bool,
}

/// `conway_spawn`'s arguments: the child starts with none of this agent's
/// context, so `prompt` must be a complete statement of the task.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SpawnArgs {
    /// Complete statement of the task for a child that starts with no
    /// context of its own. Everything the child needs to do the work must be
    /// in this prompt -- it inherits nothing from this agent's conversation.
    prompt: String,
    /// Agent definition name. Optional: omitting it means the child
    /// inherits this agent's own role/model.
    #[serde(default)]
    agent_def: Option<String>,
    /// Role alias for routing
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    budget: Option<BudgetArg>,
    /// Set the tools announced to the child by name
    #[serde(default)]
    tools: Option<Vec<String>>,
    /// JSON Schema the child's structured result must satisfy
    #[serde(default)]
    result_contract: Option<serde_json::Value>,
    /// False returns the agent_id immediately for fan-out
    #[serde(default = "default_await", rename = "await")]
    await_flag: bool,
}

/// The fields common to `ForkArgs` and `SpawnArgs` once `prompt`'s per-mode
/// meaning has been read -- an internal bundle, never itself `Deserialize`/
/// `JsonSchema`, so the two model-facing schemas stay fully independent
/// (each documents its own `prompt`) while `start_and_maybe_await` (the
/// downstream `SubagentSpec` construction, the child start, and the
/// await-or-return-immediately branch) is written once.
struct StartRequest {
    prompt: String,
    agent_def: Option<String>,
    role: Option<String>,
    budget: Option<BudgetArg>,
    tools: Option<Vec<String>>,
    result_contract: Option<serde_json::Value>,
    await_flag: bool,
}

impl From<ForkArgs> for StartRequest {
    fn from(args: ForkArgs) -> Self {
        Self {
            prompt: args.prompt,
            agent_def: args.agent_def,
            role: args.role,
            budget: args.budget,
            tools: args.tools,
            result_contract: args.result_contract,
            await_flag: args.await_flag,
        }
    }
}

impl From<SpawnArgs> for StartRequest {
    fn from(args: SpawnArgs) -> Self {
        Self {
            prompt: args.prompt,
            agent_def: args.agent_def,
            role: args.role,
            budget: args.budget,
            tools: args.tools,
            result_contract: args.result_contract,
            await_flag: args.await_flag,
        }
    }
}

pub(super) fn parse_agent_id(raw: &str) -> Result<AgentId, ToolError> {
    raw.parse::<AgentId>()
        .map_err(|e| ToolError::InvalidArguments {
            detail: format!("agent_id: {e}"),
        })
}

pub(super) fn config_u64(config: &PluginConfig, key: &str) -> Option<u64> {
    config.values.get(key).and_then(|v| v.as_u64())
}
pub(super) fn config_u32(config: &PluginConfig, key: &str) -> Option<u32> {
    config_u64(config, key).and_then(|v| u32::try_from(v).ok())
}

/// The largest `deadline_secs` accepted from a model/config-supplied budget.
/// Well under `chrono::Duration::seconds`' i64 nanosecond bound (~9.2e9s /
/// ~292y), and well over any sane deadline for a single subagent/ephemeral run
/// (~50y). Larger values are rejected as `InvalidArguments` (model-
/// supplied numeric args are range-checked, never panic) rather than
/// saturating -- the previous `i64::try_from(..).unwrap_or(i64::MAX)` saturated
/// straight into `Duration::seconds`' overflow panic.
pub(super) const MAX_DEADLINE_SECS: u64 = 1_576_800_000; // 50 * 365 * 86_400

/// Builds the `deadline` `DateTime` from a model/config-supplied `deadline_secs`,
/// range-checking, since model arguments are untrusted. Out-of-range ->
/// `ToolError::InvalidArguments`
/// (never a panic). Shared by `conway_fork`, `conway_spawn`, and `conway_ask`.
pub(super) fn deadline_from_secs(secs: u64) -> Result<chrono::DateTime<chrono::Utc>, ToolError> {
    if secs > MAX_DEADLINE_SECS {
        return Err(ToolError::InvalidArguments {
            detail: format!(
                "deadline_secs ({secs}) exceeds the maximum ({MAX_DEADLINE_SECS} seconds, ~50 years)"
            ),
        });
    }
    // `secs <= MAX_DEADLINE_SECS` (~1.58e9) fits i64 and is well under chrono's
    // nanosecond bound, so this cast and `Duration::seconds` cannot overflow.
    Ok(chrono::Utc::now() + chrono::Duration::seconds(secs as i64))
}

/// Precedence: the call's `budget` argument, then `ctx.config`'s
/// `subagent.*` keys, then the defaults (40 steps, 10-minute deadline).
fn resolve_budget(arg: Option<BudgetArg>, config: &PluginConfig) -> Result<Budget, ToolError> {
    let max_steps = arg
        .as_ref()
        .and_then(|b| b.max_steps)
        .or_else(|| config_u32(config, "subagent.max_steps"))
        .unwrap_or(DEFAULT_MAX_STEPS);
    let deadline_secs = arg
        .as_ref()
        .and_then(|b| b.deadline_secs)
        .or_else(|| config_u64(config, "subagent.deadline_secs"))
        .unwrap_or(DEFAULT_DEADLINE_SECS);
    let max_tokens = arg
        .as_ref()
        .and_then(|b| b.max_tokens)
        .or_else(|| config_u32(config, "subagent.max_tokens"));
    let max_tool_calls = arg
        .as_ref()
        .and_then(|b| b.max_tool_calls)
        .or_else(|| config_u32(config, "subagent.max_tool_calls"));

    Ok(Budget {
        max_steps,
        deadline: Some(deadline_from_secs(deadline_secs)?),
        max_tokens,
        max_tool_calls,
    })
}

/// `is_error` is `false` only for `ResultStatus::Completed`.
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

/// Waits for `child`'s result, honoring `ctx.cancel` cooperatively. On
/// cancellation, best-effort cancels the child (its own error is ignored)
/// and returns `Err(ToolError::Cancelled)`.
pub(super) async fn wait_for_result(
    ctx: &ToolCtx,
    child: AgentId,
) -> Result<ToolOutput, ToolError> {
    // One pinned future, re-polled across iterations: selecting on a fresh
    // `await_result` call each loop would drop and re-issue the in-flight
    // wait every poll tick — ~30k redundant host calls over a default
    // 10-minute await.
    let result_fut = ctx.subagents.await_result(child);
    tokio::pin!(result_fut);
    loop {
        tokio::select! {
            result = &mut result_fut => {
                return Ok(agent_result_output(&result.map_err(ToolError::from)?));
            }
            _ = tokio::time::sleep(CANCEL_POLL_INTERVAL) => {
                if ctx.cancel.is_cancelled() {
                    let _ = ctx.subagents.cancel(child, "parent tool cancelled".to_string()).await;
                    return Err(ToolError::Cancelled);
                }
            }
        }
    }
}

/// Builds the `SubagentSpec` for a fixed `mode`, starts the child, and
/// either returns its `agent_id` immediately (fan-out) or blocks for its
/// terminal result -- the logic `conway_fork` and `conway_spawn` share once
/// each has read its own `prompt` under its own meaning. `mode` is a
/// parameter here, never a model-supplied argument: each caller below passes
/// its own fixed `SubagentMode`, which is the entire point of the split (a
/// model reaches the choice by tool name, not by filling in a field).
async fn start_and_maybe_await(
    ctx: &ToolCtx,
    mode: SubagentMode,
    req: StartRequest,
) -> Result<ToolOutput, ToolError> {
    // the "agent_def required for spawn" rule is relaxed: a spawn
    // with no agent_def inherits this agent's own role/model.

    let result_contract = req
        .result_contract
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e: serde_json::Error| ToolError::InvalidArguments {
            detail: format!("result_contract: {e}"),
        })?;

    let spec = SubagentSpec {
        mode,
        prompt: req.prompt,
        agent_def: req.agent_def.map(AgentDefRef),
        role: req.role.map(RoleAlias::new),
        tools: req.tools.map(ToolSelector::Only),
        budget: resolve_budget(req.budget, &ctx.config)?,
        result_contract,
        // The model-invoked `conway_fork`/`conway_spawn` tools are always
        // the autonomous, one-shot fork/spawn primitives ("exactly two
        // subagent primitives") -- `keep_alive` is an opt-in only the
        // interactive-session facade paths (`conway`'s `SpawnSpec`/
        // `ForkSpec::keep_alive`, the TUI's bare `/spawn`/`/fork`) ever
        // set.
        keep_alive: false,
        ephemeral: false,
        // Not an `/ask` child (B5): the `conway_ask` tool (`ask.rs`)
        // stamps `AskOrigin::ToolAsk`, the TUI's modal `/ask` stamps
        // `ModalAsk`; `conway_fork`/`conway_spawn` are neither.
        ask_origin: None,
        // The model-invoked `conway_fork`/`conway_spawn` tools have no
        // `cwd` argument (C1 only adds `cwd` to the facade's `SpawnSpec`,
        // not either tool's own args schema) -- inherit the parent's,
        // unchanged.
        cwd: None,
        // (S3) Likewise, neither tool has a `root` argument (
        // embedder-only for this first slice) -- inherit the parent's
        // root, unchanged, for both fork and spawn.
        root: None,
        // The consumer tag is an embedder-only
        // correlation mechanism, same reasoning as `root` above: neither
        // model-invoked tool has a tag argument in its schema, so a
        // model-initiated fork/spawn never carries one.
        tag: None,
    };

    let child = ctx.subagents.start(spec).await.map_err(ToolError::from)?;

    if !req.await_flag {
        return Ok(text_output(
            serde_json::json!({ "agent_id": child }).to_string(),
            TRUNCATION,
        ));
    }
    wait_for_result(ctx, child).await
}

/// `conway_fork`: continues this agent's own context in a new child.
#[derive(Debug, Default)]
pub struct ForkTool;

impl ForkTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for ForkTool {
    /// No path arguments: `ForkArgs` carries a prompt, agent_def, role,
    /// budget, tool selection, and result contract -- no filesystem path.
    /// The model-invoked tool deliberately has no `cwd` argument (see
    /// `cwd: None` in `start_and_maybe_await`; only the facade's `ForkSpec`
    /// takes one), so there is genuinely nothing here for a root check to
    /// evaluate.
    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }

    /// `conway_fork` never overrides `render`, so its rendering is
    /// always the trait's own default JSON dump -- never a shell command.
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("conway_fork"),
            description: "Fork this agent: start a new child continuing from this agent's full context, plus a directive for what it should do next. `agent_def` is optional: name one to set the child's system prompt/tools/model, or omit it to inherit this agent's role and model.".into(),
            schema: schemars::schema_for!(ForkArgs),
            category: ToolCategory::Delegate,
            // Starting a child grants it the capability to itself perform
            // arbitrary tool calls, transitively — the same risk class as
            // `bash`, one hop removed.
            permission: PermissionClass::Dangerous,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        check_cancel(&ctx)?;
        let args: ForkArgs = parse_args(&call)?;
        start_and_maybe_await(&ctx, SubagentMode::Fork, args.into()).await
    }
}

/// `conway_spawn`: starts a new child with none of this agent's context.
#[derive(Debug, Default)]
pub struct SpawnTool;

impl SpawnTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for SpawnTool {
    /// No path arguments: `SpawnArgs` carries a prompt, agent_def, role,
    /// budget, tool selection, and result contract -- no filesystem path.
    /// The model-invoked tool deliberately has no `cwd` argument (see
    /// `cwd: None` in `start_and_maybe_await`; only the facade's `SpawnSpec`
    /// takes one), so there is genuinely nothing here for a root check to
    /// evaluate.
    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }

    /// `conway_spawn` never overrides `render`, so its rendering is
    /// always the trait's own default JSON dump -- never a shell command.
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("conway_spawn"),
            description: "Spawn a new, independent child agent with none of this agent's context, plus a complete statement of its task. `agent_def` is optional: name one to set the child's system prompt/tools/model, or omit it to inherit this agent's role and model.".into(),
            schema: schemars::schema_for!(SpawnArgs),
            category: ToolCategory::Delegate,
            // Starting a child grants it the capability to itself perform
            // arbitrary tool calls, transitively — the same risk class as
            // `bash`, one hop removed.
            permission: PermissionClass::Dangerous,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        check_cancel(&ctx)?;
        let args: SpawnArgs = parse_args(&call)?;
        start_and_maybe_await(&ctx, SubagentMode::Spawn, args.into()).await
    }
}
