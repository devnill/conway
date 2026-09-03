//! `conway plugin list|install|remove` (board item
//! `01M1FSDRF20E2EGHCG3RK28DKH`): the headless half of the interactive
//! `/plugin` command, plus the startup notice for a settings document that
//! predates conway's own default-plugin opinion.
//!
//! Reuses the harness (`tests/common/mod.rs`) unchanged, the same way
//! `plugin_subcommand.rs`/`config_warnings.rs` do: `common::command` points
//! `CONWAY_CONFIG_DIR` at the fixture's own temp dir, so
//! `~/.conway/settings.json` resolves to `<fixture>/settings.json` -- a
//! file this suite writes and reads directly, never a real developer's own
//! home directory.

mod common;

use std::path::PathBuf;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{command, write_fixture, Fixture};

/// The exact path `/plugin`'s own writer (and this command) targets for
/// `fixture` -- mirrors `common::command`'s own `CONWAY_CONFIG_DIR` choice.
fn settings_path(fixture: &Fixture) -> PathBuf {
    fixture.dir.path().join("settings.json")
}

/// conway's own six-id default opinion set (decision
/// `01M1FQFP5D0R3M9GC8R8Z24F5N`) -- pinned here, by literal, as the
/// independent expectation every assertion below checks the real binary's
/// output against. If this list and `first_party_plugins::
/// DEFAULT_OPINION_SET` ever disagree, that is exactly the drift this test
/// exists to catch -- so this is deliberately NOT `use`d from the source
/// crate.
const DEFAULT_IDS: [&str; 6] = [
    "conway.idiom",
    "conway.stepguard",
    "conway.skills",
    "conway.memory",
    "conway.names",
    "conway.history",
];

/// Acceptance 1: on a fixture with no `plugins.install` at all, `conway
/// plugin list` prints one row per compiled-in candidate, every one `[ ]`,
/// exit 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_with_no_install_key_shows_every_bundle_member_off() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);

    let out = command(&["plugin", "list"], &fixture)
        .output()
        .expect("run conway binary");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    // A handful of known first-party ids, each present and off -- proves
    // this is the real bundle, not an empty or stubbed listing.
    for id in ["conway.memory", "conway.history", "conway.idiom", "conway.confine"] {
        assert!(
            stdout.contains(&format!("[ ] {id} ")),
            "expected an off row for {id}, got stdout:\n{stdout}"
        );
    }
    // BREAK-THE-GUARD: no row anywhere claims to be on.
    assert!(
        !stdout.contains("[x]"),
        "no plugin should show installed on a fixture with no plugins.install key: {stdout}"
    );
    // One row per bundle member: at least a dozen, matching this binary's
    // own linked candidate count (skeleton, history, stepguard, skills,
    // memory, path, discover, idiom, trim, names, ui, confine) without
    // hardcoding the exact figure, which would drift the moment a new
    // first-party plugin lands.
    let row_count = stdout.lines().filter(|l| l.starts_with('[')).count();
    assert!(
        row_count >= 12,
        "expected at least 12 rows (this binary's own linked bundle), got {row_count}:\n{stdout}"
    );
}

/// Acceptance 2: `conway plugin install --defaults` writes exactly the six
/// ruled ids, and a subsequent `conway plugin list` shows exactly those six
/// as `[x]` -- every other bundle member stays `[ ]`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_defaults_writes_exactly_the_six_ruled_ids() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);

    let install_out = command(&["plugin", "install", "--defaults"], &fixture)
        .output()
        .expect("run conway binary");
    assert!(
        install_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&install_out.stderr)
    );

    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(settings_path(&fixture)).expect("read settings.json"),
    )
    .expect("settings.json must be valid JSON");
    let installed: Vec<String> = settings["plugins"]["install"]
        .as_array()
        .expect("plugins.install must be an array")
        .iter()
        .map(|v| v.as_str().expect("id must be a string").to_string())
        .collect();
    assert_eq!(
        installed,
        DEFAULT_IDS.to_vec(),
        "settings.json plugins.install must equal the ruled default set, in order"
    );

    let list_out = command(&["plugin", "list"], &fixture)
        .output()
        .expect("run conway binary");
    assert!(list_out.status.success());
    let stdout = String::from_utf8_lossy(&list_out.stdout);
    for id in DEFAULT_IDS {
        assert!(
            stdout.contains(&format!("[x] {id} ")),
            "expected {id} to show installed after --defaults, got:\n{stdout}"
        );
    }
    // Not every member of the default set -- `conway.confine` must stay off.
    assert!(
        stdout.contains("[ ] conway.confine "),
        "conway.confine is not part of the default opinion set and must stay off: {stdout}"
    );
}

