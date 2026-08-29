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
use std::sync::Arc;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HookEntry, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::plugin::{Plugin, PluginHookRule, PluginManifest, Tool};
use conway::test_support::test_builder;
use conway::{ForkSpec, SessionSpec, SpawnSpec};
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
    // Board item `01M163T1KGX3HTCC2YMDPT655J`: `build()` used to require at
    // least one backend even though `new_session` alone never calls it --
    // an unused `ScriptedBackend` used to sit here purely to satisfy that
    // gate. That gate is gone (`crates/conway/tests/builder.rs`'s
    // `build_succeeds_with_no_backends_configured_and_a_turn_names_no_
    // candidate`), so this test registers no backend at all now, which is
    // one fewer thing to explain in a test that is not about routing.
    let conway = test_builder(minimal_config(cwd.path(), hooks))
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

// -------------------- SubagentStart / child_spawned (board item
// `01M129Y98V4C1050QBPPMY37X0`) --------------------

/// A test-local mirror of `crates/conway-cli/src/claude_compat_plugins.rs`'s
/// own `ClaudeCompatHooksPlugin`/`to_plugin_hook_rule` -- this crate does
/// not depend on `conway-cli` (a binary crate, not a library one this crate
/// could reach anyway), so the ONE conversion line is duplicated here for a
/// test, the same "duplicated locally rather than shared" precedent
/// `crates/conway/tests/session_handle_subagent.rs`'s own `DelayedEchoBackend`
/// doc states for the identical reason. **This is deliberately NOT
/// `to_hook_entry` above**: `HookRegistration::spawn_only` has no
/// `HookEntry` counterpart at all (that field's own doc) -- routing a
/// `SubagentStart` registration through `to_hook_entry`/`HooksConfig.rules`
/// the way the two tests above do would silently LOSE the one bit this
/// test exists to prove, since `ConwayBuilder::build` only reads
/// `spawn_only` off a `Plugin::hooks()`-registered `PluginHookRule`, never
/// off `HookEntry`. Going through a REAL `Plugin`/`with_plugin`, matching
/// production (`claude_compat_plugins.rs::install`), is not optional here.
struct TranslatedHooksPlugin(Vec<PluginHookRule>);

impl Plugin for TranslatedHooksPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "acme-tools".to_string(),
            version: "0.0.0".to_string(),
            tools: Vec::new(),
            required_host_caps: Vec::new(),
            optional_host_caps: Vec::new(),
            requires: Vec::new(),
            optional: Vec::new(),
        }
    }
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }
    fn hooks(&self) -> Vec<PluginHookRule> {
        self.0.clone()
    }
}

fn to_plugin_hook_rule(registration: HookRegistration) -> PluginHookRule {
    PluginHookRule {
        id: registration.id,
        event: registration.event.to_string(),
        match_tool: registration.match_tool,
        command: registration.command,
        timeout_ms: registration.timeout_ms,
        enabled: registration.enabled,
        on_failure: Default::default(),
        spawn_only: registration.spawn_only,
    }
}

/// **ACCEPTANCE 1 + 3, end to end, one fixture (P-15).** A real Claude Code
/// `SubagentStart` rule, translated and attached the SAME way
/// `claude_compat_plugins.rs::install` attaches one in production
/// (`Plugin::hooks()` -> `ConwayBuilder::with_plugin`), driven through a
/// REAL `SessionHandle::fork`/`::spawn` (`conway_runtime::subagent::
/// SubagentHost::start`, the single entry point both modes share) and a
/// REAL `ProcessHookRunner`.
///
/// **The discriminating observable, named before writing this test**: does
/// `subagent-start.marker` exist on disk after the call returns? A `Fork`
/// must leave it absent; a `Spawn` must create it. Checking ONLY the fork
/// side (marker absent) would pass just as happily against a hook that
/// never registered at all, a runner never installed, or `event`/`command`
/// typo'd at translation time -- every one of those ALSO leaves the marker
/// absent (P-15's own warning, and this file's module doc: "asserting on
/// the observable outcome ... is what makes these discriminating"). The
/// spawn assertion in the SAME fixture is what rules all of those out: the
/// only way this test can pass is if the SAME wiring that stays silent for
/// a fork actually dispatches for a spawn.
///
/// **Reproduction record (acceptance 1):** before `HookSpec::spawn_only`
/// existed, this exact fixture's fork assertion FAILED -- the marker
/// existed after the fork alone, proving the finding as fact rather than a
/// code reading (confirmed by hand during this item's own development by
/// temporarily neutralizing `HookSpec::applies_to`'s `spawn_only` check;
/// see this item's own completion report for the literal command and
/// output).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_translated_subagent_start_hooks_json_rule_fires_for_a_spawn_but_not_for_a_fork() {
    let plugin_dir = tempfile::tempdir().expect("plugin dir");
    let root = plugin_dir.path();
    std::fs::create_dir_all(root.join("hooks")).unwrap();
    std::fs::write(
        root.join("hooks").join("hooks.json"),
        r#"{"hooks":{"SubagentStart":[{"hooks":[{"type":"command","command":"touch \"${CLAUDE_PLUGIN_ROOT}/subagent-start.marker\"","timeout":5}]}]}}"#,
    )
    .unwrap();

    let report = conway_plugin_claude::discover(root).expect("discover the plugin directory");
    let registrations = report.hook_registrations();
    assert_eq!(registrations.len(), 1);
    assert_eq!(registrations[0].event, "child_spawned");
    assert!(
        registrations[0].spawn_only,
        "a translated SubagentStart rule must set spawn_only"
    );

    let plugin_rules: Vec<PluginHookRule> =
        registrations.into_iter().map(to_plugin_hook_rule).collect();

    let cwd = tempfile::tempdir().expect("cwd");
    // Board item `01M163T1KGX3HTCC2YMDPT655J`: as above, no backend needed
    // to satisfy `build()` any more -- neither `fork` nor `spawn` below
    // ever prompts a model, so registering one here was always only about
    // the now-removed empty-backend-map gate.
    let conway = test_builder(minimal_config(cwd.path(), HooksConfig::default()))
        .with_default_hook_runner()
        .with_plugin(Arc::new(TranslatedHooksPlugin(plugin_rules)))
        .build()
        .expect("build should succeed with the real translated rule and hook runner");

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let root_agent = handle.root();

    let marker = root.join("subagent-start.marker");
    assert!(!marker.exists(), "the marker must not exist yet");

    // A FORK first -- `/ask`'s own shape (the modal `/ask` and the
    // `conway_ask` tool both go through this identical path).
    handle
        .fork(root_agent, ForkSpec::new("do a thing"))
        .await
        .expect("fork must start");
    assert!(
        !marker.exists(),
        "a translated SubagentStart hook must NOT fire for a Fork -- this is the exact bug \
         board item 01M129Y98V4C1050QBPPMY37X0 reports (an audible SubagentStart hook firing \
         on every /ask)"
    );

    // Then a SPAWN -- the clean, ancestry-free shape a SubagentStart hook
    // author is actually picturing (Claude Code's own Task tool).
    handle
        .spawn(root_agent, SpawnSpec::new("do a thing"))
        .await
        .expect("spawn must start");
    assert!(
        marker.exists(),
        "a translated SubagentStart hook must still fire for a real Spawn"
    );
}
