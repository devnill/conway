//! The `PermissionGate` port (architecture §4.3).

use async_trait::async_trait;

use crate::agent::{PermissionDecision, PermissionRequest};

/// Approves or denies one tool call. Always implemented by the consumer
/// (CLI/IDE/embedder) — there is no built-in privileged bypass; GP-08: no
/// worktree/sandbox logic anywhere in the harness.
///
/// The gate may block indefinitely: the runtime holds the tool call pending
/// and emits `Event::PermissionRequested` while it waits. Gate cancellation
/// (e.g. the process shutting down) surfaces as
/// `PermissionDecision::Deny { reason: "cancelled" }`, never as a hang or a
/// silently dropped call (architecture §8).
#[async_trait]
pub trait PermissionGate: Send + Sync + 'static {
    async fn check(&self, req: PermissionRequest) -> PermissionDecision;
}
