//! Compiled-binary tests for `conway tools list`, run against the real
//! compiled `conway` binary (`tests/common/mod.rs`'s harness, reused
//! unchanged, matching `tests/subcommands.rs`'s own binding note for the
//! sibling `sessions`/`routes` subcommands).
//!
//! **Disclosed deviation from this item's own spec.** WHAT TO BUILD point 5
//! asked for a test that flips `tools.builtin_plugins`'s `"conway.shell"`
//! entry and observes `bash` appear/disappear from `conway tools list`'s
//! output. That test cannot be written honestly against this binary as it
//! stands today: `crates/conway-cli/src/main.rs`'s `build_conway` (see its
//! own doc comment, right above the `is_tui` branch) deliberately forces
//! `PluginSelection::All` for EVERY non-interactive CLI dispatch target --
//! `sessions`, `routes`, one-shot `-p`, and (this item's own choice: match
//! the existing four rather than invent a fifth, inconsistent policy)
//! `tools` too -- precisely so a script's own `-p --allowed-tools bash`
//! invocation always sees `bash` registered regardless of
//! `tools.builtin_plugins`. Only the interactive TUI defers to that config
//! key. Making `conway tools list` answer differently from what a script's
//! own `-p` invocation actually registers would make this new subcommand
//! LIE to the exact audience ("self-describing allow-list") it exists to
//! serve -- see `commands::tools`'s own module doc for the full
//! "registration vs. confinement" framing this mirrors.
//!
//! [`tools_builtin_plugins_toggle_has_no_effect_on_bash_registration`]
//! pins that this is real, current, and intentional, not a regression this
//! suite failed to catch. [`plugins_install_toggle_changes_the_registered_
//! tool_set`] is this file's actual P-15 compliance instead: a config
//! value THAT DOES genuinely change every non-interactive target's
//! registered set (`[plugins].install` -- `main.rs`'s own comment on that
//! tier: "every dispatch target ... shares this single `build_conway`
//! choke point, so all five see the same installed set from the same
//! config") is flipped, with an observed, real difference in `conway tools
//! list`'s own output.

// This test binary only exercises a subset of the shared harness's surface
// (no `MockBackend`: `tools list` never dials a backend) -- each
// `tests/*.rs` file compiles `common` fresh as its own independent crate,
// so `dead_code` would otherwise fire here for surface this crate itself
// never calls, matching `tests/subcommands.rs`'s identical note.
#[allow(dead_code)]
mod common;

use common::{run_conway, write_fixture_with, Fixture};

/// Mirrors `tests/subcommands.rs::static_fixture` exactly: `tools list`
/// never dials a backend, so these tests skip `MockBackend` entirely.
fn static_fixture() -> Fixture {
    write_fixture_with("http://127.0.0.1:1/v1", "test-model", 10)
}

fn set_tools_builtin_plugins(fixture: &Fixture, ids: &[&str]) {
    let raw = std::fs::read_to_string(&fixture.config_path).expect("read rendered conway.json");
    let mut value: serde_json::Value = serde_json::from_str(&raw).expect("parse conway.json");
    value["tools"] = serde_json::json!({ "builtin_plugins": ids });
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&value).expect("serialize conway.json"),
    )
    .expect("rewrite conway.json with tools.builtin_plugins");
}

fn set_plugins_install(fixture: &Fixture, ids: &[&str]) {
    let raw = std::fs::read_to_string(&fixture.config_path).expect("read rendered conway.json");
    let mut value: serde_json::Value = serde_json::from_str(&raw).expect("parse conway.json");
    value["plugins"] = serde_json::json!({ "install": ids });
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&value).expect("serialize conway.json"),
    )
    .expect("rewrite conway.json with [plugins].install");
}

fn has_tool_row(text: &str, name: &str) -> bool {
    text.lines().any(|l| l.starts_with(&format!("{name} ")))
}

// ---------------------------------------------------------------------
// Acceptance 1: every registered tool, category, permission, count line.
// ---------------------------------------------------------------------

