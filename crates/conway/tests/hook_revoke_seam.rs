//! Acceptance test for ("Show
//! hook-backed permission rules in `/settings` as a fourth revocable
//! list").
//!
//! Before this item, no surface listed an active hook-backed rule at all,
//! and there was no way to revoke one short of hand-editing `enabled` in
//! `settings.json` and restarting. This file drives the REAL production
//! seam -- [`Conway::revoke_hook_rule`], the EXACT method `conway-cli`'s
//! `App::run` calls when the operator selects a hook row in `/settings`
//! and presses `Enter` (`crates/conway-cli/src/tui/app.rs`'s
//! `Action::RevokeHookRule` arm) -- against a real `[hooks]` config, a
//! real `ProcessHookRunner` spawning a real `/bin/sh` process, and a real
//! agent turn through the real `bash` tool and `PermissionBroker`. Same
//! shape `permission_revoke_seam.rs` established for the identical
//! reason: a hand-built fixture proves nothing about whether the real
//! pipeline enforces anything.
//!
//! Both DENY-CAPABLE events get their own headline test:
//! `pre_tool_use` (narrows a tool call) and `prompt_submitted` (narrows a
//! submitted prompt) -- the context that reshaped this item away from its
//! original `pre_tool_use`-only scoping (see `Conway::
//! active_deny_capable_hook_rules`'s own doc for the full reasoning).
#![cfg(feature = "builtin-tools")]

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HookEntry, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
    TuiSection,
};
use conway::{Conway, ConwayBuilder, HookRuleView, PluginSelection};
use conway_core::agent::{PermissionDecision, PermissionRequest};
use conway_core::content::{ContentBlock, StopReason, ToolCall, Usage};
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias, ToolName};
use conway_core::log::LogRecord;
use conway_core::ports::{Backend, GenerateResponse, PermissionGate};
use conway_testkit::{FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};
use tempfile::TempDir;

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
        stop: StopReason::EndTurn,
        usage: Usage::default(),
    }
}

fn bash_call_response(command: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: "call_1".to_string(),
            name: ToolName::new("bash"),
            arguments: serde_json::json!({ "command": command }),
        }],
        stop: StopReason::ToolUse,
        usage: Usage::default(),
    }
}

fn base_config(cwd: &Path, hooks: HooksConfig) -> ConwayConfig {
    let mut roles = std::collections::BTreeMap::new();
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
        cwd: cwd.to_path_buf(),
        session: SessionConfig::default(),
        limits: LimitsConfig::default(),
        permissions: PermissionsConfig::default(),
        backends: std::collections::BTreeMap::new(),
        routing: RoutingSection::default(),
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig::default(),
        tui: TuiSection::default(),
        tools: ToolsConfig::default(),
        plugins: PluginsConfig::default(),
        hooks,
    }
}

/// A `pre_tool_use` rule whose command always fails (`exit 1`) --
/// deliberately a FAILURE, not an explicit `HookPermissionVerdict::Deny`
/// answer, so this proves the real `ProcessHookRunner` (fail-closed on a
/// nonzero exit) is actually spawned, not a stub always answering the same
/// way regardless of what it ran (`hook_runner_wiring.rs`'s identical
/// rationale, this crate's `conway-cli` sibling).
fn denying_pre_tool_use_rule(id: &str) -> HookEntry {
    HookEntry {
        id: id.to_string(),
        event: "pre_tool_use".to_string(),
        command: vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "exit 1".to_string(),
        ],
        ..Default::default()
    }
}

/// Records every `PermissionRequest` it receives and always answers Allow
/// -- unlike `permission_revoke_seam.rs`'s `RecordingGate` (which denies),
/// this file's own headline signal is WHETHER the gate is reached AT ALL:
/// `PermissionBroker::decide`'s `pre_tool_use` hook step runs BEFORE the
/// gate, so a denying hook means the gate is never consulted, and a
/// revoked one means it is.
struct RecordingAllowGate {
    requests: Mutex<Vec<PermissionRequest>>,
}

impl RecordingAllowGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
        })
    }

    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

#[async_trait]
impl PermissionGate for RecordingAllowGate {
    async fn check(&self, req: PermissionRequest) -> PermissionDecision {
        self.requests.lock().unwrap().push(req);
        PermissionDecision::AllowOnce
    }
}

