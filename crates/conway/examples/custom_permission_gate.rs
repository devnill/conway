//! An embedded agent guarded by a THIRD PARTY's own [`PermissionGate`]
//! implementation -- the extension mechanism a host application uses when
//! it wants to decide "may this tool call proceed?" with its own policy
//! (a UI dialog, an allow-list keyed off something conway's own three
//! built-in gates don't know about, an audit log), rather than one of the
//! `permissions.mode` presets [`conway::ConwayBuilder::discover`] can select
//! for you.
//!
//! ```console
//! cargo run -p conway --example custom_permission_gate
//! ```
//!
//! There is exactly one extension mechanism for this
//! (`conway::ConwayBuilder::with_permission_gate`) -- a third party's gate
//! is installed on the identical surface a built-in preset
//! ([`conway::presets`]) would be, no privileged path either way. This
//! example's [`LoggingReportOnlyGate`] is deliberately simple (allow the
//! `report` tool, deny everything else, print every request it is asked to
//! decide) so the POINT -- your own type, your own policy, no
//! `conway`-internal trait to reimplement beyond `PermissionGate` itself --
//! stays visible.
//!
//! Runs fully offline via `conway_testkit::ScriptedBackend`, which plays
//! back one scripted turn: a `report` tool call. This is the one built-in
//! tool [`LoggingReportOnlyGate`] allows, so the turn completes with the
//! report's own summary as the agent's terminal result -- proving the
//! custom gate was genuinely consulted and not merely constructed, the same
//! discriminating standard `crates/conway/tests/builder.rs`'s own gate tests
//! hold themselves to.

use std::sync::Arc;

use async_trait::async_trait;
use conway::backend::{BackendId, GenerateResponse, ModelId, StopReason, ToolCall};
use conway::{
    ConwayBuilder, ModelRef, PermissionDecision, PermissionGate, PermissionRequest, SessionSpec,
    ToolName,
};
// See `discover_getting_started.rs`'s own comment on this same import: a
// normal, unconditional dev-dependency, not the feature-gated
// `conway::testkit` re-export a third party would use instead.
use conway_testkit::{FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};

/// See `discover_getting_started.rs`'s own doc/copy of this same helper:
/// isolates `ConwayBuilder::discover()` below from whatever happens to be
/// configured on the machine running this example, purely for
/// reproducibility. A real host application does not do this.
fn isolate_ambient_config_for_this_example() {
    let scratch = std::env::temp_dir().join(format!(
        "conway-custom-permission-gate-example-{}",
        std::process::id()
    ));
    let config_dir = scratch.join("config_dir-config-home");
    let cwd = scratch.join("cwd");
    std::fs::create_dir_all(&config_dir).expect("create scratch CONWAY_CONFIG_DIR");
    std::fs::create_dir_all(&cwd).expect("create scratch cwd");
    std::env::set_var("CONWAY_CONFIG_DIR", &config_dir);
    std::env::set_current_dir(&cwd).expect("set scratch cwd");
}

/// A minimal, real, third-party [`PermissionGate`]: allows the `report`
/// tool, denies everything else, and prints every request it decides --
/// standing in for whatever a host's own policy actually is (a UI prompt, an
/// allow-list against the host's own data, an audit sink).
struct LoggingReportOnlyGate;

#[async_trait]
impl PermissionGate for LoggingReportOnlyGate {
    async fn check(&self, req: PermissionRequest) -> PermissionDecision {
        println!(
            "custom gate: asked to decide tool={:?} rendered={:?}",
            req.tool, req.rendered
        );
        if req.tool.as_str() == "report" {
            PermissionDecision::AllowOnce
        } else {
            PermissionDecision::Deny {
                reason: "this example's gate only allows the report tool".to_string(),
            }
        }
    }
}

fn report_call() -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: "call_1".to_string(),
            name: ToolName::new("report"),
            arguments: serde_json::json!({
                "summary": "decided by a third-party PermissionGate",
            }),
        }],
        stop: StopReason::ToolUse,
        usage: Default::default(),
    }
}

/// The agent loop drives ONE MORE round trip after a tool call completes
/// (`report` included) before reaching a terminal result -- this text-only
/// response, with no further tool call, is what lets that second round
/// naturally finish the turn (`conway-runtime`'s own `report_only_agent.rs`
/// test scripts the identical two-turn shape for the same reason).
fn done() -> GenerateResponse {
    GenerateResponse {
        content: vec![conway::backend::ContentBlock::Text {
            text: "done".to_string(),
        }],
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Default::default(),
    }
}

#[tokio::main]
async fn main() -> conway::Result<()> {
    isolate_ambient_config_for_this_example();

    let backend = Arc::new(ScriptedBackend::new(vec![
        ScriptedTurn::Respond(report_call()),
        ScriptedTurn::Respond(done()),
    ]));
    let route = ModelRef {
        backend: BackendId::new("scripted"),
        model: ModelId::new("scripted-model"),
    };

    let conway = ConwayBuilder::discover()?
        .with_backend(backend)
        .with_router(Arc::new(FakeRouter::single(route)))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(Arc::new(LoggingReportOnlyGate))
        .build()?;

    let session = conway.new_session(SessionSpec::default()).await?;
    let turn = session.prompt("please file a report").await?;
    let result = turn.result().await?;
    println!("agent result -> {}", result.summary);

    Ok(())
}
