//! `conway_ask`: run a prompt in an ephemeral fork of this agent, returning
//! the child's full reply text. A pure wrapper over
//! `ToolCtx::subagents.ask` (P-1: `ask` composes `SubagentHost::ask`, it is
//! NOT a third primitive — fork+await-text, no mode parameter, GP-02).
//!
//! Fork-only (v1): the child inherits this agent's full context, agent_def,
//! role, and tool set (fork semantics), so this tool takes only `prompt` and
//! an optional `budget` — no `mode`/`result_contract`/`tools`/`role`/
//! `agent_def` args. Returns the full reply text (GP-01) so the model can
//! compose a fresh spawn out-of-band, keeping the curation reasoning out of
//! this agent's context window.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use conway_core::agent::{Budget, ResultStatus, SubagentSpec};
use conway_core::content::{
    Artifact, ArtifactKind, ContentBlock, PermissionClass, ToolCall, ToolCategory, ToolSpec,
};
use conway_core::error::ToolError;
use conway_core::ids::ToolName;
use conway_core::log::SubagentMode;
use conway_core::ports::{PluginConfig, Tool, ToolCtx, ToolOutput};

use crate::common::{check_cancel, parse_args};
use super::tools::{config_u32, config_u64, deadline_from_secs, host_error, BudgetArg, TRUNCATION};

/// `conway_ask` defaults: tighter than `conway_subagent` — curation is a
/// bounded drafting step, not an open-ended delegation.
const DEFAULT_ASK_MAX_STEPS: u32 = 20;
const DEFAULT_ASK_DEADLINE_SECS: u64 = 120;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct AskArgs {
    /// The prompt to run in the ephemeral fork. The child inherits this
    /// agent's full context, agent_def, role, and tool set (fork semantics).
    prompt: String,
    #[serde(default)]
    budget: Option<BudgetArg>,
}

/// Precedence: the call's `budget` argument, then `ctx.config`'s `ask.*`
/// keys, then the defaults (20 steps, 2-minute deadline).
fn resolve_ask_budget(arg: Option<BudgetArg>, config: &PluginConfig) -> Result<Budget, ToolError> {
    let max_steps = arg
        .as_ref()
        .and_then(|b| b.max_steps)
        .or_else(|| config_u32(config, "ask.max_steps"))
        .unwrap_or(DEFAULT_ASK_MAX_STEPS);
    let deadline_secs = arg
        .as_ref()
        .and_then(|b| b.deadline_secs)
        .or_else(|| config_u64(config, "ask.deadline_secs"))
        .unwrap_or(DEFAULT_ASK_DEADLINE_SECS);
    let max_tokens = arg
        .as_ref()
        .and_then(|b| b.max_tokens)
        .or_else(|| config_u32(config, "ask.max_tokens"));

    Ok(Budget {
        max_steps,
        // P-10: range-check the model-supplied deadline rather than saturate
        // (the prior `unwrap_or(i64::MAX)` saturated into a `Duration::seconds`
        // overflow panic -- cycle-3 review SIG-1). Out-of-range -> a typed
        // `InvalidArguments` error via `deadline_from_secs`.
        deadline: Some(deadline_from_secs(deadline_secs)?),
        max_tokens,
        max_tool_calls: None,
    })
}

/// `conway_ask`: run a prompt in an ephemeral fork, return the child's full
/// reply text.
#[derive(Debug, Default)]
pub struct AskTool;

impl AskTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for AskTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("conway_ask"),
            description: "Run a prompt in an ephemeral fork of this agent and return the child's full reply text. Use this to draft or curate context for a fresh spawn out-of-band, keeping the curation reasoning out of this agent's context window.".into(),
            schema: schemars::schema_for!(AskArgs),
            category: ToolCategory::Delegate,
            // The child inherits the parent's full tool set, so arbitrary
            // tool calls are one hop away — same risk class as
            // `conway_subagent` and `bash`.
            permission: PermissionClass::Dangerous,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        check_cancel(&ctx)?;
        let args: AskArgs = parse_args(&call)?;

        let spec = SubagentSpec {
            mode: SubagentMode::Fork,
            prompt: args.prompt,
            agent_def: None,
            role: None,
            tools: None,
            budget: resolve_ask_budget(args.budget, &ctx.config)?,
            cache_hint: true,
            result_contract: None,
            await_result: true,
            keep_alive: false,
            ephemeral: true,
        };

        let outcome = ctx
            .subagents
            .ask(ctx.agent_id, spec)
            .await
            .map_err(host_error)?;

        // P-2: the persisted ToolOutput names the child session via an
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