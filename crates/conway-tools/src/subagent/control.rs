//! `conway_steer`, `conway_await`, `conway_cancel`: the small delegation
//! control tools. Pure wrappers over `ToolCtx::subagents`, sharing
//! helpers with `tools.rs`.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use conway_core::agent::CancelMode;
use conway_core::content::{PermissionClass, ToolCall, ToolCategory, ToolSpec};
use conway_core::error::ToolError;
use conway_core::ids::ToolName;
use conway_core::ports::{PathArgs, RenderKind, Tool, ToolCtx, ToolOutput};

use super::tools::{parse_agent_id, wait_for_result, TRUNCATION};
use crate::common::{check_cancel, parse_args, text_output};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct SteerArgs {
    agent_id: String,
    text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct AwaitArgs {
    agent_id: String,
}

/// The model-facing shape of [`CancelMode`] -- a local, `JsonSchema`-deriving enum the tool
/// layer owns, mapped onto the domain type at `invoke` time rather than
/// deriving `JsonSchema` on `conway_core::agent::CancelMode` itself.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum CancelModeArg {
    /// Stops now; does not wait for the current turn to finish, and
    /// propagates to the whole subtree. The default.
    #[default]
    Immediate,
    /// Lets the target finish its in-flight turn, then stops at the next
    /// turn boundary. Stops only the named agent -- descendants are
    /// unaffected. Does not reach an idle `keep_alive` agent parked at the
    /// resume gate between turns; use `immediate` for that case.
    Graceful,
}

impl From<CancelModeArg> for CancelMode {
    fn from(arg: CancelModeArg) -> Self {
        match arg {
            CancelModeArg::Immediate => CancelMode::Immediate,
            CancelModeArg::Graceful => CancelMode::Graceful,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CancelArgs {
    agent_id: String,
    /// Why `agent_id` is being cancelled. Reaches `agent_id`'s OWN terminal
    /// result (`AgentResult.status`'s `Cancelled { reason }`) on BOTH modes
    /// -- but in `immediate` mode, which propagates to the whole subtree
    /// structurally, only `agent_id` itself carries this reason; a
    /// descendant swept up by the same cancellation was never itself named
    /// here, so its own result falls back to a generic reason instead of
    /// misattributing this one to it. Defaults to "cancelled by parent
    /// agent" when omitted.
    #[serde(default)]
    reason: Option<String>,
    /// `immediate` (default) stops now and propagates to the whole subtree.
    /// `graceful` lets the target finish its in-flight turn, then stops
    /// only the named agent -- it does not itself cancel descendants, and
    /// it cannot reach an idle `keep_alive` agent parked at the resume gate
    /// between turns (that agent is not mid-turn to finish; use `immediate`
    /// instead).
    #[serde(default)]
    mode: CancelModeArg,
}

/// `conway_steer`: sends a text message to a running child.
#[derive(Debug, Default)]
pub struct SteerTool;

impl SteerTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for SteerTool {
    /// No path arguments: steering carries an agent id and a message.
    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }

    /// `conway_steer` never overrides `render`, so its rendering is always
    /// the trait's own default JSON dump -- never a shell command. Board
    /// item.
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("conway_steer"),
            description: "Send `text` to a running child agent, to redirect it or hand it \
                information it did not have when it started. `agent_id` must be the id a \
                prior `conway_fork`/`conway_spawn` call returned -- there is no other way \
                to name a child. The message is delivered at the child's next turn \
                boundary: it is queued, not injected immediately, so it never interrupts a \
                tool call already in flight (a child mid-`bash` finishes that call first). \
                Prefer `conway_cancel` instead when the child should stop entirely rather \
                than keep going on new information."
                .into(),
            schema: schemars::schema_for!(SteerArgs),
            category: ToolCategory::Delegate,
            permission: PermissionClass::RequiresApproval,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        check_cancel(&ctx)?;
        let args: SteerArgs = parse_args(&call)?;
        let target = parse_agent_id(&args.agent_id)?;
        ctx.subagents
            .steer(target, args.text)
            .await
            .map_err(ToolError::from)?;
        Ok(text_output(format!("steered agent {target}"), TRUNCATION))
    }
}

/// `conway_await`: blocks for a child's terminal result.
#[derive(Debug, Default)]
pub struct AwaitTool;

impl AwaitTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for AwaitTool {
    /// No path arguments: awaiting carries an agent id (and timing only).
    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }

