//! Board item `01M38MDGCNVF2GE3JEQTRXSN2S`: the load-bearing regression
//! proof for the session-scoped shell-prefix grant's sanitizer-laundering
//! hole.
//!
//! **Why this file exists alongside `shell_prefix_grant_seam.rs` and
//! `conway-runtime`'s own `permission.rs` unit tests.** Every test guarding
//! `PermissionBroker::shell_prefix_grant_allows` before this item built its
//! `AuthorizedCall` fixture by hand (`permission.rs`'s own `bash_call`
//! helper sets `rendered` VERBATIM) or, one level better, called the shared
//! `conway_core::text::sanitize_control_chars` directly against a
//! hand-built fixture (`permission.rs`'s
//! `a_grant_never_covers_a_candidate_laundered_by_the_real_sanitizer`).
//! Neither routes through the actual production seam that produces
//! `AuthorizedCall::rendered` in the first place:
//! `conway_runtime::tools::runner::execute_one` resolving the real `bash`
//! `Tool`, calling `render_call` (`Tool::render` + `sanitize_rendered`), and
//! handing the result to `PermissionBroker::decide`. That gap is exactly
//! what let the defect ship: the compound-command exclusion's own literal
//! `\n`/`\r` checks are correct in isolation but unreachable once a real
//! embedded newline has already been laundered into
//! `SANITIZED_CONTROL_PLACEHOLDER` upstream of them, and nothing testing
//! against a hand-typed string (sanitized or not) can see that ordering
//! problem -- only a test that goes through the real call site can.
//!
//! This file drives the REAL stack end to end: a real `Conway` (the
//! `builtin-tools` feature's genuine `BashTool`, not a fixture), a real
//! session per call, a scripted model response whose `command` argument
//! carries an actual embedded control character, and a gate that RECORDS
//! whether it was consulted -- following `shell_prefix_grant_seam.rs`'s own
//! established shape (which this file's module doc borrows) rather than
//! inventing a new one. A `Deny` decision, not `AllowOnce`, is used
//! throughout: the smuggled second command in these fixtures (`rm -rf`
//! against a throwaway path) must never actually execute even if this
//! file's own assertions are wrong about whether the grant covered the
//! call, and a gate consultation is exactly as visible either way (this
//! file's only observable is whether the gate was asked at all, never its
//! answer).
#![cfg(feature = "builtin-tools")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conway::test_support::{base_config, build_conway_with_builtins, scripted_backend};
use conway::{Conway, PermissionScope, SessionSpec};
use conway_core::agent::{PermissionDecision, PermissionRequest};
use conway_core::content::{StopReason, ToolCall, Usage};
use conway_core::ids::ToolName;
use conway_core::ports::{GenerateResponse, PermissionGate};
use conway_testkit::{text_response, ScriptedTurn};

/// One scripted `bash` call whose `command` argument is `command` VERBATIM
/// -- including, for the tests in this file, a genuine embedded control
/// character (a real byte in the JSON string a model's tool call carries,
/// never an escaped two-character `\n` a person would type at a shell
/// prompt).
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

/// Records every `PermissionRequest` it receives and always answers `Deny`
/// -- the smuggled second command in every fixture below (`rm -rf` against
/// a throwaway `/tmp` path) must never actually run, even if this file's
/// own assertions are wrong; `Deny` makes that true regardless of which way
/// this test would otherwise fail.
struct RecordingGate {
    requests: Mutex<Vec<PermissionRequest>>,
}

impl RecordingGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
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
        PermissionDecision::Deny {
            reason: "operator said no".into(),
        }
    }
}

/// Runs one scripted `bash` call to completion on a fresh, non-keep-alive
/// session -- mirrors `shell_prefix_grant_seam.rs`'s own `run_one_bash_call`
/// exactly, including the reasoning in its doc for why a fresh session per
/// call is a faithful stand-in for "the operator runs this again later"
/// under a `GrantScope::Session` grant.
async fn run_one_bash_call(conway: &Conway) {
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("do the thing").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");
}