/// Acceptance 3, half one: `conway plugin remove <id>` removes only that
/// id, leaving the rest of a prior `--defaults` install untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remove_takes_out_only_the_named_id() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);

    command(&["plugin", "install", "--defaults"], &fixture)
        .output()
        .expect("run conway binary");

    let remove_out = command(&["plugin", "remove", "conway.memory"], &fixture)
        .output()
        .expect("run conway binary");
    assert!(
        remove_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&remove_out.stderr)
    );

    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(settings_path(&fixture)).expect("read settings.json"),
    )
    .expect("settings.json must be valid JSON");
    let installed: Vec<String> = settings["plugins"]["install"]
        .as_array()
        .expect("plugins.install must be an array")
        .iter()
        .map(|v| v.as_str().expect("id must be a string").to_string())
        .collect();
    assert!(
        !installed.iter().any(|id| id == "conway.memory"),
        "conway.memory must be gone: {installed:?}"
    );
    for id in DEFAULT_IDS {
        if id == "conway.memory" {
            continue;
        }
        assert!(
            installed.iter().any(|installed_id| installed_id == id),
            "{id} must survive the removal of a DIFFERENT id: {installed:?}"
        );
    }
}

/// Acceptance 3, half two: an unrecognized id is a usage error (exit 2)
/// naming at least one id this binary actually links -- never a silent
/// no-op write.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_unknown_id_exits_usage_and_names_a_known_id() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);

    let out = command(&["plugin", "install", "conway.nope"], &fixture)
        .output()
        .expect("run conway binary");

    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("conway.nope"), "{stderr}");
    assert!(
        stderr.contains("conway.memory"),
        "expected at least one known id named in the error: {stderr}"
    );

    // BREAK-THE-GUARD: nothing was written for the unknown id.
    assert!(
        !settings_path(&fixture).exists(),
        "an unknown id must never create settings.json at all"
    );
}

/// Acceptance 4: `conway plugin` with no subcommand is a usage error (exit
/// 2) that prints help text, not a hang or a silent no-op.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bare_plugin_exits_usage_with_help_text() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);

    let out = command(&["plugin"], &fixture)
        .output()
        .expect("run conway binary");

    assert_eq!(out.status.code(), Some(2));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("Usage:") && combined.contains("list") && combined.contains("install"),
        "expected clap's own help text naming the list/install/remove subcommands: {combined}"
    );
}

/// Acceptance 5, half one: on a fixture whose user-scope settings document
/// has NO `plugins.install` key at all, `conway -p hi` (against a live mock
/// backend) prints the startup notice on stderr, naming `conway plugin
/// install --defaults`.
///
/// Observed failing before the implementation: with no such notice wired,
/// this fixture's stderr never contained "no first-party plugins" at all --
/// confirmed by reverting to the pre-item code and re-running this exact
/// assertion by hand (P-15: every "shown to fail first" claim here was
/// checked, not assumed).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_install_key_notice_is_visible_on_stderr_for_one_shot_print() {
    let mock =
        MockBackend::start(Script(vec![vec![Chunk::Text("hi"), Chunk::Finish("stop")]])).await;
    let fixture = write_fixture(&mock, 10);
    // Deliberately no settings.json at all -- the most literal "no
    // plugins.install key" case.
    assert!(!settings_path(&fixture).exists());

    let out = command(&["-p", "hi"], &fixture)
        .output()
        .expect("run conway binary");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no first-party plugins are installed"),
        "expected the startup notice on stderr, got: {stderr:?}"
    );
    assert!(
        stderr.contains("conway plugin install --defaults"),
        "expected the notice to name the exact remedy command, got: {stderr:?}"
    );
    for id in DEFAULT_IDS {
        assert!(
            stderr.contains(id),
            "expected the notice to name {id} (declaration honesty -- built from \
             DEFAULT_OPINION_SET itself), got: {stderr:?}"
        );
    }
}

/// Acceptance 5, half two (BREAK-THE-GUARD): with `"plugins": {"install":
/// []}` -- an EXPLICIT empty array, not an absent key -- the same notice
/// must be silent. Proves this checks key PRESENCE, not the merged
/// `plugins.install`'s own emptiness.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_explicit_empty_install_array_silences_the_notice() {
    let mock =
        MockBackend::start(Script(vec![vec![Chunk::Text("hi"), Chunk::Finish("stop")]])).await;
    let fixture = write_fixture(&mock, 10);
    std::fs::write(
        settings_path(&fixture),
        serde_json::json!({ "plugins": { "install": [] } }).to_string(),
    )
    .expect("write settings.json with an explicit empty plugins.install");

    let out = command(&["-p", "hi"], &fixture)
        .output()
        .expect("run conway binary");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("no first-party plugins are installed"),
        "an explicit empty plugins.install must silence the notice, got: {stderr:?}"
    );
}