#[test]
fn tools_list_prints_bash_with_category_permission_and_a_trailing_count_line() {
    let fixture = static_fixture();
    let out = run_conway(&["tools", "list"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).expect("utf8 stdout");

    let bash_line = text
        .lines()
        .find(|l| l.starts_with("bash "))
        .unwrap_or_else(|| panic!("expected a `bash` row: {text:?}"));
    assert!(
        bash_line.contains("execute"),
        "bash's category must be `execute`: {bash_line:?}"
    );
    assert!(
        bash_line.contains("dangerous"),
        "bash's permission class must be `dangerous`: {bash_line:?}"
    );

    let last = text.lines().last().expect("at least one line");
    assert!(
        last.contains("tools registered from") && last.contains("plugins"),
        "trailing count line: {last:?}"
    );
}

#[test]
fn tools_list_json_is_a_valid_array_of_tool_specs() {
    let fixture = static_fixture();
    let out = run_conway(&["tools", "list", "--json"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is one JSON array");
    let arr = value.as_array().expect("top-level array");
    assert!(!arr.is_empty());
    assert!(
        arr.iter().any(|v| v["name"] == "bash"),
        "expected a `bash` entry: {arr:?}"
    );
    let bash = arr.iter().find(|v| v["name"] == "bash").unwrap();
    assert_eq!(bash["category"], "execute");
    assert_eq!(bash["permission"], "dangerous");
}

// ---------------------------------------------------------------------
// Acceptance 2 (per this file's own disclosed deviation, above): a REAL
// config value flip that DOES change the registered set, plus a pin of
// the ONE config value (`tools.builtin_plugins`) that, by design, does
// not.
// ---------------------------------------------------------------------

#[test]
fn plugins_install_toggle_changes_the_registered_tool_set() {
    let fixture = static_fixture();

    let without = run_conway(&["tools", "list"], &fixture);
    assert!(without.status.success());
    let without_text = String::from_utf8(without.stdout).expect("utf8 stdout");
    assert!(
        !has_tool_row(&without_text, "remember"),
        "`remember` must be absent with no [plugins].install: {without_text:?}"
    );

    set_plugins_install(&fixture, &["conway.memory"]);
    let with = run_conway(&["tools", "list"], &fixture);
    assert!(
        with.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&with.stderr)
    );
    let with_text = String::from_utf8(with.stdout).expect("utf8 stdout");
    assert!(
        has_tool_row(&with_text, "remember"),
        "`remember` must appear once `conway.memory` is installed: {with_text:?}"
    );

    // The trailing count line must climb too -- this is real registration,
    // not a filtered VIEW over a fixed set.
    let without_n: u32 = without_text
        .lines()
        .last()
        .and_then(|l| l.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .expect("leading tool count on the trailing line");
    let with_n: u32 = with_text
        .lines()
        .last()
        .and_then(|l| l.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .expect("leading tool count on the trailing line");
    assert!(
        with_n > without_n,
        "installing conway.memory must raise the registered tool count: {without_n} -> {with_n}"
    );
}

#[test]
fn tools_builtin_plugins_toggle_has_no_effect_on_bash_registration() {
    let fixture = static_fixture();

    set_tools_builtin_plugins(&fixture, &[]);
    let excluded = run_conway(&["tools", "list"], &fixture);
    assert!(excluded.status.success());
    let excluded_text = String::from_utf8(excluded.stdout).expect("utf8 stdout");
    assert!(
        has_tool_row(&excluded_text, "bash"),
        "non-interactive CLI dispatch registers every built-in regardless of \
         tools.builtin_plugins (main.rs::build_conway's own doc) -- bash must \
         still be present: {excluded_text:?}"
    );

    set_tools_builtin_plugins(
        &fixture,
        &[
            "conway.shell",
            "conway.fs",
            "conway.subagent",
            "conway.report",
        ],
    );
    let included = run_conway(&["tools", "list"], &fixture);
    assert!(included.status.success());
    let included_text = String::from_utf8(included.stdout).expect("utf8 stdout");
    assert!(has_tool_row(&included_text, "bash"));

    assert_eq!(
        excluded_text, included_text,
        "conway tools list's output must be IDENTICAL regardless of tools.builtin_plugins \
         (a CLI-subcommand-dispatch invariant, not just a bash-specific one)"
    );
}

// ---------------------------------------------------------------------
// Acceptance 3: `--root` prints the same list plus a confinable footnote.
// ---------------------------------------------------------------------

#[test]
fn tools_list_under_root_prints_the_same_list_plus_a_confinable_footnote() {
    let fixture = static_fixture();
    let without_root = run_conway(&["tools", "list"], &fixture);
    assert!(without_root.status.success());
    let without_root_text = String::from_utf8(without_root.stdout).expect("utf8 stdout");

    let root = fixture.dir.path().to_str().expect("utf8 tempdir path");
    let with_root = run_conway(&["--root", root, "tools", "list"], &fixture);
    assert!(
        with_root.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&with_root.stderr)
    );
    let with_root_text = String::from_utf8(with_root.stdout).expect("utf8 stdout");

    let with_root_lines: Vec<&str> = with_root_text.lines().collect();
    let without_root_lines: Vec<&str> = without_root_text.lines().collect();
    assert_eq!(
        with_root_lines.len(),
        without_root_lines.len() + 1,
        "the tool list itself must not shrink under --root, only gain one \
         trailing footnote line: with={with_root_lines:?} without={without_root_lines:?}"
    );
    assert_eq!(
        &with_root_lines[..without_root_lines.len()],
        &without_root_lines[..]
    );

    let footnote = with_root_lines.last().expect("a footnote line");
    assert!(
        footnote.starts_with("--root ") && footnote.contains("confines:"),
        "expected a trailing confinable-tools footnote: {footnote:?}"
    );
    assert!(
        footnote.contains("read") || footnote.contains("write") || footnote.contains("edit"),
        "footnote must name at least one path-confinable tool: {footnote:?}"
    );
}
