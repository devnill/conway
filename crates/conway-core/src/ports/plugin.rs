//! The `Plugin`/`Tool` ports (architecture §4.2) and the `CancellationToken`
//! used to interrupt an in-flight tool call.
//!
//! **There is exactly one extension mechanism (GP-03).** Built-in
//! read/write/edit/bash and the subagent tool are `Plugin` implementations
//! registered by default in `ConwayBuilder`; nothing about them is
//! privileged. MVP plugins are in-process `Arc<dyn Plugin>` (Tension T-8).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::content::{Artifact, ContentBlock, ToolCall, ToolSpec, TruncationPolicy};
use crate::error::{PluginError, ToolError};
use crate::ids::{AgentId, SessionId, ToolName};
use crate::ports::{EventSinkHandle, SubagentHost};

/// A source of tools: a plugin declares its identity, the tools it provides,
/// and an optional one-time initialization hook.
pub trait Plugin: Send + Sync + 'static {
    /// This plugin's static identity: id, semver, provided tools, required
    /// host capabilities.
    fn manifest(&self) -> PluginManifest;

    fn tools(&self) -> Vec<Arc<dyn Tool>>;

    /// Called once at startup. The default no-op is correct for plugins that
    /// need no setup. No default method on this trait may perform I/O; a
    /// concrete `on_init` implementation may, but that is the implementer's
    /// responsibility, not this trait's contract.
    fn on_init(&self, _ctx: &PluginInitCtx) -> Result<(), PluginError> {
        Ok(())
    }
}

/// One invocable tool: aligned with ACP's tool-call categories (`ToolCategory`
/// in `content.rs`) for free future compatibility, zero present cost.
#[async_trait]
pub trait Tool: Send + Sync + 'static {
    /// This tool's name, description, JSON Schema, category, and permission
    /// class.
    fn spec(&self) -> ToolSpec;

    /// Invoke the tool.
    ///
    /// PRE: `call.arguments` has already been validated against
    /// `self.spec().schema`. PRE: permission has already been granted for
    /// `(agent, tool, arguments)`. POST: honors `ctx.cancel`; returns within
    /// the runtime's deadline or `Err(ToolError::Cancelled)`. POST: declares
    /// a `TruncationPolicy` on the returned `ToolOutput`; the runtime applies
    /// it and records the truncation in the log (architecture §8).
    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError>;
}

/// A plugin's static identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub version: String,
    pub tools: Vec<ToolName>,
    pub required_host_caps: Vec<String>,
}

/// Context passed to `Plugin::on_init`.
#[derive(Clone, Debug)]
pub struct PluginInitCtx {
    pub config: Arc<PluginConfig>,
    pub cwd: PathBuf,
}

/// A plugin's untyped configuration values, as loaded and handed down by the
/// facade. This crate does no config loading itself.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginConfig {
    pub values: serde_json::Map<String, serde_json::Value>,
}

/// Per-invocation context handed to `Tool::invoke`.
///
/// `Clone` (every field is an `Arc`, `Copy`, or otherwise cheap to clone).
/// **Not** `Serialize` — it holds trait objects (`events`, `subagents`).
/// This is the known T-8 limitation: `ToolCall` and `ToolOutput` are fully
/// serializable, so a future subprocess/RPC plugin transport only needs an
/// RPC-shaped form of `ToolCtx`, not this one.
#[derive(Clone)]
pub struct ToolCtx {
    pub agent_id: AgentId,
    pub session_id: SessionId,
    pub cwd: PathBuf,
    pub cancel: CancellationToken,
    /// Progress reporting; see [`EventSinkHandle`].
    pub events: EventSinkHandle,
    /// The cycle-breaker for the fork/spawn tool: the same trait object the
    /// developer API (`SessionHandle::fork`/`spawn`) calls.
    pub subagents: Arc<dyn SubagentHost>,
    pub config: Arc<PluginConfig>,
}

