//! WI-116: integration tests for the read-only `sessions`/`routes`
//! introspection subcommands, run against the real compiled `conway`
//! binary. Reuses the WI-113 harness (`tests/common/mod.rs`) unchanged --
//! no new harness code is added here, per this item's own binding notes.

// This test binary only exercises a subset of the shared WI-113 harness's
// `Chunk` variants and `MockHandle` accessors (`ToolCall`/`Delay`/`Hang`,
// `requests()` are exercised by `tests/oneshot.rs`'s own suite, not this
// one) -- each integration-test file compiles as its own independent
// crate, so `dead_code` would otherwise fire here for surface this crate
// itself never calls. Scoped to this one `mod` item, so it has no effect
// on `oneshot.rs`'s separate compilation of the same shared source.
#[allow(dead_code)]
mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{command, run_conway, write_fixture, write_fixture_with, Fixture};
use serde_json::Value;

const NO_ESC: u8 = 0x1b;

fn assert_no_esc_byte(bytes: &[u8]) {
    assert!(
        !bytes.contains(&NO_ESC),
        "output must never contain a raw ESC byte (0x1b)"
    );
}

fn sessions_dir(fixture: &Fixture) -> std::path::PathBuf {
    fixture.dir.path().join(".conway/sessions")
}

/// Scans `fixture`'s session store for the single session file it expects
/// to find (every test that calls this creates exactly one session via
/// one-shot mode before calling it), returning its id as a string.
fn only_session_id(fixture: &Fixture) -> String {
    let dir = sessions_dir(fixture);
    let mut found: Option<String> = None;
    for entry in std::fs::read_dir(&dir).expect("read sessions dir") {
        let entry = entry.expect("dir entry");
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(stem) = name.strip_suffix(".jsonl") else {
            continue;
        };
        if stem == "index" {
            continue;
        }
        assert!(
            found.is_none(),
            "expected exactly one session file in {}, also found {stem}",
            dir.display()
        );
        found = Some(stem.to_string());
    }
    found.unwrap_or_else(|| panic!("no session file found in {}", dir.display()))
}

/// Hand-writes a forked child's header line directly into `fixture`'s
/// session store -- the "fixture store" half of this item's binding
/// criterion for `sessions tree` ("through the CLI's own one-shot mode plus
/// a fixture store"). `JsonlSessionStore`'s index is a rebuildable cache
/// (`conway-session/src/index.rs`): a session file present on disk but
/// absent from `index.jsonl` makes the index's own consistency check fail,
/// which triggers a full rebuild-by-scan on the next store open -- so the
/// next `conway` invocation against this fixture picks the child up without
/// this test ever touching `index.jsonl` itself. Returns the child's id.
fn write_forked_child(fixture: &Fixture, parent: &str, at_seq: u64) -> String {
    let child = conway::SessionId::new().to_string();
    let agent = conway::AgentId::new().to_string();
    let line = format!(
        "{{\"kind\":\"header\",\"session\":\"{child}\",\"agent\":\"{agent}\",\
         \"created\":\"2026-07-20T00:00:00Z\",\"origin\":{{\"parent\":\"{parent}\",\
         \"at_seq\":{at_seq},\"mode\":\"fork\"}},\"agent_def\":null,\"role\":null,\
         \"cwd\":\"/tmp\",\"status\":\"active\"}}\n"
    );
    std::fs::write(sessions_dir(fixture).join(format!("{child}.jsonl")), line)
        .expect("write forked child session file");
    child
}

fn jsonl_lines(stdout: &[u8]) -> Vec<Value> {
    String::from_utf8(stdout.to_vec())
        .expect("stdout is valid utf8")
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line `{line}` did not parse as JSON: {e}"))
        })
        .collect()
}

