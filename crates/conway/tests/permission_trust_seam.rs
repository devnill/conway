//! Acceptance test for board item 01KYT8SGX32CP56PRJNG72V2W5 ("SECURITY: a
//! cloned repo's `.conway/permissions.json` auto-grants at startup with no
//! consent").
//!
//! Before this item, `crates/conway-cli/src/tui/app.rs`'s startup loader
//! read every discovered `.conway/permissions.json` (project-then-global)
//! and installed EVERY `allow` rule at `PermissionScope::Session` --
//! `Session` covers any requester -- with no consent, no diff, and no
//! record of origin. A cloned repository shipping
//! `{"allow": ["bash:npm run build"]}` therefore auto-approved that command
//! on first launch, in a repo that also controls `package.json`.
//!
//! This file drives the REAL production seam -- [`Conway::load_permission_files`],
//! the exact method `conway-cli`'s `App::new` calls, feeding a real project
//! directory on a real filesystem (not a hand-built `PatternRule` list) --
//! and then runs an actual agent turn through the real `bash` tool and
//! `PermissionBroker`, asserting on what the gate actually received. This
//! is deliberately the same shape `permission_pattern_seam.rs` and
//! `root_containment_seam.rs` established for exactly this reason: a
//! hand-written fixture proves nothing about whether the real pipeline
//! enforces anything.
//!
//! **The headline test, `an_untrusted_project_allow_rule_does_not_take_effect`,
//! fails against pre-fix behavior.** Confirmed by temporarily removing the
//! trust check `Conway::load_permission_files` applies to a project-scoped
//! file (installing its `allow` rules unconditionally, matching
//! `tui/app.rs:200-216` before this item) and re-running this file: the
//! `RecordingGate` then sees ZERO requests for `git status` (the project
//! file's rule auto-grants it) instead of one, and the assertion on
//! `requests.len()` fails immediately.
#![cfg(feature = "builtin-tools")]

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, LimitsConfig, ModelsConfig, PermissionsConfig,
    RoleEntry, RoutingSection, SessionConfig, TuiSection,
};
use conway::{Conway, ConwayBuilder, SessionSpec};
use conway_core::agent::{PermissionDecision, PermissionRequest};
use conway_core::content::{ContentBlock, StopReason, ToolCall, Usage};
use conway_core::fakes::{FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias, ToolName};
use conway_core::ports::{Backend, GenerateResponse, PermissionGate};
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
        cwd: cwd.to_path_buf(),
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

/// Records every `PermissionRequest` it receives and always answers `Deny`
/// -- see `permission_pattern_seam.rs`'s identical fixture for why this
/// (rather than an always-allowing fake) is the right shape: a test that
/// expects a call to reach the gate must never let it actually execute, and
/// a test that expects the gate to be BYPASSED needs to see zero requests.
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
        .build()
        .expect("build should succeed with the real builtin `bash` tool registered")
}

/// Runs one `bash` call end to end -- the SCRIPTED backend already encodes
/// which command it issues; the prompt text here is inert filler.
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

/// A project checkout: `<dir>/.conway/permissions.json` with the given raw
/// contents. No `settings.json` alongside it -- `permission_file_paths`'s
/// own fallback ("no ancestor config discovered -> `cwd`'s own `.conway/`")
/// is exactly the common case for a freshly cloned repo that has never run
/// conway before, so exercising THAT path (rather than requiring a
/// `settings.json` to exist first) is the more honest fixture.
fn project_dir_with_permissions(contents: &str) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".conway")).expect("mkdir .conway");
    std::fs::write(dir.path().join(".conway").join("permissions.json"), contents)
        .expect("write permissions.json");
    dir
}

/// An isolated, empty global config directory -- `XDG_CONFIG_HOME` pointed
/// here means `TrustStore::load` finds no `trust.json` (so the project file
/// starts untrusted) and the global `permissions.json` candidate simply
/// does not exist (so it contributes nothing, keeping every test's
/// behavior attributable to the PROJECT file alone).
fn isolated_env() -> (TempDir, HashMap<String, String>) {
    let xdg = TempDir::new().expect("tempdir");
    let mut env = HashMap::new();
    env.insert(
        "XDG_CONFIG_HOME".to_string(),
        xdg.path().display().to_string(),
    );
    (xdg, env)
}

