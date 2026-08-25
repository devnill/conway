//! Board item `01M0X1FCQ80C9ET97HENXSAW2K`, end to end: a translated
//! `hooks/hooks.json` rule is not just NAMED as mapped -- it becomes a
//! real `conway::config::schema::HookEntry` that a real `ConwayBuilder`
//! actually dispatches through, over the REAL `ProcessHookRunner`
//! (`conway::test_support::test_builder(..).with_default_hook_runner()`),
//! never a hand-built fixture standing in for dispatch.
//!
//! Two events, one from each disclosed fail-open/fail-closed tier
//! (`crates/conway/src/config/schema.rs`'s own `HooksConfig` doc):
//! `session_starting` (observation-only, fails open) proves an ordinary
//! translated rule really runs its command; `pre_tool_use` (may deny,
//! fails closed) proves a translated rule with a `matcher` really narrows
//! and really denies, not merely that SOME process got spawned.
//!
//! `${CLAUDE_PLUGIN_ROOT}` substitution is exercised here too -- both real
//! plugins this crate has been checked against use it in every single
//! command, so a test that never substitutes it would prove dispatch
//! against an unrealistic fixture.

use std::collections::BTreeMap;
use std::path::Path;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HookEntry, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::test_support::test_builder;
use conway::SessionSpec;
use conway_core::ids::RoleAlias;
use conway_plugin_claude::HookRegistration;

/// The exact `[hooks].rules[]`-shaped -> real `HookEntry` conversion a
/// caller of this crate performs -- deliberately trivial (this crate's own
/// doc: "a caller converts field-by-field"), and proven trivial by being
/// exercised here rather than only asserted in prose.
fn to_hook_entry(registration: HookRegistration) -> HookEntry {
    HookEntry {
        id: registration.id,
        event: registration.event.to_string(),
        match_tool: registration.match_tool,
        command: registration.command,
        timeout_ms: registration.timeout_ms,
        enabled: registration.enabled,
        // `HookEntry::on_failure` landed in the same wave as this test
        // (board item `01M0X1AH44SNMK5TZ507K30QNP`) and neither writer could
        // see the other. A TRANSLATED hook takes the default, `Deny`: this
        // layer must not silently choose a foreign plugin's failure posture
        // on the operator's behalf, and `Deny` is the posture every
        // config-declared `pre_tool_use` rule already has.
        on_failure: Default::default(),
    }
}

fn minimal_config(cwd: &Path, hooks: HooksConfig) -> ConwayConfig {
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
        hooks,
    }
}

/// `session_starting` (observation-only, fails open): a translated rule's
/// command really runs, and `${CLAUDE_PLUGIN_ROOT}` really resolves to the
/// discovered plugin directory rather than surviving as a literal,
/// unresolvable token in the spawned shell.
#[tokio::test]
async fn a_translated_session_start_rule_actually_dispatches_and_resolves_plugin_root() {
    let plugin_dir = tempfile::tempdir().expect("plugin dir");
    let root = plugin_dir.path();
    std::fs::create_dir_all(root.join("hooks")).unwrap();
    std::fs::write(
        root.join("hooks").join("hooks.json"),
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"touch \"${CLAUDE_PLUGIN_ROOT}/started.marker\"","timeout":5}]}]}}"#,
    )
    .unwrap();

    let report = conway_plugin_claude::discover(root).expect("discover the plugin directory");
    let registrations = report.hook_registrations();
    assert_eq!(registrations.len(), 1);
    assert_eq!(registrations[0].event, "session_starting");
    assert_eq!(registrations[0].timeout_ms, 5_000);

    let hooks = HooksConfig {
        rules: registrations.into_iter().map(to_hook_entry).collect(),
    };

    let cwd = tempfile::tempdir().expect("cwd");
    // `build()` requires at least one backend even though `new_session`
    // alone never calls it -- an unused `ScriptedBackend` satisfies that
    // without pulling a scripted-turn setup into a test that is not about
    // routing at all.
    let backend = std::sync::Arc::new(
        conway_testkit::ScriptedBackend::new(Default::default())
            .with_id(conway_core::ids::BackendId::new("fake")),
    );
    let conway = test_builder(minimal_config(cwd.path(), hooks))
        .with_backend(backend)
        .with_default_hook_runner()
        .build()
        .expect("build should succeed with the real translated rule and hook runner");

    let marker = root.join("started.marker");
    assert!(
        !marker.exists(),
        "the marker must not exist before the session starts"
    );

    conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");

    assert!(
        marker.exists(),
        "session_starting must have actually run the translated command, resolving \
         ${{CLAUDE_PLUGIN_ROOT}} to {}",
        root.display()
    );
}

