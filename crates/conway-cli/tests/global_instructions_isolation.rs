//! Compiled-binary regression test for board item
//! `01M0W5Q569F0T97HSEP6F0MPCR`: `conway_plugin_idiom::global_instructions_path`
//! used to derive from `conway::config::discovery::home_settings_path` -- the
//! raw, `CONWAY_CONFIG_DIR`-independent home path -- rather than
//! `user_config_path(env)`, the one that honours the variable. An operator
//! (or embedder) relocating conway's config directory via `CONWAY_CONFIG_DIR`
//! believed every layer had moved; the operator-instructions file quietly
//! stayed pinned to the real `$HOME`, the same isolation gap board item
//! `01M0VV6CVSZM4XH8J4G6EBV5E3` closed for `settings.json` itself.
//!
//! **No real `$HOME`, no live network, ever.** `SimulatedHome` below is an
//! ordinary [`tempfile::TempDir`] this process created and owns, standing in
//! for the operator's real home directory -- never the real one, never read
//! or written to. The mock backend is a loopback listener this test binds
//! itself (`common::mock_backend::MockBackend`), never a real provider.
//!
//! Mirrors `config_isolation_binary.rs`'s own shape (a simulated `$HOME` via
//! `HOME`/`USERPROFILE`, because `directories::BaseDirs` resolves what the
//! OS reports as home) but drives the request through a real one-shot `-p`
//! turn against `common::mock_backend::MockBackend` instead of a read-only
//! `sessions list`, so the assertion is on the actual wire request the
//! `conway.idiom` plugin's operator-global fragment lands in -- the real
//! pipeline, not a unit-level path comparison.
//!
//! **Unlike `config_isolation_binary.rs`'s two `#[cfg(unix)]`-gated tests
//! (board item `01M18Q8AASY761DQ5HNN83TFY4`), the test below is NOT
//! gated**, and deliberately so: `common::command` (which this test uses)
//! always sets `CONWAY_CONFIG_DIR` on the child to the fixture's own temp
//! dir, and `global_instructions_path` -> `conway::config::discovery::
//! user_config_path` returns as soon as `CONWAY_CONFIG_DIR` is set and
//! non-empty in `env`, *before* ever calling `home_settings_path()` (the
//! `directories::BaseDirs`-backed lookup that does not honour
//! `HOME`/`USERPROFILE` on Windows). This test's own `HOME`/`USERPROFILE`
//! overrides and its `simulated_home`/`POISON_MARKER` setup are therefore
//! inert on every platform under the code path this specific scenario
//! exercises (`CONWAY_CONFIG_DIR` always set) -- not merely on Windows --
//! so there is no platform-dependent behavior here to gate against. Kept
//! anyway as a belt-and-suspenders negative assertion (see the test's own
//! doc below).

mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{command, write_fixture};

/// Written into the SIMULATED home's `.conway/instructions.md` -- the file
/// the pre-fix code reads regardless of `CONWAY_CONFIG_DIR`. Must never
/// appear in the wire request once the fix holds.
const POISON_MARKER: &str = "POISON_GLOBAL_INSTRUCTIONS_MARKER_9F3E1A";

/// Written into the ISOLATED `CONWAY_CONFIG_DIR`'s own `instructions.md` --
/// the file `CONWAY_CONFIG_DIR` isolation promises will be read instead. Must
/// appear in the wire request once the fix holds.
const EXPECTED_MARKER: &str = "EXPECTED_GLOBAL_INSTRUCTIONS_MARKER_2C7B08";

/// Adds `"plugins": {"install": ["conway.idiom"]}` to an already-rendered
/// fixture `conway.json` -- `TEMPLATE` (`common::mod.rs`) carries no
/// `[plugins]` section at all, and `conway.idiom` is off by default
/// (`PluginsConfig::default().install` is empty), so this is the one edit
/// needed to make the plugin under test actually selected.
fn enable_idiom_plugin(config_path: &std::path::Path) {
    let raw = std::fs::read_to_string(config_path).expect("read fixture conway.json");
    let mut value: serde_json::Value = serde_json::from_str(&raw).expect("parse fixture json");
    value["plugins"] = serde_json::json!({ "install": ["conway.idiom"] });
    std::fs::write(
        config_path,
        serde_json::to_vec_pretty(&value).expect("serialize fixture json"),
    )
    .expect("rewrite fixture conway.json");
}

