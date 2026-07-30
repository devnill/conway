//! `conway_steer`, `conway_await`, `conway_cancel`: the small delegation
//! control tools. Pure wrappers over `ToolCtx::subagents` (WI-066), sharing
//! helpers with `tools.rs`.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use conway_core::content::{PermissionClass, ToolCall, ToolCategory, ToolSpec};
use conway_core::error::ToolError;
use conway_core::ids::ToolName;
use conway_core::ports::{PathArgs, Tool, ToolCtx, ToolOutput};

use crate::common::{check_cancel, parse_args, text_output};
use super::tools::{host_error, parse_agent_id, wait_for_result, TRUNCATION};

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

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CancelArgs {
    agent_id: String,
    #[serde(default)]
    reason: Option<String>,
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
        check_cancel(&ctx)?;
        let args: SteerArgs = parse_args(&call)?;
        let target = parse_agent_id(&args.agent_id)?;
        ctx.subagents
            .steer(target, args.text)
            .await
            .map_err(host_error)?;
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
        check_cancel(&ctx)?;
        let args: CancelArgs = parse_args(&call)?;
        let target = parse_agent_id(&args.agent_id)?;
        let reason = args
            .reason
            .unwrap_or_else(|| "cancelled by parent agent".to_string());
        ctx.subagents
            .cancel(target, reason.clone())
            .await
            .map_err(host_error)?;
        Ok(text_output(
            format!("cancelled agent {target}: {reason}"),
            TRUNCATION,
        ))
    }
}