/// The shared WI-113 template (`fixtures/conway.toml.tmpl`) declares no
/// `[permissions]` section, so it defaults to `PermissionsConfig::default()`
/// (`mode = "prompt"`). One-shot mode's dispatch always supplies its own
/// gate override (`oneshot::build_gate`), so that default is invisible to
/// `tests/oneshot.rs` -- but `sessions`/`routes` dispatch supplies no
/// override (WI-116's own binding notes: these subcommands never touch a
/// tool or a permission decision), so `ConwayBuilder::build` falls through
/// to `gates::from_config(&config.permissions, None)`, which errors for
/// `mode = "prompt"` with no handler supplied. Appending a `[permissions]`
/// table to the rendered config (any mode other than `prompt` -- `"deny"`
/// is the one every other fixture in this workspace already uses for
/// exactly this reason, e.g. `cli_surface.rs::MINIMAL_CONFIG`) sidesteps it;
/// harmless here since no subcommand under test ever proposes a tool call.
fn allow_build_without_prompt_handler(fixture: &Fixture) {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&fixture.config_path)
        .expect("open fixture config for append");
    writeln!(f, "\n[permissions]\nmode = \"deny\"\n").expect("append permissions section");
}

/// A fixture that never dials a backend -- routing decisions and an empty
/// session store don't need one, so these tests skip `MockBackend`
/// entirely (matching `cli_surface.rs`'s `MINIMAL_CONFIG` pattern for the
/// same reason).
fn static_fixture() -> Fixture {
    let fixture = write_fixture_with("http://127.0.0.1:1/v1", "test-model", 10);
    allow_build_without_prompt_handler(&fixture);
    fixture
}

/// [`write_fixture`], plus the `[permissions]` override described on
/// [`allow_build_without_prompt_handler`].
fn fixture_with_mock(mock: &common::mock_backend::MockHandle) -> Fixture {
    let fixture = write_fixture(mock, 10);
    allow_build_without_prompt_handler(&fixture);
    fixture
}

// ---------------------------------------------------------------------
// sessions list
// ---------------------------------------------------------------------

