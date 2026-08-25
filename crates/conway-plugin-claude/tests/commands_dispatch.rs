//! Board item `01M0X1G29EZSFEWB1YAG40SE69`, end to end: a translated
//! `commands/*.md` file is not just NAMED as translated -- it becomes a
//! real `conway_core::ports::Command` that a real `Conway`/`SessionHandle`
//! actually submits as a turn, through the SAME library-API path
//! `conway_plugin_skeleton`'s own `FilePromptCommand` end-to-end test
//! (`tests/file_prompt_command.rs`) already proved out for an
//! operator-authored prompt file -- here fed a translated Claude Code
//! command file instead of a hand-written one, and driving through
//! `ClaudeCompatReport::command_registrations()` rather than a direct
//! constructor.
//!
//! No TUI anywhere in this test: `Command::invoke` is called directly
//! (the library-API path), and `SessionHandle::prompt_command` is what a
//! host (`conway-cli`'s own `App`, or any other embedder) does with the
//! returned `CommandOutcome::SubmitPrompt` -- disclosed, not performed, by
//! `conway_core::ports::CommandOutcome::SubmitPrompt`'s own doc.
//!
//! The fixture below mirrors `beepboop` 1.4.0's real `commands/config.md`
//! shape (frontmatter: `description`, `argument-hint`, `allowed-tools`; a
//! body with no `$ARGUMENTS` placeholder) -- the item's own named test
//! subject, and the same fixture `crate::commands`'s own unit test suite
//! uses verbatim.

use std::collections::BTreeMap;
use std::sync::Arc;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::plugin::{
    Command, CommandCtx, CommandOutcome, Plugin as PluginTrait, PluginManifest, Tool,
};
use conway::test_support::test_builder;
use conway::{Conway, SessionSpec};
use conway_core::ids::{BackendId, RoleAlias, SeqRange};
use conway_core::log::LogRecord;
use conway_core::ports::SessionStore;
use conway_core::provenance::Provenance;
use conway_testkit::FakeBackend;

const BEEPBOOP_CONFIG_MD: &str = "---\ndescription: Configure beepboop plugin settings (sounds and notifications)\nargument-hint: \"[show | enable sounds | disable sounds]\"\nallowed-tools: Read, Edit, Bash\n---\n\nManage the beepboop plugin configuration.\n\nFind the settings file and update it as directed.\n";

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
        // Deliberately left empty, the identical reason
        // `conway-plugin-skeleton`'s own `file_prompt_command.rs::
        // base_config` gives: this test installs the plugin directly via
        // `with_plugin`, never through `[plugins].install`.
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

/// Wraps every command `ClaudeCompatReport::command_registrations()`
/// produced in a minimal ad-hoc `Plugin` -- the identical shape
/// `conway-plugin-skeleton`'s own `file_prompt_command.rs::DemoPlugin`
/// uses, for the identical reason (a translated command is fallible to
/// build and so cannot be a zero-argument, always-installed built-in).
struct DemoPlugin {
    id: String,
    commands: Vec<Arc<dyn Command>>,
}

impl PluginTrait for DemoPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.id.clone(),
            version: "0.1.0".to_string(),
            tools: vec![],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![]
    }

    fn commands(&self) -> Vec<Arc<dyn Command>> {
        self.commands.clone()
    }
}

fn demo_conway(commands: Vec<Arc<dyn Command>>) -> (Conway, Arc<conway_testkit::FakeStore>) {
    let store = Arc::new(conway_testkit::FakeStore::new());
    let conway = test_builder(base_config())
        .with_backend(Arc::new(FakeBackend::echo(BackendId::new("fake"))))
        .with_session_store(store.clone())
        .with_plugin(Arc::new(DemoPlugin {
            id: "acme-tools".to_string(),
            commands,
        }))
        .build()
        .expect("build should succeed with every port injected");
    (conway, store)
}

