//! Regression test for ("deny rules
//! are evadable with one leading tab -- sanitizer laundering").
//!
//! Mirrors `permission_pattern_seam.rs`/`root_containment_seam.rs` for the
//! identical reason stated in both of those files: a hand-written
//! `AuthorizedCall`/`PermissionRequest` fixture proves nothing about
//! whether the REAL pipeline launders evidence a deny rule depends on. The
//! bug this item fixes lived entirely in that seam --
//! `BashTool::render` -> `render_call` -> `sanitize_rendered` ->
//! `AuthorizedCall::rendered` -> `PatternRule::matches_deny` -- and a
//! unit test on `matches_deny` alone (which every existing hand-copied
//! sanitizer fixture already covers) cannot prove the REAL
//! `sanitize_rendered` still produces the exact byte the fix depends on
//! recognizing. This file drives the genuine `Conway` (the `builtin-tools`
//! feature's real `bash` `Tool`) through a real agent turn so the
//! `rendered` text a `deny` rule is matched against is whatever the real
//! sanitizer actually produced, not a string a test author typed by hand.
//!
//! `command` in both tests below carries a LEADING TAB before `curl` --
//! invisible to every POSIX shell (the command would run identically to
//! `curl http://example.invalid`), but `sanitize_rendered` rewrites it to
//! `U+FFFD`, fusing it onto `curl` into a single token a naive
//! `prefix_matches` cannot align with the `bash:curl` deny rule's `curl`
//! prefix. Before this item's fix, that fusion made `deny_matches` miss
//! entirely.
#![cfg(feature = "builtin-tools")]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::{Conway, ConwayBuilder, PatternRule, PluginSelection, SessionSpec};
use conway_core::agent::{PermissionDecision, PermissionRequest};
use conway_core::content::{ContentBlock, ToolResult};
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias};
use conway_core::log::LogRecord;
use conway_core::permission_mode::PermissionMode;
use conway_core::ports::{Backend, GenerateResponse, PermissionGate};
use conway_testkit::{text_response, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};

fn fake_router() -> Arc<dyn conway_core::ports::Router> {
    Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }))
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
            ..Default::default()
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
        tools: ToolsConfig::default(),
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

/// Records every `PermissionRequest` it receives and always answers with a
/// fixed `decision`. Scripted to `AllowOnce` in the tests below so that,
/// if the deny match is defeated by laundering, the outcome flips to
/// actually running the (never-really-executed, since it targets a bogus
/// host) `curl` call instead of being refused -- proving the assertion is
/// exercising the real bug, not merely agreeing with a gate that would
/// have refused anyway.
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
        // (bash ships on by default and cannot be declined):
        // this file drives the REAL `bash` tool end to end, so it must now
        // opt in explicitly -- the facade's own default excludes it.
        .with_builtin_plugins(PluginSelection::All)
        .build()
        .expect("build should succeed with the real builtin `bash` tool registered")
}

/// The LAST `ToolResultRecord` in `handle`'s own (root-agent) transcript --
/// i.e. the outcome of the one `bash` call each test below dispatches.
fn tool_result(records: &[LogRecord]) -> &ToolResult {
    records
        .iter()
        .rev()
        .find_map(|r| match r {
            LogRecord::ToolResultRecord { result, .. } => Some(result),
            _ => None,
        })
        .expect("expected a ToolResultRecord in the transcript")
}

