//! Facade-level acceptance tests for board item `01M3TD844GXJFEVF69M0HH1X5Q`
//! ("`PermissionMode::Prompt`'s own in-project-read default-allow"), the
//! same shape every other `*_seam.rs` file in this crate establishes: a
//! hand-built fixture proves nothing about whether the real pipeline
//! enforces anything, so this drives the real production seam end to end --
//! [`conway_tools::fs::read::ReadTool`]'s real `render`/dispatch, the real
//! [`conway_runtime::permission::PermissionBroker::decide`], through a real
//! [`Conway`] built with the real builtin tools (`builtin-tools`), never a
//! hand-typed `AuthorizedCall`.
//!
//! The broker-level proof already exists, one layer down, in
//! `crates/conway-runtime/tests/permission_broker.rs`
//! (`an_in_project_read_matched_by_a_deny_rule_is_still_denied` and
//! `an_in_project_read_matched_by_a_prompt_rule_still_prompts`, both cited
//! by `docs/permissions.md`'s own "The default: in-project reads don't ask"
//! section). What was still missing was the FACADE-level proof: that the
//! default actually fires with zero gate consultations through
//! `Conway::new_session` -> a real agent turn -> the real `ReadTool`, with
//! no permissions file and no rule installed at all (the positive case),
//! and that an operator's own (user-scope, trusted-by-authorship) `deny`
//! rule still wins over it end to end -- the asymmetry
//! `docs/permissions.md`'s own trust section states (a `deny` rule applies
//! unconditionally regardless of trust; this file proves that holds even
//! against the one case Prompt mode would otherwise never even ask about).
#![cfg(feature = "builtin-tools")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conway::test_support::{base_config_at, build_conway_with_builtins, scripted_backend};
use conway_core::agent::{PermissionDecision, PermissionRequest, PermissionScope};
use conway_core::content::{ContentBlock, StopReason, ToolCall, ToolResult, Usage};
use conway_core::ids::{AgentId, ToolName};
use conway_core::log::LogRecord;
use conway_core::ports::{GenerateResponse, PermissionGate};
use conway_testkit::{text_response, ScriptedTurn};
use tempfile::TempDir;

fn read_call_response(path: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: "call_1".to_string(),
            name: ToolName::new("read"),
            arguments: serde_json::json!({ "path": path }),
        }],
        stop: StopReason::ToolUse,
        usage: Usage::default(),
    }
}

/// Records every `PermissionRequest` it receives and always answers with a
/// fixed `decision` -- see every sibling `*_seam.rs` file's identical
/// fixture for why: a test proving the gate is BYPASSED needs to see zero
/// requests, and one proving the gate is REACHED must never let the call
/// actually execute on `AllowOnce` alone.
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

/// An isolated, empty global config directory -- `CONWAY_CONFIG_DIR`
/// pointed here means the operator's own (user-scope) `permissions.json`
/// candidate lives at `<config_dir>/permissions.json`, trusted by
/// authorship (D4 §3): its `deny` rules install unconditionally, with no
/// `/trust permissions` ceremony and regardless of project trust.
fn isolated_env() -> (TempDir, HashMap<String, String>) {
    let config_dir = TempDir::new().expect("tempdir");
    let mut env = HashMap::new();
    env.insert(
        "CONWAY_CONFIG_DIR".to_string(),
        config_dir.path().display().to_string(),
    );
    (config_dir, env)
}

