//! Facade-level acceptance tests for the session shell-prefix grant's
//! operator round trip (board items `01M32EBPWZZG6EA77ZG5KYC8KQ`, which
//! added the grant itself, and `01M350FR4SM6QT0EM6M35EY5AZ`, which added
//! its review/revoke surface).
//!
//! Both of those items were tested only at two levels: unit tests inside
//! `conway-runtime` (against a hand-built `AuthorizedCall`/`PermissionCtx`,
//! never a real session) and unit tests inside `conway-cli` (against the
//! TUI's own state, never a real `PermissionBroker`). Neither drove the
//! PUBLIC facade -- `Conway::grant_session_shell_prefix`,
//! `Conway::active_shell_prefix_grants`, `Conway::revoke_shell_prefix_grant`,
//! `Conway::revoke_all_shell_prefix_grants` -- against a real `Conway`, a
//! real session, and the real `bash` tool. That gap matters more here than
//! usual: this feature's whole promise is "grant it, and it stops asking;
//! revoke it, and it asks again" -- a promise only the FACADE, not an
//! internal broker method, can actually keep or break. This file drives
//! that round trip end to end, following `permission_scope_seam.rs`'s and
//! `permission_revoke_seam.rs`'s established shape (a real `Conway` with
//! the `builtin-tools` feature's real `bash` tool, a real `ToolRunner`, a
//! real `PermissionBroker`, and a gate that RECORDS every request it sees)
//! rather than inventing a new one, and asserts throughout on the
//! observable outcome -- whether the gate was consulted at all -- never on
//! `PermissionBroker` directly.
#![cfg(feature = "builtin-tools")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conway::test_support::{base_config, build_conway_with_builtins, scripted_backend};
use conway::{Conway, GrantScope, PermissionScope, SessionSpec};
use conway_core::agent::{PermissionDecision, PermissionRequest};
use conway_core::content::{StopReason, ToolCall, Usage};
use conway_core::ids::ToolName;
use conway_core::ports::{GenerateResponse, PermissionGate};
use conway_testkit::{text_response, ScriptedTurn};

/// One scripted `bash` call, rendering exactly `command` -- the
/// `RenderKind::ShellCommand` shape a shell-prefix grant exists to cover
/// (see `permission_scope_seam.rs`'s own module doc for why the OTHER
/// grant classes' seam tests deliberately moved off `bash` -- this file is
/// the one place `bash` is still the right fixture, since the prefix grant
/// is `bash`-only by construction).
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

/// Records every `PermissionRequest` it receives and always answers
/// `AllowOnce` -- a call this file expects to reach the gate then runs for
/// real (`git status --short` in the fixture cwd is harmless), and a call
/// the grant covers must never reach it at all, making gate-consultation
/// the one observable every assertion below turns on.
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
        PermissionDecision::AllowOnce
    }
}

/// Runs one scripted `bash` call to completion, on a fresh, non-keep-alive
/// session -- mirrors `permission_revoke_seam.rs`'s own
/// `run_one_bash_call`. A `GrantScope::Session` shell-prefix grant covers
/// every requester in the process regardless of which `SessionId` it came
/// from (`GrantScope::covers` returns `true` unconditionally for
/// `Session`), so a fresh session per call is a faithful, simpler stand-in
/// for "the operator runs this again later" than threading one keep-alive
/// session through every step.
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

