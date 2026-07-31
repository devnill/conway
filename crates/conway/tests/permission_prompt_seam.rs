//! Real-path acceptance tests for board item 01KYTP1D3XWEZPW4AKPH54FNB3
//! ("the extension design's flagship `prompt` effect was never evaluated
//! anywhere in `PermissionBroker::decide`").
//!
//! Mirrors `permission_pattern_seam.rs`/`permission_deny_laundering_seam.rs`
//! for the identical reason stated in both: a hand-written
//! `AuthorizedCall`/`PermissionRequest` fixture (as `conway-runtime`'s own
//! `tests/permission_broker.rs` uses) proves the BROKER's internal
//! machinery works, but says nothing about whether a rule installed through
//! the real `Conway` facade, matched against the real `bash` `Tool`'s own
//! `render` output, actually reaches the real gate through a real agent
//! turn. This file drives that stack end to end.
//!
//! Before this item, `must_reach_gate` was set EXCLUSIVELY by
//! `PermissionBroker::check_root`, so a `prompt` rule -- the extension
//! design's OWN flagship worked example
//! (`{"categories":["edit","delete"],"then":"prompt"}`,
//! `.design/extension-architecture.md` §2020) -- had nothing evaluating
//! it anywhere. The two tests below are the item's own named failures,
//! reproduced end to end:
//!
//! - **Failure A** (`a_prompt_rule_forces_the_gate_under_auto_allow`):
//!   nothing could force the gate under `AutoAllow`, so the rule did
//!   nothing in the one mode a guardrail plugin matters most.
//! - **Failure B**
//!   (`a_prompt_rule_forces_the_gate_over_a_matching_allow_pattern_grant`):
//!   `pattern_allows` resolves a matching call before any prompt rule could
//!   be consulted, so an operator's own pattern grant silently defeated a
//!   plugin's narrower `prompt` rule for the identical call.
//!
//! Per this item's own GP-14 corollary, each test asserts on the persisted
//! `ToolResult` text (a distinctive `Deny { reason }` the REAL gate is
//! scripted to return), not merely on the gate's call count: a silently
//! bypassed call and a genuinely refused one can otherwise both leave a
//! misleading trail, so the assertion pins that the refusal text -- which
//! only the real gate, actually consulted, could have produced -- shows up
//! in the tool's own output, not that `curl` simply failed for an unrelated
//! reason (e.g. DNS resolution against a `.invalid` host).
#![cfg(feature = "builtin-tools")]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, LimitsConfig, ModelsConfig, PermissionsConfig,
    RoleEntry, RoutingSection, SessionConfig, TuiSection,
};
use conway::{Conway, ConwayBuilder, PatternRule, SessionSpec};
use conway_core::agent::{PermissionDecision, PermissionRequest, PermissionScope};
use conway_core::content::{ContentBlock, ToolResult};
use conway_core::fakes::{FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias};
use conway_core::log::LogRecord;
use conway_core::permission_mode::PermissionMode;
use conway_core::ports::{Backend, GenerateResponse, PermissionGate};

fn fake_router() -> Arc<dyn conway_core::ports::Router> {
    Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }))
}

fn text_response(text: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        tool_calls: vec![],
        stop: conway_core::content::StopReason::EndTurn,
        usage: conway_core::content::Usage::default(),
    }
}

/// A single scripted `bash` call, followed immediately by a final text
/// response once the tool step completes.
fn bash_call_response(command: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![conway_core::content::ToolCall {
            call_id: "call_1".to_string(),
            name: conway_core::ids::ToolName::new("bash"),
            arguments: serde_json::json!({ "command": command }),
        }],
        stop: conway_core::content::StopReason::ToolUse,
        usage: conway_core::content::Usage::default(),
    }
}

fn base_config() -> ConwayConfig {
    let mut roles = BTreeMap::new();
    roles.insert(
        "default".to_string(),
        RoleEntry {
            chain: vec![],
            headroom_tokens: None,
        },
    );
    ConwayConfig {
        default_role: RoleAlias::new("default"),
        cwd: std::path::PathBuf::from("."),
        session: SessionConfig::default(),
        limits: LimitsConfig::default(),
        permissions: PermissionsConfig::default(),
        backends: BTreeMap::new(),
        routing: RoutingSection::default(),
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig::default(),
        tui: TuiSection::default(),
    }
}

/// Records every `PermissionRequest` it receives and always answers with a
/// fixed `decision` -- see `permission_deny_laundering_seam.rs`'s identical
/// fixture for the reasoning.
struct RecordingGate {
    decision: PermissionDecision,
    requests: Mutex<Vec<PermissionRequest>>,
}

