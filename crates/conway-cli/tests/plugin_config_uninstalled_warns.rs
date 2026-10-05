//! Board item `01M3TJHCFA3R9PDVZHQTKKVWNR`, the RULING recorded under
//! DOGFOOD 3 `01M1YYB8STS6NDGR254YZ76XKJ`: `[plugins.config."<id>"]` stays an
//! opaque block its own plugin owns, validated strictly -- but a bad block
//! for a plugin that is NOT installed warns rather than blocking startup,
//! while the identical block for a plugin that IS installed still fails the
//! build exactly as before. This suite drives the real compiled `conway`
//! binary (never the unit-level `apply_plugin_config` alone, which
//! `crates/conway-cli/src/first_party_plugins.rs`'s own test module already
//! pins) to prove the OPERATOR-FACING outcome: does the process start, and
//! what, if anything, does it say on stderr.
//!
//! **Break-the-guard note for the "installed still fails" case (the
//! orchestrator's own stub run, `apply_plugin_config_refuses_an_unknown_key_
//! by_name_for_an_installed_plugin` in `first_party_plugins.rs` carries the
//! unit-level version of this same note).** Change
//! `first_party_plugins::apply_plugin_config`'s `if install_ids.contains(&id)
//! { return Err(...) }` arm to always push a warning instead of ever
//! returning `Err` (i.e. delete the `if`/`return Err` and always fall
//! through to `warnings.push(...)`) -- `an_installed_plugins_bad_config_
//! block_still_fails_startup` below goes red (`out.status.success()` is
//! `true` when the assertion expects `false`), while `an_uninstalled_
//! plugins_bad_config_block_warns_and_the_session_still_runs` stays green,
//! proving the first test is the one actually pinned on the installed-vs-
//! not distinction. Restore afterward and confirm with `git diff`.

mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{run_conway, write_fixture, Fixture};

/// Rewrites `fixture`'s rendered `conway.json` (project scope -- the exact
/// file `conway::config::load`'s own project layer merges, same footing
/// `first_party_plugins.rs`'s own `add_plugins_install` already writes to)
/// to carry `[plugins].install = install_ids` and `[plugins.config."<id>"]
/// = value` for a single id. `install_ids` empty means "install nothing",
/// the exact absent-by-default state every other fixture in this workspace
/// starts from.
fn write_plugin_config(
    fixture: &Fixture,
    install_ids: &[&str],
    config_id: &str,
    value: serde_json::Value,
) {
    let raw = std::fs::read_to_string(&fixture.config_path).expect("read rendered conway.json");
    let mut parsed: serde_json::Value = serde_json::from_str(&raw).expect("parse conway.json");
    parsed["plugins"] = serde_json::json!({
        "install": install_ids,
        "config": { config_id: value },
    });
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&parsed).expect("serialize conway.json"),
    )
    .expect("rewrite conway.json with [plugins].config");
}

fn one_turn_script() -> Script {
    Script(vec![vec![Chunk::Text("hi"), Chunk::Finish("stop")]])
}

/// Acceptance 2, half one: `conway.trim` is NOT in `[plugins].install`, and
/// its own `[plugins.config."conway.trim"]` table names an unrecognized key
/// -- the SAME invalid value `apply_plugin_config_refuses_an_unknown_key_
/// by_name_for_an_installed_plugin` (`first_party_plugins.rs`) proves fails
/// the build when the plugin IS installed. Here the run must still SUCCEED
/// (a real, running one-shot turn), with a named warning on stderr.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_uninstalled_plugins_bad_config_block_warns_and_the_session_still_runs() {
    let mock = MockBackend::start(one_turn_script()).await;
    let fixture = write_fixture(&mock, 10);
    write_plugin_config(
        &fixture,
        &[],
        conway_plugin_trim::PLUGIN_ID,
        serde_json::json!({ "keep_tuns": 3 }),
    );

    let out = run_conway(&["-p", "hi"], &fixture);

    assert!(
        out.status.success(),
        "an uninstalled plugin's own bad config block must not block startup; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("conway: warning:"),
        "expected the standard diag::warn prefix on stderr, got: {stderr:?}"
    );
    assert!(
        stderr.contains(conway_plugin_trim::PLUGIN_ID),
        "the warning must name the offending plugin, got: {stderr:?}"
    );
    assert!(
        stderr.contains("keep_tuns"),
        "the warning must still name the offending key, got: {stderr:?}"
    );
}

/// Acceptance 2, half two: the IDENTICAL bad block, for a plugin that IS in
/// `[plugins].install`, must still refuse to start -- the pre-existing
/// fail-closed behavior this item narrows, not removes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_installed_plugins_bad_config_block_still_fails_startup() {
    let mock = MockBackend::start(one_turn_script()).await;
    let fixture = write_fixture(&mock, 10);
    write_plugin_config(
        &fixture,
        &[conway_plugin_trim::PLUGIN_ID],
        conway_plugin_trim::PLUGIN_ID,
        serde_json::json!({ "keep_tuns": 3 }),
    );

    let out = run_conway(&["-p", "hi"], &fixture);

    assert!(
        !out.status.success(),
        "an INSTALLED plugin's own bad config block must still fail the run"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("keep_tuns"),
        "the error must name the offending key, got: {stderr:?}"
    );
}

/// The spec's third named case, resolved per this item's own recommendation
/// (see `WarningCode::PluginConfigIgnored`'s own doc for the "why"): an id
/// in `[plugins.config]` matching NO known plugin at all gets the identical
/// warn-not-fail treatment as the real-but-uninstalled case above -- there
/// is no plugin here to even validate against, let alone refuse to start
/// over.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_config_block_naming_no_known_plugin_warns_rather_than_failing() {
    let mock = MockBackend::start(one_turn_script()).await;
    let fixture = write_fixture(&mock, 10);
    write_plugin_config(
        &fixture,
        &[],
        "conway.totally_unknown",
        serde_json::json!({ "anything": 1 }),
    );

    let out = run_conway(&["-p", "hi"], &fixture);

    assert!(
        out.status.success(),
        "a config block naming no known plugin must not block startup; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("conway: warning:"),
        "expected the standard diag::warn prefix on stderr, got: {stderr:?}"
    );
    assert!(
        stderr.contains("conway.totally_unknown"),
        "the warning must name the offending id, got: {stderr:?}"
    );
}
