//! `conway_subagent`: a pure wrapper over `ToolCtx::subagents` (WI-066).
//! Zero delegation logic: argument parsing, one `ToolCtx::subagents` call
//! (the same `SubagentHost` port the developer API's `fork`/`spawn` calls),
//! and result shaping.
//!
//! The small delegation tools `conway_steer`/`conway_await`/`conway_cancel`
//! live in `control.rs`; `conway_ask` lives in `ask.rs`. This file holds the
//! shared helpers those siblings need (`host_error`, `parse_agent_id`,
//! `wait_for_result`, `BudgetArg`, the config helpers, `TRUNCATION`) plus
//! `SubagentTool` itself.

use std::time::Duration;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use conway_core::agent::{AgentDefRef, AgentResult, Budget, ResultStatus, SubagentSpec, ToolSelector};
use conway_core::content::{
    ContentBlock, PermissionClass, ToolCall, ToolCategory, ToolSpec, TruncationPolicy,
};
use conway_core::error::{RuntimeError, ToolError};
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

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ModeArg {
    Fork,
    Spawn,
}

/// A resource ceiling passed to a subagent call. Shared by `conway_subagent`
/// and `conway_ask`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct BudgetArg {
    #[schemars(range(min = 1))]
    pub(super) max_steps: Option<u32>,
    #[schemars(range(min = 1))]
    pub(super) deadline_secs: Option<u64>,
    #[schemars(range(min = 1))]
    pub(super) max_tokens: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SubagentArgs {
    /// "fork": new agent continuing from this agent's context, plus the
    /// prompt. "spawn": new independent agent, fresh context.
    mode: ModeArg,
    /// Fork: the fork directive. Spawn: the whole task.
    prompt: String,
    /// Agent definition name. Optional for both modes: omitting it on a
    /// spawn means the child inherits this agent's own role/model.
    #[serde(default)]
    agent_def: Option<String>,
    /// Role alias for routing
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    budget: Option<BudgetArg>,
    /// Restrict the child's tool set to these names
    #[serde(default)]
    tools: Option<Vec<String>>,
    /// JSON Schema the child's structured result must satisfy
    #[serde(default)]
    result_contract: Option<serde_json::Value>,
    /// false returns the agent_id immediately for fan-out
    #[serde(default = "default_await", rename = "await")]
    await_flag: bool,
}