/// **The headline round trip.** Grant a prefix; the matching command is
/// then allowed WITHOUT reaching the gate; the review list reports the
/// exact prefix text and scope back; revoke it; the same command REACHES
/// THE GATE AGAIN. The last assertion is the load-bearing one -- a revoked
/// grant that still silently authorizes is exactly the failure this test
/// exists to catch.
#[tokio::test]
async fn granting_a_shell_prefix_authorizes_it_then_revoking_prompts_it_again() {
    let gate = RecordingGate::new();
    let conway = build_conway_with_builtins(
        base_config(),
        scripted_backend(vec![
            ScriptedTurn::Respond(bash_call_response("git status --short")),
            ScriptedTurn::Respond(text_response("done")),
            ScriptedTurn::Respond(bash_call_response("git status --short")),
            ScriptedTurn::Respond(text_response("done")),
            ScriptedTurn::Respond(bash_call_response("git status --short")),
            ScriptedTurn::Respond(text_response("done")),
        ]),
        gate.clone() as Arc<dyn PermissionGate>,
    );

    // No grant yet: the first call must reach the gate -- which is also
    // how this test learns the real requesting agent's id (never a
    // hand-picked fixture id that could accidentally agree).
    run_one_bash_call(&conway).await;
    let requests = gate.requests();
    assert_eq!(requests.len(), 1, "the first call must reach the gate");
    let agent = requests[0].agent_id;

    // The grant, installed through the exact facade method the TUI's `[p]`
    // editor calls after an operator edits and confirms a proposed prefix.
    let installed = conway.grant_session_shell_prefix(
        "git status".to_string(),
        PermissionScope::Session,
        agent,
    );
    assert!(installed, "a simple, non-blank prefix must install");

    // The list assertion: the prefix text and scope come back EXACTLY as
    // granted, not just a count.
    assert_eq!(
        conway.active_shell_prefix_grants(),
        vec![("git status".to_string(), GrantScope::Session)],
        "the review list must report the exact prefix text and scope granted"
    );

    // The matching command is now allowed WITHOUT reaching the gate.
    run_one_bash_call(&conway).await;
    assert_eq!(
        gate.requests().len(),
        1,
        "a granted prefix must cover its matching command without a new prompt \
         (or the grant is simply inert and the revoke case below proves nothing)"
    );

    // Revoke it, addressed by the same (prefix, scope) pair the review
    // list handed back.
    let revoked = conway.revoke_shell_prefix_grant("git status", &GrantScope::Session);
    assert!(revoked, "revoking an installed grant must report success");
    assert!(
        conway.active_shell_prefix_grants().is_empty(),
        "the review list must no longer show the revoked grant"
    );

    // The load-bearing assertion: the same command must now REACH THE GATE
    // AGAIN. A revoked grant that still silently authorizes is the failure
    // this whole class of test exists to catch.
    run_one_bash_call(&conway).await;
    assert_eq!(
        gate.requests().len(),
        2,
        "a revoked shell-prefix grant must no longer authorize its command -- \
         the operator must be prompted again"
    );
}

/// **The revoke-ALL path.** Two independently-installed prefixes, both
/// covering their commands without a prompt; `revoke_all_shell_prefix_grants`
/// must drop both at once, leaving the review list empty and making a
/// PREVIOUSLY-COVERED command prompt again.
#[tokio::test]
async fn revoking_all_shell_prefix_grants_drops_every_one_and_prompts_again() {
    let gate = RecordingGate::new();
    let conway = build_conway_with_builtins(
        base_config(),
        scripted_backend(vec![
            ScriptedTurn::Respond(bash_call_response("git status --short")),
            ScriptedTurn::Respond(text_response("done")),
            ScriptedTurn::Respond(bash_call_response("git status --short")),
            ScriptedTurn::Respond(text_response("done")),
            ScriptedTurn::Respond(bash_call_response("git status --short")),
            ScriptedTurn::Respond(text_response("done")),
        ]),
        gate.clone() as Arc<dyn PermissionGate>,
    );

    // Learn the real requesting agent's id the same way the headline test
    // does.
    run_one_bash_call(&conway).await;
    let agent = gate.requests()[0].agent_id;

    // Two independently-installed grants, one at Session scope and one
    // scoped to the requesting agent alone -- `revoke_all` must not
    // discriminate by scope.
    assert!(conway.grant_session_shell_prefix(
        "git status".to_string(),
        PermissionScope::Session,
        agent
    ));
    assert!(conway.grant_session_shell_prefix(
        "git status --short".to_string(),
        PermissionScope::Agent,
        agent
    ));
    assert_eq!(
        conway.active_shell_prefix_grants().len(),
        2,
        "both grants must be installed before revoke-all runs"
    );

    // The now-granted command is covered without a prompt -- the positive
    // control proving revoke-all below actually removes something live,
    // not an already-inert entry.
    run_one_bash_call(&conway).await;
    assert_eq!(
        gate.requests().len(),
        1,
        "the granted prefix must cover its matching command without a new prompt"
    );

    conway.revoke_all_shell_prefix_grants();
    assert!(
        conway.active_shell_prefix_grants().is_empty(),
        "revoke-all must leave no shell-prefix grants behind"
    );

    // The previously-covered command must prompt again.
    run_one_bash_call(&conway).await;
    assert_eq!(
        gate.requests().len(),
        2,
        "a command covered by a grant that revoke-all just dropped must prompt again"
    );
}