fn build_conway(
    cwd: &Path,
    hooks: HooksConfig,
    script: Vec<ScriptedTurn>,
    gate: Arc<dyn PermissionGate>,
) -> Conway {
    let backend = Arc::new(ScriptedBackend::new(script).with_id(BackendId::new("fake")));
    let store = Arc::new(FakeStore::new());
    ConwayBuilder::from_parts(base_config(cwd, hooks))
        .with_backend(backend as Arc<dyn Backend>)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        // This file drives the REAL `bash` tool end to end (bash ships off
        // by default), and needs the REAL `ProcessHookRunner` a hook
        // config actually dispatches through.
        .with_builtin_plugins(PluginSelection::All)
        .with_default_hook_runner()
        .build()
        .expect("build should succeed with the real builtin `bash` tool and hook runner")
}

async fn run_one_bash_call(conway: &Conway) -> Vec<LogRecord> {
    let handle = conway
        .new_session(conway::SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let root = handle.root();
    let turn = handle.prompt("do the thing").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");
    handle.transcript(root).await.expect("transcript")
}

fn bash_tool_result_text(records: &[LogRecord]) -> Option<String> {
    records.iter().find_map(|r| match r {
        LogRecord::ToolResultRecord { result, .. } if result.tool.as_str() == "bash" => {
            result.blocks.iter().find_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
        }
        _ => None,
    })
}

fn hook_row<'a>(rows: &'a [HookRuleView], id: &str) -> &'a HookRuleView {
    rows.iter()
        .find(|r| r.id == id)
        .unwrap_or_else(|| panic!("no active hook rule for {id:?}: {rows:?}"))
}

// ---- headline: pre_tool_use ----

