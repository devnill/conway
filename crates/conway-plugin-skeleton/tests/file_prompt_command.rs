//! `FilePromptCommand`'s own end-to-end proof (board item
//! `01M0VSMF71S6VXX81YRAAF5S8Q`, acceptance 4: "a file-backed command works
//! end to end -- a markdown file becomes a typeable command that submits
//! its body"). Written the way a library embedder would write it:
//! `ConwayBuilder`, `FakeBackend`/`FakeStore` (the credential-free fakes
//! family CONTRIBUTING's check-liveness rule names as the strongest form
//! of coverage -- no live provider, no network), and
//! `conway_plugin_skeleton::FilePromptCommand` constructed from a REAL
//! markdown file on disk (a tempdir this test creates itself), wrapped in
//! a minimal ad-hoc `Plugin` and installed via `ConwayBuilder::with_plugin`
//! exactly as any third-party plugin would be.
//!
//! `file_prompt_command_submits_the_files_own_body_as_a_real_turn` is the
//! VERIFICATION ANCHOR: constructs the command from a real file, invokes
//! it directly against `conway::plugin::Command` -- the LIBRARY-API path
//! this item's own acceptance 1 requires (no TUI anywhere in this test) --
//! and applies the returned `CommandOutcome::SubmitPrompt` through
//! `SessionHandle::prompt_command`, the SAME facade primitive
//! `conway-cli`'s own `App` uses. The persisted record is checked
//! directly: the file's own body, verbatim, stamped `Provenance::
//! CommandPrompt` -- never `Provenance::UserPrompt` -- and the turn runs
//! to real completion.

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
use conway_plugin_skeleton::FilePromptCommand;
use conway_testkit::FakeBackend;

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
        // Deliberately left empty: this test installs the plugin directly
        // via `with_plugin`, mirroring `conway-plugin-skeleton`'s own
        // `skeleton_end_to_end.rs::base_config` (module doc there explains
        // why `[plugins].install` is a separate, `conway-cli`-owned
        // concern this test does not exercise).
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

/// Wraps a single already-constructed [`Command`] in a minimal `Plugin` --
/// the ad-hoc shape this test needs since `FilePromptCommand` is fallible
/// to construct (a real file read) and so cannot be `SkeletonPlugin`'s own
/// zero-argument, always-installed member (see that command's own doc for
/// why it is deliberately NOT wired there).
struct DemoPlugin {
    command: Arc<dyn Command>,
}

impl PluginTrait for DemoPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "acme".to_string(),
            version: "0.1.0".to_string(),
            tools: vec![],
            required_host_caps: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![]
    }

    fn commands(&self) -> Vec<Arc<dyn Command>> {
        vec![self.command.clone()]
    }
}

fn demo_conway(command: Arc<dyn Command>) -> (Conway, Arc<conway_testkit::FakeStore>) {
    let store = Arc::new(conway_testkit::FakeStore::new());
    let conway = test_builder(base_config())
        .with_backend(Arc::new(FakeBackend::echo(BackendId::new("fake"))))
        .with_session_store(store.clone())
        .with_plugin(Arc::new(DemoPlugin { command }))
        .build()
        .expect("build should succeed with every port injected");
    (conway, store)
}

/// **The VERIFICATION ANCHOR.** A real markdown file's content becomes a
/// real, running turn -- through the library API alone, with no TUI in
/// the loop.
#[tokio::test]
async fn file_prompt_command_submits_the_files_own_body_as_a_real_turn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("review.md");
    std::fs::write(
        &path,
        "Review the diff for obvious bugs and say what you found.\n",
    )
    .expect("write fixture file");

    let command: Arc<dyn Command> =
        Arc::new(FilePromptCommand::from_file("review", &path).expect("from_file should succeed"));
    let (conway, store) = demo_conway(command.clone());

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");

    // Invoked directly against the `Command` trait -- the library-API path
    // this item's own acceptance 1 requires: no `CommandRegistry`, no TUI
    // dispatch, just the port.
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
    assert_eq!(
        text, "Review the diff for obvious bugs and say what you found.\n",
        "the submitted text must be the file's own body, verbatim -- no interpolation"
    );

    // What the host actually does with the outcome, disclosed by
    // `CommandOutcome::SubmitPrompt`'s own doc: `SessionHandle::
    // prompt_command`, stamping `Provenance::CommandPrompt`.
    let turn = handle
        .prompt_command(handle.root(), text.clone(), "acme.review")
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
        .expect("the file's own body must be appended as a real UserTurn record");
    match submitted {
        LogRecord::UserTurn { prov, .. } => match prov {
            Provenance::CommandPrompt { command } => assert_eq!(command, "acme.review"),
            other => panic!(
                "expected Provenance::CommandPrompt, got {other:?} -- a file-backed command's \
                 turn must never be stamped as if the operator typed it"
            ),
        },
        _ => unreachable!(),
    }

    // The turn actually ran: an assistant reply exists for it.
    assert!(
        records
            .iter()
            .any(|r| matches!(r, LogRecord::Assistant { .. })),
        "the submitted prompt must run a real agent turn to completion: {records:?}"
    );
}

/// `CommandCtx::args` is read and ignored -- determine-first question 3's
/// v1 answer (no interpolation of any kind): typing arguments after the
/// command word changes nothing about the submitted text.
#[tokio::test]
async fn file_prompt_command_ignores_operator_supplied_arguments() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("greet.md");
    std::fs::write(&path, "Say hello.\n").expect("write fixture file");

    let command = FilePromptCommand::from_file("greet", &path).expect("from_file should succeed");
    let ctx = CommandCtx {
        focused_agent: conway::AgentId::new(),
        root_agent: conway::AgentId::new(),
        session_id: conway::SessionId::new(),
        args: "ignored operator text".to_string(),
    };
    let outcome = command.invoke(ctx).await;
    assert_eq!(
        outcome,
        CommandOutcome::SubmitPrompt {
            text: "Say hello.\n".to_string()
        }
    );
}

/// A missing file is a construction-time error, not a panic or a silent
/// empty command -- surfaced to the caller directly (this type's own doc).
#[test]
fn from_file_surfaces_a_missing_file_as_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("does-not-exist.md");
    assert!(FilePromptCommand::from_file("nope", &missing).is_err());
}