impl std::fmt::Debug for ToolCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolCtx")
            .field("agent_id", &self.agent_id)
            .field("session_id", &self.session_id)
            .field("cwd", &self.cwd)
            .field("cancel", &self.cancel)
            .field("events", &"<dyn EventSink>")
            .field("subagents", &"<dyn SubagentHost>")
            .field("config", &self.config)
            .finish()
    }
}

/// The outcome of a tool invocation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    pub blocks: Vec<ContentBlock>,
    pub is_error: bool,
    /// The tool declares how it wants oversized output handled; the runtime
    /// enforces the policy and records the truncation in the log.
    pub truncation: TruncationPolicy,
    pub artifacts: Vec<Artifact>,
}

/// A minimal, serialization-free cancellation flag.
///
/// `conway-core` cannot depend on `tokio`, so this is a small
/// `Arc<AtomicBool>`-based token rather than `tokio_util::sync::
/// CancellationToken`. Downstream crates that need an async cancellation
/// *await* (rather than a poll of `is_cancelled`) bridge this token to
/// `tokio_util`'s token themselves — see `conway-runtime`.
///
/// `child()` produces a token that observes both its own cancellation and
/// every ancestor's, to arbitrary depth: internally each child holds a
/// shared handle to its parent (rather than the parent's raw flag alone), so
/// cancelling a root token cancels every descendant transitively.
#[derive(Clone, Default)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
    parent: Option<Arc<CancellationToken>>,
}

impl std::fmt::Debug for CancellationToken {
    // Manual impl: a derived Debug would walk (and print) the entire ancestor
    // chain, which is unbounded in a deep agent tree.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .field("has_parent", &self.parent.is_some())
            .finish()
    }
}

impl CancellationToken {
    /// A fresh, uncancelled, parentless token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks this token cancelled. Every token derived from it via
    /// [`Self::child`] (to any depth) observes this immediately.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// `true` if this token, or any ancestor it was derived from, has been
    /// cancelled. Iterative: walks the ancestor chain without recursion, so
    /// arbitrarily deep agent trees cannot overflow the stack.
    pub fn is_cancelled(&self) -> bool {
        let mut current = self;
        loop {
            if current.flag.load(Ordering::SeqCst) {
                return true;
            }
            match current.parent.as_deref() {
                Some(parent) => current = parent,
                None => return false,
            }
        }
    }

    /// A new token that is independently cancellable but also observes this
    /// token's (and its ancestors') cancellation.
    pub fn child(&self) -> CancellationToken {
        CancellationToken {
            flag: Arc::new(AtomicBool::new(false)),
            parent: Some(Arc::new(self.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_token_is_not_cancelled() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
    }

    #[test]
    fn cancel_is_observed() {
        let token = CancellationToken::new();
        token.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn child_observes_parent_cancellation() {
        let parent = CancellationToken::new();
        let child = parent.child();
        assert!(!child.is_cancelled());
        parent.cancel();
        assert!(child.is_cancelled());
    }

    #[test]
    fn child_can_be_cancelled_independently_of_parent() {
        let parent = CancellationToken::new();
        let child = parent.child();
        child.cancel();
        assert!(child.is_cancelled());
        assert!(!parent.is_cancelled());
    }

    #[test]
    fn grandchild_observes_root_cancellation() {
        let root = CancellationToken::new();
        let child = root.child();
        let grandchild = child.child();
        assert!(!grandchild.is_cancelled());
        root.cancel();
        assert!(grandchild.is_cancelled());
    }

    #[test]
    fn plugin_manifest_round_trips() {
        let manifest = PluginManifest {
            id: "builtin.fs".into(),
            version: "0.1.0".into(),
            tools: vec![ToolName::new("read"), ToolName::new("write")],
            required_host_caps: vec![],
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let back: PluginManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest, back);
    }

    #[test]
    fn tool_output_round_trips() {
        let out = ToolOutput {
            blocks: vec![ContentBlock::Text { text: "ok".into() }],
            is_error: false,
            truncation: TruncationPolicy::None,
            artifacts: vec![],
        };
        let json = serde_json::to_string(&out).unwrap();
        let back: ToolOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(out, back);
    }
}
