//! The `SubagentHost` port: the cycle-breaker (architecture §4.6).
//!
//! The developer API (`SessionHandle::fork`/`spawn`) and the
//! `conway_subagent` tool both call this same trait (decision 2, mechanically
//! enforced): the tool is a thin wrapper with no privileged access.

use async_trait::async_trait;

use crate::agent::{AgentResult, AgentTreeSnapshot, SubagentSpec};
use crate::error::RuntimeError;
use crate::ids::AgentId;

#[async_trait]
pub trait SubagentHost: Send + Sync + 'static {
    async fn start(&self, parent: AgentId, spec: SubagentSpec) -> Result<AgentId, RuntimeError>;

    async fn steer(&self, target: AgentId, text: String) -> Result<(), RuntimeError>;

    /// Always terminates: the supervisor synthesizes a result on budget
    /// exhaustion, cancellation, or task panic. A parent's pending
    /// `conway_subagent` tool call can never hang on this call.
    async fn await_result(&self, target: AgentId) -> Result<AgentResult, RuntimeError>;

    async fn cancel(&self, target: AgentId, reason: String) -> Result<(), RuntimeError>;

    fn tree(&self) -> AgentTreeSnapshot;
}