/// Maps a `SubagentHost` failure (host/infrastructure, never
/// model-recoverable) to a `ToolError`. `conway-core` has no `Host` variant
/// (WI-061 assumption 3 named one that doesn't exist); `Internal` is the
/// closest fit, vs. `InvalidArguments` (a caller mistake).
pub(super) fn host_error(err: RuntimeError) -> ToolError {
    ToolError::Internal {
        detail: err.to_string(),
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
/// (~50y). Larger values are rejected as `InvalidArguments` (P-10: model-
/// supplied numeric args are range-checked, never panic) rather than
/// saturating -- the previous `i64::try_from(..).unwrap_or(i64::MAX)` saturated
/// straight into `Duration::seconds`' overflow panic (cycle-3 review SIG-1;
/// the earlier "saturate rather than wrap" note was a cycle-1 fix for a
/// silent-wrap bug that traded one defect for another).
pub(super) const MAX_DEADLINE_SECS: u64 = 1_576_800_000; // 50 * 365 * 86_400

/// Builds the `deadline` `DateTime` from a model/config-supplied `deadline_secs`,
/// range-checking per P-10. Out-of-range -> `ToolError::InvalidArguments`
/// (never a panic). Shared by `conway_subagent` and `conway_ask`.
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

    Ok(Budget {
        max_steps,
        deadline: Some(deadline_from_secs(deadline_secs)?),
        max_tokens,
        max_tool_calls: None,
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
pub(super) async fn wait_for_result(ctx: &ToolCtx, child: AgentId) -> Result<ToolOutput, ToolError> {
    // One pinned future, re-polled across iterations: selecting on a fresh
    // `await_result` call each loop would drop and re-issue the in-flight
    // wait every poll tick — ~30k redundant host calls over a default
    // 10-minute await (cycle-1 review S1).
    let result_fut = ctx.subagents.await_result(child);
    tokio::pin!(result_fut);
    loop {
        tokio::select! {
            result = &mut result_fut => {
                return Ok(agent_result_output(&result.map_err(host_error)?));
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

/// `conway_subagent`: forks or spawns a child agent, optionally blocking for
/// its terminal result.
#[derive(Debug, Default)]
pub struct SubagentTool;

impl SubagentTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for SubagentTool {
    /// No path arguments: `SubagentArgs` carries a mode, prompt, agent_def,
    /// role, budget, tool selection, and result contract -- no filesystem
    /// path. The model-invoked tool deliberately has no `cwd` argument (see
    /// `cwd: None` in `invoke`; only the facade's `SpawnSpec` takes one), so
    /// there is genuinely nothing here for a root check to evaluate.
    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }

    /// `conway_subagent` never overrides `render`, so its rendering is
    /// always the trait's own default JSON dump -- never a shell command.
    /// Board item 01KYT3NSWRHMPEAXVXRJ73KDYR.
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("conway_subagent"),
            description: "Fork or spawn a child agent. `agent_def` is optional for `spawn`: name one to set the child's system prompt/tools/model, or omit it to inherit this agent's role and model.".into(),
            schema: schemars::schema_for!(SubagentArgs),
            category: ToolCategory::Delegate,
            // Starting a child grants it the capability to itself perform arbitrary tool
            // calls, transitively — the same risk class as `bash`, one hop removed.
            permission: PermissionClass::Dangerous,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        check_cancel(&ctx)?;
        let args: SubagentArgs = parse_args(&call)?;

        let mode = match args.mode {
            ModeArg::Fork => SubagentMode::Fork,
            ModeArg::Spawn => SubagentMode::Spawn,
        };
        // WI-099's "agent_def required for spawn" rule is relaxed: a spawn
        // with no agent_def inherits this agent's own role/model.

        let result_contract = args
            .result_contract
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e: serde_json::Error| ToolError::InvalidArguments {
                detail: format!("result_contract: {e}"),
            })?;

        let spec = SubagentSpec {
            cache_hint: matches!(mode, SubagentMode::Fork),
            mode,
            prompt: args.prompt,
            agent_def: args.agent_def.map(AgentDefRef),
            role: args.role.map(RoleAlias::new),
            tools: args.tools.map(ToolSelector::Only),
            budget: resolve_budget(args.budget, &ctx.config)?,
            result_contract,
            await_result: args.await_flag,
            // The model-invoked `conway_subagent` tool is always the
            // autonomous, one-shot fork/spawn primitive (P-1: "exactly two
            // subagent primitives") -- `keep_alive` is an opt-in only the
            // interactive-session facade paths (`conway`'s `SpawnSpec`/
            // `ForkSpec::keep_alive`, the TUI's bare `/spawn`/`/fork`) ever
            // set.
            keep_alive: false,
            ephemeral: false,
            // Not an `/ask` child (B5): the `conway_ask` tool (`ask.rs`)
            // stamps `AskOrigin::ToolAsk`, the TUI's modal `/ask` stamps
            // `ModalAsk`; `conway_subagent` is neither.
            ask_origin: None,
            // The model-invoked `conway_subagent` tool has no `cwd` argument
            // (C1 only adds `cwd` to the facade's `SpawnSpec`, not this
            // tool's own args schema) -- inherit the parent's, unchanged.
            cwd: None,
            // (S3) Likewise, `conway_subagent` has no `root` argument (GP-04:
            // embedder-only for this first slice) -- inherit the parent's
            // root, unchanged, for both fork and spawn.
            root: None,
        };

        let child = ctx
            .subagents
            .start(ctx.agent_id, spec)
            .await
            .map_err(host_error)?;

        if !args.await_flag {
            return Ok(text_output(
                serde_json::json!({ "agent_id": child }).to_string(),
                TRUNCATION,
            ));
        }
        wait_for_result(&ctx, child).await
    }
}