fn write_global_permissions(config_dir: &TempDir, contents: &str) {
    std::fs::write(config_dir.path().join("permissions.json"), contents)
        .expect("write global permissions.json");
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

/// The LAST `ToolResultRecord` in the transcript -- this file never spawns
/// a child, so the root agent's own transcript carries exactly one.
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

/// **The positive case.** No permissions file anywhere, no rule installed,
/// nothing granted -- a `read` of a file inside the project (the session's
/// own cwd, no `--root`) is still allowed, with ZERO gate consultations,
/// through the real `Conway` facade. The gate is wired to `Deny` so a call
/// that reached it would be observable twice over: as a nonempty
/// `gate.requests()` AND as a denied `ToolResult`. Neither fires.
#[tokio::test]
async fn an_in_project_read_is_allowed_with_no_gate_call_through_the_facade() {
    let project = TempDir::new().expect("tempdir");
    let file_path = project.path().join("notes.txt");
    std::fs::write(&file_path, "project notes").expect("write fixture file");

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "must not be consulted -- the in-project default must grant this".into(),
    });
    let conway = build_conway_with_builtins(
        base_config_at(project.path()),
        scripted_backend(vec![
            ScriptedTurn::Respond(read_call_response(&file_path.display().to_string())),
            ScriptedTurn::Respond(text_response("done")),
        ]),
        gate.clone() as Arc<dyn PermissionGate>,
    );

    let handle = conway
        .new_session(conway::SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("read my notes").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");

    assert!(
        gate.requests().is_empty(),
        "an in-project read with no rule, no grant, and nothing configured must still be \
         allowed by conway's own Prompt-mode default, never reaching the gate: {:?}",
        gate.requests()
    );

    let records = handle
        .transcript(handle.root())
        .await
        .expect("transcript should resolve");
    let result = tool_result(&records);
    assert!(
        !result.is_error,
        "the read must actually have succeeded (allowed, not merely un-gated): {:?}",
        blocks_text(&result.blocks)
    );
    assert!(
        blocks_text(&result.blocks).contains("project notes"),
        "the real file content must come back through the real ReadTool: {:?}",
        blocks_text(&result.blocks)
    );
}

/// **DENY-WINS, through the facade.** The operator's own (user-scope)
/// `permissions.json` -- trusted by authorship, no `/trust` ceremony needed
/// at all -- carries a `deny` rule covering every `read`. Despite the call
/// being an in-project read that conway's own default would otherwise grant
/// with no prompt, the deny rule wins: the real `ReadTool` never runs, the
/// gate is never consulted (a `deny` match is decided before the gate, same
/// as an ordinary read), and the persisted `ToolResult` reports the refusal.
/// This is the trust asymmetry `docs/permissions.md` states (`deny` applies
/// unconditionally, regardless of trust) proven against the ONE case Prompt
/// mode would otherwise never even ask about.
#[tokio::test]
async fn an_in_project_read_matched_by_a_deny_rule_in_the_operators_own_file_is_denied_through_the_facade(
) {
    let project = TempDir::new().expect("tempdir");
    let file_path = project.path().join("secret.txt");
    std::fs::write(&file_path, "TOP SECRET").expect("write fixture file");

    let (config_dir, env) = isolated_env();
    write_global_permissions(&config_dir, r#"{"deny": ["read:*"]}"#);

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway_with_builtins(
        base_config_at(project.path()),
        scripted_backend(vec![
            ScriptedTurn::Respond(read_call_response(&file_path.display().to_string())),
            ScriptedTurn::Respond(text_response("done")),
        ]),
        gate.clone() as Arc<dyn PermissionGate>,
    );

    let report = conway.load_permission_files(
        project.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );
    assert!(
        report.notices.is_empty(),
        "the operator's own global file must never be flagged as untrusted: {:?}",
        report.notices
    );

    let handle = conway
        .new_session(conway::SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("read the secret").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");

    assert!(
        gate.requests().is_empty(),
        "a `deny` rule is decided before the gate, exactly like an ordinary read -- the gate \
         (wired to AllowOnce here) must never be consulted: {:?}",
        gate.requests()
    );

    let records = handle
        .transcript(handle.root())
        .await
        .expect("transcript should resolve");
    let result = tool_result(&records);
    assert!(
        result.is_error,
        "the operator's own deny rule must refuse the read even though it is an in-project \
         read conway's own default would otherwise grant with no prompt: {:?}",
        blocks_text(&result.blocks)
    );
    assert!(
        !blocks_text(&result.blocks).contains("TOP SECRET"),
        "the real file content must never come back once denied: {:?}",
        blocks_text(&result.blocks)
    );
}