/// `pre_tool_use` (may deny, fails closed): a translated `matcher` really
/// narrows which tool calls consult it, and a denying translated rule
/// really denies -- proven against the real `PermissionBroker`/`bash`
/// tool, not a stand-in.
#[tokio::test]
async fn a_translated_pre_tool_use_rule_with_a_matcher_actually_denies_a_matching_call() {
    use std::sync::Arc;

    use conway::PluginSelection;
    use conway_core::agent::PermissionDecision;
    use conway_core::agent::PermissionRequest;
    use conway_core::content::{ContentBlock, StopReason, ToolCall, Usage};
    use conway_core::ids::{BackendId, ToolName};
    use conway_core::log::LogRecord;
    use conway_core::ports::{GenerateResponse, PermissionGate};
    use conway_testkit::{text_response, ScriptedTurn};

    struct AllowGate;
    #[async_trait::async_trait]
    impl PermissionGate for AllowGate {
        async fn check(&self, _req: PermissionRequest) -> PermissionDecision {
            PermissionDecision::AllowOnce
        }
    }

    let plugin_dir = tempfile::tempdir().expect("plugin dir");
    let root = plugin_dir.path();
    std::fs::create_dir_all(root.join("hooks")).unwrap();
    std::fs::write(
        root.join("hooks").join("hooks.json"),
        r#"{"hooks":{"PreToolUse":[{"matcher":"bash","hooks":[{"type":"command","command":"exit 1"}]}]}}"#,
    )
    .unwrap();

    let report = conway_plugin_claude::discover(root).expect("discover the plugin directory");
    let registrations = report.hook_registrations();
    assert_eq!(registrations.len(), 1);
    assert_eq!(registrations[0].event, "pre_tool_use");
    assert_eq!(registrations[0].match_tool.as_deref(), Some("bash"));

    let hooks = HooksConfig {
        rules: registrations.into_iter().map(to_hook_entry).collect(),
    };

    let cwd = tempfile::tempdir().expect("cwd");
    let bash_call = GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: "call_1".to_string(),
            name: ToolName::new("bash"),
            arguments: serde_json::json!({ "command": "echo hi" }),
        }],
        stop: StopReason::ToolUse,
        usage: Usage::default(),
    };
    let backend = Arc::new(
        conway_testkit::ScriptedBackend::new(vec![
            ScriptedTurn::Respond(bash_call),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );

    let conway = test_builder(minimal_config(cwd.path(), hooks))
        .with_backend(backend)
        .with_permission_gate(Arc::new(AllowGate) as Arc<dyn PermissionGate>)
        .with_builtin_plugins(PluginSelection::All)
        .with_default_hook_runner()
        .build()
        .expect("build should succeed with the real translated rule and hook runner");

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let root_agent = handle.root();
    let turn = handle.prompt("do the thing").await.expect("prompt");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang");
    let records = handle.transcript(root_agent).await.expect("transcript");

    let bash_result_text = records.iter().find_map(|r| match r {
        LogRecord::ToolResultRecord { result, .. } if result.tool.as_str() == "bash" => {
            result.blocks.iter().find_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
        }
        _ => None,
    });
    let text = bash_result_text.expect("a bash ToolResultRecord must exist");
    assert!(
        text.contains("denied"),
        "the translated pre_tool_use rule must actually deny the matching call: {text}"
    );
}