/// **ACCEPTANCE 1/2 (board item `01M0W5Q569F0T97HSEP6F0MPCR`).** Before the
/// fix: `global_instructions_path` reads `<simulated $HOME>/.conway/
/// instructions.md` (via `home_settings_path`, override-independent)
/// regardless of `CONWAY_CONFIG_DIR`, so the wire request carries
/// `POISON_MARKER` and never `EXPECTED_MARKER`. After the fix:
/// `global_instructions_path` reads `<CONWAY_CONFIG_DIR>/instructions.md`
/// instead, so the request carries `EXPECTED_MARKER` and never
/// `POISON_MARKER`.
///
/// Not `#[cfg(unix)]`-gated (see this file's own module doc for why, and
/// contrast with `config_isolation_binary.rs`'s two gated tests, board item
/// `01M18Q8AASY761DQ5HNN83TFY4`): `command()` always sets
/// `CONWAY_CONFIG_DIR`, so `global_instructions_path` never falls back to
/// the `directories::BaseDirs`-backed `home_settings_path()` in this
/// scenario on any platform, Windows included.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conway_config_dir_relocates_the_global_operator_instructions_file() {
    let simulated_home = tempfile::tempdir().expect("tempdir for simulated $HOME");
    let home_conf_dir = simulated_home.path().join(".conway");
    std::fs::create_dir_all(&home_conf_dir).expect("create simulated $HOME/.conway");
    std::fs::write(
        home_conf_dir.join("instructions.md"),
        format!("{POISON_MARKER}\n"),
    )
    .expect("write poisoned instructions.md under simulated $HOME");

    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("orchestrator done"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);
    enable_idiom_plugin(&fixture.config_path);

    // `command()`'s own doc: `CONWAY_CONFIG_DIR` is set to `fixture.dir
    // .path()` -- so, after the fix, this is exactly where
    // `global_instructions_path` resolves `instructions.md` against.
    std::fs::write(
        fixture.dir.path().join("instructions.md"),
        format!("{EXPECTED_MARKER}\n"),
    )
    .expect("write the isolated CONWAY_CONFIG_DIR's own instructions.md");

    let out = command(&["-p", "hello"], &fixture)
        // `HOME`/`USERPROFILE` point at the SIMULATED home, never this
        // developer machine's real one -- `directories::BaseDirs::home_dir`
        // is what the pre-fix `home_settings_path` call consults, and what
        // makes this test a faithful reproduction rather than a no-op.
        .env("HOME", simulated_home.path())
        .env("USERPROFILE", simulated_home.path())
        .output()
        .expect("run conway binary");

    assert!(
        out.status.success(),
        "conway -p should succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "exactly one /chat/completions request");
    let body = serde_json::to_string(&requests[0]).expect("serialize captured request body");

    assert!(
        body.contains(EXPECTED_MARKER),
        "the wire request must carry the operator-global instructions fragment sourced from \
         <CONWAY_CONFIG_DIR>/instructions.md; it did not. Isolation is defeated if this fails: \
         global_instructions_path is not honouring CONWAY_CONFIG_DIR. Full request body: {body}"
    );
    assert!(
        !body.contains(POISON_MARKER),
        "the wire request must NOT carry the poisoned instructions.md sourced from the \
         simulated $HOME -- this is board item 01M0W5Q569F0T97HSEP6F0MPCR's own defect, \
         reproduced: global_instructions_path read the real (simulated) home directory instead \
         of CONWAY_CONFIG_DIR. Full request body: {body}"
    );
}
