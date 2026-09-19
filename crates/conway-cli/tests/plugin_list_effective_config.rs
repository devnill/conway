//! `conway plugin list` reports a plugin's EFFECTIVE configuration, not its
//! compiled-in default -- board item `01M2VA3ARE8RGRZ40VDC349HVK`.
//!
//! The defect this suite pins: with `plugins.config."conway.trim".keep_turns
//! = 3` in `settings.json` and the plugin installed, the listing printed the
//! unconfigured default (`8`). The setting itself worked -- proven at the
//! wire, counting messages in a mock backend's logged request bodies -- so
//! the ONE surface that reports the active window was reporting a number the
//! running system did not use, which reads as authoritative and is worse
//! than showing nothing.
//!
//! Kept as its own file rather than appended to `plugin_cli.rs`: that suite
//! covers the `list`/`install`/`remove` command surface itself (rows, exit
//! codes, what gets written to `settings.json`), while everything here is
//! about config RESOLUTION reaching a renderer. Each `tests/*.rs` compiles
//! `common/` fresh as its own crate, so the small `block_for` helper below
//! is duplicated rather than shared, matching this directory's existing
//! convention.

mod common;

use common::mock_backend::{MockBackend, Script};
use common::{command, write_fixture, Fixture};

/// The exact file `common::command`'s own `CONWAY_CONFIG_DIR` makes
/// `~/.conway/settings.json` resolve to for `fixture` -- the user-scoped
/// document `[plugins.config.<id>]` is written into here, never a real
/// developer's home directory.
fn settings_path(fixture: &Fixture) -> std::path::PathBuf {
    fixture.dir.path().join("settings.json")
}

/// Installs `conway.trim` and gives it `keep_turns = <keep_turns>`, unless
/// `keep_turns` is `None` (installed, no `plugins.config` table at all --
/// the default case).
fn write_trim_settings(fixture: &Fixture, keep_turns: Option<u32>) {
    let mut plugins = serde_json::json!({ "install": ["conway.trim"] });
    if let Some(keep_turns) = keep_turns {
        plugins["config"] = serde_json::json!({ "conway.trim": { "keep_turns": keep_turns } });
    }
    std::fs::write(
        settings_path(fixture),
        serde_json::json!({ "plugins": plugins }).to_string(),
    )
    .expect("write settings.json");
}

/// One plugin's whole block out of `conway plugin list --verbose`: its `[x]`
/// row plus every indented line under it, up to the next row.
fn block_for(stdout: &str, id: &str) -> String {
    let marker = format!(" {id} ");
    let mut lines = stdout.lines().peekable();
    while let Some(line) = lines.next() {
        if line.starts_with('[') && line.contains(&marker) {
            let mut block = vec![line.to_string()];
            while let Some(next) = lines.peek() {
                if next.starts_with('[') {
                    break;
                }
                block.push(lines.next().expect("peeked line must exist").to_string());
            }
            return block.join("\n");
        }
    }
    panic!("no row found for {id} in stdout:\n{stdout}");
}

/// P-15's load-bearing test: with `keep_turns = 3` configured, `conway
/// plugin list --verbose` reports **3**, the value the curator actually
/// uses. This fails against HEAD, where `commands::plugin::browser_entries`
/// read `first_party_plugins::all_bundle_plugins` -- the unfiltered
/// candidate scan, which never applies `[plugins.config.<id>]` -- and the
/// block therefore said `8`.
///
/// Asserts on the rendered window itself (`older than 3 turns`), the
/// operator-visible outcome, rather than on which function the command now
/// calls: the sentence an operator reads is the thing that was wrong.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_list_reports_the_configured_keep_turns_not_the_default() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    write_trim_settings(&fixture, Some(3));

    let out = command(&["plugin", "list", "--verbose"], &fixture)
        .output()
        .expect("run conway binary");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let block = block_for(&stdout, conway_plugin_trim::PLUGIN_ID);
    assert!(
        block.contains("older than 3 turns"),
        "expected the CONFIGURED window (keep_turns = 3) in conway.trim's block, got:\n{block}"
    );
    // BREAK-THE-GUARD: the default must be gone, not merely accompanied.
    // Without this, a renderer that printed both numbers would still pass.
    assert!(
        !block.contains(&format!(
            "older than {} turns",
            conway_plugin_trim::DEFAULT_KEEP_TURNS
        )),
        "the unconfigured default must not appear once keep_turns is configured, got:\n{block}"
    );
}