/// VERIFICATION ANCHOR (the item's own ACCEPTANCE, restated end to end):
/// a `pre_tool_use` hook denies a real `bash` call; revoking it through
/// `Conway::revoke_hook_rule` -- the exact facade call `/settings`'
/// `Action::RevokeHookRule` arm makes -- makes the SAME call reach the
/// operator's gate instead, within the same session.
#[tokio::test]
async fn revoking_a_pre_tool_use_hook_lets_the_next_matching_call_reach_the_gate() {
    let cwd = TempDir::new().expect("tempdir");
    let gate = RecordingAllowGate::new();
    let hooks = HooksConfig {
        rules: vec![denying_pre_tool_use_rule("deny-bash")],
    };
    let conway = build_conway(
        cwd.path(),
        hooks,
        vec![
            ScriptedTurn::Respond(bash_call_response("echo hi")),
            ScriptedTurn::Respond(text_response("done")),
            ScriptedTurn::Respond(bash_call_response("echo hi again")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    // Visible before anything happens -- own bar: an operator must
    // be able to SEE a rule before deciding to revoke it.
    let rows = conway.active_deny_capable_hook_rules();
    assert_eq!(hook_row(&rows, "deny-bash").event, "pre_tool_use");

    // First call: denied by the hook. The gate is never reached.
    let records = run_one_bash_call(&conway).await;
    let text = bash_tool_result_text(&records).expect("a bash ToolResultRecord must exist");
    assert!(text.contains("denied"), "{text}");
    assert!(
        text.contains("deny-bash"),
        "LOAD-BEARING: the denial must name the hook that fired, not just say \
         'denied' by some other mechanism: {text}"
    );
    assert_eq!(
        gate.request_count(),
        0,
        "a denying pre_tool_use hook runs BEFORE the gate -- it must never be reached"
    );

    // Revoke through the exact facade method the UI action calls.
    let revoked = conway.revoke_hook_rule("pre_tool_use", "deny-bash");
    assert!(
        revoked,
        "the rule was installed, so revocation must report true"
    );
    assert!(
        conway.active_deny_capable_hook_rules().is_empty(),
        "the revoked rule must be gone from the review list"
    );

    // Second call, same session: no longer denied by the (now absent)
    // hook -- the gate is finally consulted, and allows it.
    let records = run_one_bash_call(&conway).await;
    assert_eq!(
        gate.request_count(),
        1,
        "the revoked hook must no longer deny a matching call this session"
    );
    let text = bash_tool_result_text(&records).expect("a bash ToolResultRecord must exist");
    assert!(
        !text.contains("denied"),
        "the second call must actually run, not be denied by anything: {text}"
    );
}

/// Revoking an id that was never installed reports `false` and changes
/// nothing (mirrors `permission_revoke_seam.rs`'s `NotFound` case).
#[tokio::test]
async fn revoking_an_absent_hook_rule_reports_false() {
    let cwd = TempDir::new().expect("tempdir");
    let gate = RecordingAllowGate::new();
    let conway = build_conway(
        cwd.path(),
        HooksConfig::default(),
        vec![],
        gate as Arc<dyn PermissionGate>,
    );
    assert!(!conway.revoke_hook_rule("pre_tool_use", "does-not-exist"));
}

/// An unrecognized event name (never one this review list lists in the
/// first place) is refused rather than silently matching anything.
#[tokio::test]
async fn revoking_with_an_unrecognized_event_name_reports_false() {
    let cwd = TempDir::new().expect("tempdir");
    let gate = RecordingAllowGate::new();
    let hooks = HooksConfig {
        rules: vec![denying_pre_tool_use_rule("deny-bash")],
    };
    let conway = build_conway(cwd.path(), hooks, vec![], gate as Arc<dyn PermissionGate>);
    assert!(!conway.revoke_hook_rule("post_tool_use", "deny-bash"));
    assert_eq!(
        conway.active_deny_capable_hook_rules().len(),
        1,
        "untouched"
    );
}

// ---- prompt_submitted: the second deny-capable event ----

/// The identical property, for the OTHER deny-capable event this item's
/// own reshaping made in-scope: a `prompt_submitted` hook denies the
/// prompt itself (before any tool call is even proposed); revoking it
/// through the same facade method lets the next prompt through.
#[tokio::test]
async fn revoking_a_prompt_submitted_hook_lets_the_next_prompt_through() {
    let cwd = TempDir::new().expect("tempdir");
    let gate = RecordingAllowGate::new();
    let hooks = HooksConfig {
        rules: vec![HookEntry {
            id: "deny-prompts".to_string(),
            event: "prompt_submitted".to_string(),
            command: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 1".to_string(),
            ],
            ..Default::default()
        }],
    };
    let conway = build_conway(
        cwd.path(),
        hooks,
        vec![
            ScriptedTurn::Respond(text_response("should never run")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate as Arc<dyn PermissionGate>,
    );

    let rows = conway.active_deny_capable_hook_rules();
    assert_eq!(hook_row(&rows, "deny-prompts").event, "prompt_submitted");

    // First prompt: denied before the model is ever consulted -- the
    // scripted backend's first turn is never taken.
    let handle = conway
        .new_session(conway::SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let first = handle.prompt("do the thing").await;
    assert!(
        first.is_err(),
        "a prompt_submitted denial must surface as a prompt error, not a turn to await"
    );

    let revoked = conway.revoke_hook_rule("prompt_submitted", "deny-prompts");
    assert!(revoked);
    assert!(conway.active_deny_capable_hook_rules().is_empty());

    // Second prompt, same session: no longer denied -- reaches the
    // (now-first-in-queue) scripted turn and completes normally.
    let second = handle
        .prompt("do the thing")
        .await
        .expect("the revoked prompt_submitted hook must no longer deny this prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), second.result())
        .await
        .expect("turn must not hang");
}

// ---- both events revoke independently ----

/// Revoking one deny-capable event's rule must not touch the other's --
/// each is addressed by its own `(event, id)` pair, never a bare id.
#[tokio::test]
async fn revoking_one_event_leaves_the_other_untouched() {
    let cwd = TempDir::new().expect("tempdir");
    let gate = RecordingAllowGate::new();
    let hooks = HooksConfig {
        rules: vec![
            denying_pre_tool_use_rule("deny-bash"),
            HookEntry {
                id: "deny-prompts".to_string(),
                event: "prompt_submitted".to_string(),
                command: vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "exit 1".to_string(),
                ],
                ..Default::default()
            },
        ],
    };
    let conway = build_conway(cwd.path(), hooks, vec![], gate as Arc<dyn PermissionGate>);
    assert_eq!(conway.active_deny_capable_hook_rules().len(), 2);

    assert!(conway.revoke_hook_rule("pre_tool_use", "deny-bash"));

    let remaining = conway.active_deny_capable_hook_rules();
    assert_eq!(remaining.len(), 1, "{remaining:?}");
    assert_eq!(remaining[0].id, "deny-prompts");
    assert_eq!(remaining[0].event, "prompt_submitted");
}