/// **The headline test.** An untrusted project `permissions.json`'s
/// `allow` rule must not take effect: `git status` must still reach the
/// operator's gate through the real render/broker seam, exactly as if no
/// permissions file existed at all.
///
/// Confirmed to fail against pre-fix behavior -- see this file's own doc.
#[tokio::test]
async fn an_untrusted_project_allow_rule_does_not_take_effect() {
    let project = project_dir_with_permissions(r#"{"allow": ["bash:git status"]}"#);
    let (_xdg, env) = isolated_env();

    let gate = RecordingGate::new();
    let conway = build_conway(
        project.path(),
        vec![
            ScriptedTurn::Respond(bash_call_response("git status")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    let report = conway.load_permission_files(project.path(), &env, AgentId::new());
    assert_eq!(
        report.notices.len(),
        1,
        "an untrusted project file with a nonempty allow list must surface one notice"
    );
    assert!(
        report.notices[0].contains("require an explicit trust decision"),
        "{}",
        report.notices[0]
    );

    run_one_bash_call(&conway).await;

    assert_eq!(
        gate.requests().len(),
        1,
        "an ALLOW rule from an UNTRUSTED project file must never take effect -- \
         `git status` must still reach the operator's gate, exactly as if the \
         file did not exist"
    );
    assert_eq!(gate.requests()[0].rendered, "git status");
}

/// The mirror image: once the SAME file is explicitly trusted
/// (`Conway::trust_permission_file`, the only path that writes a trust
/// record), its allow rule takes effect immediately, in the same session --
/// no restart required.
#[tokio::test]
async fn trusting_a_project_file_makes_its_allow_rule_take_effect() {
    let project = project_dir_with_permissions(r#"{"allow": ["bash:git status"]}"#);
    let (_xdg, env) = isolated_env();

    let gate = RecordingGate::new();
    let conway = build_conway(
        project.path(),
        vec![
            ScriptedTurn::Respond(bash_call_response("git status")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    let agent = AgentId::new();
    let report = conway.load_permission_files(project.path(), &env, agent);
    assert_eq!(report.notices.len(), 1, "untrusted before the trust call");

    let path = project.path().join(".conway").join("permissions.json");
    let installed = conway
        .trust_permission_file(&env, &path, agent)
        .expect("trust succeeds");
    assert_eq!(installed, 1);

    run_one_bash_call(&conway).await;

    assert!(
        gate.requests().is_empty(),
        "after an explicit trust decision, the project file's allow rule must \
         grant `git status` without consulting the operator: {:?}",
        gate.requests()
    );
}

/// A project file's `deny` rule applies immediately, with NO trust
/// decision at all -- the asymmetric other half (D4 §3).
#[tokio::test]
async fn an_untrusted_project_deny_rule_still_applies_immediately() {
    let project = project_dir_with_permissions(r#"{"deny": ["bash:curl"]}"#);
    let (_xdg, env) = isolated_env();

    let gate = RecordingGate::new();
    let conway = build_conway(
        project.path(),
        vec![
            ScriptedTurn::Respond(bash_call_response("curl evil.example")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    let report = conway.load_permission_files(project.path(), &env, AgentId::new());
    assert!(
        report.notices.is_empty(),
        "a deny-only file needs no trust decision and must not be flagged: {:?}",
        report.notices
    );

    run_one_bash_call(&conway).await;

    assert!(
        gate.requests().is_empty(),
        "an untrusted project file's deny rule must refuse the call directly, \
         without ever consulting the operator's gate: {:?}",
        gate.requests()
    );
}

/// A CHAINED command must still reach the operator even if it happens to
/// match a deny prefix's tool+first-token -- proving the deny rule's
/// short-circuit (return `Deny`, never fall through to a prompt) does not
/// somehow let a chained command execute instead of being caught by either
/// half. (The command below matches no rule at all -- neither the deny
/// prefix `curl` nor any allow -- so it must reach the gate exactly once.)
#[tokio::test]
async fn a_call_matching_neither_rule_still_reaches_the_gate() {
    let project =
        project_dir_with_permissions(r#"{"allow": ["bash:git status"], "deny": ["bash:curl"]}"#);
    let (_xdg, env) = isolated_env();

    let gate = RecordingGate::new();
    let conway = build_conway(
        project.path(),
        vec![
            ScriptedTurn::Respond(bash_call_response("git push --force")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    conway
        .trust_permission_file(
            &env,
            &project.path().join(".conway").join("permissions.json"),
            AgentId::new(),
        )
        .expect("trust succeeds");

    run_one_bash_call(&conway).await;

    assert_eq!(
        gate.requests().len(),
        1,
        "a command matching neither the allow nor the deny rule must still reach \
         the operator: {:?}",
        gate.requests()
    );
}

/// The GLOBAL file needs no trust decision at all -- trusted by
/// authorship. Simulated here by pointing the project's OWN cwd discovery
/// at a directory with no project-scoped file, so the only candidate that
/// exists is the "global" one this test writes directly into the isolated
/// XDG directory.
#[tokio::test]
async fn a_global_permissions_file_installs_with_no_trust_decision() {
    let cwd = TempDir::new().expect("tempdir with no project permissions file");
    let (xdg, env) = isolated_env();
    let global_dir = xdg.path().join("conway");
    std::fs::create_dir_all(&global_dir).expect("mkdir global conway dir");
    std::fs::write(
        global_dir.join("permissions.json"),
        r#"{"allow": ["bash:git status"]}"#,
    )
    .expect("write global permissions.json");

    let gate = RecordingGate::new();
    let conway = build_conway(
        cwd.path(),
        vec![
            ScriptedTurn::Respond(bash_call_response("git status")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    let report = conway.load_permission_files(cwd.path(), &env, AgentId::new());
    assert!(
        report.notices.is_empty(),
        "the operator's own global file must never be flagged as untrusted: {:?}",
        report.notices
    );

    run_one_bash_call(&conway).await;

    assert!(
        gate.requests().is_empty(),
        "the global file's allow rule must grant without any trust ceremony: {:?}",
        gate.requests()
    );
}

/// Editing a trusted project file's content silently de-trusts it -- no
/// modal, just the rule reverting to requiring the gate again, driven
/// through the same two real calls (`load_permission_files` after a `git
/// pull`-shaped edit).
#[tokio::test]
async fn editing_a_trusted_project_files_content_de_trusts_it() {
    let project = project_dir_with_permissions(r#"{"allow": ["bash:git status"]}"#);
    let (_xdg, env) = isolated_env();
    let path = project.path().join(".conway").join("permissions.json");
    let agent = AgentId::new();

    // Establish trust against the original bytes with one throwaway
    // `Conway` (the trust record lives on disk, not in the broker).
    {
        let gate = RecordingGate::new();
        let setup = build_conway(project.path(), vec![], gate as Arc<dyn PermissionGate>);
        setup
            .trust_permission_file(&env, &path, agent)
            .expect("trust succeeds");
    }

    // A hostile (or merely later) edit changes the bytes -- adds a second
    // rule an operator never reviewed.
    std::fs::write(
        &path,
        r#"{"allow": ["bash:git status", "bash:curl"]}"#,
    )
    .expect("edit permissions.json");

    let gate = RecordingGate::new();
    let conway = build_conway(
        project.path(),
        vec![
            ScriptedTurn::Respond(bash_call_response("git status")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    let report = conway.load_permission_files(project.path(), &env, agent);
    assert_eq!(
        report.notices.len(),
        1,
        "the edited file must be untrusted again -- its digest no longer \
         matches the recorded one"
    );

    run_one_bash_call(&conway).await;
    assert_eq!(
        gate.requests().len(),
        1,
        "even the UNCHANGED rule must stop auto-granting once the file's \
         content has changed since it was trusted -- trust is per-content, \
         not per-directory"
    );
}
