//! CLI-level acceptance test for the first-party plugin tier's install
//! mechanism: `[plugins].install`
//! in `settings.json`, resolved by `conway-cli`'s own
//! `first_party_plugins::install` before `ConwayBuilder::build`, against the
//! REAL compiled `conway` binary and a mock OpenAI-compatible server -- no
//! unit test of the resolution function substitutes for this (a unit
//! test is not a liveness test).
//!
//! One-shot (`-p`) is the mode driven here; the TUI is not (no headless TUI
//! driver exists in this suite). This is still full, not partial, coverage
//! of the "every mode reachable" requirement: `conway-cli`'s
//! `main::build_conway` is the SINGLE choke point both the TUI and one-shot
//! dispatch targets call (see that function's own doc comment: `is_tui`
//! only affects the *built-in* selection, never the first-party-plugin
//! resolution immediately below it), so a real run through either one
//! exercises the identical `first_party_plugins::install` call the other
//! one would make from the same config. `crates/conway-plugin-skeleton`'s
//! own `tests/skeleton_end_to_end.rs` covers the library-embedder path the
//! same capability takes when no binary is involved at all.
//!
//! **The VERIFICATION ANCHOR pair asserts on the announced tool set** --
//! the exact wording the's acceptance criterion uses -- read
//! straight off the real wire request `MockBackend` received
//! (`request["tools"]`, the identical field `interactive_tools.rs`'s own
//! `announced_names` helper reads at the library level). This sidesteps a
//! real wrinkle: a model that actually tries to CALL a tool the request
//! never announced trips `conway-plugin-backends`' own streaming-tool-call
//! validation (`dialect: "openai"` defaults to `Streaming{validated:true}`)
//! as a transport-level bad-request, before the runtime's own
//! "unknown tool" resolution would even see it -- a different, unrelated
//! failure this test does not exist to cover. The separate
//! `skeleton_tool_is_callable_from_one_shot_once_installed` test below
//! drives a real call, always with the plugin installed (so the call is
//! legitimate), to prove invocation rather than mere announcement.
//!
//! **`default_backends_attach_with_no_plugins_install_entry_and_complete_a_
//! one_shot_prompt` is this file's
//! second VERIFICATION ANCHOR:** a fresh install with an ordinary
//! `settings.json` -- no `[plugins]` section at all, the exact fixture
//! every other test in this file already renders -- must still complete a
//! one-shot prompt against a configured backend, with no credentials and
//! no network beyond the loopback mock. Every OTHER test in this file
//! already depends on this property implicitly (each one calls
//! `run_conway` against `write_fixture`'s rendered config and asserts
//! success), which is real, load-bearing regression coverage -- but none of
//! them NAMES the property this item exists to prove, so this test states
//! it explicitly and would be the one to fail first if `conway-plugin-
//! backends`'s two `BackendFactory`s ever stopped attaching by default.

mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{run_conway, write_fixture};

/// [`common::write_fixture`] renders the shared `conway.json.tmpl` (backend,
/// role, `max_steps`) with no `[plugins]` section at all -- proving the
/// absent-by-default half needs nothing added, `PluginsConfig`'s own
/// `#[serde(default)]` already covers it (`config::schema`'s doc). This
/// helper adds exactly one key to that rendered file: `{"plugins":
/// {"install": [...]}}`, leaving everything else the shared fixture already
/// established untouched.
fn add_plugins_install(fixture: &common::Fixture, ids: &[&str]) {
    let raw = std::fs::read_to_string(&fixture.config_path).expect("read rendered conway.json");
    let mut value: serde_json::Value = serde_json::from_str(&raw).expect("parse conway.json");
    value["plugins"] = serde_json::json!({ "install": ids });
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&value).expect("serialize conway.json"),
    )
    .expect("rewrite conway.json with [plugins].install");
}

/// The sorted tool names named in the LAST `/chat/completions` request the
/// mock received -- the wire-level "announced tool set", mirroring
/// `conway/tests/interactive_tools.rs`'s own `announced_names` helper one
/// layer down (a `GenerateRequest.tools` field there; the JSON `tools[].
/// function.name` array an OpenAI-compatible request actually sends over
/// the wire here).
fn announced_names(request: &serde_json::Value) -> Vec<String> {
    let mut names: Vec<String> = request["tools"]
        .as_array()
        .expect("request must carry a tools array")
        .iter()
        .map(|t| {
            t["function"]["name"]
                .as_str()
                .expect("each tool entry names itself")
                .to_string()
        })
        .collect();
    names.sort();
    names
}

fn jsonl_lines(stdout: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8(stdout.to_vec())
        .expect("stdout is utf8")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad jsonl line {l}: {e}")))
        .collect()
}

