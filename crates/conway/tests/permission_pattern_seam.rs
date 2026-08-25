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
};
use conway::{Conway, ConwayBuilder, PatternRule, PluginSelection, Rule, SessionSpec};
use conway_core::agent::{PermissionDecision, PermissionRequest, PermissionScope};
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias};
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
/// fixed `decision`. Unlike `conway_testkit::FakeGate`, this combines
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

/// **AMENDED by board item `01KZDDPC5MMD49F6JPV9CW4TVM`.** This test used
/// to be the mirror-image acceptance criterion: a command that genuinely
/// matches the granted prefix must NOT reach the operator at all -- proving
/// the pattern grant, which was completely inert before an earlier fix
/// (every `rendered` value carried JSON metacharacters that made
/// `PatternRule::matches` always return `false`), actually worked through
/// the real tool.
///
/// That is no longer the correct expectation, on purpose. A durable pattern
/// grant does not exist for `bash` (or any `RenderKind::ShellCommand` tool)
/// at all any more -- see `conway_core::permission_pattern`'s own module
/// doc for the resolution -- so even the exact, unchained `git status` the
/// grant names must now reach the operator, through the identical real
/// production seam this file exists to drive end to end (not a hand-typed
/// fixture). `read:*` remains a real, working grant for a `Structured` tool
/// -- see `a_read_wildcard_grant_actually_grants_through_the_real_render_seam`
/// below, unaffected by this amendment.
#[tokio::test]
async fn a_bash_pattern_grant_never_auto_allows_even_its_own_exact_command() {
    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "operator said no".into(),
    });

    let requests = run_one_bash_call(gate, "git status --short").await;

    assert_eq!(
        requests.len(),
        1,
        "a `bash:git status` grant must not auto-allow even the exact, \
         unchained command it names -- a durable pattern grant does not \
         exist for `bash` at all: {requests:?}"
    );
    assert_eq!(requests[0].rendered, "git status --short");
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

// ---------------------------------------------------------------------
// `Conway::grant_permission_rule` -- the structured (`When::ArgsMatch`)
// counterpart to `grant_permission_pattern` above. Board item
// `01M0EMDVBJVT510GBJHPWBZ3G6`: this method had ZERO coverage outside the
// `conway-runtime` unit level (which drives `PermissionBroker` directly,
// bypassing the facade) and the TUI-input level (which asserts the
// `Action` is produced, never that it is applied). Exactly the seam that
// hid the two prior inert-pattern-grant bugs this file's own module doc
// names -- so these tests drive a REAL `Conway`, grant through the real
// facade method, and run a real second turn through the same tool, never a
// hand-built `AuthorizedCall`.
// ---------------------------------------------------------------------

fn tool_call_response(tool: &str, arguments: serde_json::Value) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![conway_core::content::ToolCall {
            call_id: "call_1".to_string(),
            name: conway_core::ids::ToolName::new(tool),
            arguments,
        }],
        stop: conway_core::content::StopReason::ToolUse,
        usage: conway_core::content::Usage::default(),
    }
}

