//! Integration tests for the read-only `sessions`/`routes`
//! introspection subcommands, run against the real compiled `conway`
//! binary. Reuses the harness (`tests/common/mod.rs`) unchanged --
//! no new harness code is added here, per this item's own binding notes.

// This test binary only exercises a subset of the shared harness's
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
    common::session_dir(fixture)
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

/// Same shape as [`write_forked_child`], but `origin.mode` is `"spawn"`
/// rather than `"fork"` -- the fixture for "fork and spawn are
/// distinct primitives that must never be blurred into one label"
/// regression: a session `conway_spawn` created must never
/// render as a fork in `sessions list` (neither the text `ORIGIN` cell nor
/// the JSON `origin.mode` field).
fn write_spawned_child(fixture: &Fixture, parent: &str, at_seq: u64) -> String {
    let child = conway::SessionId::new().to_string();
    let agent = conway::AgentId::new().to_string();
    let line = format!(
        "{{\"kind\":\"header\",\"session\":\"{child}\",\"agent\":\"{agent}\",\
         \"created\":\"2026-07-20T00:00:00Z\",\"origin\":{{\"parent\":\"{parent}\",\
         \"at_seq\":{at_seq},\"mode\":\"spawn\"}},\"agent_def\":null,\"role\":null,\
         \"cwd\":\"/tmp\",\"status\":\"active\"}}\n"
    );
    std::fs::write(sessions_dir(fixture).join(format!("{child}.jsonl")), line)
        .expect("write spawned child session file");
    child
}

