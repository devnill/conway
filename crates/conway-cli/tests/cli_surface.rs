//! Integration tests for the WI-111 command surface and exit-code contract:
//! everything here runs the actual compiled `conway` binary.

use std::io::Write;

use assert_cmd::Command;
use predicates::prelude::*;

/// A minimal, valid `conway.toml`: one `deny`-mode permissions block (so
/// `build()` never hits the undocumented "mode = prompt requires a handler"
/// gap -- WI-111 wires no gate override, that's WI-112/114's job), one
/// `openai-compat` backend (never actually dialed -- these tests only need
/// `build()` to succeed, not a live connection), and one role so
/// `default_role` resolves.
const MINIMAL_CONFIG: &str = r#"
default_role = "default"

[permissions]
mode = "deny"

[backends.local]
kind = "openai-compat"
base_url = "http://127.0.0.1:1"
dialect = "ollama"

[roles.default]
chain = ["local/test-model"]

# `default_document()` bakes in a `roles.coder = { chain = [] }` at the
# lowest merge layer, and routing validation rejects an empty chain on ANY
# role (not just `default_role`). Give `coder` a valid chain here too, or
# `build()` fails with EmptyChain before dispatch ever reaches the stub
# (cycle-1 review S1).
[roles.coder]
chain = ["local/test-model"]
"#;

fn bin() -> Command {
    Command::cargo_bin("conway").expect("conway binary built")
}

/// Writes `MINIMAL_CONFIG` into a fresh temp dir and returns (the dir, the
/// config path) -- the dir must stay alive for the whole test (it also
/// backs `session.root`/`agents.dir`'s relative defaults).
fn minimal_config_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("conway.toml");
    let mut f = std::fs::File::create(&path).expect("create conway.toml");
    f.write_all(MINIMAL_CONFIG.as_bytes())
        .expect("write conway.toml");
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

// **WI-116 reconciliation (disclosed):** this test originally asserted the
// WI-111 stub's contract (empty stdout, a "not implemented" stderr note).
// WI-116 replaces that stub with the real `sessions list` formatter, whose
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
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "sessions",
            "list",
        ])
        .assert()
        .success()
        .stdout("ID  CREATED  ROLE  STATUS  ORIGIN\n")
        .stderr(predicate::str::is_empty());
}
