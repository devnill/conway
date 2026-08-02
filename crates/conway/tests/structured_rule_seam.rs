//! Acceptance tests for board item F12 ("the structured rule form: general
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
    AgentsConfig, ConwayConfig, HealthSection, LimitsConfig, ModelsConfig, PermissionsConfig,
    RoleEntry, RoutingSection, SessionConfig, TuiSection,
};
use conway::{Conway, ConwayBuilder, RuleRegistrationReason, SessionSpec};
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
    std::fs::write(root_dir.path().join("file.txt"), b"hello from inside the root")
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
    let report = conway.load_permission_files(root_dir.path(), &env, AgentId::new());
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
    let secret_canon = outside_dir.path().join("secret.txt").canonicalize().expect("canonicalize secret");

    let (xdg, env) = isolated_env();
    write_global_permissions(
        &xdg,
        &format!(
            r#"{{"rules":[{{"select":{{"tools":["read"]}},"when":{{"paths_under":"{}"}},"then":"allow"}}]}}"#,
            root_canon.display()
        ),
    );

    let gate = RecordingGate::new(PermissionDecision::Deny { reason: "operator said no".into() });
    let conway = build_conway(
        root_dir.path(),
        vec![
            ScriptedTurn::Respond(read_call_response(&secret_canon.display().to_string())),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    conway.load_permission_files(root_dir.path(), &env, AgentId::new());

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
    let traversal = format!(
        "{}/../outside/secret.txt",
        root_canon.display()
    );

    let gate = RecordingGate::new(PermissionDecision::Deny { reason: "operator said no".into() });
    let conway = build_conway(
        root_dir.as_path(),
        vec![
            ScriptedTurn::Respond(read_call_response(&traversal)),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    conway.load_permission_files(root_dir.as_path(), &env, AgentId::new());

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = handle.prompt("read through the traversal").await.expect("prompt");
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
        gate.requests()[0].rendered.contains(&root_canon.display().to_string()),
        "the rendered string DOES contain the rule's canonical prefix (so a rendered-based \
         or naive string-prefix check would falsely match): {:?}",
        gate.requests()[0].rendered
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

    let gate = RecordingGate::new(PermissionDecision::Deny { reason: "operator said no".into() });
    let conway = build_conway(
        root_dir.path(),
        vec![
            ScriptedTurn::Respond(bash_call_response("echo hi")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    let report = conway.load_permission_files(root_dir.path(), &env, AgentId::new());
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
/// `RuleRegistrationError` in the load report instead (P-10: untrusted input
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

    let gate = RecordingGate::new(PermissionDecision::Deny { reason: "unused".into() });
    let conway = build_conway(
        project.path(),
        vec![ScriptedTurn::Respond(text_response("no call needed"))],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    let report = conway.load_permission_files(project.path(), &env, AgentId::new());

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
// (d) byte-identical decisions: one evaluator, not two.
// =====================================================================

/// **THE headline equivalence proof.** A flat `bash:git status` rule and its
/// structured equivalent `tools(["bash"]) + command_prefix("git status") +
/// allow` produce BYTE-IDENTICAL gate decisions across a matrix of bash
/// calls: an exact match, a subcommand match, a different command, and a
/// chained command (the metacharacter gate must refuse the chained command
/// under BOTH forms -- trap 4, the gate is not weakened). Both rules are
/// loaded from a real global `permissions.json` through the real
/// `load_permission_files` -> `PermissionBroker` seam, and the matrix is
/// driven through the real `BashTool::render` -> `render_call` -> `decide`
/// path. The gate request COUNTS are equal for every command -- the
/// strongest available evidence there is one evaluator and not two.
#[tokio::test]
async fn flat_and_structured_command_prefix_produce_byte_identical_gate_decisions() {
    let (xdg_flat, env_flat) = isolated_env();
    write_global_permissions(&xdg_flat, r#"{"allow":["bash:git status"]}"#);

    let (xdg_struct, env_struct) = isolated_env();
    write_global_permissions(
        &xdg_struct,
        r#"{"rules":[{"select":{"tools":["bash"]},"when":{"command_prefix":"git status"},"then":"allow"}]}"#,
    );

    // The matrix: exact, subcommand, different, chained (gated). Each is
    // run against BOTH the flat and structured conway instances, and the
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
    }

    // The semantic assertions that make the count meaningful: the exact and
    // subcommand matches auto-allow (0 gate requests), the different command
    // and the chained command both reach the gate (1 request each).
    let exact = run_bash_and_count_gate(&xdg_flat, &env_flat, "git status").await;
    let subcmd = run_bash_and_count_gate(&xdg_flat, &env_flat, "git status --short").await;
    let different = run_bash_and_count_gate(&xdg_flat, &env_flat, "git push --force").await;
    let chained = run_bash_and_count_gate(&xdg_flat, &env_flat, "git status && rm -rf /tmp/x").await;
    assert_eq!(exact, 0, "exact match auto-allows");
    assert_eq!(subcmd, 0, "subcommand prefix match auto-allows");
    assert_eq!(different, 1, "a different command reaches the gate");
    assert_eq!(
        chained, 1,
        "TRAP 4: a chained command must reach the gate even under a matching prefix -- the \
         allow-side metacharacter gate is not weakened for the structured form"
    );
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
    let gate = RecordingGate::new(PermissionDecision::Deny { reason: "operator said no".into() });
    let conway = build_conway(
        cwd.path(),
        vec![
            ScriptedTurn::Respond(bash_call_response(command)),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    conway.load_permission_files(cwd.path(), env, AgentId::new());
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
    let report = conway.load_permission_files(project.path(), &env, AgentId::new());
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

    let gate = RecordingGate::new(PermissionDecision::Deny { reason: "operator said no".into() });
    let conway = build_conway(
        project.path(),
        vec![
            ScriptedTurn::Respond(bash_call_response("rm -rf /tmp/something")),
            ScriptedTurn::Respond(text_response("done")),
        ],
        gate.clone() as Arc<dyn PermissionGate>,
    );
    conway.load_permission_files(project.path(), &env, AgentId::new());

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