impl RecordingGate {
    fn new(decision: PermissionDecision) -> Arc<Self> {
        Arc::new(Self {
            decision,
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<PermissionRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl PermissionGate for RecordingGate {
    async fn check(&self, req: PermissionRequest) -> PermissionDecision {
        self.requests.lock().unwrap().push(req);
        self.decision.clone()
    }
}

fn blocks_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn build_conway(backend: Arc<dyn Backend>, gate: Arc<dyn PermissionGate>) -> Conway {
    let store = Arc::new(FakeStore::new());
    ConwayBuilder::from_parts(base_config())
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build()
        .expect("build should succeed with the real builtin `bash` tool registered")
}

/// The LAST `ToolResultRecord` in `handle`'s own (root-agent) transcript.
fn last_tool_result(records: &[LogRecord]) -> &ToolResult {
    records
        .iter()
        .rev()
        .find_map(|r| match r {
            LogRecord::ToolResultRecord { result, .. } => Some(result),
            _ => None,
        })
        .expect("expected a ToolResultRecord in the transcript")
}

/// Runs one `bash` call end to end (the command itself is already baked
/// into `conway`'s scripted backend) and returns every request the gate
/// actually saw plus the tool call's own persisted result.
async fn run_one_bash_call(
    conway: &Conway,
    gate: Arc<RecordingGate>,
) -> (Vec<PermissionRequest>, ToolResult) {
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("do the thing").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");

    let root = handle.root();
    let records = handle
        .transcript(root)
        .await
        .expect("transcript should resolve");
    let result = last_tool_result(&records).clone();

    (gate.requests(), result)
}

/// **Failure A.** `AutoAllow` mode has no allow path a `prompt` rule could
/// previously interrupt: `PermissionBroker::decide`'s `AutoAllow` branch
/// returns `Allow` unconditionally with zero gate calls whenever
/// `must_reach_gate` is false, and before this item nothing but
/// `check_root` could ever set it. This proves a `prompt` rule now forces
/// the gate even in this mode, and that the outcome is the REAL gate's
/// decision (a distinctive `Deny`), not an auto-approval.
#[tokio::test]
async fn a_prompt_rule_forces_the_gate_under_auto_allow() {
    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "prompt rule forced a real ask; the operator refused".into(),
    });
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(bash_call_response("curl http://example.invalid")),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(backend, gate.clone() as Arc<dyn PermissionGate>);
    conway.set_permission_mode(PermissionMode::AutoAllow);
    conway.grant_prompt_pattern(
        PatternRule::parse("bash:curl").expect("valid rule"),
        PathBuf::from("/repo/.conway/permissions.json"),
    );

    let (requests, result) = run_one_bash_call(&conway, gate).await;

    assert_eq!(
        requests.len(),
        1,
        "a matching prompt rule must force gate.check even under AutoAllow -- \
         before this item's fix, AutoAllow's own branch would have returned \
         Allow with zero gate calls, and this rule would have done nothing: \
         {requests:?}"
    );
    assert!(
        result.is_error,
        "the call must be refused by the REAL gate's decision, not silently \
         allowed by AutoAllow"
    );
    let text = blocks_text(&result.blocks);
    assert!(
        text.contains("prompt rule forced a real ask"),
        "the refusal must come from the scripted gate actually being \
         consulted, not `curl` failing on its own for an unrelated reason \
         (e.g. DNS): {text:?}"
    );
}

/// **Failure B.** An operator's own pattern ALLOW grant for `bash:curl`
/// would normally resolve this call with zero gate calls
/// (`PermissionBroker::pattern_allows`, checked before this item's fix
/// added anything that could outrank it). A `prompt` rule matching the
/// identical call must force the gate anyway -- narrowing an existing
/// grant is always permitted, even when the grant and the narrowing rule
/// name the exact same pattern.
#[tokio::test]
async fn a_prompt_rule_forces_the_gate_over_a_matching_allow_pattern_grant() {
    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "prompt rule forced a real ask despite the matching allow grant".into(),
    });
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(bash_call_response("curl http://example.invalid")),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(backend, gate.clone() as Arc<dyn PermissionGate>);
    // Prompt is the broker's default mode -- not set explicitly, so this
    // also pins that the default behaves correctly.
    conway.grant_permission_pattern(
        PatternRule::parse("bash:curl").expect("valid rule"),
        PermissionScope::Session,
        AgentId::new(),
    );
    conway.grant_prompt_pattern(
        PatternRule::parse("bash:curl").expect("valid rule"),
        PathBuf::from("/repo/.conway/permissions.json"),
    );

    let (requests, result) = run_one_bash_call(&conway, gate).await;

    assert_eq!(
        requests.len(),
        1,
        "a matching prompt rule must force gate.check even over a matching \
         pattern ALLOW grant for the identical call: {requests:?}"
    );
    assert!(
        result.is_error,
        "the call must be refused by the REAL gate's decision, not silently \
         allowed by the pattern grant"
    );
    let text = blocks_text(&result.blocks);
    assert!(
        text.contains("despite the matching allow grant"),
        "the refusal must come from the scripted gate actually being \
         consulted, not `curl` failing on its own for an unrelated reason: \
         {text:?}"
    );
}

/// Deny still beats prompt, through the real seam: a call matching BOTH a
/// `deny` rule and a `prompt` rule is refused outright, without ever
/// reaching the gate -- an escalated ask is not what a `deny` violation
/// degrades to.
#[tokio::test]
async fn a_deny_rule_still_overrides_a_matching_prompt_rule_through_the_real_seam() {
    // Scripted to ALLOW: if deny were somehow defeated and the call fell
    // through to prompt-forced-gate instead, this would let it through,
    // flipping the outcome from "refused" to "ran" -- so seeing it refused
    // proves deny (not an agreeable gate) did the refusing.
    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(bash_call_response("curl http://example.invalid")),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(backend, gate.clone() as Arc<dyn PermissionGate>);
    conway.set_permission_mode(PermissionMode::AutoAllow);
    conway.grant_prompt_pattern(
        PatternRule::parse("bash:curl").expect("valid rule"),
        PathBuf::from("/repo/.conway/permissions.json"),
    );
    conway.grant_deny_pattern(
        PatternRule::parse("bash:curl").expect("valid rule"),
        PathBuf::from("/repo/.conway/permissions.json"),
    );

    let (requests, result) = run_one_bash_call(&conway, gate).await;

    assert!(
        requests.is_empty(),
        "deny must short-circuit before the gate (and before the prompt \
         step) is ever reached: {requests:?}"
    );
    assert!(result.is_error, "the call must be refused, not run");
    let text = blocks_text(&result.blocks);
    assert!(
        text.contains("denied by a `deny` rule"),
        "the refusal must come from the deny rule itself: {text:?}"
    );
}
