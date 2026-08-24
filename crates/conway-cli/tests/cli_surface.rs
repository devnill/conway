//! Integration tests for the command surface and exit-code contract:
//! everything here runs the actual compiled `conway` binary.

use std::io::Write;

use assert_cmd::Command;
use predicates::prelude::*;

/// A minimal, valid `conway.json`: one `deny`-mode permissions block (so
/// `build()` never hits the undocumented "mode = prompt requires a handler"
/// gap -- this layer wires no gate override, that's a later one's job), one
/// `openai-compat` backend (never actually dialed -- these tests only need
/// `build()` to succeed, not a live connection), and one role so
/// `default_role` resolves.
///
/// `default_document()` bakes in a `roles.coder = { chain = [] }` at the
/// lowest merge layer, and routing validation rejects an empty chain on ANY
/// role (not just `default_role`). `coder` therefore gets a valid chain
/// here too, or `build()` fails with EmptyChain before dispatch ever
/// reaches the stub.
const MINIMAL_CONFIG: &str = r#"
{
  "default_role": "default",
  "permissions": { "mode": "deny" },
  "backends": {
    "local": { "kind": "openai-compat", "base_url": "http://127.0.0.1:1", "dialect": "ollama" }
  },
  "roles": {
    "default": { "chain": ["local/test-model"] },
    "coder": { "chain": ["local/test-model"] }
  }
}
"#;

fn bin() -> Command {
    Command::cargo_bin("conway").expect("conway binary built")
}

/// Writes `MINIMAL_CONFIG` into a fresh temp dir and returns (the dir, the
/// config path) -- the dir must stay alive for the whole test (it also
/// backs `session.root`/`agents.dir`'s relative defaults).
fn minimal_config_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("conway.json");
    let mut f = std::fs::File::create(&path).expect("create conway.json");
    f.write_all(MINIMAL_CONFIG.as_bytes())
        .expect("write conway.json");
    (dir, path)
}

#[test]
fn no_forbidden_deps() {
    let manifest_path = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
    let text = std::fs::read_to_string(manifest_path).expect("read Cargo.toml");
    let value: toml::Value = text.parse().expect("parse Cargo.toml");

    let bin_name = value["bin"][0]["name"].as_str().expect("[[bin]] name");
    assert_eq!(bin_name, "conway");

    let deps = value["dependencies"]
        .as_table()
        .expect("[dependencies] table");
    assert!(
        deps.contains_key("conway"),
        "expected a dependency on the `conway` facade crate"
    );

    // "conway-routing" stays in this
    // list even though that crate no longer exists (renamed and relocated
    // to `conway-plugin-routing`) -- the string simply never matches any
    // key in this crate's `[dependencies]` table again, which is exactly
    // the state a deleted internal-engine crate should leave behind here.
    // did the identical relocation
    // for "conway-backends" (-> `conway-plugin-backends`), for the same
    // reason and with the same outcome here: it stays too, a second dead
    // string this list will never match again either.
    // `conway-plugin-routing`/`conway-plugin-backends` are deliberately NOT
    // added alongside their retired names: this list guards against
    // conway-cli reaching an internal IMPLEMENTATION crate `conway` itself
    // used to assemble (conway-runtime, -backends, -session, -core, -tools
    // -- see each one's own doc); a first-party PLUGIN crate is a different
    // tier entirely, explicitly meant
    // to be linked by exactly one binary through `src/
    // first_party_plugins.rs`'s `router_bundle`/`backend_bundle` --
    // `conway-plugin-skeleton`, immediately below in this crate's own
    // `[dependencies]`, already establishes that this list has never
    // covered the plugin tier. Verified, not asserted, for BOTH: adding
    // `"conway-plugin-routing"` here fails `no_forbidden_deps` outright
    // (conway-cli's Cargo.toml genuinely, necessarily depends on it for
    // `router_bundle` to construct a real `RoutingRouterFactory`) -- tried
    // during that item's implementation and reverted. This item repeated
    // the identical experiment for `"conway-plugin-backends"` (conway-cli
    // genuinely depends on it too, for `backend_bundle` to construct the
    // two real `BackendFactory`s) with the identical result -- both
    // recorded in their own completion reports rather than left as a
    // silent contradiction between "add the new crate to FORBIDDEN" and
    // "and the test passes".
    const FORBIDDEN: &[&str] = &[
        "conway-runtime",
        "conway-backends",
        "conway-session",
        "conway-routing",
        "conway-core",
        "conway-tools",
    ];
    for forbidden in FORBIDDEN {
        assert!(
            !deps.contains_key(*forbidden),
            "[dependencies] must not contain '{forbidden}' -- conway-cli may only depend on the \
             `conway` facade"
        );
    }
}

#[test]
fn help_lists_subcommands() {
    bin()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("sessions"))
        .stdout(predicate::str::contains("routes"));
}

#[test]
fn sessions_help_lists_actions() {
    bin()
        .args(["sessions", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("show"))
        .stdout(predicate::str::contains("tree"))
        .stdout(predicate::str::contains("export"));
}

#[test]
fn routes_help_lists_explain() {
    bin()
        .args(["routes", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("explain"));
}

#[test]
fn unknown_flag_exits_usage_with_empty_stdout() {
    bin()
        .arg("--nonexistent-flag")
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty());
}

#[test]
fn sessions_show_missing_id_exits_usage() {
    bin()
        .args(["sessions", "show"])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::is_empty().not());
}

// **An earlier reconciliation (disclosed):** this test originally asserted the
// stub's contract (empty stdout, a "not implemented" stderr note).
// replaces that stub with the real `sessions list` formatter, whose
// own binding criterion is "prints only the header when there are no
// sessions (never an error)" -- the opposite of "writes nothing to
// stdout". Updated in place rather than left asserting since-removed
// behavior; `tests/subcommands.rs::sessions_list_empty_store_prints_header_only`
// covers the identical contract against a different fixture (no live
// backend at all, vs. this file's `MINIMAL_CONFIG`), so this rename keeps
// both perspectives without duplicating either fixture style.
#[test]
fn sessions_list_on_empty_store_prints_header_only() {
    let (dir, config_path) = minimal_config_dir();
    bin()
        .current_dir(dir.path())
        // Isolate user-scoped config discovery from a real ~/.conway (see
        // `common::command`).
        .env("CONWAY_CONFIG_DIR", dir.path())
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "sessions",
            "list",
        ])
        .assert()
        .success()
        // `NAME` (added alongside `sessions name`/`unname`) sits right
        // after `ID` -- see `commands/sessions.rs::list`.
        .stdout("ID  NAME  CREATED  ROLE  ORIGIN\n")
        .stderr(predicate::str::is_empty());
}
