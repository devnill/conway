//! Regression test for ("CRITICAL:
//! pattern grants are inert"): every existing pattern-grant test fed
//! `PatternRule::matches`/the broker a HAND-WRITTEN `rendered` string --
//! never the string the production path actually produces
//! (`ToolRunner::execute_one` -> `render_call` -> `Tool::render`). That is
//! exactly what hid the bug: `render_call` used to synthesize
//! `bash({"command":"git status"})` unconditionally (a JSON debug dump, not
//! a shell command), which `PatternRule::matches`'s metacharacter gate
//! rejects on sight because of the JSON's own `(){}` -- so no pattern grant
//! could ever match ANY call, for ANY tool. `[p]` never had anything to
//! offer and no persisted rule could ever fire.
//!
//! This file drives the REAL stack end to end -- `Conway` (built with the
//! `builtin-tools` feature's genuine `bash` `Tool`, not a fixture) running
//! an actual agent turn through `ToolRunner`/`PermissionBroker` -- so the
//! `rendered` text a pattern is matched against is whatever
//! `conway_tools::shell::bash::BashTool::render` (via `Tool::render`)
//! actually produces, not a string a test author typed by hand.
//!
//! `a_chained_command_still_reaches_the_operator_through_the_real_render_seam`
//! is the headline test: it is the item's own regression proof, and fails
//! against pre-fix `main` (confirmed by running it with `render_call`
//! reverted to its old unconditional `format!("{}({})", call.name,
//! call.arguments)` form -- see the completion report for how).
#![cfg(feature = "builtin-tools")]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
    TuiSection,
};
use conway::{Conway, ConwayBuilder, PatternRule, PluginSelection, SessionSpec};
use conway_core::agent::{PermissionDecision, PermissionRequest, PermissionScope};
use conway_core::content::ContentBlock;
use conway_core::fakes::{FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias};
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
        tui: TuiSection::default(),
        tools: ToolsConfig::default(),
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

/// Records every `PermissionRequest` it receives and always answers with a
/// fixed `decision`. Unlike `conway_core::fakes::FakeGate`, this combines
/// recording with a non-`AllowOnce` decision -- needed here because a call
/// this test EXPECTS to reach the gate (`git push --force`, a chained
/// command) must never actually execute; `Deny` proves it reached the
/// operator without running it for real.
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

/// Runs one `bash` call end to end (a fresh session, one prompt, one
/// scripted tool call, one final text response) and returns the requests
/// the gate actually saw.
async fn run_one_bash_call(gate: Arc<RecordingGate>, command: &str) -> Vec<PermissionRequest> {
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(bash_call_response(command)),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(backend, gate.clone() as Arc<dyn PermissionGate>);

    conway.grant_permission_pattern(
        PatternRule::parse("bash:git status").expect("valid rule"),
        PermissionScope::Session,
        AgentId::new(), // Session scope covers any requester; the granting
                        // agent's identity is irrelevant to it.
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("do the thing").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");

    gate.requests()
}

/// **The most important test in this file.** `git status && rm -rf /`
/// starts with the granted prefix `git status`, but carries a shell
/// metacharacter -- it must still reach the operator's gate, THROUGH THE
/// REAL PRODUCTION RENDER PATH (`BashTool::render` -> `render_call` ->
/// `AuthorizedCall::rendered` -> `PatternRule::matches`), not a hand-typed
/// fixture. This is the actual lesson of the bug this item fixes.
#[tokio::test]
async fn a_chained_command_still_reaches_the_operator_through_the_real_render_seam() {
    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "operator said no".into(),
    });

    let requests = run_one_bash_call(gate, "git status && rm -rf /tmp/should-never-run").await;

    assert_eq!(
        requests.len(),
        1,
        "the chained command must actually reach the operator's gate -- \
         a pattern grant for `git status` must never silently authorize it"
    );
    assert_eq!(
        requests[0].rendered, "git status && rm -rf /tmp/should-never-run",
        "the gate must see the tool's OWN rendering of the call, not a \
         synthesized JSON dump -- this is what the production render seam \
         (BashTool::render) now provides"
    );
}