/// The `tool_call_finished.preview` (first 200 chars of the tool's own
/// reply text -- `conway-runtime`'s `preview_text` contract) for the call
/// naming `tool_name` in `event.tool_call_proposed`, correlated by
/// `call_id`.
fn tool_call_finished_preview(lines: &[serde_json::Value], tool_name: &str) -> Option<String> {
    let call_id = lines.iter().find_map(|line| {
        if line["event"] == "tool_call_proposed" && line["tool"] == tool_name {
            line["call_id"].as_str().map(str::to_string)
        } else {
            None
        }
    })?;
    lines.iter().find_map(|line| {
        if line["event"] == "tool_call_finished" && line["call_id"] == call_id {
            line["preview"].as_str().map(str::to_string)
        } else {
            None
        }
    })
}

/// VERIFICATION ANCHOR, half 1: absent by default. With no
/// `[plugins].install`, the announced tool set the real binary sends over
/// the wire never names `skeleton_ping`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skeleton_tool_is_absent_from_the_announced_set_without_plugins_install() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("hi back"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);
    // Deliberately no `add_plugins_install` call: the rendered config has
    // no `[plugins]` section at all.

    let out = run_conway(&["-p", "hi"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let requests = mock.requests();
    let names = announced_names(
        requests
            .last()
            .expect("mock must have received one request"),
    );
    assert!(
        !names.iter().any(|n| n == "skeleton_ping"),
        "with no [plugins].install, 'skeleton_ping' must not be in the announced tool set, \
         got {names:?}"
    );
}

/// VERIFICATION ANCHOR, half 2: present once installed. The same request
/// shape, differing only by `[plugins].install`, now announces
/// `skeleton_ping` alongside every built-in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skeleton_tool_is_present_in_the_announced_set_once_installed() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("hi back"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.plugin_skeleton"]);

    let out = run_conway(&["-p", "hi"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let requests = mock.requests();
    let names = announced_names(
        requests
            .last()
            .expect("mock must have received one request"),
    );
    assert!(
        names.iter().any(|n| n == "skeleton_ping"),
        "with [plugins].install = [\"conway.plugin_skeleton\"], 'skeleton_ping' must be in the \
         announced tool set, got {names:?}"
    );
}

/// The default-install gate for `conway.path` (board item
/// `01M0PEFMG96SVBBD5D2E06H34A`): with no `[plugins].install` entry, the
/// real compiled binary never announces `compose_context_path` -- proving
/// this plugin behaves like every other first-party plugin (opt-in, not
/// silently on), the SAME shape the skeleton pair above proves for
/// `skeleton_ping`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compose_context_path_tool_is_absent_from_the_announced_set_without_plugins_install() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("hi back"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);
    // Deliberately no `add_plugins_install` call.

    let out = run_conway(&["-p", "hi"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let requests = mock.requests();
    let names = announced_names(
        requests
            .last()
            .expect("mock must have received one request"),
    );
    assert!(
        !names.iter().any(|n| n == "compose_context_path"),
        "with no [plugins].install, 'compose_context_path' must not be in the announced tool \
         set, got {names:?}"
    );
}

/// The other half of the gate: naming `"conway.path"` in `[plugins].install`
/// against the REAL compiled binary makes `compose_context_path` reachable
/// -- this is what proves `write_head`/`ValidatedPath::derive_with` are no
/// longer unreachable in a running build, the exact defect this item exists
/// to close. History: at the time this test was written,
/// `conway-plugin-trim` (`"conway.trim"`) had no equivalent passing test
/// anywhere in this tree, which is precisely how its own unreachability
/// went unnoticed -- board item `01M0TV447NAJ1R06S455DZPP54` closed that
/// gap (`conway_trim_curator_omits_old_tool_round_trips_once_installed`,
/// below), but the cautionary shape stays true in general: this test is
/// still the regression guard that keeps `conway.path` from quietly
/// regressing to that same unreachable state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compose_context_path_tool_is_present_in_the_announced_set_once_installed() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("hi back"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.path"]);

    let out = run_conway(&["-p", "hi"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let requests = mock.requests();
    let names = announced_names(
        requests
            .last()
            .expect("mock must have received one request"),
    );
    assert!(
        names.iter().any(|n| n == "compose_context_path"),
        "with [plugins].install = [\"conway.path\"], 'compose_context_path' must be in the \
         announced tool set, got {names:?}"
    );
}

/// The default-install gate for `conway.discover` (board item
/// `01M0PS8J3AK7Z7253Z3E3RD3GY`): with no `[plugins].install` entry, the
/// real compiled binary never announces `search_sessions` -- the same
/// shape the `conway.path` pair immediately above proves for
/// `compose_context_path`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_sessions_tool_is_absent_from_the_announced_set_without_plugins_install() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("hi back"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);
    // Deliberately no `add_plugins_install` call.

    let out = run_conway(&["-p", "hi"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let requests = mock.requests();
    let names = announced_names(
        requests
            .last()
            .expect("mock must have received one request"),
    );
    assert!(
        !names.iter().any(|n| n == "search_sessions"),
        "with no [plugins].install, 'search_sessions' must not be in the announced tool \
         set, got {names:?}"
    );
}

/// The other half of the gate: naming `"conway.discover"` in
/// `[plugins].install` against the REAL compiled binary makes
/// `search_sessions` reachable -- one line in `settings.json`, the same
/// day this item lands, proving `SessionDiscoveryHost` is not another
/// `conway-plugin-trim` in its ORIGINAL sense: a workspace member
/// `conway-cli` never depended on, so no `[plugins].install` entry could
/// ever have reached it (board item `01M0TV447NAJ1R06S455DZPP54` closed
/// that specific gap for `conway-plugin-trim` itself by adding the
/// dependency and its own reachability test, below).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_sessions_tool_is_present_in_the_announced_set_once_installed() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("hi back"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.discover"]);

    let out = run_conway(&["-p", "hi"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let requests = mock.requests();
    let names = announced_names(
        requests
            .last()
            .expect("mock must have received one request"),
    );
    assert!(
        names.iter().any(|n| n == "search_sessions"),
        "with [plugins].install = [\"conway.discover\"], 'search_sessions' must be in the \
         announced tool set, got {names:?}"
    );
}

/// Beyond mere announcement: once installed, the tool actually dispatches
/// through the real compiled binary to this plugin's own `Tool::invoke`,
/// and its exact reply text is observable in the finished event's preview.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skeleton_tool_is_callable_from_one_shot_once_installed() {
    let mock = MockBackend::start(Script(vec![
        vec![
            Chunk::ToolCall {
                name: "skeleton_ping",
                args: serde_json::json!({ "message": "hi" }),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("done"), Chunk::Finish("stop")],
    ]))
    .await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.plugin_skeleton"]);

    let out = run_conway(
        &[
            "-p",
            "ping it",
            "--allowed-tools",
            "skeleton_ping",
            "--output-format",
            "jsonl",
        ],
        &fixture,
    );

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines = jsonl_lines(&out.stdout);
    assert_eq!(
        tool_call_finished_preview(&lines, "skeleton_ping").as_deref(),
        Some("skeleton pong: hi"),
        "the real plugin's own Tool::invoke must have produced this exact reply, dispatched \
         through the real compiled binary -- got jsonl: {lines:?}"
    );
}

/// Scripts `rounds` successive tool-call turns (`bash`, each echoing a
/// round-numbered marker so a later request's `messages` array can be
/// grepped for exactly which round survived), followed by one final
/// plain-text turn that ends the run -- the shape both `conway.trim`
/// reachability tests below share, differing only in `[plugins].install`.
fn trim_probe_script(rounds: u32) -> Script {
    let mut turns: Vec<Vec<Chunk>> = (0..rounds)
        .map(|i| {
            vec![
                Chunk::ToolCall {
                    name: "bash",
                    args: serde_json::json!({ "command": format!("echo round-marker-{i}") }),
                },
                Chunk::Finish("tool_calls"),
            ]
        })
        .collect();
    turns.push(vec![Chunk::Text("done"), Chunk::Finish("stop")]);
    Script(turns)
}

/// `conway_plugin_trim::DEFAULT_KEEP_TURNS` (8) is the window `bundle`
/// installs `conway.trim` with -- 10 tool-call rounds before the final,
/// curator-observing request puts `CurateCtx::turn` at 10 for that
/// request, well past the 8-turn window, so round 0's call/result
/// round-trip is the oldest thing a curator that is actually wired in
/// would omit.
const TRIM_PROBE_ROUNDS: u32 = 10;

/// Control half: with NO `[plugins].install` entry (so `conway.trim` is not
/// attached, exactly like every other opt-in first-party plugin), round 0's
/// tool round-trip is still present in the LAST request's `messages` array
/// -- nothing in this binary curates it away. This is the baseline the
/// paired test below contrasts against: if this one ever started failing,
/// the marker text or round count would need adjusting, not the claim
/// `conway.trim` exists to prove.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_tool_round_trips_persist_across_many_turns_without_plugins_install() {
    let mock = MockBackend::start(trim_probe_script(TRIM_PROBE_ROUNDS)).await;
    let fixture = write_fixture(&mock, 20);
    // Deliberately no `add_plugins_install` call.

    let out = run_conway(&["-p", "hi", "--allowed-tools", "bash"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let requests = mock.requests();
    assert_eq!(
        requests.len(),
        (TRIM_PROBE_ROUNDS + 1) as usize,
        "one request per scripted round plus the final text-ending turn"
    );
    let last = requests.last().expect("mock must have received a request");
    let last_wire = serde_json::to_string(last).expect("serialize the last request");
    assert!(
        last_wire.contains("round-marker-0"),
        "with no [plugins].install, the oldest tool round-trip must still reach the wire on \
         the final request -- got: {last_wire}"
    );
}

/// THE reachability test this board item exists to add: naming
/// `"conway.trim"` in `[plugins].install` against the REAL compiled binary
/// makes its `Curator` actually run and actually omit the same old
/// round-trip the control test above shows survives when it is not
/// installed -- direct evidence that `conway-cli` links `conway-plugin-trim`
/// and resolves `"conway.trim"` through this same `[plugins].install`
/// mechanism as every other first-party id, closing the gap this file's own
/// former note described ("`conway-plugin-trim` ... has no equivalent
/// passing test anywhere in this tree").
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conway_trim_curator_omits_old_tool_round_trips_once_installed() {
    let mock = MockBackend::start(trim_probe_script(TRIM_PROBE_ROUNDS)).await;
    let fixture = write_fixture(&mock, 20);
    add_plugins_install(&fixture, &["conway.trim"]);

    let out = run_conway(&["-p", "hi", "--allowed-tools", "bash"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let requests = mock.requests();
    let last = requests.last().expect("mock must have received a request");
    let last_wire = serde_json::to_string(last).expect("serialize the last request");
    assert!(
        !last_wire.contains("round-marker-0"),
        "with [plugins].install = [\"conway.trim\"], the oldest tool round-trip must have been \
         curated away by the final request -- got: {last_wire}"
    );
}

/// An id in `[plugins].install` this binary does not link is a hard config
/// error, never a silent no-op.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_plugins_install_id_is_a_hard_error() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("unreachable"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.totally_unknown"]);

    let out = run_conway(&["-p", "hi"], &fixture);

    assert!(
        !out.status.success(),
        "an unrecognized plugins.install id must fail the run, not silently ignore it"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("conway.totally_unknown"),
        "the error must name the offending id, got stderr: {stderr}"
    );
    // the message widened to list
    // linked router factory ids alongside linked plugin ids, resolved
    // against `[plugins].install` in the same pass.
    assert!(
        stderr.contains("conway.plugin_skeleton"),
        "the error must list the linked first-party plugin ids, got stderr: {stderr}"
    );
    assert!(
        stderr.contains("linked router factories"),
        "the error must also list linked router factory ids: 'conway.routing' \
         today -- see first_party_plugins::router_bundle's own doc), got \
         stderr: {stderr}"
    );
    assert!(
        stderr.contains("conway.routing"),
        "the linked router factory list must name the installed router plugin's own \
         published id, got stderr: {stderr}"
    );
    // the message widened again to
    // list linked backend factory ids alongside the other two.
    assert!(
        stderr.contains("linked backend factories"),
        "the error must also list linked backend factory ids, got stderr: {stderr}"
    );
    assert!(
        stderr.contains("anthropic") && stderr.contains("openai-compat"),
        "the linked backend factory list must name both published kind ids, got stderr: {stderr}"
    );
}

/// The VERIFICATION ANCHOR for (this
/// module's own doc, above): an ORDINARY rendered fixture -- no
/// `[plugins].install` entry, no `[plugins].default_backends` override, the
/// exact config every other test in this file also renders -- completes a
/// real one-shot prompt through the real compiled binary against a
/// loopback mock server. No credentials (the fixture's `dialect: "openai"`
/// entry carries none), no network beyond that loopback listener. If
/// `conway_plugin_backends`'s two `BackendFactory`s ever stopped attaching
/// by default (`[plugins].default_backends`'s own default value, unioned
/// into `wanted` by `main.rs`'s `build_conway` before `first_party_plugins
/// ::install` ever runs), `ConwayBuilder::build` would fail with "unknown
/// kind 'openai-compat'" and this run would exit non-zero.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_backends_attach_with_no_plugins_install_entry_and_complete_a_one_shot_prompt() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("hi from the default-attached backend"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);
    // Deliberately no `add_plugins_install` call: an ordinary settings
    // file, exactly as a fresh install ships.

    let out = run_conway(&["-p", "hi"], &fixture);
    assert!(
        out.status.success(),
        "a fresh install with an ordinary settings file must complete a one-shot prompt with \
         no [plugins].install entry at all -- stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "hi from the default-attached backend",
        "stdout must carry the model's real reply, proving the request actually reached the \
         mock backend through the default-attached OpenAiCompatBackendFactory"
    );

    let requests = mock.requests();
    assert_eq!(
        requests.len(),
        1,
        "exactly one real HTTP request must have reached the mock server"
    );
}