/// Grants `rule` through the real `Conway::grant_permission_rule` facade
/// method (never `PermissionBroker::remember_pattern_rule` directly), then
/// runs one real turn scripting a single `tool` call with `arguments`, and
/// returns whatever the gate actually saw. Asserts the grant installs (an
/// `ArgsMatch` rule is never dropped -- see `grant_permission_rule`'s own
/// doc), so a regression that silently drops the rule fails loudly here
/// rather than being read as "the call happened to not need it".
async fn run_one_call_with_rule(
    gate: Arc<RecordingGate>,
    rule: Rule,
    scope: PermissionScope,
    tool: &str,
    arguments: serde_json::Value,
) -> Vec<PermissionRequest> {
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(tool_call_response(tool, arguments)),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(backend, gate.clone() as Arc<dyn PermissionGate>);

    let installed = conway.grant_permission_rule(rule, scope, AgentId::new());
    assert!(
        installed,
        "grant_permission_rule must install an ArgsMatch rule -- it is never dropped \
         (unlike PathsUnder, it carries no canonicalizable prefix)"
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

/// **The headline `grant_permission_rule` regression proof.** Nothing
/// pinned (the `[p]` editor's all-wildcard default, byte-identical in
/// intent to the old `tool:*` grant) must auto-allow a real `read` call
/// through the real facade -- the gate must never even be consulted.
#[tokio::test]
async fn an_argsmatch_grant_with_nothing_pinned_auto_allows_every_call_through_the_real_facade() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let file_path = dir.path().join("f.txt");
    std::fs::write(&file_path, "hello from the real facade seam").expect("write fixture file");

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason:
            "must not be consulted -- an all-wildcard ArgsMatch grant must allow this on its own"
                .into(),
    });
    let rule = Rule::args_match_allow_rule("read", BTreeMap::new());

    let requests = run_one_call_with_rule(
        gate,
        rule,
        PermissionScope::Session,
        "read",
        serde_json::json!({ "path": file_path.display().to_string() }),
    )
    .await;

    assert!(
        requests.is_empty(),
        "grant_permission_rule with nothing pinned must auto-allow every future call to that \
         tool -- through the real Conway facade, not a hand-built AuthorizedCall: {requests:?}"
    );
}

/// A single pinned field auto-allows only the call whose arguments equal it
/// exactly -- through the real facade.
#[tokio::test]
async fn an_argsmatch_grant_with_one_field_pinned_auto_allows_the_matching_call() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let file_path = dir.path().join("f.txt");
    std::fs::write(&file_path, "hello from the pinned field").expect("write fixture file");
    let pinned_path = file_path.display().to_string();

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "must not be consulted -- the pinned field matches this call exactly".into(),
    });
    let mut pinned = BTreeMap::new();
    pinned.insert(
        "path".to_string(),
        serde_json::Value::String(pinned_path.clone()),
    );
    let rule = Rule::args_match_allow_rule("read", pinned);

    let requests = run_one_call_with_rule(
        gate,
        rule,
        PermissionScope::Session,
        "read",
        serde_json::json!({ "path": pinned_path }),
    )
    .await;

    assert!(
        requests.is_empty(),
        "a pinned `path` field must auto-allow a call whose `path` matches it exactly: {requests:?}"
    );
}

/// The mirror case: the same pinned rule must fall through to the operator
/// for a call whose pinned field carries a DIFFERENT value -- narrowing, not
/// widening, is the entire point of the field editor.
#[tokio::test]
async fn an_argsmatch_grant_falls_through_to_the_gate_for_a_different_pinned_value() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let granted_path = dir.path().join("granted.txt");
    std::fs::write(&granted_path, "the value the rule pins").expect("write fixture file");
    let other_path = dir.path().join("other.txt");

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "operator said no".into(),
    });
    let mut pinned = BTreeMap::new();
    pinned.insert(
        "path".to_string(),
        serde_json::Value::String(granted_path.display().to_string()),
    );
    let rule = Rule::args_match_allow_rule("read", pinned);

    let requests = run_one_call_with_rule(
        gate,
        rule,
        PermissionScope::Session,
        "read",
        serde_json::json!({ "path": other_path.display().to_string() }),
    )
    .await;

    assert_eq!(
        requests.len(),
        1,
        "a call whose pinned field differs from the granted value must still reach the \
         operator -- a pinned `path` grant must not widen to cover any path: {requests:?}"
    );
}

