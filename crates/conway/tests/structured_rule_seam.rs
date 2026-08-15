//! Acceptance tests for F12 ("the structured rule form: general
//! rules for tool use") -- the REAL-STACK seam tests for the structured
//! `Rule { select, when, then }` permission form.
//!
//! Every test here drives the genuine production path end to end: a real
//! `permissions.json` (project or global) loaded by the real
//! [`Conway::load_permission_files`], parsed by the real `parse_rules`/
//! `parse_deny_rules`/`parse_prompt_rules`, installed by the real
//! `PermissionBroker::remember_*_rule`, and evaluated by the real
//! `PermissionBroker::decide` against a real `ReadTool`/`BashTool` (the
//! `builtin-tools` feature, never a hand-rolled fixture) driven by a real
//! agent turn through `ToolRunner`. The `RecordingGate` records exactly
//! what the operator's gate saw -- the only honest proof that a structured
//! rule fired (zero requests) or fell through (one request) through the
//! real render/resolve/broker seam, not a hand-typed `AuthorizedCall`.
//!
//! This file deliberately mirrors `permission_pattern_seam.rs` and
//! `permission_trust_seam.rs` in shape, for the identical reason both of
//! those files state: a hand-written fixture proves nothing about whether
//! the real pipeline enforces anything, and the absence of seam-spanning
//! tests is exactly what hid both 0.5.0 permission bugs.
//!
//! The five traps the spec names are each pinned here by a dedicated test:
//! (1) `paths_under` reads `call.arguments` via `resolve_like_the_tool_will`
//!     + `CanonicalRoot::contains`, NEVER the sanitized/lossy `call.rendered`
//!     -- pinned by `a_paths_under_rule_reads_arguments_not_rendered_so_a_
//!     traversal_path_reaches_the_gate` (a path whose RENDERED contains the
//!     rule's prefix but whose resolved argument lands outside it must NOT
//!     match).
//! (2) `PathArgs::Unconfinable` NEVER satisfies `paths_under` -- pinned by
//!     `an_unconfinable_tool_never_satisfies_paths_under_so_bash_reaches_the_gate`.
//! (3) `command_prefix` on a `Structured`-rendering tool is a typed
//!     REGISTRATION error surfaced to the operator, not a silent no-op --
//!     pinned by `command_prefix_on_a_structured_tool_is_a_registration_error`.
//! (4) the allow-side metacharacter gate is not weakened -- pinned for the
//!     structured form by `flat_and_structured_command_prefix_produce_byte_
//!     identical_gate_decisions` (a structured `command_prefix` allow rule
//!     still refuses `git status && rm -rf /` exactly as the flat one does).
//! (5) plugin-contributed rules (the `deny`/`prompt` half) install from
//!     every file unconditionally -- covered by the structured `deny`/`prompt`
//!     rules loaded straight from a project file with no trust decision.
#![cfg(feature = "builtin-tools")]

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::{Conway, ConwayBuilder, PluginSelection, RuleRegistrationReason, SessionSpec};
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

fn tool_call_response(tool: &str, arguments: serde_json::Value) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: "call_1".to_string(),
            name: ToolName::new(tool),
            arguments,
        }],
        stop: StopReason::ToolUse,
        usage: Usage::default(),
    }
}

fn bash_call_response(command: &str) -> GenerateResponse {
    tool_call_response("bash", serde_json::json!({ "command": command }))
}

fn read_call_response(path: &str) -> GenerateResponse {
    tool_call_response("read", serde_json::json!({ "path": path }))
}

/// AMENDED by board item `01KZDDPC5MMD49F6JPV9CW4TVM`: the `RenderKind::
/// Structured` sibling-grant fixture used wherever this file used to pair a
/// `paths_under`/other structured rule against a `bash:...` flat sibling to
/// prove "revoking one leaves the other in force" -- a durable pattern
/// grant no longer exists for `bash` at all, so a `bash` sibling can no
/// longer demonstrate that. See `conway_core::permission_pattern`'s own
/// module doc.
fn write_call_response(path: &str) -> GenerateResponse {
    tool_call_response("write", serde_json::json!({ "path": path, "content": "x" }))
}

fn base_config(cwd: &Path) -> ConwayConfig {
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
        tools: ToolsConfig::default(),
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

/// Records every `PermissionRequest` it receives and always answers with a
/// fixed `decision` -- see `permission_pattern_seam.rs`'s identical fixture
/// for why: a test that expects a call to reach the gate must never let it
/// actually execute (`Deny`), and a test that expects the gate to be
/// BYPASSED needs to see zero requests.
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
        .expect("build should succeed with the real builtin fs/bash tools registered")
}

/// An isolated, empty global config directory: `XDG_CONFIG_HOME` pointed
/// here means `TrustStore::load` finds no `trust.json` (project files start
/// untrusted) and the global `permissions.json` candidate lives at
/// `<xdg>/conway/permissions.json` -- the one file `load_permission_files`
/// treats as trusted-by-authorship, so its `allow` rules install
/// unconditionally. Returns the tempdir (kept alive for the test's duration)
/// and the env map to pass to `load_permission_files`.
fn isolated_env() -> (TempDir, HashMap<String, String>) {
    let xdg = TempDir::new().expect("tempdir");
    let mut env = HashMap::new();
    env.insert(
        "XDG_CONFIG_HOME".to_string(),
        xdg.path().display().to_string(),
    );
    (xdg, env)
}

/// Writes `contents` to the GLOBAL permissions file (`<xdg>/conway/permissions.json`)
/// -- trusted-by-authorship, so its `allow` rules install unconditionally via
/// `load_permission_files`. The `conway/` subdirectory is created to mirror
/// `xdg_config_path`'s `$XDG_CONFIG_HOME/conway/settings.json` layout.
fn write_global_permissions(xdg: &TempDir, contents: &str) {
    let dir = xdg.path().join("conway");
    std::fs::create_dir_all(&dir).expect("mkdir <xdg>/conway");
    std::fs::write(dir.join("permissions.json"), contents).expect("write global permissions.json");
}

/// Writes `contents` to the PROJECT permissions file
/// (`<project>/.conway/permissions.json`) -- untrusted until `/trust
/// permissions`, so its `allow` rules install only after an explicit trust
/// decision; its `deny`/`prompt` rules install unconditionally (D4 §3).
fn write_project_permissions(project: &TempDir, contents: &str) {
    let dir = project.path().join(".conway");
    std::fs::create_dir_all(&dir).expect("mkdir <project>/.conway");
    std::fs::write(dir.join("permissions.json"), contents).expect("write project permissions.json");
}

// =====================================================================
// (a) `paths_under`: the structured allow rule that reads ARGUMENTS.
// =====================================================================