/// Runs one `bash` call, carrying a LEADING TAB before `curl`, end to end
/// against a `deny bash:curl` rule, and returns every request the gate
/// actually saw plus the tool call's own persisted result.
///
/// The result is essential, not merely belt-and-suspenders: in `AutoAllow`
/// mode a MISSED deny check and a CORRECT deny refusal are BOTH zero gate
/// calls (`AutoAllow`'s whole point is skipping the gate) -- so
/// `gate.requests().is_empty()` alone cannot distinguish "refused" from
/// "silently allowed and actually ran". `result.is_error` is what proves
/// which one happened.
async fn run_laundered_curl_call(
    conway: &Conway,
    gate: Arc<RecordingGate>,
) -> (Vec<PermissionRequest>, ToolResult) {
    conway.grant_deny_pattern(
        PatternRule::parse("bash:curl").expect("valid rule"),
        PathBuf::from("/repo/.conway/permissions.json"),
    );

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
    let result = tool_result(&records).clone();

    (gate.requests(), result)
}

/// **The headline regression, Prompt mode.** Before this fix, a leading
/// tab defeated `deny bash:curl`, so the call silently fell through to the
/// operator's gate -- a hard deny DEGRADING TO A PROMPT. This proves the
/// deny rule refuses the laundered command WITHOUT ever consulting the
/// gate, through the real `sanitize_rendered` seam.
#[tokio::test]
async fn a_leading_tab_still_denies_curl_under_prompt_mode_through_the_real_seam() {
    // Scripted to ALLOW: if the deny match is defeated by laundering, the
    // call reaches the gate and this decision lets it through, flipping
    // the outcome from "refused" to "ran". Seeing zero gate calls is what
    // proves the deny rule -- not an agreeable gate -- did the refusing.
    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(bash_call_response("\tcurl http://example.invalid")),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(backend, gate.clone() as Arc<dyn PermissionGate>);
    // Prompt is the broker's default mode -- not set explicitly, so this
    // test also pins that the default behaves correctly.

    let (requests, result) = run_laundered_curl_call(&conway, gate).await;

    assert!(
        requests.is_empty(),
        "a leading tab must not launder a `curl` call past `deny bash:curl` -- \
         the deny rule must refuse it before the operator's gate is ever \
         consulted, through the real render/sanitize seam: {requests:?}"
    );
    assert!(result.is_error, "the call must be refused, not run");
    let text = blocks_text(&result.blocks);
    assert!(
        text.contains("denied by a `deny` rule"),
        "the refusal must come from the deny rule itself, not `curl` \
         actually running and failing for an unrelated reason (e.g. DNS): \
         {text:?}"
    );
}

/// **The full-bypass regression, AutoAllow mode.** In `AutoAllow`
/// (`PermissionBroker::decide`, the branch that returns `Allow` without
/// ever calling `gate.check`), a missed deny match is not a downgraded
/// prompt -- it is silent execution with no prompt at all. This proves the
/// laundered `curl` call is still refused.
#[tokio::test]
async fn a_leading_tab_still_denies_curl_under_auto_allow_through_the_real_seam() {
    // An empty-script gate (any call to it panics via `RecordingGate`'s
    // decision being consulted at all is already wrong in AutoAllow) --
    // scripted to `Deny` anyway so a bug that DID reach the gate would
    // still show up as a gate call rather than accidentally passing.
    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "must not be consulted in AutoAllow".into(),
    });
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(bash_call_response("\tcurl http://example.invalid")),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(backend, gate.clone() as Arc<dyn PermissionGate>);
    conway.set_permission_mode(PermissionMode::AutoAllow);

    let (requests, result) = run_laundered_curl_call(&conway, gate).await;

    assert!(
        requests.is_empty(),
        "AutoAllow must never consult the gate for a deny-matched call, \
         laundered or not: {requests:?}"
    );
    assert!(
        result.is_error,
        "AutoAllow must not silently ALLOW a laundered command a deny rule \
         was meant to refuse -- the last remaining guardrail in this mode \
         is the deny match itself"
    );
    let text = blocks_text(&result.blocks);
    assert!(
        text.contains("denied by a `deny` rule"),
        "the refusal must come from the deny rule itself, not `curl` \
         actually running and failing for an unrelated reason (e.g. DNS), \
         which would ALSO set `is_error` and could mask a full bypass: \
         {text:?}"
    );
}
