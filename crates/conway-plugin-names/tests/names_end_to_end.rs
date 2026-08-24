//! `conway.names` against a REAL store file (board item
//! `01M0TV5BSE98S16SFYECG9G9WP`).
//!
//! Everything here writes to a `tempfile::TempDir`, never to the operator's
//! own `~/.conway/`. The plugin's store path is resolved from an EXPLICIT
//! env map (`default_store_path`), so there is no ambient read to leak
//! through -- the defect class `crates/conway/tests/config_isolation_guard.
//! rs` exists to catch. No test in this file consults `std::env`.
//!
//! The unit tests in `src/lib.rs` cover argument parsing and the in-memory
//! store; this file covers the two things only a real file can prove:
//! **the name survives a restart**, and a rename made through the PLUGIN'S
//! OWN COMMAND is what lands in the file a second process would read.
//! The other half of the loop -- resolving that name back to the same agent
//! through `resolve_agent` -- lives in `conway-cli`'s own
//! `tui::commands` tests, because `resolve_agent` is private to that crate.

use std::collections::HashMap;
use std::sync::Arc;

use conway::plugin::{Command, CommandCtx, CommandOutcome, Plugin};
use conway::AgentId;
use conway_plugin_names::{
    default_store_path, AgentNames, AgentNamesError, FsAgentNames, InMemoryAgentNames, NamesPlugin,
    COMMAND_NAME_RENAME, COMMAND_NAME_UNNAME, PLUGIN_ID, STORE_FILE_NAME,
};

/// An env map naming `dir` as the whole conway config directory -- the same
/// redirection every hermetic test in this workspace uses.
fn env_at(dir: &std::path::Path) -> HashMap<String, String> {
    [(
        "CONWAY_CONFIG_DIR".to_string(),
        dir.to_string_lossy().to_string(),
    )]
    .into_iter()
    .collect()
}

fn command_named(plugin: &NamesPlugin, name: &str) -> Arc<dyn Command> {
    plugin
        .commands()
        .into_iter()
        .find(|c| c.spec().name == name)
        .unwrap_or_else(|| panic!("{PLUGIN_ID} declares no `{name}` command"))
}

fn ctx(focused: AgentId, args: &str) -> CommandCtx {
    CommandCtx {
        focused_agent: focused,
        root_agent: focused,
        session_id: conway::SessionId::new(),
        args: args.to_string(),
    }
}

/// Acceptance 2, the durable half: a name set through the plugin's own
/// `/conway.names.rename` is still there after the store is dropped and a
/// FRESH one is opened over the same path -- which is what a restart is.
#[tokio::test]
async fn a_name_set_through_the_command_survives_reopening_the_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = default_store_path(&env_at(dir.path())).expect("CONWAY_CONFIG_DIR resolves a path");
    assert_eq!(path, dir.path().join(STORE_FILE_NAME));

    let agent = AgentId::new();
    {
        let store: Arc<dyn AgentNames> =
            Arc::new(FsAgentNames::open(path.clone()).expect("open a fresh store"));
        let plugin = NamesPlugin::new(store);
        let outcome = command_named(&plugin, COMMAND_NAME_RENAME)
            .invoke(ctx(agent, "scout"))
            .await;
        assert!(
            matches!(outcome, CommandOutcome::Output(_)),
            "the rename must succeed: {outcome:?}"
        );
    }
    assert!(path.exists(), "the rename must have written {path:?}");

    // The restart.
    let reopened = FsAgentNames::open(path).expect("reopen");
    assert_eq!(
        reopened.get(&agent).as_deref(),
        Some("scout"),
        "the name did not survive a restart"
    );
}

/// Acceptance 5: removal reaches the FILE, not only the in-memory map --
/// otherwise an unnamed agent would come back named after a restart.
#[tokio::test]
async fn unname_removes_the_entry_from_the_file_too() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = default_store_path(&env_at(dir.path())).expect("path");
    let agent = AgentId::new();
    {
        let store: Arc<dyn AgentNames> = Arc::new(FsAgentNames::open(path.clone()).expect("open"));
        let plugin = NamesPlugin::new(store);
        command_named(&plugin, COMMAND_NAME_RENAME)
            .invoke(ctx(agent, "scout"))
            .await;
        let outcome = command_named(&plugin, COMMAND_NAME_UNNAME)
            .invoke(ctx(agent, ""))
            .await;
        assert!(matches!(outcome, CommandOutcome::Output(_)), "{outcome:?}");
    }
    let reopened = FsAgentNames::open(path).expect("reopen");
    assert_eq!(reopened.get(&agent), None, "the removal was not persisted");
    assert!(reopened.list().is_empty());
}

/// Two agents, two names, one file -- and the file is a plain JSON document
/// keyed by the full agent id, so an operator who opens it can read it.
#[test]
fn the_store_file_is_readable_json_keyed_by_the_full_agent_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(STORE_FILE_NAME);
    let a = AgentId::new();
    let b = AgentId::new();
    let store = FsAgentNames::open(path.clone()).expect("open");
    store.set(&a, "scout").expect("set a");
    store.set(&b, "runner").expect("set b");

    let body = std::fs::read_to_string(&path).expect("read");
    let doc: serde_json::Value = serde_json::from_str(&body).expect("valid json");
    assert_eq!(doc["names"][a.to_string()], "scout");
    assert_eq!(doc["names"][b.to_string()], "runner");
}

/// A missing file is an empty store (the ordinary first run), not an error.
#[test]
fn a_missing_store_file_opens_as_an_empty_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FsAgentNames::open(dir.path().join(STORE_FILE_NAME)).expect("open");
    assert!(store.list().is_empty());
}

/// A file this crate cannot parse is a hard error, NOT an empty store: a
/// silent fallback would let the very next `set` overwrite whatever was
/// actually in it.
#[test]
fn a_corrupt_store_file_is_an_error_rather_than_a_silent_empty_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(STORE_FILE_NAME);
    std::fs::write(&path, "{ this is not json").expect("write");
    let err = FsAgentNames::open(path).expect_err("a corrupt store must not open as empty");
    assert!(
        matches!(err, AgentNamesError::Corrupt { .. }),
        "expected Corrupt, got {err:?}"
    );
    assert!(
        err.to_string().contains("conway.names"),
        "the message must tell the operator how to get running again: {err}"
    );
}

/// An entry whose key is not a valid agent id is dropped on load rather
/// than failing the whole store -- it names nothing that could ever exist.
#[test]
fn an_entry_with_an_unparseable_key_is_dropped_not_fatal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(STORE_FILE_NAME);
    let good = AgentId::new();
    std::fs::write(
        &path,
        format!(r#"{{"names":{{"not-a-ulid":"ghost","{good}":"scout"}}}}"#),
    )
    .expect("write");
    let store = FsAgentNames::open(path).expect("open");
    assert_eq!(store.list(), vec![(good, "scout".to_string())]);
}

/// The uninstalled/opted-out path: `InMemoryAgentNames` is what the host
/// constructs when nobody named `conway.names` in `[plugins].install`, and
/// it must touch no file at all.
#[test]
fn the_in_memory_store_writes_nothing_to_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = InMemoryAgentNames::new();
    store.set(&AgentId::new(), "scout").expect("set");
    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read_dir")
        .filter_map(Result::ok)
        .collect();
    assert!(
        entries.is_empty(),
        "the non-durable store must create no files: {entries:?}"
    );
}