/// **The headline regression proof.** Grant `git status` at session scope
/// through the real `Conway::grant_session_shell_prefix` facade method (the
/// exact one the TUI's `[p]` editor calls); then run a real `bash` call
/// whose `command` is `"git status \n rm -rf /tmp/should-never-run..."` --
/// a genuine embedded newline, surrounded by spaces so the sanitizer's
/// placeholder becomes its own whitespace-delimited token (the variant that
/// slips a naive token-wise prefix match, per `shell_prefix_grant_allows`'s
/// own doc). The grant must NOT cover this call: it must still reach the
/// operator's gate, through the REAL `BashTool::render` ->
/// `conway_runtime::tools::runner::render_call` (which sanitizes) ->
/// `AuthorizedCall::rendered` -> `PermissionBroker::shell_prefix_grant_allows`
/// pipeline, exactly as it would with no grant installed at all.
#[tokio::test]
async fn a_newline_smuggled_command_still_reaches_the_operator_despite_a_matching_shell_prefix_grant(
) {
    let gate = RecordingGate::new();
    let conway = build_conway_with_builtins(
        base_config(),
        scripted_backend(vec![
            ScriptedTurn::Respond(bash_call_response("git status --short")),
            ScriptedTurn::Respond(text_response("done")),
            ScriptedTurn::Respond(bash_call_response(
                "git status \n rm -rf /tmp/should-never-run-shell-prefix-newline",
            )),
            ScriptedTurn::Respond(text_response("done")),
        ]),
        gate.clone() as Arc<dyn PermissionGate>,
    );

    // Learn the real requesting agent's id the same way
    // `shell_prefix_grant_seam.rs` does: the first (ordinary) call must
    // reach the gate since nothing is granted yet.
    run_one_bash_call(&conway).await;
    let requests = gate.requests();
    assert_eq!(
        requests.len(),
        1,
        "the first, ungranted call must reach the gate"
    );
    let agent = requests[0].agent_id;

    let installed = conway.grant_session_shell_prefix(
        "git status".to_string(),
        PermissionScope::Session,
        agent,
    );
    assert!(installed, "a simple, non-compound prefix must install");

    // The newline-smuggled candidate must still reach the gate -- a SECOND
    // request, not zero.
    run_one_bash_call(&conway).await;
    assert_eq!(
        gate.requests().len(),
        2,
        "a bash call whose command contains an embedded real newline followed by another \
         command must still reach the operator's gate, through the real render_call/\
         sanitize_rendered seam, even though its first two tokens match a granted \
         `git status` prefix"
    );
}

/// The carriage-return sibling of the headline test above -- the acceptance
/// criteria name it explicitly as a second construct that must be caught
/// identically, not merely the one newline case.
#[tokio::test]
async fn a_carriage_return_smuggled_command_still_reaches_the_operator_despite_a_matching_shell_prefix_grant(
) {
    let gate = RecordingGate::new();
    let conway = build_conway_with_builtins(
        base_config(),
        scripted_backend(vec![
            ScriptedTurn::Respond(bash_call_response("git status --short")),
            ScriptedTurn::Respond(text_response("done")),
            ScriptedTurn::Respond(bash_call_response(
                "git status \r rm -rf /tmp/should-never-run-shell-prefix-cr",
            )),
            ScriptedTurn::Respond(text_response("done")),
        ]),
        gate.clone() as Arc<dyn PermissionGate>,
    );

    run_one_bash_call(&conway).await;
    let agent = gate.requests()[0].agent_id;

    assert!(conway.grant_session_shell_prefix(
        "git status".to_string(),
        PermissionScope::Session,
        agent
    ));

    run_one_bash_call(&conway).await;
    assert_eq!(
        gate.requests().len(),
        2,
        "a bash call whose command contains an embedded real carriage return followed by \
         another command must still reach the operator's gate"
    );
}

/// **The ordinary case must still work.** A `git status` grant must still
/// cover `git status --short --branch` -- extra, benign arguments with no
/// control character involved -- through the identical real seam, proving
/// the fix above narrows nothing about the documented, intended behavior.
#[tokio::test]
async fn the_ordinary_case_is_still_covered_through_the_real_render_seam() {
    let gate = RecordingGate::new();
    let conway = build_conway_with_builtins(
        base_config(),
        scripted_backend(vec![
            ScriptedTurn::Respond(bash_call_response("git status --short")),
            ScriptedTurn::Respond(text_response("done")),
            ScriptedTurn::Respond(bash_call_response("git status --short --branch")),
            ScriptedTurn::Respond(text_response("done")),
        ]),
        gate.clone() as Arc<dyn PermissionGate>,
    );

    run_one_bash_call(&conway).await;
    let agent = gate.requests()[0].agent_id;

    assert!(conway.grant_session_shell_prefix(
        "git status".to_string(),
        PermissionScope::Session,
        agent
    ));

    run_one_bash_call(&conway).await;
    assert_eq!(
        gate.requests().len(),
        1,
        "a `git status` grant must still cover `git status --short --branch` with no \
         laundering involved -- the fix must not narrow the ordinary case"
    );
}
