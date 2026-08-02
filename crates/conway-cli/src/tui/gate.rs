//! The TUI's in-process [`conway::PermissionGate`] implementation (WI-114).
//!
//! `TuiGate::check` never decides anything itself: it forwards every
//! [`conway::PermissionRequest`] over an `mpsc` channel as a [`PendingPrompt`]
//! and awaits the app loop's `oneshot` reply. This is what lets the runtime's
//! tool-call thread block (per `PermissionGate`'s own doc: "the gate may
//! block indefinitely") while the ratatui app loop renders a prompt and
//! waits for a keypress on its own task.
//!
//! **Wiring note (disclosed):** `conway_runtime::runtime::Runtime` bakes its
//! `PermissionGate` in at construction (`RuntimeDeps.gate`) and exposes no
//! later swap point, so a `TuiGate` must be handed to
//! `ConwayBuilder::with_permission_gate` *before* `ConwayBuilder::build()`
//! returns -- after which `tui::run(cli, conway)` only ever receives the
//! already-built `Conway`. Threading the matching [`GateReceiver`] in from
//! `main.rs` (so the app loop can service the same channel the live
//! `Runtime` calls into) is exactly the change `ConwayBuilder`'s own module
//! doc flags as missing ("the CLI or a future item should likely add a way
//! to supply a prompt handler") and `crates/conway-cli/tests/cli_surface.rs`'s
//! `MINIMAL_CONFIG` comment attributes to "WI-112/114" -- see `main.rs`'s
//! `build_conway`/`main` for the resulting (disclosed) widening of this
//! item's file scope beyond `src/tui/`.

use conway::{PermissionDecision, PermissionGate, PermissionRequest};
use tokio::sync::{mpsc, oneshot};

/// One permission request awaiting the app loop's decision.
pub struct PendingPrompt {
    pub request: PermissionRequest,
    reply: oneshot::Sender<PermissionDecision>,
}

impl PendingPrompt {
    /// Sends `decision` back to the blocked `TuiGate::check` call. A closed
    /// receiver (the gate side already gave up -- e.g. the runtime dropped
    /// the tool call) is not an error here; there is nothing left to notify.
    pub fn resolve(self, decision: PermissionDecision) {
        let _ = self.reply.send(decision);
    }

    #[cfg(test)]
    pub(crate) fn reply_sender(self) -> oneshot::Sender<PermissionDecision> {
        self.reply
    }

    /// Test-only constructor (01KYB0F7V65QAMZWWYH8K7DWDC): `reply` is
    /// private outside this module by design -- only `TuiGate::check` ever
    /// builds a real one, tied to the live gate channel. Render/input tests
    /// elsewhere under `tui/` that need a `Mode::AwaitingPermission`
    /// `AppState` with no live gate at all (e.g. the permission overlay's
    /// own render tests) go through this instead of reaching into a private
    /// field. The paired `oneshot::Receiver` is returned so a test can
    /// assert on what gets sent if it cares to; most just drop it.
    #[cfg(test)]
    pub(crate) fn new_for_test(
        request: PermissionRequest,
    ) -> (PendingPrompt, oneshot::Receiver<PermissionDecision>) {
        let (reply, rx) = oneshot::channel();
        (PendingPrompt { request, reply }, rx)
    }
}

/// The app loop's half of a [`TuiGate`] channel -- selected on alongside the
/// event and input streams (module notes' three-task architecture).
pub type GateReceiver = mpsc::UnboundedReceiver<PendingPrompt>;

/// Implements [`conway::PermissionGate`] by relaying every request to the
/// app loop and blocking on its reply. Cheap to `Clone` (an `Arc`-backed
/// channel sender under the hood).
#[derive(Clone)]
pub struct TuiGate {
    tx: mpsc::UnboundedSender<PendingPrompt>,
}

impl TuiGate {
    /// Builds a linked `(gate, receiver)` pair. The gate half is `Arc`-boxed
    /// and handed to `ConwayBuilder::with_permission_gate`; the receiver
    /// half is driven by the app loop.
    pub fn channel() -> (TuiGate, GateReceiver) {
        let (tx, rx) = mpsc::unbounded_channel();
        (TuiGate { tx }, rx)
    }
}