#[test]
fn sessions_list_empty_store_prints_header_only() {
    let fixture = static_fixture();
    let out = run_conway(&["sessions", "list"], &fixture);

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"ID  CREATED  ROLE  STATUS  ORIGIN\n".to_vec());
    assert_no_esc_byte(&out.stdout);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_list_json_has_id_created_status() {
    let mock =
        MockBackend::start(Script(vec![vec![Chunk::Text("hi"), Chunk::Finish("stop")]])).await;
    let fixture = fixture_with_mock(&mock);
    let created = run_conway(&["-p", "hi"], &fixture);
    assert!(created.status.success());

    let out = run_conway(&["sessions", "list", "--json"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_no_esc_byte(&out.stdout);

    let value: Value = serde_json::from_slice(&out.stdout).expect("stdout is one JSON array");
    let arr = value.as_array().expect("top-level array");
    assert_eq!(arr.len(), 1);
    let obj = arr[0].as_object().expect("element is an object");
    assert!(obj.contains_key("id"));
    assert!(obj.contains_key("created"));
    assert!(obj.contains_key("status"));
}

// ---------------------------------------------------------------------
// sessions show
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_show_json_each_line_parses() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("hello"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = fixture_with_mock(&mock);
    let created = run_conway(&["-p", "hi"], &fixture);
    assert!(created.status.success());
    let id = only_session_id(&fixture);

    let out = run_conway(&["sessions", "show", &id, "--json"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_no_esc_byte(&out.stdout);

    let lines = jsonl_lines(&out.stdout);
    assert!(!lines.is_empty(), "expected at least one transcript record");
    for value in &lines {
        let obj = value.as_object().expect("each line is a JSON object");
        assert!(obj.contains_key("kind"));
    }
    let kinds: Vec<&str> = lines
        .iter()
        .map(|v| v["kind"].as_str().expect("kind is a string"))
        .collect();
    assert!(kinds.contains(&"user_turn"));
}

#[test]
fn sessions_show_unknown_id_exits_2_with_empty_stdout() {
    let fixture = static_fixture();
    let unknown = conway::SessionId::new().to_string();

    let out = run_conway(&["sessions", "show", &unknown], &fixture);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(!out.stderr.is_empty());
}

// ---------------------------------------------------------------------
// sessions tree
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_tree_shows_two_forked_children_indented_under_parent() {
    let mock =
        MockBackend::start(Script(vec![vec![Chunk::Text("hi"), Chunk::Finish("stop")]])).await;
    let fixture = fixture_with_mock(&mock);
    let created = run_conway(&["-p", "hi"], &fixture);
    assert!(created.status.success());
    let parent = only_session_id(&fixture);

    let child_a = write_forked_child(&fixture, &parent, 1);
    let child_b = write_forked_child(&fixture, &parent, 1);

    let out = run_conway(&["sessions", "tree", &parent], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_no_esc_byte(&out.stdout);

    let text = String::from_utf8(out.stdout).expect("utf8 stdout");
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].starts_with(&parent[..8]),
        "root line: {:?}",
        lines[0]
    );

    let child_a_short = &child_a[..8];
    let child_b_short = &child_b[..8];
    let branch_lines: Vec<&&str> = lines[1..]
        .iter()
        .filter(|l| l.contains(child_a_short) || l.contains(child_b_short))
        .collect();
    assert_eq!(
        branch_lines.len(),
        2,
        "both children must appear: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("├─ ")),
        "expected a non-final branch glyph: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("└─ ")),
        "expected a final branch glyph: {lines:?}"
    );
}

// ---------------------------------------------------------------------
// sessions export
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_export_is_deterministic_jsonl() {
    let mock =
        MockBackend::start(Script(vec![vec![Chunk::Text("hi"), Chunk::Finish("stop")]])).await;
    let fixture = fixture_with_mock(&mock);
    let created = run_conway(&["-p", "hi"], &fixture);
    assert!(created.status.success());
    let id = only_session_id(&fixture);

    let first = run_conway(&["sessions", "export", &id], &fixture);
    let second = run_conway(&["sessions", "export", &id], &fixture);
    assert!(first.status.success());
    assert!(second.status.success());
    assert_no_esc_byte(&first.stdout);
    assert_eq!(
        first.stdout, second.stdout,
        "export must be byte-identical across runs"
    );

    for value in jsonl_lines(&first.stdout) {
        assert!(value.as_object().expect("json object").contains_key("kind"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_export_writes_to_out_file() {
    let mock =
        MockBackend::start(Script(vec![vec![Chunk::Text("hi"), Chunk::Finish("stop")]])).await;
    let fixture = fixture_with_mock(&mock);
    let created = run_conway(&["-p", "hi"], &fixture);
    assert!(created.status.success());
    let id = only_session_id(&fixture);

    let out_path = fixture.dir.path().join("export.jsonl");
    let mut cmd = command(
        &[
            "sessions",
            "export",
            &id,
            "--out",
            out_path.to_str().unwrap(),
        ],
        &fixture,
    );
    let output = cmd.output().expect("run conway binary");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "--out must suppress stdout");

    let contents = std::fs::read(&out_path).expect("read exported file");
    assert_no_esc_byte(&contents);
    for line in String::from_utf8(contents).unwrap().lines() {
        let _: Value = serde_json::from_str(line).expect("each exported line parses");
    }
}

// ---------------------------------------------------------------------
// routes explain
// ---------------------------------------------------------------------

#[test]
fn routes_explain_text_shows_position_reason_and_breaker() {
    let fixture = static_fixture();
    let out = run_conway(&["routes", "explain", "default"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_no_esc_byte(&out.stdout);

    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("role: default"));
    assert!(text.contains("SELECTED"));
    assert!(text.contains("primary for role `default`"));
    assert!(text.contains("breaker: closed"));
}

#[test]
fn routes_explain_json_has_role_chain_skipped_health() {
    let fixture = static_fixture();
    let out = run_conway(&["routes", "explain", "default", "--json"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_no_esc_byte(&out.stdout);

    let value: Value = serde_json::from_slice(&out.stdout).expect("stdout is one JSON object");
    let obj = value.as_object().expect("top-level object");
    assert_eq!(obj["role"], "default");
    let chain = obj["chain"].as_array().expect("chain is an array");
    assert_eq!(chain.len(), 1);
    assert!(obj["skipped"].as_array().is_some());
    let health = obj["health"].as_array().expect("health is an array");
    assert_eq!(health.len(), 1);
}

#[test]
fn routes_explain_unknown_role_exits_2_lists_configured_roles() {
    let fixture = static_fixture();
    let out = run_conway(&["routes", "explain", "no-such-role"], &fixture);

    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("no-such-role"));
    assert!(stderr.contains("default"));
    assert!(stderr.contains("coder"));
}