/// The same block, with no `plugins.config` table at all, still reports the
/// compiled-in default -- so the test above is pinning config APPLICATION,
/// not a hardcoded `3` that would pass however the number was produced.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_list_reports_the_default_keep_turns_when_nothing_is_configured() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    write_trim_settings(&fixture, None);

    let out = command(&["plugin", "list", "--verbose"], &fixture)
        .output()
        .expect("run conway binary");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let block = block_for(&stdout, conway_plugin_trim::PLUGIN_ID);
    assert!(
        block.contains(&format!(
            "older than {} turns",
            conway_plugin_trim::DEFAULT_KEEP_TURNS
        )),
        "with no config table, the compiled-in default must be what is reported, got:\n{block}"
    );
}

/// `conway plugin list <id>` -- the single-id detail form, which prints the
/// full breakdown without `--verbose` -- resolves config on the identical
/// path. Pinned separately because it is a different arm of `list` (a
/// one-element slice through `rows_from_plugin_browser`), and an
/// implementation that fixed only the table would leave it printing `8`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_list_single_id_detail_reports_the_configured_keep_turns() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    write_trim_settings(&fixture, Some(2));

    let out = command(&["plugin", "list", conway_plugin_trim::PLUGIN_ID], &fixture)
        .output()
        .expect("run conway binary");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("older than 2 turns"),
        "expected the configured window in the single-id detail output, got:\n{stdout}"
    );
}

/// The provenance ruling (this item's point 3): a configured plugin's block
/// names the `[plugins.config."<id>"]` table its values came from, with the
/// keys conway actually accepted. Without this an operator cannot tell an
/// operator-set value from a default that happens to match it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_configured_plugins_block_names_the_table_its_values_came_from() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    write_trim_settings(&fixture, Some(3));

    let out = command(&["plugin", "list", "--verbose"], &fixture)
        .output()
        .expect("run conway binary");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let block = block_for(&stdout, conway_plugin_trim::PLUGIN_ID);
    assert!(
        block.contains("keep_turns = 3"),
        "expected the accepted key and value to be named, got:\n{block}"
    );
    assert!(
        block.contains(&format!(
            "[plugins.config.\"{}\"]",
            conway_plugin_trim::PLUGIN_ID
        )),
        "expected the table the values came from to be named, got:\n{block}"
    );
}

/// The other half of that ruling, and the silent-fallback case the whole
/// item is about: an operator who mistypes the plugin ID in
/// `plugins.config` gets no error (the table simply matches no candidate),
/// so `conway.trim`'s block must say outright that it is running on
/// defaults. Reading `8` with no provenance line is exactly the ambiguity
/// this closes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unconfigured_plugins_block_says_defaults_outright() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    // A mistyped id: `conway.trimm` names no compiled-in plugin, so nothing
    // applies it and `conway.trim` itself keeps its defaults.
    std::fs::write(
        settings_path(&fixture),
        serde_json::json!({
            "plugins": {
                "install": ["conway.trim"],
                "config": { "conway.trimm": { "keep_turns": 3 } },
            }
        })
        .to_string(),
    )
    .expect("write settings.json");

    let out = command(&["plugin", "list", "--verbose"], &fixture)
        .output()
        .expect("run conway binary");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let block = block_for(&stdout, conway_plugin_trim::PLUGIN_ID);
    assert!(
        block.contains("defaults"),
        "a plugin with no table of its own must say so, got:\n{block}"
    );
    assert!(
        block.contains(&format!(
            "older than {} turns",
            conway_plugin_trim::DEFAULT_KEEP_TURNS
        )),
        "a mistyped plugin id must leave the real plugin on its default, got:\n{block}"
    );
}
