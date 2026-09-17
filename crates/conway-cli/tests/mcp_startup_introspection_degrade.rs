//! CLI-level acceptance test for board item `01M2PJCT90G2010KGCJ4YSFREM`:
//! a crashing `[plugins].mcp[]` entry used to fail `main.rs::build_conway`
//! for every dispatch target alike, including the six that never start an
//! agent or call a tool (`routes explain`, `sessions`, `tools list`,
//! `plugin list`/`install`/`remove`) -- the exact choke point the item's
//! own traced mechanism names, reached through the real compiled binary
//! here rather than only through `mcp_plugins.rs`'s own in-process unit
//! tests (those prove `install_tolerant` degrades a `ConwayBuilder`
//! directly; this file proves the wiring at `main.rs`'s own dispatch choke
//! point actually reaches it, and that the sibling turn-taking posture is
//! untouched).
//!
//! Two tests, deliberately paired (the item's own P-15 asks for exactly
//! this pairing so the fix cannot over-apply):
//!
//! 1. [`routes_explain_succeeds_and_names_the_crashing_mcp_entry`] -- an
//!    introspection command degrades instead of refusing.
//! 2. [`a_turn_taking_invocation_still_fails_loudly_on_the_same_crashing_
//!    mcp_entry`] -- the SAME fixture, run through `-p` instead, still
//!    fails the whole build, unchanged from before this item.

#[allow(dead_code)]
mod common;

// A sibling top-level module, not nested inside `common` (`common/mod.rs`
// is out of this writer's fence) -- see `dogfood_routes_and_status.rs`'s
// own identical `mcp_fixtures` inclusion for the precedent this follows.
#[path = "common/mcp_fixtures.rs"]
mod mcp_fixtures;

use common::mock_backend::{MockBackend, Script};

/// A minimal MCP "server" that exits before ever answering `initialize` --
/// board item `01M2PJCT90G2010KGCJ4YSFREM`'s own traced shape ("MCP plugin
/// 'x' session died: closed stdout (EOF) mid-session"). Deliberately NOT
/// one of `mcp_fixtures`'s own constants (that module is read-only, and
/// none of its constants die this early -- `ALWAYS_DIE_SERVER` completes
/// the handshake and only dies on `tools/call`, `SLEEP_SERVER` never dies
/// at all): reproduced here as a fresh, minimal script, mirroring
/// `claude_compat_plugins.rs`'s own `write_dying_mcp_server` test helper's
/// identical shape for the identical reason (a crate-boundary fixture
/// cannot be imported, so it is reproduced rather than referenced).
const DIES_BEFORE_HANDSHAKE: &str = "#!/usr/bin/env python3\nimport sys\nsys.exit(1)\n";

/// Writes `<dir>/dies.py` (the crashing MCP entry) and returns the
/// `[plugins].mcp[]` JSON value naming it -- shared by both fixtures below
/// so the exact same failure mode drives both tests.
async fn crashing_mcp_entry(dir: &std::path::Path) -> serde_json::Value {
    let script_path = mcp_fixtures::write_script(dir, "dies.py", DIES_BEFORE_HANDSHAKE);
    // Board item 01M09MPZ9C188AHNBKWEJ3CEQA: warm the freshly-written
    // script once before the real, timed install-time handshake below --
    // every other `mcp_fixtures` consumer in this crate follows the same
    // precedent (see that item's own doc, restated in `mcp_fixtures::warm`).
    mcp_fixtures::warm(&script_path).await;
    serde_json::json!({
        "id": "acme-crashing-mcp",
        "command": [script_path.display().to_string()],
    })
}

/// A fixture with no reachable backend at all (`routes explain` never
/// dials one) plus the one crashing `[plugins].mcp[]` entry -- mirrors
/// `dogfood_routes_and_status.rs::fixture_with_unverified_floor`'s own
/// "own JSON, not the shared template" shape, since `common::write_fixture`
/// has no `[plugins]` slot to patch into.
async fn fixture_for_introspection() -> common::Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let mcp_entry = crashing_mcp_entry(dir.path()).await;
    let config = serde_json::json!({
        "default_role": "default",
        "limits": { "max_steps": 5 },
        "backends": {
            "unreachable": {
                "kind": "openai-compat",
                "base_url": "http://127.0.0.1:1/v1",
                "dialect": "openai"
            }
        },
        "roles": {
            "default": { "chain": ["unreachable/does-not-matter"] }
        },
        "plugins": { "mcp": [mcp_entry] }
    });
    let config_path = dir.path().join("conway.json");
    std::fs::write(
        &config_path,
        serde_json::to_vec(&config).expect("serialize conway.json"),
    )
    .expect("write conway.json");
    common::Fixture { dir, config_path }
}

