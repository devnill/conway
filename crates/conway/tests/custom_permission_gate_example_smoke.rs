//! Smoke test for the `custom_permission_gate` example's facade flow,
//! mirroring `example_smoke.rs`'s own precedent: the example itself is only
//! compile-checked by `cargo build --examples`; this test exercises the
//! same public-facade path at runtime, and additionally asserts the ONE
//! thing the example can only demonstrate by printing (that the third-party
//! gate was genuinely consulted, not merely constructed): the gate records
//! the exact `PermissionRequest` it decided.
//!
//! Builds via `ConwayBuilder::from_parts` directly rather than
//! `ConwayBuilder::discover()` -- see `discover_getting_started_example_
//! smoke.rs`'s own module doc for why an in-process test cannot call
//! `discover()` at all (`config_isolation_guard.rs`). The example's own
//! `isolate_ambient_config_for_this_example` reaches the same "discover()
//! finds nothing" outcome a different way (isolating the whole PROCESS,
//! legitimate for a standalone `main`, not for a test sharing a process with
//! every other test in this binary) -- `from_parts` with the bare-default
//! config sidesteps needing either mechanism here.

mod support;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use conway::backend::{BackendId, ContentBlock, GenerateResponse, ModelId, StopReason, ToolCall};
use conway::config::ConwayConfig;
use conway::{
    ConwayBuilder, ModelRef, PermissionDecision, PermissionGate, PermissionRequest, SessionSpec,
    ToolName,
};
use conway_testkit::{FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};

const T: Duration = Duration::from_secs(5);

/// The identical fixture the example's own `LoggingReportOnlyGate` uses,
/// plus recording -- so this test can assert it was genuinely consulted,
/// not just that `build()`/`prompt()` happened to succeed.
struct RecordingReportOnlyGate {
    requests: std::sync::Mutex<Vec<PermissionRequest>>,
}

impl RecordingReportOnlyGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            requests: std::sync::Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl PermissionGate for RecordingReportOnlyGate {
    async fn check(&self, req: PermissionRequest) -> PermissionDecision {
        let decision = if req.tool.as_str() == "report" {
            PermissionDecision::AllowOnce
        } else {
            PermissionDecision::Deny {
                reason: "this example's gate only allows the report tool".to_string(),
            }
        };
        self.requests.lock().unwrap().push(req);
        decision
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

fn done() -> GenerateResponse {
    GenerateResponse {
        content: vec![ContentBlock::Text {
            text: "done".to_string(),
        }],
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Default::default(),
    }
}

#[tokio::test]
async fn custom_permission_gate_example_flow_actually_consults_the_gate() {
    let cwd = support::unique_temp_dir("custom-permission-gate");
    let outcome = conway::config::load(conway::config::LoadOptions {
        cwd,
        explicit_path: None,
        env: support::isolated_env(),
        cli_overrides: conway::config::CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .expect("load with no user/project layer must still succeed via built-in defaults");
    let config: ConwayConfig = outcome.config;

    let backend = Arc::new(ScriptedBackend::new(vec![
        ScriptedTurn::Respond(report_call()),
        ScriptedTurn::Respond(done()),
    ]));
    let route = ModelRef {
        backend: BackendId::new("scripted"),
        model: ModelId::new("scripted-model"),
    };
    let gate = RecordingReportOnlyGate::new();

    let conway = ConwayBuilder::from_parts(config)
        .with_backend(backend)
        .with_router(Arc::new(FakeRouter::single(route)))
        .with_session_store(Arc::new(FakeStore::new()))
        .with_permission_gate(gate.clone())
        .build()
        .expect("build should succeed");

    let session = tokio::time::timeout(T, conway.new_session(SessionSpec::default()))
        .await
        .expect("new_session must not hang")
        .expect("new_session should succeed");
    let turn = tokio::time::timeout(T, session.prompt("please file a report"))
        .await
        .expect("prompt must not hang")
        .expect("prompt should succeed");
    let result = tokio::time::timeout(T, turn.result())
        .await
        .expect("result must not hang")
        .expect("result should succeed");

    assert_eq!(
        result.summary, "decided by a third-party PermissionGate",
        "the turn must complete via the report tool the custom gate allowed"
    );

    let requests = gate.requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "the custom gate must have been consulted exactly once, for the report call"
    );
    assert_eq!(requests[0].tool.as_str(), "report");
}