#[async_trait::async_trait]
impl PermissionGate for TuiGate {
    async fn check(&self, req: PermissionRequest) -> PermissionDecision {
        let (reply_tx, reply_rx) = oneshot::channel();
        let prompt = PendingPrompt {
            request: req,
            reply: reply_tx,
        };
        // The receiver is gone (app loop already exited) -- fail closed
        // rather than hang the tool call forever.
        if self.tx.send(prompt).is_err() {
            return PermissionDecision::Deny {
                reason: "cancelled".to_string(),
            };
        }
        // A dropped `reply_tx` (app loop exited mid-prompt, or panicked)
        // must also fail closed, never hang -- `PermissionGate`'s own doc:
        // "Gate cancellation ... surfaces as `PermissionDecision::Deny {
        // reason: 'cancelled' }`, never as a hang".
        reply_rx.await.unwrap_or(PermissionDecision::Deny {
            reason: "cancelled".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use conway::{AgentId, PermissionScope, ToolCategory, ToolName};

    use super::*;

    fn sample_request() -> PermissionRequest {
        PermissionRequest {
            agent_id: AgentId::new(),
            agent_path: Vec::new(),
            tool: ToolName::new("bash"),
            category: ToolCategory::Execute,
            arguments: serde_json::json!({"command": "ls"}),
            rendered: "bash: ls".to_string(),
            call_id: "tc_1".to_string(),
            render_kind: conway::RenderKind::ShellCommand,
        }
    }

    #[tokio::test]
    async fn allow_once_round_trips() {
        let (gate, mut rx) = TuiGate::channel();
        let check = tokio::spawn(async move { gate.check(sample_request()).await });

        let pending = rx.recv().await.expect("pending prompt");
        pending.resolve(PermissionDecision::AllowOnce);

        let decision = check.await.expect("check task");
        assert_eq!(decision, PermissionDecision::AllowOnce);
    }

    #[tokio::test]
    async fn allow_always_session_round_trips() {
        let (gate, mut rx) = TuiGate::channel();
        let check = tokio::spawn(async move { gate.check(sample_request()).await });

        let pending = rx.recv().await.expect("pending prompt");
        pending.resolve(PermissionDecision::AllowAlways {
            scope: PermissionScope::Session,
        });

        let decision = check.await.expect("check task");
        assert_eq!(
            decision,
            PermissionDecision::AllowAlways {
                scope: PermissionScope::Session
            }
        );
    }

    #[tokio::test]
    async fn deny_round_trips() {
        let (gate, mut rx) = TuiGate::channel();
        let check = tokio::spawn(async move { gate.check(sample_request()).await });

        let pending = rx.recv().await.expect("pending prompt");
        pending.resolve(PermissionDecision::Deny {
            reason: "user denied".to_string(),
        });

        let decision = check.await.expect("check task");
        assert_eq!(
            decision,
            PermissionDecision::Deny {
                reason: "user denied".to_string()
            }
        );
    }

    #[tokio::test]
    async fn dropped_reply_channel_denies_as_cancelled() {
        let (gate, mut rx) = TuiGate::channel();
        let check = tokio::spawn(async move { gate.check(sample_request()).await });

        let pending = rx.recv().await.expect("pending prompt");
        // Drop the reply sender without resolving -- simulates the app loop
        // exiting mid-prompt.
        drop(pending.reply_sender());

        let decision = check.await.expect("check task");
        assert_eq!(
            decision,
            PermissionDecision::Deny {
                reason: "cancelled".to_string()
            }
        );
    }

    #[tokio::test]
    async fn dropped_receiver_denies_as_cancelled() {
        let (gate, rx) = TuiGate::channel();
        drop(rx);

        let decision = gate.check(sample_request()).await;
        assert_eq!(
            decision,
            PermissionDecision::Deny {
                reason: "cancelled".to_string()
            }
        );
    }
}