/// **The VERIFICATION ANCHOR.** `beepboop`'s own `commands/config.md`
/// content becomes a real, running turn -- through the library API alone,
/// with no TUI in the loop, exactly mirroring
/// `file_prompt_command_submits_the_files_own_body_as_a_real_turn`'s own
/// shape one crate over.
#[tokio::test]
async fn a_translated_claude_command_submits_its_own_body_as_a_real_turn() {
    let plugin_dir = tempfile::tempdir().expect("plugin dir");
    let root = plugin_dir.path();
    std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
    std::fs::write(
        root.join(".claude-plugin").join("plugin.json"),
        r#"{"name":"acme-tools"}"#,
    )
    .unwrap();
    std::fs::create_dir_all(root.join("commands")).unwrap();
    std::fs::write(root.join("commands").join("config.md"), BEEPBOOP_CONFIG_MD).unwrap();

    let report = conway_plugin_claude::discover(root).expect("discover the plugin directory");
    assert_eq!(report.commands.len(), 1);

    // `allowed-tools`/`argument-hint` are named, not silently honored --
    // acceptance point 3, checked here against the real fixture too (the
    // crate's own unit suite already checks this in isolation).
    let unsupported_names: Vec<_> = report.unsupported.iter().map(|u| u.name.as_str()).collect();
    assert!(
        unsupported_names.contains(&"commands/config.md#allowed-tools"),
        "{unsupported_names:?}"
    );
    assert!(
        unsupported_names.contains(&"commands/config.md#argument-hint"),
        "{unsupported_names:?}"
    );

    let commands = report.command_registrations();
    assert_eq!(commands.len(), 1);
    let command = commands[0].clone();
    let spec = command.spec();
    assert_eq!(spec.name, "config");
    assert_eq!(
        spec.summary,
        "Configure beepboop plugin settings (sounds and notifications)"
    );

    let (conway, store) = demo_conway(commands);
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");

    // Invoked directly against the `Command` trait -- the library-API path,
    // no `CommandRegistry`, no TUI dispatch.
    let ctx = CommandCtx {
        focused_agent: handle.root(),
        root_agent: handle.root(),
        session_id: handle.id(),
        args: String::new(),
    };
    let outcome = command.invoke(ctx).await;
    let CommandOutcome::SubmitPrompt { text } = outcome else {
        panic!("expected CommandOutcome::SubmitPrompt, got {outcome:?}");
    };
    assert!(
        !text.contains("$ARGUMENTS"),
        "a translated command must never submit a raw placeholder: {text:?}"
    );
    assert!(text.starts_with("Manage the beepboop plugin configuration."));

    // What the host actually does with the outcome, disclosed by
    // `CommandOutcome::SubmitPrompt`'s own doc: `SessionHandle::
    // prompt_command`, stamping `Provenance::CommandPrompt`.
    let turn = handle
        .prompt_command(handle.root(), text.clone(), "acme-tools.config")
        .await
        .expect("prompt_command should succeed");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), turn.text())
        .await
        .expect("text() must not hang")
        .expect("text() should succeed");

    let records = store
        .read(&handle.id(), SeqRange::full())
        .await
        .expect("read should succeed");
    let submitted = records
        .iter()
        .find(|r| matches!(r, LogRecord::UserTurn { text: t, .. } if t == &text))
        .expect("the command's own body must be appended as a real UserTurn record");
    match submitted {
        LogRecord::UserTurn { prov, .. } => match prov {
            Provenance::CommandPrompt { command } => {
                assert_eq!(command, "acme-tools.config");
            }
            other => panic!(
                "expected Provenance::CommandPrompt, got {other:?} -- a translated command's \
                 turn must never be stamped as if the operator typed it"
            ),
        },
        _ => unreachable!(),
    }

    assert!(
        records
            .iter()
            .any(|r| matches!(r, LogRecord::Assistant { .. })),
        "the submitted prompt must run a real agent turn to completion: {records:?}"
    );
}

/// A command file whose body contains a raw `$ARGUMENTS` placeholder never
/// reaches `command_registrations()` at all -- acceptance point 4, proven
/// against the real `discover`/`command_registrations` path (the crate's
/// own unit suite proves the translation decision in isolation; this
/// proves it survives the full report-to-registration trip).
#[test]
fn a_command_with_a_raw_arguments_placeholder_never_becomes_a_registered_command() {
    let plugin_dir = tempfile::tempdir().expect("plugin dir");
    let root = plugin_dir.path();
    std::fs::create_dir_all(root.join("commands")).unwrap();
    std::fs::write(
        root.join("commands").join("explain.md"),
        "Explain $ARGUMENTS in plain language.\n",
    )
    .unwrap();

    let report = conway_plugin_claude::discover(root).expect("discover the plugin directory");
    assert_eq!(report.commands.len(), 1);
    assert!(report.command_registrations().is_empty());
    assert!(report
        .unsupported
        .iter()
        .any(|u| u.name == "commands/explain.md" && u.reason.contains("$ARGUMENTS")));
}
