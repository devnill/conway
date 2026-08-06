//! `conway_ask`: run a prompt in an ephemeral fork of this agent, returning
//! the child's full reply text. A pure wrapper over
//! `ToolCtx::subagents.ask` (P-1: `ask` composes the underlying `ask`
//! primitive `SubagentHandle` wraps, it is NOT a third primitive --
//! fork+await-text, no mode parameter, GP-02).
//!
//! Fork-only (v1): the child inherits this agent's full context and
//! effective role (fork semantics; role via the runtime's parent-role
//! fallback, WI-136, `conway_runtime::subagent`). It does **not** inherit
//! this agent's `agent_def`: `invoke` below always passes `agent_def: None`
//! on the `SubagentSpec`, so a def-declared `result_contract`, tools
//! selector, system prompt, and model pin never apply to a `conway_ask`
//! child, even when the parent itself was spawned from a def. This tool
//! takes only `prompt`, an optional `budget`, and an optional `tools`
//! narrowing list — no `mode`/`result_contract`/`role`/`agent_def` args.
//! Returns the full reply text (GP-01) so the model can compose a fresh
//! spawn out-of-band, keeping the curation reasoning out of this agent's
//! context window.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use conway_core::agent::{Budget, ResultStatus, SubagentSpec, ToolSelector};
use conway_core::content::{
    Artifact, ArtifactKind, ContentBlock, PermissionClass, ToolCall, ToolCategory, ToolSpec,
};
use conway_core::error::ToolError;
use conway_core::ids::ToolName;
use conway_core::log::SubagentMode;
use conway_core::ports::{PathArgs, PluginConfig, RenderKind, Tool, ToolCtx, ToolOutput};

use super::tools::{config_u32, config_u64, deadline_from_secs, BudgetArg, TRUNCATION};
use crate::common::{check_cancel, parse_args};

/// `conway_ask` defaults: tighter than `conway_subagent` — curation is a
/// bounded drafting step, not an open-ended delegation.
const DEFAULT_ASK_MAX_STEPS: u32 = 20;
const DEFAULT_ASK_DEADLINE_SECS: u64 = 120;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct AskArgs {
    /// The prompt to run in the ephemeral fork. The child inherits this
    /// agent's full context and effective role (fork semantics), but NOT
    /// this agent's agent_def -- a parent def's result contract, tools
    /// selector, system prompt, and model pin do not apply to the child.
    prompt: String,
    #[serde(default)]
    budget: Option<BudgetArg>,
    /// Restrict the ephemeral child's tool set to these names
    /// (`ToolSelector::Only`, the same mapping `conway_subagent`'s `tools`
    /// arg uses). Narrowing-only (P-10): the runtime resolves the selector
    /// against the registered tools the child would otherwise inherit in
    /// full, so this can restrict but never widen the child's set. Optional
    /// (C-04): absent means the child inherits the full set, as before.
    #[serde(default)]
    tools: Option<Vec<String>>,
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
    /// No path arguments: `AskArgs` is a prompt, an optional budget, and an
    /// optional tool selection. The forked child inherits the parent's cwd
    /// (`cwd: None` in `invoke`), so no path crosses this tool's boundary.
    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }

    /// `conway_ask` never overrides `render`, so its rendering is always
    /// the trait's own default JSON dump -- never a shell command. Board
    /// item 01KYT3NSWRHMPEAXVXRJ73KDYR.
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("conway_ask"),
            description: "Run a prompt in an ephemeral fork of this agent and return the child's full reply text. Use this to draft or curate context for a fresh spawn out-of-band, keeping the curation reasoning out of this agent's context window. Pass `tools` to restrict the child's tool set to the named tools (narrowing-only — it can never grant tools the child would not otherwise inherit).".into(),
            schema: schemars::schema_for!(AskArgs),
            category: ToolCategory::Delegate,
            // The child inherits AT MOST the caller's requested tool set
            // (an absent `tools` arg means the full inherited set), so
            // arbitrary tool calls are one hop away — same risk class as
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
            tools: args.tools.map(ToolSelector::Only),
            budget: resolve_ask_budget(args.budget, &ctx.config)?,
            result_contract: None,
            keep_alive: false,
            ephemeral: true,
            // B5: tag this child as TOOL-ask residue -- DISTINCT from the
            // TUI's modal `/ask` (`AskOrigin::ModalAsk`, set by `conway`'s
            // `SessionHandle::ask`). LOAD-BEARING: the TUI's startup
            // crash-residue sweep purges only `ModalAsk`-tagged ephemeral
            // sessions; this child's transcript is referenced by the
            // `EphemeralSessionRef` artifact below, so sweeping it would
            // leave that artifact dangling (see
            // `conway_core::log::AskOrigin`'s own doc).
            ask_origin: Some(conway_core::log::AskOrigin::ToolAsk),
            // A fork inherits the caller's entire context (C1's rationale) --
            // inherit its cwd too.
            cwd: None,
            // (S3) A fork always inherits the forker's root, never overrides
            // it -- same rationale as `cwd` above.
            root: None,
        };

        let outcome = ctx.subagents.ask(spec).await.map_err(ToolError::from)?;

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
