//! Acceptance tests for ("Per-rule
//! permission revocation in `/settings`").
//!
//! Before this item, `/settings`'s grant list rendered
//! `PermissionBroker::active_patterns()` as inert label text -- the only
//! way to remove a grant was `revoke_all_grants()`, which drops every
//! pattern at once. This file drives the REAL production seam --
//! [`Conway::revoke_permission_pattern`], the exact method
//! `conway-cli`'s app loop calls when the operator selects a grant row in
//! `/settings` and presses `Enter` -- against real permission files on a
//! real filesystem, plus a real agent turn through the real `bash` tool and
//! `PermissionBroker` for the "the other grant still works" case. Same
//! shape `permission_trust_seam.rs` established, for the identical reason:
//! a hand-built fixture proves nothing about whether the real pipeline
//! enforces anything.
#![cfg(feature = "builtin-tools")]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
    TuiSection,
};
use conway::permission_pattern::{PatternOrigin, PatternRule};
use conway::{Conway, ConwayBuilder, PluginSelection, RevokeOutcome, SessionSpec};
use conway_core::agent::{PermissionDecision, PermissionRequest, PermissionScope};
use conway_core::content::{ContentBlock, StopReason, ToolCall, Usage};
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias, ToolName};
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

fn base_config(cwd: &Path) -> ConwayConfig {
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
        hooks: HooksConfig::default(),
    }
}

/// Records every `PermissionRequest` it receives and always answers `Deny`
/// -- see `permission_trust_seam.rs`'s identical fixture for why.
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

fn build_conway(cwd: &Path, script: Vec<ScriptedTurn>, gate: Arc<dyn PermissionGate>) -> Conway {
    let backend = Arc::new(ScriptedBackend::new(script).with_id(BackendId::new("fake")));
    let store = Arc::new(FakeStore::new());
    ConwayBuilder::from_parts(base_config(cwd))
        .with_backend(backend as Arc<dyn Backend>)
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

fn project_dir_with_permissions(contents: &str) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".conway")).expect("mkdir .conway");
    std::fs::write(
        dir.path().join(".conway").join("permissions.json"),
        contents,
    )
    .expect("write permissions.json");
    dir
}

fn isolated_env() -> (TempDir, HashMap<String, String>) {
    let xdg = TempDir::new().expect("tempdir");
    let mut env = HashMap::new();
    env.insert(
        "XDG_CONFIG_HOME".to_string(),
        xdg.path().display().to_string(),
    );
    (xdg, env)
}

/// Finds the `(rule, origin)` pair for a given wire form -- the identity
/// `Conway::revoke_permission_pattern` is addressed by, exactly as
/// `/settings`'s review row would hand it back.
fn find_grant(conway: &Conway, wire: &str) -> (PatternRule, PatternOrigin) {
    conway
        .active_permission_patterns()
        .into_iter()
        .find(|(rule, _)| rule.to_wire() == wire)
        .unwrap_or_else(|| panic!("no active grant for {wire:?}"))
}

// ---- headline: revoking one grant leaves the other working ----