/// Regression, end to end: a NEWLINE-chained command must still reach the
/// operator through the real seam.
///
/// This is the one case the rest of this file could not catch. `rendered`
/// is sanitized for display safety (control chars -> `U+FFFD`) inside
/// `render_call`, BEFORE `PatternRule::matches` ever sees it -- and `\n` is
/// simultaneously a control char and one of the gate's own metacharacters.
/// So a gate that only looked for a literal `\n` would find nothing wrong,
/// `prefix_matches` would consume the replacement char as its own
/// whitespace-delimited token, and `git status \n rm -rf ...` would be
/// SILENTLY AUTO-APPROVED under the `bash:git status` grant -- while
/// `BashTool::invoke` executed the raw, unsanitized newline for real.
///
/// The unit-level test (`conway-core`'s `a_sanitized_chained_command_is_
/// still_gated`) now calls the shared `conway_core::text::sanitize_control_chars`
/// directly -- the sanitizer's home is in `conway-core` itself, so there is no
/// layering barrier and no hand-copy. THIS test is still load-bearing: it runs
/// the genuine sanitizer in the genuine pipeline (real render seam, real
/// broker), so it cannot drift from the real implementation even if a later
/// refactor moves where the rendering is sanitized.
#[tokio::test]
async fn a_newline_chained_command_still_reaches_the_operator_through_the_real_render_seam() {
    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "operator said no".into(),
    });

    // Spaces around the newline are deliberate: that is the variant where
    // the sanitizer's placeholder becomes its own token and slips past
    // token-wise prefix matching.
    let requests = run_one_bash_call(gate, "git status \n rm -rf /tmp/should-never-run").await;

    assert_eq!(
        requests.len(),
        1,
        "a newline-chained command must actually reach the operator's gate -- \
         sanitizing the newline for display must never launder it past the \
         metacharacter gate"
    );
}

/// A pattern grant for `git status` must not cover a different subcommand
/// -- `git push --force` must still reach the operator, and the request the
/// operator sees must be the bare command (proving `BashTool::render`, not
/// the old `bash({"command":...})` default, produced it).
#[tokio::test]
async fn a_different_subcommand_still_reaches_the_operator_through_the_real_render_seam() {
    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "operator said no".into(),
    });

    let requests = run_one_bash_call(gate, "git push --force").await;

    assert_eq!(
        requests.len(),
        1,
        "a different subcommand must still prompt"
    );
    assert_eq!(requests[0].rendered, "git push --force");
}

/// The mirror-image acceptance criterion: a command that genuinely matches
/// the granted prefix must NOT reach the operator at all -- proving the
/// pattern grant, which was completely inert before this fix (every
/// `rendered` value carried JSON metacharacters that made
/// `PatternRule::matches` always return `false`), now actually works
/// through the real tool.
#[tokio::test]
async fn a_granted_command_never_reaches_the_operator_through_the_real_render_seam() {
    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "must not be consulted".into(),
    });

    let requests = run_one_bash_call(gate, "git status --short").await;

    assert!(
        requests.is_empty(),
        "a command matching the granted `git status` prefix must never \
         reach the operator: {requests:?}"
    );
}

// ---------------------------------------------------------------------
// "pattern grants are still inert
// for every tool except bash". The tests above are all `bash` -- the ONE
// tool `Tool::render` was overridden for, which is exactly what hid the
// follow-on bug: `read:*` (the broadest non-`bash` grant the language can
// express) matched nothing, because `read`'s rendering is `Tool::render`'s
// UNCHANGED default JSON dump (`read({"path":"..."})`), whose own `(`/`)`/
// `{`/`}` tripped `PatternRule::matches`'s metacharacter gate exactly as
// unconditionally as a `bash` command's did before that fix. The test below
// drives `read` through the identical real seam
// (`ReadTool::render` -> `render_call` -> `AuthorizedCall::rendered` ->
// `PatternRule::matches_render`), proving `read:*` now actually grants.
// ---------------------------------------------------------------------

fn read_call_response(path: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![conway_core::content::ToolCall {
            call_id: "call_1".to_string(),
            name: conway_core::ids::ToolName::new("read"),
            arguments: serde_json::json!({ "path": path }),
        }],
        stop: conway_core::content::StopReason::ToolUse,
        usage: conway_core::content::Usage::default(),
    }
}

/// **The headline non-`bash` regression proof.** `read:*` must grant a real
/// `read` call end to end -- the gate must never even be consulted -- using
/// the real `ReadTool::render` (the trait's untouched default JSON dump),
/// not a hand-typed fixture.
#[tokio::test]
async fn a_read_wildcard_grant_actually_grants_through_the_real_render_seam() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let file_path = dir.path().join("f.txt");
    std::fs::write(&file_path, "hello from the real seam").expect("write fixture file");

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "must not be consulted -- read:* must grant this on its own".into(),
    });
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(read_call_response(&file_path.display().to_string())),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(backend, gate.clone() as Arc<dyn PermissionGate>);

    conway.grant_permission_pattern(
        PatternRule::parse("read:*").expect("valid rule"),
        PermissionScope::Session,
        AgentId::new(),
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("read the file").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");

    assert!(
        gate.requests().is_empty(),
        "a `read:*` grant must authorize a real `read` call without ever consulting the \
         operator -- before this fix, EVERY non-`bash` wildcard was inert because \
         `ReadTool`'s untouched default JSON-dump rendering always tripped the \
         metacharacter gate: {:?}",
        gate.requests()
    );
}