/// **Test 1 of the pair.** `routes explain` -- pure introspection, never
/// starts an agent, never calls a tool -- must succeed even though its one
/// configured MCP entry cannot even complete the handshake, and the failure
/// must be named on stderr (GP-14: a degraded run must say which plugin
/// failed).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn routes_explain_succeeds_and_names_the_crashing_mcp_entry() {
    let fixture = fixture_for_introspection().await;

    let out = common::run_conway(&["routes", "explain", "default", "--json"], &fixture);
    assert!(
        out.status.success(),
        "routes explain must succeed despite the crashing MCP entry; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Structural assertion on the rendered `--json` payload, per this
    // wave's own "no contains() where a structural check is available"
    // instruction: `routes explain` still prints a real, parseable report
    // rather than a truncated or empty one.
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("routes explain --json must print valid JSON");
    assert!(
        json.get("chain").is_some(),
        "the report must still carry its own `chain` field: {json}"
    );

    // The crashing entry's own id has no structured surface at the CLI
    // layer (`Conway::warnings()` is a plain-text diagnostic, not part of
    // `routes explain`'s own JSON report) -- `contains` is the only
    // available check for this half of the assertion.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("conway: warning:"),
        "expected the standard diag::warn prefix on stderr, got: {stderr:?}"
    );
    assert!(
        stderr.contains("acme-crashing-mcp"),
        "the failing entry's own id must be named: {stderr:?}"
    );
}

/// **Test 2 of the pair -- the guard rail.** The SAME crashing MCP entry,
/// the SAME fixture shape, but a turn-taking `-p` invocation instead of
/// `routes explain`: this must still fail the whole build, exactly as it
/// did before board item `01M2PJCT90G2010KGCJ4YSFREM`, proving the fix did
/// not over-apply past the six introspection-only commands it names.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_taking_invocation_still_fails_loudly_on_the_same_crashing_mcp_entry() {
    // A live, reachable mock backend this time -- `-p` needs
    // `fleet_usability.should_offer_guided_setup()` to read `false` (a
    // real `Override` capability entry, exactly like `common::write_fixture`
    // already stamps for every other turn-taking test in this crate) so
    // this run reaches `mcp_plugins::install`'s OWN failure rather than
    // stopping earlier at the unrelated guided-setup gate -- the scripted
    // turn itself is never reached either way, since the crashing MCP entry
    // must fail the build before any request goes out.
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = common::write_fixture(&mock, 5);
    let raw = std::fs::read_to_string(&fixture.config_path).expect("read fixture config");
    let mut doc: serde_json::Value = serde_json::from_str(&raw).expect("parse fixture config");
    let mcp_entry = crashing_mcp_entry(fixture.dir.path()).await;
    doc["plugins"] = serde_json::json!({ "mcp": [mcp_entry] });
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&doc).expect("serialize fixture config"),
    )
    .expect("rewrite fixture config with the crashing MCP entry");

    let out = common::run_conway(&["-p", "hello"], &fixture);
    assert!(
        !out.status.success(),
        "a turn-taking invocation must still fail over a crashing MCP entry; stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    // `FacadeError::Build` -> `ExitCode::Usage` (2) -- the same exit code a
    // config-shaped startup refusal has always produced, never a raw Rust
    // panic/abort code.
    assert_eq!(
        out.status.code(),
        Some(2),
        "expected the ordinary Usage exit code for a build-time refusal, got: {out:?}"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("conway: error:"),
        "expected the standard diag::error prefix on stderr, got: {stderr:?}"
    );
    assert!(
        stderr.contains("acme-crashing-mcp"),
        "the failing entry's own id must still be named: {stderr:?}"
    );
}