/// Same shape as [`write_forked_child`], plus an `"ephemeral":true` header
/// field -- for exercising `sessions tree <id>`'s direct-id resolution over
/// an ephemeral session (the same shape `/ask`'s fork-ask primitive
/// produces via `crate::fork_child`, `conway`'s own crate under test here).
fn write_ephemeral_child(fixture: &Fixture, parent: &str, at_seq: u64) -> String {
    let child = conway::SessionId::new().to_string();
    let agent = conway::AgentId::new().to_string();
    let line = format!(
        "{{\"kind\":\"header\",\"session\":\"{child}\",\"agent\":\"{agent}\",\
         \"created\":\"2026-07-20T00:00:00Z\",\"origin\":{{\"parent\":\"{parent}\",\
         \"at_seq\":{at_seq},\"mode\":\"fork\"}},\"agent_def\":null,\"role\":null,\
         \"cwd\":\"/tmp\",\"status\":\"active\",\"ephemeral\":true}}\n"
    );
    std::fs::write(sessions_dir(fixture).join(format!("{child}.jsonl")), line)
        .expect("write ephemeral child session file");
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

/// A fixture that never dials a backend -- routing decisions and an empty
/// session store don't need one, so these tests skip `MockBackend`
/// entirely (matching `cli_surface.rs`'s `MINIMAL_CONFIG` pattern for the
/// same reason).
fn static_fixture() -> Fixture {
    write_fixture_with("http://127.0.0.1:1/v1", "test-model", 10)
}

/// The shared fixture template declares no `permissions` section, so it
/// defaults to `mode = "prompt"`. These tests deliberately leave it that
/// way: `sessions`/`routes` used to fail outright under that mode, because
/// dispatch supplied no gate and `ConwayBuilder::build` fell through to
/// `gates::from_config`, which needs an interactive handler no subcommand
/// can provide. Read-only subcommands now carry a deny-all gate, so running
/// them against a prompt-mode config is the regression guard for that fix.
fn fixture_with_mock(mock: &common::mock_backend::MockHandle) -> Fixture {
    write_fixture(mock, 10)
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
    // `NAME` (added alongside `sessions name`/`unname`) sits right after
    // `ID` -- see `commands/sessions.rs::list`.
    assert_eq!(out.stdout, b"ID  NAME  CREATED  ROLE  ORIGIN\n".to_vec());
    assert_no_esc_byte(&out.stdout);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_list_json_has_id_created_no_status() {
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
    // The `status` field is gone (dead machinery removed): a session has
    // no terminal status, and this key must never come back.
    assert!(!obj.contains_key("status"));
}

/// Regression (text output): a spawned child's `ORIGIN` cell must
/// read `spawn@...`, never `fork@...` -- `origin_cell` used to hardcode the
/// word `fork` regardless of the persisted `SessionMeta.origin.mode`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_list_text_spawned_child_shows_spawn_not_fork() {
    let mock =
        MockBackend::start(Script(vec![vec![Chunk::Text("hi"), Chunk::Finish("stop")]])).await;
    let fixture = fixture_with_mock(&mock);
    let created = run_conway(&["-p", "hi"], &fixture);
    assert!(created.status.success());
    let parent = only_session_id(&fixture);
    let _child = write_spawned_child(&fixture, &parent, 1);

    let out = run_conway(&["sessions", "list"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).expect("utf8 stdout");
    // Matched by content, not by the child's short id: `fmt::id_short`
    // truncates to 8 chars, and ULIDs created moments apart in this same
    // test share a leading timestamp prefix (same rationale as
    // `sessions_tree_resolves_ephemeral_target_by_direct_id`'s own line-count
    // check), so a `starts_with(child_short)` row lookup is false-negative
    // prone. The parent row has no origin (its `ORIGIN` cell is empty), so
    // "some row says spawn@, no row ever says fork@" unambiguously targets
    // the spawned child's own cell.
    assert!(
        text.contains("spawn@"),
        "spawned child's row must say spawn: {text:?}"
    );
    assert!(
        !text.contains("fork@"),
        "spawned child's row must never say fork (): {text:?}"
    );
}

/// Regression (JSON output), the `origin_json`-side counterpart of
/// [`sessions_list_text_spawned_child_shows_spawn_not_fork`] -- text and
/// JSON must agree.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_list_json_spawned_child_origin_mode_is_spawn() {
    let mock =
        MockBackend::start(Script(vec![vec![Chunk::Text("hi"), Chunk::Finish("stop")]])).await;
    let fixture = fixture_with_mock(&mock);
    let created = run_conway(&["-p", "hi"], &fixture);
    assert!(created.status.success());
    let parent = only_session_id(&fixture);
    let child = write_spawned_child(&fixture, &parent, 1);

    let out = run_conway(&["sessions", "list", "--json"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: Value = serde_json::from_slice(&out.stdout).expect("stdout is one JSON array");
    let arr = value.as_array().expect("top-level array");
    let child_obj = arr
        .iter()
        .find(|v| v["id"].as_str() == Some(child.as_str()))
        .unwrap_or_else(|| panic!("no element for spawned child {child} in {arr:?}"));
    assert_eq!(
        child_obj["origin"]["mode"].as_str(),
        Some("spawn"),
        "spawned child's origin.mode must be \"spawn\", never \"fork\" (): {child_obj:?}"
    );
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

/// MINOR 6: `tree()`'s per-node label lost its `status=` segment in the same
/// change that dropped `sessions list`'s `STATUS` column (dead machinery
/// removed -- see `sessions_list_json_has_id_created_no_status` above) but,
/// unlike that one, got no regression test of its own. Sibling of that test
/// for `sessions tree`'s text output: a session has no terminal status, and
/// this label segment must never come back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_tree_label_has_no_status_segment() {
    let mock =
        MockBackend::start(Script(vec![vec![Chunk::Text("hi"), Chunk::Finish("stop")]])).await;
    let fixture = fixture_with_mock(&mock);
    let created = run_conway(&["-p", "hi"], &fixture);
    assert!(created.status.success());
    let parent = only_session_id(&fixture);
    let _child = write_forked_child(&fixture, &parent, 1);

    let out = run_conway(&["sessions", "tree", &parent], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).expect("utf8 stdout");
    assert!(
        !text.contains("status="),
        "tree label must never carry a status= segment: {text:?}"
    );
}

/// Regression test: `tree()` used to resolve its explicitly-named target
/// via the default-filtered `Conway::sessions` catalog (which excludes
/// ephemeral sessions), so `sessions tree <ephemeral-id>` reported "unknown
/// session" for a session that genuinely exists -- inconsistent with
/// `show`/`export`, which resolve via `Conway::resume` (a direct id lookup)
/// and so already worked on an ephemeral id. Also confirms the paired
/// distinction: an ephemeral session stays hidden as a *child* within a
/// parent's rendered tree, even though it resolves fine as a direct target.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_tree_resolves_ephemeral_target_by_direct_id() {
    let mock =
        MockBackend::start(Script(vec![vec![Chunk::Text("hi"), Chunk::Finish("stop")]])).await;
    let fixture = fixture_with_mock(&mock);
    let created = run_conway(&["-p", "hi"], &fixture);
    assert!(created.status.success());
    let parent = only_session_id(&fixture);

    let ephemeral_child = write_ephemeral_child(&fixture, &parent, 1);

    let out = run_conway(&["sessions", "tree", &ephemeral_child], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).expect("utf8 stdout");
    assert!(
        !text.to_lowercase().contains("unknown session"),
        "direct ephemeral-id lookup must resolve: {text:?}"
    );
    assert!(
        text.contains(&ephemeral_child[..8]),
        "root line must show the resolved ephemeral session: {text:?}"
    );

    // The paired distinction: the ephemeral session must still be hidden
    // as a *child* within the parent's own rendered tree -- asserted via
    // line count (not an id-prefix substring check: `fmt::id_short`
    // truncates to 8 chars, and ULIDs created moments apart in this same
    // test share a leading timestamp prefix, so a substring check on the
    // short id would be a false-negative-prone test).
    let parent_out = run_conway(&["sessions", "tree", &parent], &fixture);
    assert!(parent_out.status.success());
    let parent_text = String::from_utf8(parent_out.stdout).expect("utf8 stdout");
    assert_eq!(
        parent_text.lines().count(),
        1,
        "parent's tree must show only its own root line, no ephemeral child: {parent_text:?}"
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
    // Board item 01M0ASX466G3PW3SJJS3KGNS55: this fixture names no
    // `[plugins].install` entry, so `ConwayBuilder::build` falls back to
    // `conway_core::routing::MinimalRouter` (see `docs/routing.md`'s
    // "Asking why a route was chosen") -- it holds no `Arc<dyn Backend>` at
    // all, so it honestly reports "unknown" rather than guessing.
    // `routes_explain_with_routing_plugin_shows_declared_token_fidelity`
    // below proves the non-degenerate answer through the same real binary.
    assert!(
        text.contains("tokens: unknown"),
        "stdout must surface token fidelity per candidate: {text:?}"
    );
}

#[test]
fn routes_explain_with_routing_plugin_shows_declared_token_fidelity() {
    let fixture = static_fixture();
    let mut value: Value =
        serde_json::from_str(&std::fs::read_to_string(&fixture.config_path).unwrap()).unwrap();
    value["plugins"] = serde_json::json!({ "install": ["conway.routing"] });
    std::fs::write(&fixture.config_path, serde_json::to_vec(&value).unwrap()).unwrap();

    let out = run_conway(&["routes", "explain", "default"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    // Board item 01M0ASX466G3PW3SJJS3KGNS55: with `conway.routing` (the
    // `DeclarativeRouter` factory) installed exactly as a real operator
    // would in `settings.json`, the router actually reaches the
    // constructed `mock` (`openai-compat`) backend's own
    // `Backend::token_fidelity()`, which declares `Heuristic` honestly --
    // an operator asking "how much should I trust this backend's token
    // estimate?" gets a real, named answer without reading source.
    assert!(
        text.contains("tokens: heuristic"),
        "stdout must surface the routing plugin's real token fidelity: {text:?}"
    );

    let out = run_conway(&["routes", "explain", "default", "--json"], &fixture);
    assert!(out.status.success());
    let value: Value = serde_json::from_slice(&out.stdout).expect("stdout is one JSON object");
    let chain = value["chain"].as_array().expect("chain is an array");
    assert_eq!(chain[0]["token_fidelity"], "heuristic");
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
    // Board item 01M0ASX466G3PW3SJJS3KGNS55: every chain entry carries the
    // same operator-visible answer the text renderer above does -- "unknown"
    // here for the same `MinimalRouter` reason documented on the text test
    // above; `routes_explain_with_routing_plugin_shows_declared_token_fidelity`
    // proves the non-degenerate "heuristic" answer through the real binary.
    assert_eq!(chain[0]["token_fidelity"], "unknown");
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