    /// `conway_await` never overrides `render`, so its rendering is always
    /// the trait's own default JSON dump -- never a shell command. Board
    /// item.
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("conway_await"),
            description: "Blocks this turn until the named child agent finishes, then \
                returns its terminal result: summary, any typed facts and artifacts it \
                reported, any structured output, and its terminal status (completed, \
                failed, cancelled, or budget-exceeded). If the child already finished \
                before this call, it returns immediately with that same result -- waiting \
                never blocks longer than the child actually ran. To fan out several \
                children, keep working (or fire more forks/spawns) instead of awaiting \
                each one right away; await each only once you actually need its result."
                .into(),
            schema: schemars::schema_for!(AwaitArgs),
            category: ToolCategory::Delegate,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        check_cancel(&ctx)?;
        let args: AwaitArgs = parse_args(&call)?;
        let target = parse_agent_id(&args.agent_id)?;
        wait_for_result(&ctx, target).await
    }
}

/// `conway_cancel`: cancels a running child.
#[derive(Debug, Default)]
pub struct CancelTool;

impl CancelTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for CancelTool {
    /// No path arguments: cancelling carries an agent id and a mode.
    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }

    /// `conway_cancel` never overrides `render`, so its rendering is always
    /// the trait's own default JSON dump -- never a shell command. Board
    /// item.
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("conway_cancel"),
            description: "Cancel a running child agent. `mode` defaults to `immediate`: \
                stops now and propagates to the whole subtree. `graceful` instead lets \
                the target finish its in-flight turn, then stops only the named agent \
                (descendants are unaffected); it cannot reach an idle keep_alive agent \
                waiting between turns, since that agent has no in-flight turn to finish \
                -- use `immediate` for that case."
                .into(),
            schema: schemars::schema_for!(CancelArgs),
            category: ToolCategory::Delegate,
            permission: PermissionClass::RequiresApproval,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        check_cancel(&ctx)?;
        let args: CancelArgs = parse_args(&call)?;
        let target = parse_agent_id(&args.agent_id)?;
        let reason = args
            .reason
            .unwrap_or_else(|| "cancelled by parent agent".to_string());
        let mode: CancelMode = args.mode.into();
        ctx.subagents
            .cancel_with(target, reason.clone(), mode)
            .await
            .map_err(ToolError::from)?;
        Ok(text_output(
            format!("cancelled agent {target}: {reason}"),
            TRUNCATION,
        ))
    }
}

#[cfg(test)]
mod description_tests {
    use super::*;

    /// GP-14 (declaration honesty): `conway_steer`'s description must say
    /// the message lands at the child's next turn boundary, never mid-tool-
    /// call, and must point at `conway_cancel` as the alternative when the
    /// child should stop rather than continue. Both facts are verified
    /// against the runtime (`AgentLoop::run_inner`'s `drain_inbox` call at
    /// the top of its turn loop, before any tool dispatch) before being
    /// promised here.
    #[test]
    fn steer_description_names_turn_boundary_and_cancel_alternative() {
        let description = SteerTool::new().spec().description;
        assert!(
            description.contains("turn boundary"),
            "description must state delivery happens at the next turn boundary: {description:?}"
        );
        assert!(
            description.contains("conway_cancel"),
            "description must point at conway_cancel as the alternative: {description:?}"
        );
    }

    /// GP-14: `conway_await`'s description must say it blocks, and must
    /// name every part of the terminal result it hands back (facts,
    /// artifacts, structured output) -- not just the summary line the old
    /// one-sentence description implied. "Returns immediately for an
    /// already-finished child" is verified against `AgentTree::await_result`
    /// (its `watch::Receiver::borrow()` check before ever awaiting
    /// `changed()`).
    #[test]
    fn await_description_names_blocks_and_result_shape() {
        let description = AwaitTool::new().spec().description;
        assert!(
            description.contains("blocks") || description.contains("waits"),
            "description must say it blocks/waits: {description:?}"
        );
        assert!(
            description.contains("facts"),
            "description must mention facts: {description:?}"
        );
        assert!(
            description.contains("artifacts"),
            "description must mention artifacts: {description:?}"
        );
        assert!(
            description.contains("structured"),
            "description must mention structured output: {description:?}"
        );
    }
}