/// Grant two patterns, revoke one -- the other must keep suppressing its
/// prompt, and the revoked one must reach the operator's gate again,
/// through the REAL bash tool + broker.
#[tokio::test]
async fn revoking_one_pattern_leaves_the_other_in_force() {
    let cwd = TempDir::new().expect("tempdir");
    let (_xdg, env) = isolated_env();
    let gate = RecordingGate::new();
    let conway = build_conway(
        cwd.path(),
        vec![
            ScriptedTurn::Respond(bash_call_response("git status")),
            ScriptedTurn::Respond(text_response("done")),
            ScriptedTurn::Respond(bash_call_response("cargo test")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    let agent = AgentId::new();

    conway.grant_permission_pattern(
        PatternRule::parse("bash:git status").expect("valid"),
        conway::PermissionScope::Session,
        agent,
    );
    conway.grant_permission_pattern(
        PatternRule::parse("bash:cargo test").expect("valid"),
        conway::PermissionScope::Session,
        agent,
    );
    assert_eq!(conway.active_permission_patterns().len(), 2);

    let (rule, origin) = find_grant(&conway, "bash:git status");
    let outcome = conway.revoke_permission_pattern(&env, &rule, &origin);
    assert!(
        matches!(outcome, RevokeOutcome::RevokedNoFile),
        "an interactively-granted rule has no file to persist to: {outcome:?}"
    );
    assert_eq!(
        conway.active_permission_patterns().len(),
        1,
        "exactly one grant should remain"
    );

    // The revoked pattern must reach the operator again...
    run_one_bash_call(&conway).await;
    assert_eq!(
        gate.requests().len(),
        1,
        "the revoked pattern must prompt again"
    );
    assert_eq!(gate.requests()[0].rendered, "git status");

    // ...while the surviving pattern keeps suppressing its prompt.
    run_one_bash_call(&conway).await;
    assert_eq!(
        gate.requests().len(),
        1,
        "the surviving grant must still auto-allow, adding no new request"
    );
}

// ---- persistence: a project-file-origin revoke rewrites THAT file ----

/// A revoked, project-file-origin rule is removed from the exact file it
/// came from, and stays gone across a simulated restart -- AND the file's
/// remaining trusted rule keeps applying with no re-`/trust` needed, proving
/// the re-trust-after-rewrite decision actually works end to end.
#[tokio::test]
async fn revoking_a_trusted_project_rule_persists_and_keeps_the_file_trusted() {
    let project =
        project_dir_with_permissions(r#"{"allow": ["bash:git status", "bash:cargo test"]}"#);
    let (_xdg, env) = isolated_env();
    let path = project.path().join(".conway").join("permissions.json");
    let agent = AgentId::new();

    let gate = RecordingGate::new();
    let conway = build_conway(
        project.path(),
        vec![],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    conway
        .trust_permission_file(&env, &path, PermissionScope::Session, agent)
        .expect("trust succeeds");
    assert_eq!(conway.active_permission_patterns().len(), 2);

    let (rule, origin) = find_grant(&conway, "bash:git status");
    let outcome = conway.revoke_permission_pattern(&env, &rule, &origin);
    match &outcome {
        RevokeOutcome::RevokedAndPersisted { retrust_warning } => {
            assert!(
                retrust_warning.is_none(),
                "re-trusting the freshly-rewritten file must succeed: {retrust_warning:?}"
            );
        }
        other => panic!("expected RevokedAndPersisted, got {other:?}"),
    }

    // The file on disk no longer mentions the revoked rule, and still has
    // the other one.
    let contents = std::fs::read_to_string(&path).expect("read back");
    assert!(!contents.contains("git status"), "{contents}");
    assert!(contents.contains("cargo test"), "{contents}");

    // Simulate a restart: a brand-new `Conway`, loading permission files
    // fresh from disk.
    let gate2 = RecordingGate::new();
    let restarted = build_conway(
        project.path(),
        vec![
            ScriptedTurn::Respond(bash_call_response("git status")),
            ScriptedTurn::Respond(text_response("done")),
            ScriptedTurn::Respond(bash_call_response("cargo test")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate2.clone() as Arc<dyn PermissionGate>,
    );
    let report =
        restarted.load_permission_files(project.path(), &env, PermissionScope::Session, agent);
    assert!(
        report.notices.is_empty(),
        "the file must still be trusted after the rewrite -- re-trust must have \
         taken effect: {:?}",
        report.notices
    );

    run_one_bash_call(&restarted).await;
    assert_eq!(
        gate2.requests().len(),
        1,
        "the revoked rule must stay revoked across a restart -- it must \
         prompt again"
    );
    assert_eq!(gate2.requests()[0].rendered, "git status");

    run_one_bash_call(&restarted).await;
    assert_eq!(
        gate2.requests().len(),
        1,
        "the SURVIVING rule must still auto-allow after the restart, proving \
         the file's other rule was not silently disarmed by de-trust"
    );
}

/// The GLOBAL file needs no re-trust step at all -- it is trusted by
/// authorship, never gated on a digest (`Conway::load_permission_files`'s
/// own doc). Revoking a global-origin rule must still persist and must not
/// even attempt a `TrustStore` write.
#[tokio::test]
async fn revoking_a_global_rule_persists_with_no_retrust_ceremony() {
    let cwd = TempDir::new().expect("tempdir with no project file");
    let (xdg, env) = isolated_env();
    let global_dir = xdg.path().join("conway");
    std::fs::create_dir_all(&global_dir).expect("mkdir global dir");
    let global_path = global_dir.join("permissions.json");
    std::fs::write(&global_path, r#"{"allow": ["bash:git status"]}"#)
        .expect("write global permissions.json");

    let gate = RecordingGate::new();
    let conway = build_conway(cwd.path(), vec![], gate.clone() as Arc<dyn PermissionGate>);
    let agent = AgentId::new();
    let report = conway.load_permission_files(cwd.path(), &env, PermissionScope::Session, agent);
    assert!(report.notices.is_empty());
    assert_eq!(conway.active_permission_patterns().len(), 1);

    let (rule, origin) = find_grant(&conway, "bash:git status");
    let outcome = conway.revoke_permission_pattern(&env, &rule, &origin);
    match &outcome {
        RevokeOutcome::RevokedAndPersisted { retrust_warning } => {
            assert!(retrust_warning.is_none());
        }
        other => panic!("expected RevokedAndPersisted, got {other:?}"),
    }

    let contents = std::fs::read_to_string(&global_path).expect("read back");
    assert!(!contents.contains("git status"), "{contents}");

    // No `trust.json` should have been created for the global path -- the
    // global file is never a trust subject.
    let trust_json = xdg.path().join("conway").join("trust.json");
    assert!(
        !trust_json.exists(),
        "revoking a global-origin rule must never write a trust record"
    );
}

// ---- Interactive origin: revoking must never create a file ----

/// Revoking an `Interactive`-origin rule -- one installed with no backing
/// file at all -- must not create a permissions file where none existed.
#[tokio::test]
async fn revoking_an_interactive_rule_creates_no_file() {
    let cwd = TempDir::new().expect("tempdir");
    let (_xdg, env) = isolated_env();
    let gate = RecordingGate::new();
    let conway = build_conway(cwd.path(), vec![], gate as Arc<dyn PermissionGate>);
    let agent = AgentId::new();

    // No permissions file exists anywhere -- confirm the candidate paths
    // are absent before proceeding.
    let candidate = cwd.path().join(".conway").join("permissions.json");
    assert!(!candidate.exists());

    conway.grant_permission_pattern(
        PatternRule::parse("bash:git status").expect("valid"),
        conway::PermissionScope::Session,
        agent,
    );
    let (rule, origin) = find_grant(&conway, "bash:git status");
    assert_eq!(origin, PatternOrigin::Interactive);

    let outcome = conway.revoke_permission_pattern(&env, &rule, &origin);
    assert!(matches!(outcome, RevokeOutcome::RevokedNoFile));
    assert!(
        conway.active_permission_patterns().is_empty(),
        "the grant must be gone from the session"
    );
    assert!(
        !candidate.exists(),
        "revoking an Interactive-origin rule must never create a file"
    );
}

// ---- persistence failure: never fails open ----

/// A file that cannot be parsed at revoke time is a PERSIST FAILURE, not a
/// silent overwrite -- but the in-session grant is dropped regardless
/// (revocation never fails open).
#[tokio::test]
async fn a_persist_failure_still_revokes_for_the_session_and_reports_the_failure() {
    let project = project_dir_with_permissions(r#"{"allow": ["bash:git status"]}"#);
    let (_xdg, env) = isolated_env();
    let path = project.path().join(".conway").join("permissions.json");
    let agent = AgentId::new();

    let gate = RecordingGate::new();
    let conway = build_conway(
        project.path(),
        vec![
            ScriptedTurn::Respond(bash_call_response("git status")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    conway
        .trust_permission_file(&env, &path, PermissionScope::Session, agent)
        .expect("trust succeeds");
    let (rule, origin) = find_grant(&conway, "bash:git status");

    // Corrupt the file out from under the broker, between load and revoke
    // -- exactly the "someone else touched the file" case this method must
    // not blindly overwrite.
    std::fs::write(&path, "not json at all").expect("corrupt the file");

    let outcome = conway.revoke_permission_pattern(&env, &rule, &origin);
    match &outcome {
        RevokeOutcome::RevokedButPersistFailed { error } => {
            assert!(!error.is_empty());
        }
        other => panic!("expected RevokedButPersistFailed, got {other:?}"),
    }

    // The in-session grant is gone regardless of the persist failure.
    assert!(
        conway.active_permission_patterns().is_empty(),
        "revocation must never fail open -- the session grant is dropped \
         even though the file write failed"
    );
    run_one_bash_call(&conway).await;
    assert_eq!(
        gate.requests().len(),
        1,
        "the revoked rule must prompt again THIS session even though \
         persistence failed"
    );

    // The corrupt file on disk is untouched (never blindly overwritten).
    let contents = std::fs::read_to_string(&path).expect("read back");
    assert_eq!(contents, "not json at all");
}

// ---- revoking something already gone ----

#[tokio::test]
async fn revoking_a_grant_that_is_not_installed_reports_not_found() {
    let cwd = TempDir::new().expect("tempdir");
    let (_xdg, env) = isolated_env();
    let gate = RecordingGate::new();
    let conway = build_conway(cwd.path(), vec![], gate as Arc<dyn PermissionGate>);

    let outcome = conway.revoke_permission_pattern(
        &env,
        &PatternRule::parse("bash:git status").expect("valid"),
        &PatternOrigin::Interactive,
    );
    assert!(matches!(outcome, RevokeOutcome::NotFound));
}

// ---- revoke-all still works alongside per-rule revoke ----

/// `revoke_permission_grants` (the pre-existing revoke-ALL escape hatch)
/// still clears every grant, unaffected by per-rule revoke's addition.
#[tokio::test]
async fn revoke_all_still_clears_every_grant() {
    let cwd = TempDir::new().expect("tempdir");
    let (_xdg, env) = isolated_env();
    let _ = &env; // unused in this test beyond satisfying the helper shape
    let gate = RecordingGate::new();
    let conway = build_conway(cwd.path(), vec![], gate as Arc<dyn PermissionGate>);
    let agent = AgentId::new();

    conway.grant_permission_pattern(
        PatternRule::parse("bash:git status").expect("valid"),
        conway::PermissionScope::Session,
        agent,
    );
    conway.grant_permission_pattern(
        PatternRule::parse("bash:cargo test").expect("valid"),
        conway::PermissionScope::Session,
        agent,
    );
    assert_eq!(conway.active_permission_patterns().len(), 2);

    conway.revoke_permission_grants();
    assert!(conway.active_permission_patterns().is_empty());
}