/// A structured `paths_under` allow rule authorizes an in-root `read`
/// WITHOUT ever consulting the operator's gate -- through the real
/// `ReadTool::render` -> `render_call` -> `PermissionBroker::decide` seam.
/// The agent is UNCONFINED (no `with_root`), so `check_root` is a no-op; the
/// `paths_under` rule is the ONLY thing authorizing the call, which is
/// exactly what isolates this test to the structured form.
#[tokio::test]
async fn a_paths_under_allow_rule_authorizes_an_in_root_read_without_the_gate() {
    let root_dir = TempDir::new().expect("tempdir");
    std::fs::write(
        root_dir.path().join("file.txt"),
        b"hello from inside the root",
    )
    .expect("write fixture file");
    let root_canon = root_dir.path().canonicalize().expect("canonicalize root");

    let (xdg, env) = isolated_env();
    write_global_permissions(
        &xdg,
        &format!(
            r#"{{"rules":[{{"select":{{"tools":["read"]}},"when":{{"paths_under":"{}"}},"then":"allow"}}]}}"#,
            root_canon.display()
        ),
    );

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "must not be consulted -- paths_under must grant this".into(),
    });
    let conway = build_conway(
        root_dir.path(),
        vec![
            ScriptedTurn::Respond(read_call_response("file.txt")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    let report = conway.load_permission_files(
        root_dir.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );
    assert!(
        report.registration_errors.is_empty(),
        "no registration errors expected: {:?}",
        report.registration_errors
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("read the file").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");

    assert!(
        gate.requests().is_empty(),
        "a `paths_under(root)` allow rule must authorize an in-root read without consulting \
         the operator -- the structured form's path-scoped grant fired through the real seam: \
         {:?}",
        gate.requests()
    );
}

/// The mirror image: a `read` whose argument resolves OUTSIDE the rule's
/// `paths_under` prefix must NOT match -- it reaches the gate. The agent is
/// unconfined, so without the rule there is no gate consultation at all
/// (AutoAllow off); the rule is what WOULD have authorized it, and it
/// correctly refuses.
#[tokio::test]
async fn a_paths_under_allow_rule_lets_an_out_of_root_read_reach_the_gate() {
    let root_dir = TempDir::new().expect("tempdir");
    let outside_dir = TempDir::new().expect("tempdir");
    std::fs::write(outside_dir.path().join("secret.txt"), b"TOP SECRET").expect("write secret");
    let root_canon = root_dir.path().canonicalize().expect("canonicalize root");
    let secret_canon = outside_dir
        .path()
        .join("secret.txt")
        .canonicalize()
        .expect("canonicalize secret");

    let (xdg, env) = isolated_env();
    write_global_permissions(
        &xdg,
        &format!(
            r#"{{"rules":[{{"select":{{"tools":["read"]}},"when":{{"paths_under":"{}"}},"then":"allow"}}]}}"#,
            root_canon.display()
        ),
    );

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "operator said no".into(),
    });
    let conway = build_conway(
        root_dir.path(),
        vec![
            ScriptedTurn::Respond(read_call_response(&secret_canon.display().to_string())),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    conway.load_permission_files(
        root_dir.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("read the secret").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");

    assert_eq!(
        gate.requests().len(),
        1,
        "an out-of-root read must reach the gate -- the `paths_under` rule must NOT match it"
    );
}

/// **TRAP 1, pinned.** `paths_under` reads `call.arguments` (via
/// `resolve_like_the_tool_will` + `CanonicalRoot::contains`), NEVER the
/// sanitized/lossy `call.rendered`. The path argument `<root>/../outside/
/// secret` RESOLVES outside the rule's prefix (the `..` is resolved away by
/// canonicalization), so the rule must NOT match -- the call reaches the
/// gate. But the RENDERED string `read({"path":"<root>/../outside/secret"})`
/// literally CONTAINS the rule's prefix `<root>`; a check that read
/// `rendered` (or did a naive substring match) would falsely match and
/// auto-allow an out-of-root read. This is the rendering-bypass test the
/// spec requires.
#[tokio::test]
async fn a_paths_under_rule_reads_arguments_not_rendered_so_a_traversal_path_reaches_the_gate() {
    let tmp = TempDir::new().expect("tempdir");
    let root_dir = tmp.path().join("repo");
    let outside_dir = tmp.path().join("outside");
    std::fs::create_dir(&root_dir).expect("mkdir repo");
    std::fs::create_dir(&outside_dir).expect("mkdir outside");
    std::fs::write(outside_dir.join("secret.txt"), b"TOP SECRET").expect("write secret");
    let root_canon = root_dir.canonicalize().expect("canonicalize root");

    let (xdg, env) = isolated_env();
    write_global_permissions(
        &xdg,
        &format!(
            r#"{{"rules":[{{"select":{{"tools":["read"]}},"when":{{"paths_under":"{}"}},"then":"allow"}}]}}"#,
            root_canon.display()
        ),
    );

    // The traversal path: starts with the rule's CANONICAL prefix VERBATIM
    // (`<root_canon>/..`), so its raw string begins with the rule's prefix --
    // a naive `starts_with` check (or a rendered-based substring check) would
    // falsely match. But it RESOLVES to `<outside>`, which is not under the
    // rule, so the real `resolve_like_the_tool_will` + `CanonicalRoot::contains`
    // path correctly refuses it. Using the CANONICAL root as the base (not
    // the non-canonical `root_dir`) makes the naive-check-falsely-matches
    // property hold on every platform, independent of OS symlink quirks
    // (e.g. macOS `/var` -> `/private/var`).
    let traversal = format!("{}/../outside/secret.txt", root_canon.display());

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "operator said no".into(),
    });
    let conway = build_conway(
        root_dir.as_path(),
        vec![
            ScriptedTurn::Respond(read_call_response(&traversal)),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    conway.load_permission_files(
        root_dir.as_path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle
        .prompt("read through the traversal")
        .await
        .expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");

    assert_eq!(
        gate.requests().len(),
        1,
        "a traversal path whose RENDERED contains the prefix but whose RESOLVED argument is \
         outside it must reach the gate -- `paths_under` reads arguments, not rendered"
    );
    assert!(
        gate.requests()[0]
            .rendered
            .contains(&root_canon.display().to_string()),
        "the rendered string DOES contain the rule's canonical prefix (so a rendered-based \
         or naive string-prefix check would falsely match): {:?}",
        gate.requests()[0].rendered
    );
}

// =====================================================================
// (a2) B2: a RELATIVE `paths_under` prefix resolves against the PROJECT,
//      not the process's cwd (finding S5).
// =====================================================================

/// **B2's headline acceptance test.** A project `permissions.json` carries
/// `paths_under: "src"` -- a RELATIVE prefix. It must confine to the
/// PROJECT's `src/` tree: a read of `<project>/src/file.txt` is authorized
/// without the gate (turn 1), and a read of the same-named path under the
/// PROCESS's cwd (`<process cwd>/src/lib.rs` -- which exists, since cargo
/// runs this test binary with the `conway` crate root as cwd) must NOT
/// match: it reaches the gate (turn 2). Before B2 the prefix canonicalized
/// against the process cwd at install, so the rule silently pointed at
/// `<process cwd>/src` -- both turns would invert (turn 1 asks, turn 2
/// auto-allows a path the operator never wrote a rule for).
///
/// The rule installs through the REAL `/trust permissions` path
/// (`Conway::trust_permission_file` -- a project file's allow rules require
/// an explicit trust decision), so the test also proves the trust-time
/// install resolves the same base as the startup load. And the rule must
/// be listed with its relative prefix INTACT (`When::PathsUnder("src")`),
/// not rewritten to an absolute path -- the base is threaded to the
/// broker's canonicalization, never folded into the stored `Rule`, so the
/// review surface shows and the revoke addresses exactly what the file
/// says.
#[tokio::test]
async fn a_relative_paths_under_prefix_resolves_against_the_project_not_the_process_cwd() {
    let project = TempDir::new().expect("tempdir");
    std::fs::create_dir(project.path().join("src")).expect("mkdir <project>/src");
    std::fs::write(
        project.path().join("src").join("file.txt"),
        b"inside the project src",
    )
    .expect("write fixture");

    let (_xdg, env) = isolated_env();
    write_project_permissions(
        &project,
        r#"{"rules":[{"select":{"tools":["read"]},"when":{"paths_under":"src"},"then":"allow"}]}"#,
    );

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "operator said no".into(),
    });
    let conway = build_conway(
        project.path(),
        vec![
            // Turn 1: authorized by the relative-prefix rule (gate bypassed).
            ScriptedTurn::Respond(read_call_response("src/file.txt")),
            ScriptedTurn::Respond(text_response("done")),
            // Turn 2: same-named path under the PROCESS cwd -- must ask.
            ScriptedTurn::Respond(read_call_response(
                &std::env::current_dir()
                    .expect("process cwd")
                    .join("src")
                    .join("lib.rs")
                    .display()
                    .to_string(),
            )),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    // The project file's allow rule is held for trust (never silently
    // installed), then installed by the real trust path.
    let report = conway.load_permission_files(
        project.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );
    assert!(
        report.registration_errors.is_empty(),
        "a relative paths_under prefix is structurally valid: {:?}",
        report.registration_errors
    );
    assert_eq!(
        report.notices.len(),
        1,
        "the untrusted project file's allow rule must be held with a notice: {:?}",
        report.notices
    );
    let project_file = project.path().join(".conway").join("permissions.json");
    let installed = conway
        .trust_permission_file(
            &env,
            &project_file,
            PermissionScope::Session,
            AgentId::new(),
        )
        .expect("trust the project file");
    assert_eq!(
        installed.installed, 1,
        "the relative-prefix rule installs once trusted"
    );

    // The stored rule keeps its relative prefix VERBATIM -- the base is a
    // canonicalization input, never a rewrite of the rule the operator
    // wrote (what the review surface shows is what a revoke addresses).
    let listed = conway.active_structured_allow_rules();
    assert_eq!(listed.len(), 1, "the trusted rule is listed: {listed:?}");
    assert!(
        matches!(&listed[0].0.when, conway::When::PathsUnder(p) if p == "src"),
        "the relative prefix must survive install unrewritten: {:?}",
        listed[0].0
    );

    // Turn 1 (observable): a read INSIDE the project's src/ is authorized
    // without the operator's gate -- the rule confined to the PROJECT tree.
    // (One prompt per session in this harness; the broker -- and with it
    // the installed rule -- is shared across a `Conway`'s sessions.)
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("read the file").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");
    assert!(
        gate.requests().is_empty(),
        "a relative `paths_under` prefix must authorize an in-tree read under the PROJECT: \
         {:?}",
        gate.requests()
    );

    // Turn 2 (observable): the same-named path under the PROCESS cwd is NOT
    // under the rule's boundary -- it reaches the gate. This is the exact
    // failure mode B2 fixes: before it, the rule's root WAS the process
    // cwd's `src/`, and this read would have auto-allowed.
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle
        .prompt("read the other src file")
        .await
        .expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");
    assert_eq!(
        gate.requests().len(),
        1,
        "the rule must NOT match a same-named path under the process cwd -- a relative prefix \
         resolves against the project, not wherever conway was launched from"
    );
}

/// The DENY half of the B2 base (code-review coverage gap): a relative
/// `paths_under` deny rule in an UNTRUSTED project file installs
/// unconditionally (D4 §3) and must confine to the REPO's tree -- a read of
/// `<project>/src/secret.txt` is denied WITHOUT the operator's gate (turn 1:
/// zero gate requests, because a deny match is decided before the gate),
/// while the same-named path under the PROCESS's cwd is NOT under the
/// rule's boundary and reaches the gate (turn 2). The gate answers Allow,
/// so the outcomes are distinguishable: if the rule's base were the process
/// cwd (the pre-B2 bug), turn 1's read would not match, would reach the
/// gate, and would be ALLOWED -- the silent fail-open this test pins
/// against.
#[tokio::test]
async fn a_relative_paths_under_deny_confines_to_the_project_not_the_process_cwd() {
    let project = TempDir::new().expect("tempdir");
    std::fs::create_dir(project.path().join("src")).expect("mkdir <project>/src");
    std::fs::write(
        project.path().join("src").join("secret.txt"),
        b"repo secret",
    )
    .expect("write fixture");

    let (_xdg, env) = isolated_env();
    write_project_permissions(
        &project,
        r#"{"rules":[{"select":{"tools":["read"]},"when":{"paths_under":"src"},"then":"deny"}]}"#,
    );

    // The gate ALLOWS everything it is asked: a read that reaches it is
    // allowed, so only a deny-rule match can stop the in-project read.
    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway(
        project.path(),
        vec![
            // Turn 1: the deny rule fires on the in-project read -- decided
            // before the gate (zero requests).
            ScriptedTurn::Respond(read_call_response("src/secret.txt")),
            ScriptedTurn::Respond(text_response("done")),
            // Turn 2: same-named path under the PROCESS cwd -- NOT confined,
            // reaches the gate (which allows it).
            ScriptedTurn::Respond(read_call_response(
                &std::env::current_dir()
                    .expect("process cwd")
                    .join("src")
                    .join("lib.rs")
                    .display()
                    .to_string(),
            )),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    // The deny installs unconditionally at load -- no trust decision (D4 §3).
    let report = conway.load_permission_files(
        project.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );
    assert!(
        report.registration_errors.is_empty(),
        "a relative paths_under prefix is structurally valid: {:?}",
        report.registration_errors
    );

    // Turn 1 (observable): denied by the RULE, before the gate -- if the
    // base were the process cwd, the rule would not match `src/secret.txt`
    // under the project and the read would reach the (allowing) gate.
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("read the secret").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");
    assert!(
        gate.requests().is_empty(),
        "a relative `paths_under` deny must fire on the in-project read -- decided before the \
         gate, zero requests: {:?}",
        gate.requests()
    );

    // Turn 2 (observable): the same-named path under the PROCESS cwd is NOT
    // under the rule's boundary -- it reaches the gate.
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle
        .prompt("read the other src file")
        .await
        .expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");
    assert_eq!(
        gate.requests().len(),
        1,
        "the deny rule must NOT reach outside the project tree"
    );
}

/// The base is derived from the FILE's own location (code-review coverage
/// gap), so a project file discovered in an ANCESTOR directory resolves its
/// relative prefix against THAT ancestor -- not against the (deeper) launch
/// cwd. Layout: `ancestor/.conway/{settings.json,permissions.json}` (a
/// relative `paths_under: "src"` allow), launched from `ancestor/subdir/`.
/// A read of `ancestor/src/file.txt` is authorized (turn 1, gate bypassed);
/// a read of `ancestor/subdir/src/file.txt` -- where a cwd-relative base
/// would point the rule -- is NOT (turn 2, reaches the gate).
#[tokio::test]
async fn a_relative_paths_under_prefix_in_an_ancestor_file_resolves_against_the_ancestor() {
    let ancestor = TempDir::new().expect("tempdir");
    let subdir = ancestor.path().join("subdir");
    std::fs::create_dir(&subdir).expect("mkdir subdir");
    std::fs::create_dir(ancestor.path().join("src")).expect("mkdir ancestor/src");
    std::fs::create_dir(subdir.join("src")).expect("mkdir subdir/src");
    std::fs::write(
        ancestor.path().join("src").join("file.txt"),
        b"ancestor src",
    )
    .expect("write");
    std::fs::write(subdir.join("src").join("file.txt"), b"subdir src").expect("write");

    // `discover` finds the project via the nearest `.conway/settings.json`
    // walking up from the launch cwd.
    let conway_dir = ancestor.path().join(".conway");
    std::fs::create_dir_all(&conway_dir).expect("mkdir ancestor/.conway");
    std::fs::write(conway_dir.join("settings.json"), "").expect("write settings.json");
    std::fs::write(
        conway_dir.join("permissions.json"),
        r#"{"rules":[{"select":{"tools":["read"]},"when":{"paths_under":"src"},"then":"allow"}]}"#,
    )
    .expect("write permissions.json");

    let (_xdg, env) = isolated_env();
    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "operator said no".into(),
    });
    let conway = build_conway(
        &subdir,
        vec![
            // Turn 1: ancestor/src -- under the rule's boundary (gate bypassed).
            ScriptedTurn::Respond(read_call_response(
                &ancestor
                    .path()
                    .join("src")
                    .join("file.txt")
                    .display()
                    .to_string(),
            )),
            ScriptedTurn::Respond(text_response("done")),
            // Turn 2: subdir/src -- where a cwd-relative base would point it; must ask.
            ScriptedTurn::Respond(read_call_response(
                &subdir.join("src").join("file.txt").display().to_string(),
            )),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    let report =
        conway.load_permission_files(&subdir, &env, PermissionScope::Session, AgentId::new());
    assert!(
        report.registration_errors.is_empty(),
        "no registration errors expected: {:?}",
        report.registration_errors
    );
    let ancestor_file = conway_dir.join("permissions.json");
    let installed = conway
        .trust_permission_file(
            &env,
            &ancestor_file,
            PermissionScope::Session,
            AgentId::new(),
        )
        .expect("trust the ancestor file");
    assert_eq!(
        installed.installed, 1,
        "the ancestor file's rule installs once trusted"
    );

    // Turn 1 (observable): ancestor/src auto-allows -- the base is the
    // ancestor, not the launch cwd.
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle
        .prompt("read the ancestor file")
        .await
        .expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");
    assert!(
        gate.requests().is_empty(),
        "a relative prefix in an ancestor-discovered file must confine to the ANCESTOR: {:?}",
        gate.requests()
    );

    // Turn 2 (observable): subdir/src -- the path a cwd-relative base would
    // have confined -- reaches the gate.
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("read the subdir file").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");
    assert_eq!(
        gate.requests().len(),
        1,
        "a cwd-relative base would have confined subdir/src -- the ancestor base must not"
    );
}

// =====================================================================
// (b) TRAP 2: `PathArgs::Unconfinable` never satisfies `paths_under`.
// =====================================================================

/// A structured `paths_under` allow rule selecting `bash` (whose
/// `PathArgs::Unconfinable` marks its `command` as something the broker
/// cannot statically confine) must NEVER fire -- `bash echo hi` under the
/// rule's prefix still reaches the gate. The agent is unconfined, so without
/// the rule there is no gate consultation; the rule is what WOULD authorize
/// it, and it correctly refuses because `Unconfinable` fails closed.
#[tokio::test]
async fn an_unconfinable_tool_never_satisfies_paths_under_so_bash_reaches_the_gate() {
    let root_dir = TempDir::new().expect("tempdir");
    let root_canon = root_dir.path().canonicalize().expect("canonicalize root");

    let (xdg, env) = isolated_env();
    write_global_permissions(
        &xdg,
        &format!(
            r#"{{"rules":[{{"select":{{"tools":["bash"]}},"when":{{"paths_under":"{}"}},"then":"allow"}}]}}"#,
            root_canon.display()
        ),
    );

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "operator said no".into(),
    });
    let conway = build_conway(
        root_dir.path(),
        vec![
            ScriptedTurn::Respond(bash_call_response("echo hi")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    let report = conway.load_permission_files(
        root_dir.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );
    assert!(
        report.registration_errors.is_empty(),
        "a paths_under bash rule is structurally valid (Unconfinable is not a registration \
         error -- the broker simply never matches it): {:?}",
        report.registration_errors
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("run the command").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");

    assert_eq!(
        gate.requests().len(),
        1,
        "an Unconfinable tool must NEVER satisfy a `paths_under` rule -- `bash echo hi` must \
         reach the gate even though the rule selects `bash` and the cwd is under the prefix"
    );
}

// =====================================================================
// (c) TRAP 3: `command_prefix` on a `Structured` tool is a registration error.
// =====================================================================

/// A structured rule pairing `command_prefix` (a shell-token predicate) with
/// a tool whose `render_kind` is `Structured` (a JSON-dump rendering whose
/// token boundaries the operator cannot predict) can never reliably match --
/// the `68ea9b1` `read:*`-matched-nothing bug, re-imagined for the structured
/// form. The loader refuses to install it silently; it surfaces a typed
/// `RuleRegistrationError` in the load report instead (: untrusted input
/// -> typed errors, never panics, never a silent inert rule).
#[tokio::test]
async fn command_prefix_on_a_structured_tool_is_a_registration_error() {
    let project = TempDir::new().expect("tempdir");
    let (xdg, env) = isolated_env();
    // A GLOBAL file (trusted-by-authorship) so the allow rule's registration
    // check actually runs (allow rules from an UNTRUSTED project file are
    // skipped before the registration check; deny rules check regardless).
    write_global_permissions(
        &xdg,
        r#"{"rules":[{"select":{"tools":["read"]},"when":{"command_prefix":"read"},"then":"allow"}]}"#,
    );

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "unused".into(),
    });
    let conway = build_conway(
        project.path(),
        vec![ScriptedTurn::Respond(text_response("no call needed"))],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    let report = conway.load_permission_files(
        project.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );

    assert_eq!(
        report.registration_errors.len(),
        1,
        "a `command_prefix` rule on `read` (Structured render) must surface exactly one typed \
         registration error, not install silently: {:?}",
        report.registration_errors
    );
    assert_eq!(
        report.registration_errors[0].reason,
        RuleRegistrationReason::CommandPrefixOnStructuredTool,
        "the error must name the precise reason"
    );
    assert!(
        matches!(
            &report.registration_errors[0].rule.select,
            conway::Select::Tools(t) if t == &["read".to_string()]
        ),
        "the rejected rule is carried whole so the operator sees exactly what was refused"
    );
}

// =====================================================================
// (c2) B1: a `paths_under` DENY rule on an Unconfinable tool (`bash`)
//      is a typed registration error -- never silently fail-open.
// =====================================================================

/// **B1, pinned.** A structured `paths_under` DENY rule selecting `bash`
/// (whose `PathArgs::Unconfinable` marks its `command` as something the
/// broker cannot statically confine) can never match -- `paths_under_match`
/// returns `false` for `Unconfinable`, so the deny is silently inert and the
/// bash call the operator expected to be refused instead goes through
/// (fail-OPEN). The loader refuses to install it silently and surfaces a
/// typed `PathsUnderOnUnconfinedTool` registration error in the load report
/// instead (: operator-visible via A1's `registration_errors` channel;
///: typed error, never a panic). The rule is NOT installed, so there is
/// no inert deny pattern lying in wait for a call the operator believed was
/// refused.
///
/// This is the deny-side mirror of the existing allow-side pin
/// `an_unconfinable_tool_never_satisfies_paths_under_so_bash_reaches_the_gate`:
/// on the ALLOW side inertness is fail-CLOSED (bash falls through to the
/// gate) and is NOT a registration error; on the DENY/PROMPT side inertness
/// is fail-OPEN and IS a registration error. The two tests together pin the
/// asymmetry.
#[tokio::test]
async fn a_paths_under_deny_rule_on_an_unconfinable_tool_is_a_registration_error() {
    let project = TempDir::new().expect("tempdir");
    let root_canon = project
        .path()
        .canonicalize()
        .expect("canonicalize project root");
    let (xdg, env) = isolated_env();
    // A GLOBAL file (trusted-by-authorship) so the deny rule's registration
    // check runs (deny rules install from every file, but the registration
    // check gates installation regardless of trust).
    write_global_permissions(
        &xdg,
        &format!(
            r#"{{"rules":[{{"select":{{"tools":["bash"]}},"when":{{"paths_under":"{}"}},"then":"deny"}}]}}"#,
            root_canon.display()
        ),
    );

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway(
        project.path(),
        vec![
            ScriptedTurn::Respond(bash_call_response("echo hi")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    let report = conway.load_permission_files(
        project.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );

    assert_eq!(
        report.registration_errors.len(),
        1,
        "a `paths_under` deny rule on `bash` (Unconfinable) must surface exactly one typed \
         registration error -- it can never match and is fail-open if installed silently: {:?}",
        report.registration_errors
    );
    assert_eq!(
        report.registration_errors[0].reason,
        RuleRegistrationReason::PathsUnderOnUnconfinedTool,
        "the error must name the precise reason B1 introduced"
    );
    assert!(
        matches!(
            &report.registration_errors[0].rule.select,
            conway::Select::Tools(t) if t == &["bash".to_string()]
        ),
        "the rejected rule is carried whole so the operator sees exactly what was refused"
    );

    // The rule was NOT installed, so a bash call is not silently denied by an
    // inert rule -- but it is also not silently ALLOWED by virtue of the
    // operator's deny being inert. The call reaches the operator's gate
    // (AutoAllow is off, no allow rule matches), which is the honest outcome:
    // the operator is informed (registration error) and the call is gated
    // rather than silently passing the refused-by-rule expectation. The
    // RecordingGate is set to AllowOnce so the turn completes without hanging.
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("run the command").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");

    assert_eq!(
        gate.requests().len(),
        1,
        "with the inert deny rule NOT installed, the bash call reaches the operator's gate \
         (the honest outcome -- the operator was informed, the call was not silently allowed)"
    );
}

/// **B1, pinned -- the category fallback.** A `paths_under` DENY rule on a
/// `Select::Categories` containing an `Unconfinable` tool cannot be rejected
/// at install time (the category's member tools may register later in the
/// session -- the same load-order-hazard reasoning that keeps the
/// `CommandPrefix` check off categories). The decision-time fail-closed in
/// `rule_denies_or_prompts` is the fallback: a `bash` call (in the
/// `Execute` category, `Unconfinable`) under a `paths_under` DENY rule
/// matching the category is REFUSED at decision time -- never silently
/// allowed. The agent runs UNCONFINED with AutoAllow off, so without the
/// deny rule the call would reach the gate; the deny rule matching the
/// category must refuse it BEFORE the gate (zero gate requests), proving the
/// decision-time guard fired through the real seam.
#[tokio::test]
async fn a_paths_under_deny_rule_on_a_category_with_an_unconfinable_tool_refuses_at_decision_time()
{
    let project = TempDir::new().expect("tempdir");
    let root_canon = project
        .path()
        .canonicalize()
        .expect("canonicalize project root");
    let (xdg, env) = isolated_env();
    write_global_permissions(
        &xdg,
        &format!(
            r#"{{"rules":[{{"select":{{"categories":["execute"]}},"when":{{"paths_under":"{}"}},"then":"deny"}}]}}"#,
            root_canon.display()
        ),
    );

    // AllowOnce so that IF the deny failed to fire the call would be allowed
    // (gate consulted, one request) -- the break-the-guard observable.
    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway(
        project.path(),
        vec![
            ScriptedTurn::Respond(bash_call_response("echo hi")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    let report = conway.load_permission_files(
        project.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );
    assert!(
        report.registration_errors.is_empty(),
        "a category select is not inspectable at install time (load-order hazard); it is NOT a \
         registration error -- the decision-time fail-closed handles it: {:?}",
        report.registration_errors
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("run the command").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");

    assert!(
        gate.requests().is_empty(),
        "a `paths_under` deny rule matching a category containing an Unconfinable tool (`bash`) \
         must REFUSE the call at decision time -- the decision-time fail-closed fired through the \
         real seam; the call must never be silently allowed: {:?}",
        gate.requests()
    );
}

// =====================================================================
// (c3) B3: a `paths_under` rule whose prefix FAILS to canonicalize is
//      surfaced as a typed registration error -- never silently dropped.
//      (Distinct from B1: there the prefix canonicalizes fine but the
//      tool's PathArgs can never be confined; here the prefix ITSELF does
//      not resolve on disk.)
// =====================================================================

/// **B3, pinned -- the allow arm.** A structured `paths_under` ALLOW rule
/// whose prefix does not exist on disk (`"nonexistent-dir"`, never created)
/// is dropped by the broker: `canonicalize_when` -> `CanonicalRoot::new`
/// fails, so `remember_pattern_rule` returns `false`. The loader surfaces
/// that as a typed `PathsUnderPrefixUncanonicalizable` registration error
/// (: operator-visible via A1's `registration_errors` channel:
/// typed error, never a panic) instead of silently swallowing the `bool`.
/// The rule is NOT installed, so a `read` call the operator expected to be
/// auto-authorized instead reaches the operator's gate (fail-CLOSED -- the
/// honest outcome, the operator informed). This is the mirror of the
/// `68ea9b1` `read:*`-matched-nothing bug: a rule that can never match is a
/// lie the operator will not notice.
#[tokio::test]
async fn a_paths_under_allow_rule_with_a_prefix_that_cannot_canonicalize_surfaces_a_registration_error(
) {
    let project = TempDir::new().expect("tempdir");
    // A real file to read -- the call is observable through the real
    // `ReadTool` -> `PermissionBroker::decide` seam.
    std::fs::write(project.path().join("file.txt"), b"inside the project")
        .expect("write fixture file");
    let (xdg, env) = isolated_env();
    // A GLOBAL file (trusted-by-authorship) so the allow rule's install path
    // runs unconditionally. The prefix `nonexistent-dir` is never created, so
    // `CanonicalRoot::new(<project>/nonexistent-dir)` fails.
    write_global_permissions(
        &xdg,
        r#"{"rules":[{"select":{"tools":["read"]},"when":{"paths_under":"nonexistent-dir"},"then":"allow"}]}"#,
    );

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway(
        project.path(),
        vec![
            ScriptedTurn::Respond(read_call_response(
                &project.path().join("file.txt").display().to_string(),
            )),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    let report = conway.load_permission_files(
        project.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );

    assert_eq!(
        report.registration_errors.len(),
        1,
        "a `paths_under` allow rule whose prefix does not resolve on disk must surface exactly \
         one typed registration error -- it was silently dropped before B3: {:?}",
        report.registration_errors
    );
    assert_eq!(
        report.registration_errors[0].reason,
        RuleRegistrationReason::PathsUnderPrefixUncanonicalizable,
        "the error must name the precise reason B3 introduced (distinct from B1's \
         PathsUnderOnUnconfinedTool -- here the prefix itself is bad, not the tool)"
    );
    assert!(
        matches!(
            &report.registration_errors[0].rule.when,
            conway::When::PathsUnder(p) if p == "nonexistent-dir"
        ),
        "the rejected rule is carried whole so the operator sees exactly what was refused: {:?}",
        report.registration_errors[0].rule
    );

    // The rule was NOT installed, so the read is NOT auto-authorized -- it
    // reaches the operator's gate (fail-CLOSED: the operator was informed,
    // and the call is gated rather than silently passing).
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("read the file").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");
    assert_eq!(
        gate.requests().len(),
        1,
        "with the bad-prefix allow rule NOT installed, the read reaches the operator's gate \
         (fail-closed -- the operator was informed, the call was not silently allowed): {:?}",
        gate.requests()
    );
}

/// **** A `paths_under` prefix
/// carrying a NUL byte (JSON's `\u0000` escape, legal, decodes to a real embedded
/// NUL) hits `canonicalize_when`'s call to `resolve_like_the_tool_will` --
/// now a thin wrapper over the ONE shared implementation,
/// `conway_core::containment::resolve_candidate`. This is a SECOND,
/// distinct production callsite from the plain `check_root` path-argument
/// resolution `read_with_nul_byte_path_under_root_is_denied_not_bypassed`
/// (in `root_containment_seam.rs`) exercises -- `resolve_like_the_tool_will`
/// has (at least) two call sites inside `PermissionBroker`, and each must
/// independently reach the guard, not just one. Same fail-closed shape as
/// B3's nonexistent-directory case: the rule is dropped, surfaced as a
/// typed `PathsUnderPrefixUncanonicalizable` registration error, and the
/// call it would have auto-authorized instead reaches the operator's gate.
///
/// **Disclosed asymmetry with `root_containment_seam.rs`'s sibling test:**
/// unlike `check_root`'s per-call candidate (resolved via
/// `CanonicalRoot::contains`'s walk-up-then-rejoin-the-nonexistent-tail
/// algorithm, which can rejoin a NUL-carrying TAIL onto an already-canonical
/// prefix without ever re-canonicalizing it), THIS callsite hands the
/// resolved candidate straight to `CanonicalRoot::new`, which canonicalizes
/// the WHOLE path in one `fs::canonicalize` call with no tail to skip. A
/// NUL byte anywhere in that call unconditionally fails at the OS level
/// (`CString::new` cannot represent an interior NUL), so removing
/// `resolve_candidate`'s own guard does NOT turn this test red: the
/// unconditional OS-level rejection inside `CanonicalRoot::new` is an
/// independent, airtight backstop for a full-path canonicalize, and this
/// test's persisted result (`PathsUnderPrefixUncanonicalizable`) is
/// identical either way. The guard is still correct to keep here (a
/// faster, filesystem-free, more specific typed rejection, and "call the
/// shared implementation rather than restate it" per this item's own
/// mandate) -- but this specific callsite's mutation test cannot be the
/// proof of a security regression the way `check_root`'s sibling test is;
/// that proof lives in `root_containment_seam.rs`, whose walk-up shape
/// genuinely CAN diverge without the explicit guard.
#[tokio::test]
async fn a_paths_under_allow_rule_with_a_nul_byte_in_its_prefix_surfaces_a_registration_error() {
    let project = TempDir::new().expect("tempdir");
    std::fs::write(project.path().join("file.txt"), b"inside the project")
        .expect("write fixture file");
    let (xdg, env) = isolated_env();
    // JSON's `\u0000` escape is legal; serde_json decodes it to a real NUL
    // byte inside the parsed `String` -- untrusted config content, not a
    // Rust source literal quirk.
    write_global_permissions(
        &xdg,
        r#"{"rules":[{"select":{"tools":["read"]},"when":{"paths_under":"bad-\u0000-prefix"},"then":"allow"}]}"#,
    );

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway(
        project.path(),
        vec![
            ScriptedTurn::Respond(read_call_response(
                &project.path().join("file.txt").display().to_string(),
            )),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    let report = conway.load_permission_files(
        project.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );

    assert_eq!(
        report.registration_errors.len(),
        1,
        "a `paths_under` allow rule whose prefix contains a NUL byte must surface exactly one \
         typed registration error -- a NUL-carrying prefix that silently resolved would be the \
         defect this item exists to prevent: {:?}",
        report.registration_errors
    );
    assert_eq!(
        report.registration_errors[0].reason,
        RuleRegistrationReason::PathsUnderPrefixUncanonicalizable,
        "the NUL-carrying prefix must fail via the SAME reason the nonexistent-directory case \
         does -- canonicalize_when's `resolve_like_the_tool_will` call returning `None` is folded \
         into the same fail-closed outcome as `CanonicalRoot::new` failing"
    );

    // The rule was NOT installed, so the read reaches the operator's gate
    // (fail-closed) rather than being silently auto-authorized against a
    // NUL-carrying boundary nobody could actually have granted.
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("read the file").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");
    assert_eq!(
        gate.requests().len(),
        1,
        "with the NUL-prefix allow rule NOT installed, the read reaches the operator's gate: {:?}",
        gate.requests()
    );
}

/// **B3, pinned -- the deny and prompt arms.** A `paths_under` DENY or
/// PROMPT rule whose prefix does not exist on disk is dropped by the broker
/// (`remember_deny_rule`/`remember_prompt_rule` return `false`). The hazard
/// is sharpest on the deny/prompt side: the operator believed a `paths_under`
/// deny was protecting them when it was never installed (fail-OPEN against
/// the operator's expectation). The loader surfaces BOTH as typed
/// `PathsUnderPrefixUncanonicalizable` registration errors so the operator
/// learns each narrowing rule never installed -- not just the allow variant.
#[tokio::test]
async fn paths_under_deny_and_prompt_rules_with_a_bad_prefix_each_surface_a_registration_error() {
    let project = TempDir::new().expect("tempdir");
    // A real file to read -- proves the dropped deny/prompt rules are inert
    // at decision time (the call reaches the gate, not silently denied nor
    // silently allowed) through the real ReadTool -> PermissionBroker seam.
    std::fs::write(project.path().join("file.txt"), b"inside the project")
        .expect("write fixture file");
    let (xdg, env) = isolated_env();
    // A GLOBAL file with one deny and one prompt rule, each over a
    // nonexistent prefix. Deny/prompt install from every file
    // unconditionally (D4 §3), so both install paths run.
    write_global_permissions(
        &xdg,
        r#"{"rules":[
            {"select":{"tools":["read"]},"when":{"paths_under":"missing-deny"},"then":"deny"},
            {"select":{"tools":["read"]},"when":{"paths_under":"missing-prompt"},"then":"prompt"}
        ]}"#,
    );

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway(
        project.path(),
        vec![
            ScriptedTurn::Respond(read_call_response(
                &project.path().join("file.txt").display().to_string(),
            )),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    let report = conway.load_permission_files(
        project.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );

    let deny_errs: Vec<_> = report
        .registration_errors
        .iter()
        .filter(|e| {
            matches!(
                &e.rule.when,
                conway::When::PathsUnder(p) if p == "missing-deny"
            )
        })
        .collect();
    let prompt_errs: Vec<_> = report
        .registration_errors
        .iter()
        .filter(|e| {
            matches!(
                &e.rule.when,
                conway::When::PathsUnder(p) if p == "missing-prompt"
            )
        })
        .collect();

    assert_eq!(
        deny_errs.len(),
        1,
        "the deny rule with a bad prefix surfaces exactly one registration error: {:?}",
        report.registration_errors
    );
    assert_eq!(
        deny_errs[0].reason,
        RuleRegistrationReason::PathsUnderPrefixUncanonicalizable,
        "the deny error must name B3's reason"
    );
    assert_eq!(
        prompt_errs.len(),
        1,
        "the prompt rule with a bad prefix surfaces exactly one registration error: {:?}",
        report.registration_errors
    );
    assert_eq!(
        prompt_errs[0].reason,
        RuleRegistrationReason::PathsUnderPrefixUncanonicalizable,
        "the prompt error must name B3's reason -- the operator learns their narrowing rule \
         never installed, not just the allow variant"
    );
    assert_eq!(
        report.registration_errors.len(),
        2,
        "exactly two registration errors total (one per bad-prefix rule), no more: {:?}",
        report.registration_errors
    );

    // Fail-closed posture (): the dropped deny/prompt rules were never
    // installed, so a matching `read` call is neither silently denied (the
    // deny never fired) nor silently allowed -- it reaches the operator's
    // gate. Symmetric with the allow-arm and trust tests' gate assertion;
    // proves the dropped narrowing rules are inert at decision time, not
    // enforced or bypassed.
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("read the file").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");
    assert_eq!(
        gate.requests().len(),
        1,
        "with the bad-prefix deny/prompt rules NOT installed, the read reaches the operator's \
         gate -- the dropped narrowing rules are inert (neither deny nor forced prompt fires), \
         so the call is gated rather than silently refused or silently allowed: {:?}",
        gate.requests()
    );
}

/// **B3, pinned -- the `/trust permissions` path.** A project file's
/// `paths_under` allow rule with a nonexistent prefix is held for trust,
/// then `trust_permission_file` is invoked. Before B3 the trust path
/// discarded the install `bool` and counted the dropped rule as installed
/// (`count += 1` unconditional) -- so `/trust permissions` reported "1
/// allow rule(s) installed" for a rule the broker dropped as
/// uncanonicalizable. The fix honors the `bool`: the dropped rule is NOT
/// counted, and the same `PathsUnderPrefixUncanonicalizable` registration
/// error is surfaced through the trust report (: operator-visible --
/// the TUI renders each through the same `Entry::Error { fatal: false }`
/// channel the load path uses). The rule is NOT installed, so a matching
/// read still reaches the gate.
#[tokio::test]
async fn trusting_a_project_file_with_a_bad_prefix_does_not_count_the_dropped_rule_and_informs_the_operator(
) {
    let project = TempDir::new().expect("tempdir");
    std::fs::write(project.path().join("file.txt"), b"inside the project")
        .expect("write fixture file");
    let (_xdg, env) = isolated_env();
    write_project_permissions(
        &project,
        r#"{"rules":[{"select":{"tools":["read"]},"when":{"paths_under":"nonexistent-dir"},"then":"allow"}]}"#,
    );

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway(
        project.path(),
        vec![
            ScriptedTurn::Respond(read_call_response(
                &project.path().join("file.txt").display().to_string(),
            )),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );

    // Load: the project file's allow rule is held for trust (not installed
    // yet), so no registration error at startup -- the bad prefix is not
    // even probed until the rule is trusted and installed.
    let report = conway.load_permission_files(
        project.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );
    assert!(
        report.registration_errors.is_empty(),
        "an untrusted project file's allow rules are not installed yet, so no bad-prefix \
         registration error at load: {:?}",
        report.registration_errors
    );
    assert_eq!(
        report.notices.len(),
        1,
        "the untrusted project file's allow rule is held with a notice: {:?}",
        report.notices
    );

    // Trust: the bad-prefix rule is probed, dropped by the broker, and
    // surfaced -- NOT counted as installed.
    let project_file = project.path().join(".conway").join("permissions.json");
    let trust_report = conway
        .trust_permission_file(
            &env,
            &project_file,
            PermissionScope::Session,
            AgentId::new(),
        )
        .expect("trust the project file");
    assert_eq!(
        trust_report.installed, 0,
        "the bad-prefix rule must NOT be counted as installed -- `/trust permissions` must \
         never report `1 installed` for a rule the broker dropped as uncanonicalizable"
    );
    assert_eq!(
        trust_report.registration_errors.len(),
        1,
        "the trust path surfaces the same typed registration error as the load path: {:?}",
        trust_report.registration_errors
    );
    assert_eq!(
        trust_report.registration_errors[0].reason,
        RuleRegistrationReason::PathsUnderPrefixUncanonicalizable,
        "the trust-path error must name B3's reason"
    );

    // The rule was NOT installed, so the read is NOT auto-authorized -- it
    // reaches the operator's gate (fail-CLOSED). The operator was informed
    // (registration error in the trust report) and the call is gated rather
    // than silently allowed.
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("read the file").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");
    assert_eq!(
        gate.requests().len(),
        1,
        "with the bad-prefix rule NOT installed, the read reaches the operator's gate: {:?}",
        gate.requests()
    );
}

// =====================================================================
// (d) byte-identical decisions: one evaluator, not two.
// =====================================================================

/// **THE headline equivalence proof.** A flat `bash:git status` rule and its
/// structured equivalent `tools(["bash"]) + command_prefix("git status") +
/// allow` produce BYTE-IDENTICAL gate decisions across a matrix of bash
/// calls: an exact match, a subcommand match, a different command, and a
/// chained command. Both rules are loaded from a real global
/// `permissions.json` through the real `load_permission_files` ->
/// `PermissionBroker` seam, and the matrix is driven through the real
/// `BashTool::render` -> `render_call` -> `decide` path. The gate request
/// COUNTS are equal for every command -- the strongest available evidence
/// there is one evaluator and not two.
///
/// **AMENDED by board item `01KZDDPC5MMD49F6JPV9CW4TVM`.** Every case in
/// the matrix now reaches the gate (count `1`), including the exact and
/// subcommand matches this test used to expect to auto-allow (count `0`):
/// a durable pattern grant no longer exists for `bash` at all (see
/// `conway_core::permission_pattern`'s own module doc), so there is no
/// longer a command that clears it. What this test still proves, and the
/// reason it survives rather than being deleted, is that the flat and
/// structured forms agree on that refusal for EVERY case, uniformly --
/// exactly the "one evaluator, not two" property named above. Trap 4's
/// chained-command proof lives on unchanged, just no longer contrasted
/// against a matching case that auto-allows.
#[tokio::test]
async fn flat_and_structured_command_prefix_produce_byte_identical_gate_decisions() {
    let (xdg_flat, env_flat) = isolated_env();
    write_global_permissions(&xdg_flat, r#"{"allow":["bash:git status"]}"#);

    let (xdg_struct, env_struct) = isolated_env();
    write_global_permissions(
        &xdg_struct,
        r#"{"rules":[{"select":{"tools":["bash"]},"when":{"command_prefix":"git status"},"then":"allow"}]}"#,
    );

    // The matrix: exact, subcommand, different, chained. Each is run
    // against BOTH the flat and structured conway instances, and the
    // gate-request count (0 = auto-allowed, 1 = reached the operator) must
    // match between them.
    let matrix = [
        "git status",
        "git status --short",
        "git push --force",
        "git status && rm -rf /tmp/should-never-run",
    ];

    for command in matrix {
        let flat_count = run_bash_and_count_gate(&xdg_flat, &env_flat, command).await;
        let struct_count = run_bash_and_count_gate(&xdg_struct, &env_struct, command).await;
        assert_eq!(
            flat_count, struct_count,
            "flat `bash:git status` and structured `command_prefix(\"git status\")` must produce \
             byte-identical gate decisions for `{command}` -- one evaluator, not two"
        );
        assert_eq!(
            flat_count, 1,
            "no `bash` command -- matching the granted prefix or not -- may be auto-allowed \
             by a pattern grant any more: `{command}`"
        );
    }
}

/// Runs one `bash` call end to end against a fresh conway built with the
/// global permissions file at `xdg`, and returns the number of requests the
/// gate saw (0 = auto-allowed by a rule, 1 = reached the operator). The gate
/// always denies so a chained command never actually executes.
async fn run_bash_and_count_gate(
    _xdg: &TempDir,
    env: &HashMap<String, String>,
    command: &str,
) -> usize {
    let cwd = TempDir::new().expect("tempdir");
    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "operator said no".into(),
    });
    let conway = build_conway(
        cwd.path(),
        vec![
            ScriptedTurn::Respond(bash_call_response(command)),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    conway.load_permission_files(cwd.path(), env, PermissionScope::Session, AgentId::new());
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("do the thing").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");
    gate.requests().len()
}

// =====================================================================
// (5) Plugin-contributed narrowing rules install from every file
//     unconditionally (deny/prompt), even from an UNTRUSTED project file.
// =====================================================================

/// A structured `deny` rule from an UNTRUSTED project file installs
/// immediately (D4 §3: narrowing has no trust precondition) and refuses a
/// matching call BEFORE the gate is ever consulted -- the structured form's
/// half of the asymmetry the flat `deny` list has always had.
#[tokio::test]
async fn a_structured_deny_rule_from_an_untrusted_project_file_refuses_before_the_gate() {
    let project = TempDir::new().expect("tempdir");
    let (_xdg, env) = isolated_env();
    write_project_permissions(
        &project,
        r#"{"rules":[{"select":{"tools":["bash"]},"when":{"command_prefix":"curl"},"then":"deny"}]}"#,
    );

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway(
        project.path(),
        vec![
            ScriptedTurn::Respond(bash_call_response("curl https://example.com")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    let report = conway.load_permission_files(
        project.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );
    assert!(
        report.registration_errors.is_empty(),
        "a bash command_prefix deny rule is structurally valid: {:?}",
        report.registration_errors
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("curl it").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");

    assert!(
        gate.requests().is_empty(),
        "a structured deny rule must refuse `curl` before the operator's gate is ever \
         consulted, even from an untrusted project file: {:?}",
        gate.requests()
    );
}

/// A structured `prompt` rule from an UNTRUSTED project file installs
/// immediately and forces a matching call to the operator (the gate sees
/// it) instead of auto-allowing it. This is the second narrowing effect,
/// admitted unconditionally at extension-architecture §5.5 stage 1.
///
/// To make the test HONEST -- "the call reached the gate BECAUSE of the
/// prompt rule, not because nothing else authorized it" -- the prompt rule
/// is composed against a GLOBAL `bash:rm` allow rule (trusted-by-authorship,
/// auto-allows `rm` on its own). The broker's `decide` ordering checks
/// `prompt` BEFORE `pattern-allow`, so the prompt rule (from the untrusted
/// project file) forces the gate even though the allow rule would otherwise
/// auto-allow `rm`. Without the prompt rule installed, `rm` auto-allows and
/// the gate sees ZERO requests -- which is exactly the failure mode a
/// broken guard (gating `prompt` behind trust) would produce.
#[tokio::test]
async fn a_structured_prompt_rule_from_an_untrusted_project_file_forces_the_gate() {
    let project = TempDir::new().expect("tempdir");
    let (xdg, env) = isolated_env();
    // A GLOBAL allow rule that would auto-allow `rm` on its own -- so the
    // only thing forcing the gate is the project prompt rule.
    write_global_permissions(&xdg, r#"{"allow":["bash:rm"]}"#);
    write_project_permissions(
        &project,
        r#"{"rules":[{"select":{"tools":["bash"]},"when":{"command_prefix":"rm"},"then":"prompt"}]}"#,
    );

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "operator said no".into(),
    });
    let conway = build_conway(
        project.path(),
        vec![
            ScriptedTurn::Respond(bash_call_response("rm -rf /tmp/something")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    conway.load_permission_files(
        project.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("clean up").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");

    assert_eq!(
        gate.requests().len(),
        1,
        "a structured prompt rule must force `rm` to the operator's gate even from an \
         untrusted project file -- narrowing installs unconditionally, and the prompt \
         check precedes the allow grant so the gate is reached despite the global \
         `bash:rm` allow rule"
    );
}
// =====================================================================
// A4: a structured `then: allow` rule from an UNTRUSTED project file does
//      not take effect -- the structured-allow half of the shared allow
//      trust invariant ( liveness corollary).
// =====================================================================

/// A4 /: an UNTRUSTED project `permissions.json`'s structured
/// `then: allow` rule must NOT authorize its call. The trust invariant lives
/// on the shared allow install path -- pinned for the FLAT allow rule by
/// `an_untrusted_project_allow_rule_does_not_take_effect` in
/// `crates/conway/tests/permission_trust_seam.rs` -- but no structured-
/// specific observable-outcome test existed. A structured
/// `{ "select": { "tools": ["read"] }, "when": "always", "then": "allow" }`
/// rule from an untrusted project file must be held for trust (not
/// installed), so a `read` call still reaches the operator's gate through
/// the real `ReadTool::render` -> `render_call` -> `PermissionBroker::decide`
/// seam, exactly as if no permissions file existed at all. If the structured
/// allow path bypassed trust, this call would be auto-allowed (zero gate
/// requests) -- the silent fail-open this test pins against.
#[tokio::test]
async fn an_untrusted_project_structured_allow_rule_does_not_take_effect() {
    let project = TempDir::new().expect("tempdir");
    std::fs::write(project.path().join("file.txt"), b"inside the project").expect("write fixture");
    let (_xdg, env) = isolated_env();
    write_project_permissions(
        &project,
        r#"{"rules":[{"select":{"tools":["read"]},"when":"always","then":"allow"}]}"#,
    );

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "operator said no".into(),
    });
    let conway = build_conway(
        project.path(),
        vec![
            ScriptedTurn::Respond(read_call_response(
                &project.path().join("file.txt").display().to_string(),
            )),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    let report = conway.load_permission_files(
        project.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );
    assert_eq!(
        report.notices.len(),
        1,
        "an untrusted project file with a structured allow rule must surface one \
         trust-held notice: {:?}",
        report.notices
    );
    assert!(
        report.notices[0].contains("require an explicit trust decision"),
        "{}",
        report.notices[0]
    );
    assert!(
        report.registration_errors.is_empty(),
        "a structured `always` allow on `read` is a valid rule (not a registration \
         error) -- it is held for trust, not rejected: {:?}",
        report.registration_errors
    );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("read the file").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");

    assert_eq!(
        gate.requests().len(),
        1,
        "a structured `then: allow` rule from an UNTRUSTED project file must NEVER take \
         effect -- the `read` call must still reach the operator's gate, exactly as if \
         the file did not exist (the structured allow path shares the trust gate the \
         flat allow path already honors): {:?}",
        gate.requests()
    );
}

// =====================================================================
// A4: the broadened `command_prefix`-on-`Structured` registration check
//      (multi-tool, wildcard, category selects).
// =====================================================================

/// A4: a `command_prefix` rule selecting a MULTI-TOOL set that resolves to
/// a MIX of `Structured`- and `ShellCommand`-rendering tools is NOT a
/// registration error -- the rule installs, and the operator is warned via a
/// NOTICE that the `Structured` member is inert.
///
/// **AMENDED by board item `01KZDDPC5MMD49F6JPV9CW4TVM`, and left as a
/// disclosed gap rather than silently fixed.** This test used to also prove
/// the `ShellCommand` (`bash`) member installs and matches, on the theory
/// that only the `Structured` member was inert. That is no longer true: a
/// durable pattern grant does not exist for `bash` at all any more (see
/// `conway_core::permission_pattern`'s own module doc), so BOTH members of
/// this rule are inert now, not just the one the registration notice names.
/// The registration check itself (`command_prefix_resolved_kinds` in
/// `crates/conway/src/permissions/mod.rs`) was not touched by that item
/// (out of its owned scope) and still only counts `Structured` vs
/// `ShellCommand` membership, so the notice's wording ("the `ShellCommand`
/// members install and match as written") is now misleading for `allow`
/// rules -- see `docs/permissions.md`'s "Rules in `permissions.json`"
/// section for the operator-facing statement of this gap. This test is
/// pinned to the CURRENT, honest behavior (both members reach the gate) so
/// a future fix to close that gap has a red test to turn green, not a green
/// one asserting the old, now-false claim.
#[tokio::test]
async fn a_command_prefix_rule_on_a_mixed_kind_multi_tool_select_installs_with_a_notice() {
    let project = TempDir::new().expect("tempdir");
    let (xdg, env) = isolated_env();
    // A GLOBAL file (trusted-by-authorship) so the allow rule's registration
    // check runs and the rule installs.
    write_global_permissions(
        &xdg,
        r#"{"rules":[
            {"select":{"tools":["bash","read"]},"when":{"command_prefix":"echo"},"then":"allow"}
        ]}"#,
    );

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "operator said no".into(),
    });
    let conway = build_conway(
        project.path(),
        vec![
            ScriptedTurn::Respond(bash_call_response("echo hi")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    let report = conway.load_permission_files(
        project.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );

    assert!(
        report.registration_errors.is_empty(),
        "a MIXED-kind multi-tool command_prefix rule is NOT a hard registration error \
         (the ShellCommand member works): {:?}",
        report.registration_errors
    );
    assert_eq!(
        report.notices.len(),
        1,
        "the mixed-kind rule surfaces exactly one notice warning the Structured member is \
         inert: {:?}",
        report.notices
    );
    assert!(
        report.notices[0].contains("Structured"),
        "the notice must name the inert Structured members: {}",
        report.notices[0]
    );

    // Observable, AMENDED: the rule installs (not silently dropped), but
    // NEITHER member authorizes anything any more -- the `ShellCommand`
    // (`bash`) member is refused by `Rule::gate_allows` exactly like every
    // other `bash` allow rule now, on top of the `Structured` member the
    // notice above already names. `echo hi` reaches the operator's gate.
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("run the command").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");
    assert_eq!(
        gate.requests().len(),
        1,
        "the rule installs (per the notice above) but authorizes nothing: the \
         `ShellCommand` (`bash`) member is refused the same way every other `bash` \
         allow rule now is, so `echo hi` must reach the operator's gate: {:?}",
        gate.requests()
    );
}

/// A4: a `command_prefix` rule selecting a MULTI-TOOL set where EVERY
/// resolvable selected tool renders `Structured` is a hard registration
/// error -- the rule is fully inert, so the loader refuses to install it
/// silently (the multi-tool generalization of the existing single-tool
/// `command_prefix_on_a_structured_tool_is_a_registration_error`). Pinned
/// through the real load seam: the report carries exactly one
/// `CommandPrefixOnStructuredTool` registration error.
#[tokio::test]
async fn a_command_prefix_rule_on_an_all_structured_multi_tool_select_is_a_registration_error() {
    let project = TempDir::new().expect("tempdir");
    let (xdg, env) = isolated_env();
    write_global_permissions(
        &xdg,
        r#"{"rules":[
            {"select":{"tools":["read","write"]},"when":{"command_prefix":"read"},"then":"allow"}
        ]}"#,
    );

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "unused".into(),
    });
    let conway = build_conway(
        project.path(),
        vec![ScriptedTurn::Respond(text_response("no call needed"))],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    let report = conway.load_permission_files(
        project.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );

    assert_eq!(
        report.registration_errors.len(),
        1,
        "a command_prefix rule selecting only Structured-rendering tools must surface \
         exactly one registration error -- the rule is fully inert: {:?}",
        report.registration_errors
    );
    assert_eq!(
        report.registration_errors[0].reason,
        RuleRegistrationReason::CommandPrefixOnStructuredTool,
        "the error must name the same reason the single-tool case uses"
    );
}

/// A4: a `command_prefix` rule selecting a trailing-`*` wildcard that
/// matches only `Structured`-rendering tools is a hard registration error --
/// the wildcard is resolved against the registered tools, and every match
/// is `Structured`, so the rule is fully inert. `re*` matches `read` and
/// `report` (both `Structured` in the builtin registry). Pinned through the
/// real load seam.
#[tokio::test]
async fn a_command_prefix_rule_on_a_wildcard_selecting_only_structured_tools_is_a_registration_error(
) {
    let project = TempDir::new().expect("tempdir");
    let (xdg, env) = isolated_env();
    write_global_permissions(
        &xdg,
        r#"{"rules":[
            {"select":{"tools":["re*"]},"when":{"command_prefix":"read"},"then":"allow"}
        ]}"#,
    );

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "unused".into(),
    });
    let conway = build_conway(
        project.path(),
        vec![ScriptedTurn::Respond(text_response("no call needed"))],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    let report = conway.load_permission_files(
        project.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );

    assert_eq!(
        report.registration_errors.len(),
        1,
        "a command_prefix rule whose wildcard resolves to only Structured-rendering tools \
         must surface a registration error -- the rule is fully inert: {:?}",
        report.registration_errors
    );
    assert_eq!(
        report.registration_errors[0].reason,
        RuleRegistrationReason::CommandPrefixOnStructuredTool,
        "the broadened check resolves the wildcard and names the same reason"
    );
}

/// A4: a `command_prefix` rule selecting a `Categories` set whose members
/// are ALL `Structured`-rendering is a hard registration error -- the
/// category is resolved against the registered tools, and every member is
/// `Structured`, so the rule is fully inert. `Read`-category builtins
/// (`read`) render `Structured`. Pinned through the real load seam.
#[tokio::test]
async fn a_command_prefix_rule_on_a_category_of_only_structured_tools_is_a_registration_error() {
    let project = TempDir::new().expect("tempdir");
    let (xdg, env) = isolated_env();
    write_global_permissions(
        &xdg,
        r#"{"rules":[
            {"select":{"categories":["read"]},"when":{"command_prefix":"read"},"then":"allow"}
        ]}"#,
    );

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "unused".into(),
    });
    let conway = build_conway(
        project.path(),
        vec![ScriptedTurn::Respond(text_response("no call needed"))],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    let report = conway.load_permission_files(
        project.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );

    assert_eq!(
        report.registration_errors.len(),
        1,
        "a command_prefix rule selecting a category whose members are all Structured must \
         surface a registration error: {:?}",
        report.registration_errors
    );
    assert_eq!(
        report.registration_errors[0].reason,
        RuleRegistrationReason::CommandPrefixOnStructuredTool,
        "the broadened check resolves the category and names the same reason"
    );
}

/// A4: B1/B3 regression guard. Broadening the `command_prefix` check must
/// NOT regress the `PathsUnder` arms. A `paths_under` deny on `bash`
/// (Unconfinable) still surfaces `PathsUnderOnUnconfinedTool`, and a
/// `paths_under` rule with an uncanonicalizable prefix still surfaces
/// `PathsUnderPrefixUncanonicalizable` -- both distinct from
/// `CommandPrefixOnStructuredTool`. Pinned in one combined load to prove the
/// three reasons do not collide.
#[tokio::test]
async fn broadening_command_prefix_check_does_not_regress_paths_under_arms() {
    let project = TempDir::new().expect("tempdir");
    let root_canon = project
        .path()
        .canonicalize()
        .expect("canonicalize project root");
    let (xdg, env) = isolated_env();
    write_global_permissions(
        &xdg,
        &format!(
            r#"{{"rules":[
                {{"select":{{"tools":["bash"]}},"when":{{"paths_under":"{}"}},"then":"deny"}},
                {{"select":{{"tools":["read"]}},"when":{{"paths_under":"nonexistent-dir"}},"then":"allow"}}
            ]}}"#,
            root_canon.display()
        ),
    );

    let gate = RecordingGate::new(PermissionDecision::AllowOnce);
    let conway = build_conway(
        project.path(),
        vec![ScriptedTurn::Respond(text_response("no call needed"))],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    let report = conway.load_permission_files(
        project.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );

    let reasons: Vec<_> = report
        .registration_errors
        .iter()
        .map(|e| e.reason.clone())
        .collect();
    assert!(
        reasons.contains(&RuleRegistrationReason::PathsUnderOnUnconfinedTool),
        "B1's reason must still fire for a paths_under deny on bash: {reasons:?}"
    );
    assert!(
        reasons.contains(&RuleRegistrationReason::PathsUnderPrefixUncanonicalizable),
        "B3's reason must still fire for an uncanonicalizable prefix: {reasons:?}"
    );
    assert_eq!(
        report.registration_errors.len(),
        2,
        "exactly two registration errors (one per paths_under rule), no command_prefix error: \
         {:?}",
        report.registration_errors
    );
}

// =====================================================================
// A2: a structured allow rule is inspectable and individually revocable.
// =====================================================================

/// **A2's headline acceptance test.** A structured `paths_under` allow rule
/// installed from a real global `permissions.json` (alongside a sibling
/// FLAT grant): the facade lists it with its origin and scope; revoking it
/// through `Conway::revoke_structured_allow_rule` removes ONLY that rule --
/// the sibling flat grant still auto-allows, the rule's wire form is gone
/// from the file on disk (the flat entry untouched), and a call the rule
/// used to authorize reaches the operator's gate again. Every assertion is
/// on the OBSERVABLE outcome (gate-request counts through the real
/// `ToolRunner` -> `PermissionBroker::decide` seam, and the file's bytes),
/// never on an internal call count.
#[tokio::test]
async fn revoking_a_structured_allow_rule_removes_only_it_and_the_call_asks_again() {
    let root_dir = TempDir::new().expect("tempdir");
    std::fs::write(root_dir.path().join("file.txt"), b"inside the root").expect("write fixture");
    let root_canon = root_dir.path().canonicalize().expect("canonicalize root");

    let (xdg, env) = isolated_env();
    write_global_permissions(
        &xdg,
        &format!(
            r#"{{"allow":["write:*"],"rules":[{{"select":{{"tools":["read"]}},"when":{{"paths_under":"{}"}},"then":"allow"}}]}}"#,
            root_canon.display()
        ),
    );
    let global_file = xdg.path().join("conway").join("permissions.json");
    let write_target = root_dir.path().join("written.txt").display().to_string();

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "operator said no".into(),
    });
    let conway = build_conway(
        root_dir.path(),
        vec![
            // Turn 1: the structured rule authorizes this read (gate bypassed).
            ScriptedTurn::Respond(read_call_response("file.txt")),
            ScriptedTurn::Respond(text_response("done")),
            // Turn 2: after the revoke, the SAME read must reach the gate.
            ScriptedTurn::Respond(read_call_response("file.txt")),
            ScriptedTurn::Respond(text_response("done")),
            // Turn 3: the sibling flat grant must still auto-allow.
            ScriptedTurn::Respond(write_call_response(&write_target)),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    let report = conway.load_permission_files(
        root_dir.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );
    assert!(
        report.registration_errors.is_empty(),
        "no registration errors expected: {:?}",
        report.registration_errors
    );

    // The rule is inspectable through the facade: exactly one structured
    // allow rule, carrying its origin (the global file) and its grant scope.
    let listed = conway.active_structured_allow_rules();
    assert_eq!(
        listed.len(),
        1,
        "the structured allow rule must be listed: {listed:?}"
    );
    assert_eq!(
        listed[0].1,
        conway::PatternOrigin::File(global_file.clone()),
        "its origin must name the file it came from"
    );
    assert_eq!(
        listed[0].2,
        conway::GrantScope::Session,
        "loaded at Session scope, and the review surface must say so"
    );
    assert!(
        matches!(
            &listed[0].0.when,
            conway::When::PathsUnder(p) if p == &root_canon.display().to_string()
        ),
        "the listed rule is the paths_under rule from the file: {:?}",
        listed[0].0
    );
    let (rule, origin, scope) = listed.into_iter().next().expect("one rule");

    // Turn 1: the rule authorizes the in-root read -- the gate sees nothing.
    // (One prompt per session: sequential prompts on a single handle are not
    // a supported pattern in this harness -- but the broker, and with it
    // every grant and revoke, is shared across a `Conway`'s sessions, so a
    // fresh session per turn still exercises the SAME installed rules.)
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("read the file").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");
    assert!(
        gate.requests().is_empty(),
        "before the revoke, the paths_under rule authorizes the read without the gate: {:?}",
        gate.requests()
    );

    // The flat revoke cannot name a structured rule (its PatternRule key
    // collapses every structured rule to None at the broker): NotFound, and
    // the rule survives -- the pre-A2 gap, pinned as a regression guard.
    let flat_outcome = conway.revoke_permission_pattern(
        &env,
        &conway::PatternRule::parse("read:*").expect("valid rule"),
        &origin,
    );
    assert!(
        matches!(flat_outcome, conway::RevokeOutcome::NotFound),
        "the flat revoke must NOT find a structured rule: {flat_outcome:?}"
    );
    assert_eq!(
        conway.active_structured_allow_rules().len(),
        1,
        "the structured rule must survive a flat-revoke attempt"
    );

    // The real revoke, addressed by exactly what the review list rendered.
    let outcome = conway.revoke_structured_allow_rule(&env, &rule, &origin, &scope);
    assert!(
        matches!(
            outcome,
            conway::RevokeOutcome::RevokedAndPersisted {
                retrust_warning: None
            }
        ),
        "the global file is trusted by authorship (no re-trust needed), so this is a \
         clean revoke-and-persist: {outcome:?}"
    );
    assert!(
        conway.active_structured_allow_rules().is_empty(),
        "the rule is gone from the review list"
    );

    // The file on disk: the structured rule's entry is gone, the flat
    // sibling's entry is untouched.
    let on_disk = std::fs::read_to_string(&global_file).expect("read rewritten permissions.json");
    assert!(
        !on_disk.contains("paths_under"),
        "the structured rule's wire form must be removed from the file: {on_disk}"
    );
    assert!(
        on_disk.contains("write:*"),
        "the sibling flat grant's wire form must survive: {on_disk}"
    );

    // Turn 2 (observable): the SAME read now reaches the operator's gate.
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("read the file again").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");
    assert_eq!(
        gate.requests().len(),
        1,
        "after the revoke, the call the rule used to authorize must ask again"
    );

    // Turn 3 (observable): the sibling flat grant still auto-allows.
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("write the file").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");
    assert_eq!(
        gate.requests().len(),
        1,
        "the sibling flat grant must still suppress its prompt -- only the structured \
         rule was revoked"
    );
}

/// Revoking a structured rule that is NOT installed returns
/// `RevokeOutcome::NotFound` (typed, never a panic --) and removes
/// nothing: the installed structured rule still authorizes afterward.
#[tokio::test]
async fn revoking_a_structured_allow_rule_that_does_not_exist_returns_not_found() {
    let root_dir = TempDir::new().expect("tempdir");
    std::fs::write(root_dir.path().join("file.txt"), b"inside the root").expect("write fixture");
    let root_canon = root_dir.path().canonicalize().expect("canonicalize root");

    let (xdg, env) = isolated_env();
    write_global_permissions(
        &xdg,
        &format!(
            r#"{{"rules":[{{"select":{{"tools":["read"]}},"when":{{"paths_under":"{}"}},"then":"allow"}}]}}"#,
            root_canon.display()
        ),
    );

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "must not be consulted -- the surviving rule must still grant".into(),
    });
    let conway = build_conway(
        root_dir.path(),
        vec![
            ScriptedTurn::Respond(read_call_response("file.txt")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    conway.load_permission_files(
        root_dir.path(),
        &env,
        PermissionScope::Session,
        AgentId::new(),
    );
    assert_eq!(conway.active_structured_allow_rules().len(), 1);

    let never_installed = conway::Rule {
        select: conway::Select::Tools(vec!["bash".to_string()]),
        when: conway::When::Always,
        then: conway::Then::Allow,
    };
    // A flat-desugarable rule never appears in the structured review list
    // (it round-trips through `to_pattern_rule`), so this identity can
    // never match the installed paths_under rule.
    let outcome = conway.revoke_structured_allow_rule(
        &env,
        &never_installed,
        &conway::PatternOrigin::Interactive,
        &conway::GrantScope::Session,
    );
    assert!(
        matches!(outcome, conway::RevokeOutcome::NotFound),
        "a rule that was never installed must report NotFound: {outcome:?}"
    );
    assert_eq!(
        conway.active_structured_allow_rules().len(),
        1,
        "nothing was removed"
    );

    // Observable: the surviving rule still authorizes its call.
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("read the file").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");
    assert!(
        gate.requests().is_empty(),
        "the installed rule must still authorize after a NotFound revoke: {:?}",
        gate.requests()
    );
}