/// A pinned field that is ABSENT from the call's arguments is a non-match
/// (not a wildcard pass), so the call must still reach the operator. Uses
/// `grep`, whose `glob` field is optional and genuinely absent when the
/// caller doesn't supply it -- proving the missing-field case is reached
/// through a real tool's real argument shape, not a synthesized one.
#[tokio::test]
async fn an_argsmatch_grant_falls_through_to_the_gate_for_a_missing_pinned_field() {
    let dir = tempfile::TempDir::new().expect("tempdir");

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "operator said no".into(),
    });
    let mut pinned = BTreeMap::new();
    pinned.insert(
        "glob".to_string(),
        serde_json::Value::String("*.rs".to_string()),
    );
    let rule = Rule::args_match_allow_rule("grep", pinned);

    let requests = run_one_call_with_rule(
        gate,
        rule,
        PermissionScope::Session,
        "grep",
        // No `glob` key at all -- the pinned field is absent, not wildcard.
        serde_json::json!({ "pattern": "fn main", "path": dir.path().display().to_string() }),
    )
    .await;

    assert_eq!(
        requests.len(),
        1,
        "a call missing a pinned field entirely must reach the operator -- absence must never \
         be treated as a wildcard match: {requests:?}"
    );
}

/// Two pinned fields are ANDed: a call matching only one of them must still
/// reach the operator. Uses `grep`'s `pattern` and `path` fields together.
#[tokio::test]
async fn an_argsmatch_grant_with_two_pinned_fields_requires_both_to_match() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let granted_path = dir.path().display().to_string();

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "operator said no".into(),
    });
    let mut pinned = BTreeMap::new();
    pinned.insert(
        "pattern".to_string(),
        serde_json::Value::String("fn main".to_string()),
    );
    pinned.insert(
        "path".to_string(),
        serde_json::Value::String(granted_path.clone()),
    );
    let rule = Rule::args_match_allow_rule("grep", pinned);

    // Matches `path` but not `pattern`: must still hit the gate.
    let requests = run_one_call_with_rule(
        gate,
        rule,
        PermissionScope::Session,
        "grep",
        serde_json::json!({ "pattern": "something else", "path": granted_path }),
    )
    .await;

    assert_eq!(
        requests.len(),
        1,
        "pinned fields are ANDed -- matching only one of two must still reach the operator: \
         {requests:?}"
    );
}

/// A grant for one tool must never authorize a call to a different tool,
/// even with identical arguments.
#[tokio::test]
async fn an_argsmatch_grant_never_authorizes_a_different_tool() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let file_path = dir.path().join("f.txt");
    std::fs::write(&file_path, "hello").expect("write fixture file");

    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "operator said no".into(),
    });
    // Granted for `read`, but the scripted call below is `grep`.
    let rule = Rule::args_match_allow_rule("read", BTreeMap::new());

    let requests = run_one_call_with_rule(
        gate,
        rule,
        PermissionScope::Session,
        "grep",
        serde_json::json!({ "pattern": "hello", "path": dir.path().display().to_string() }),
    )
    .await;

    assert_eq!(
        requests.len(),
        1,
        "an ArgsMatch grant for `read` must not authorize a `grep` call: {requests:?}"
    );
}

/// **The security-critical case.** An `ArgsMatch` grant on a `bash`
/// (`RenderKind::ShellCommand`) tool must never auto-allow anything, no
/// matter what is pinned -- `rule_allows`'s gate (`Rule::gate_allows`) must
/// refuse it before pinned fields are even consulted. This is what makes the
/// `[p]` editor narrowing-only and unreachable for `bash` at the UI layer
/// AND refused again here if it were ever mis-issued.
#[tokio::test]
async fn an_argsmatch_grant_on_a_shell_command_tool_never_auto_allows_through_the_real_facade() {
    let gate = RecordingGate::new(PermissionDecision::Deny {
        reason: "operator said no".into(),
    });
    // Nothing pinned -- the broadest possible ArgsMatch grant the language
    // can express -- must still be refused for a ShellCommand tool.
    let rule = Rule::args_match_allow_rule("bash", BTreeMap::new());

    let requests = run_one_call_with_rule(
        gate,
        rule,
        PermissionScope::Session,
        "bash",
        serde_json::json!({
            "command": "git status"
        }),
    )
    .await;

    assert_eq!(
        requests.len(),
        1,
        "an ArgsMatch grant on `bash` must never auto-allow, even with nothing pinned -- the \
         gate must refuse it before pinned fields are consulted: {requests:?}"
    );
}
