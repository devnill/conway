//! Dogfood gates 1-2 (board item `01M2MEZMH2KJ79XSADQWCBFVFH`), automating
//! the machine-assertable half of two gates that were operator-only only
//! because nobody had driven the TUI without a human at a terminal. Both
//! gates' own underlying features are already shipped and unit-tested;
//! these are END-TO-END pins through the real compiled `conway` binary
//! (`conway routes explain`, and a real pty-attached TUI session) so a
//! regression in the already-shipped behavior shows up here rather than
//! nowhere.
//!
//! ## Gate 1: `routes explain` window provenance (board item
//! `01M1ZJR3DB48FSHVGGGBDGC13S`)
//!
//! Every assertion this file automates is covered below; see each test's
//! own doc for which gate-1 bullet it proves. One bullet is deliberately
//! NOT proven here at the byte-for-byte "same number" level -- see
//! [`tui_ctx_field_marks_the_assumed_floor_exactly_when_routes_explain_does`]'s
//! own doc for exactly what is and is not checked, and why.
//!
//! **A premise-check finding, not a regression:** gate 1's own wording
//! says "a probed or FLOORED window must NEVER be labelled verified."
//! `crates/conway-cli/src/commands/routes.rs::render_context_window_source`
//! maps `ContextTokensSource::DialectDefaultFloor` to `"verified"` --
//! literally a floor, literally labelled verified. Reading the surrounding
//! code (that function's own doc, plus `ContextTokensSource::
//! DialectDefaultFloor`'s doc in `crates/conway-core/src/capabilities.rs`)
//! shows this is DELIBERATE, not an oversight: that variant means "no
//! per-model fact exists, but this dialect's own baseline IS itself a
//! real, sourced figure for the provider as a whole (`openai`'s 128,000,
//! Anthropic's 200,000) -- distinct from `Unverified`, where the
//! dialect's own baseline is ALSO just an unsourced guess (e.g. Ollama's
//! 32,768)." An existing unit test in that same file
//! (`render_context_window_source_names_every_declared_variant_distinctly`)
//! already pins exactly this mapping, so it is not a silent, undiscovered
//! defect either. This file therefore tests the vocabulary's genuinely
//! sharp edge -- `Probed`/`Unverified`/`Override` must never render
//! `"verified"` -- rather than re-asserting the `DialectDefaultFloor`
//! judgement call, which is a human design decision already made and
//! already pinned elsewhere, not a bug to catch again here.
//!
//! ## Gate 2: status line refreshes (board item `01M1ZJSBY6QXSSZF7PMV7N9FCX`)
//!
//! One gate-2 bullet -- "the example JSON pasted verbatim out of
//! `docs/plugins/statusline.md` loads" -- is already fully automated by
//! `crates/conway-cli/src/tui/config.rs`'s own
//! `tests::the_statusline_doc_example_loads_through_the_real_tui_schema`,
//! which extracts that doc's fenced block byte-for-byte at test time. That
//! test lives in `src/`, outside this wave's file fence, and is not
//! duplicated here.

#[allow(dead_code)]
mod common;

// A sibling top-level module, not nested inside `common` (`common/mod.rs`
// is out of this writer's fence) -- see `mcp_fixtures`'s own top doc, and
// `fixture_with_unverified_floor`'s own doc below for why this file needs
// it.
#[path = "common/mcp_fixtures.rs"]
mod mcp_fixtures;

use std::time::Duration;

use common::mock_backend::{MockBackend, Script};
use common::pty::PtySession;

const LANDED: &str = "Type a message, or / for commands";

// ---------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------

/// Runs `conway routes explain <role> --json` against `fixture` and
/// parses the single JSON object it prints (`commands::routes::
/// print_json`'s own shape: one object, one `println!`, never JSON Lines).
fn explain_json(fixture: &common::Fixture, role: &str) -> serde_json::Value {
    let out = common::run_conway(&["routes", "explain", role, "--json"], fixture);
    assert!(
        out.status.success(),
        "routes explain --json must succeed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "routes explain --json must print valid JSON: {e}\nstdout={}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

/// Runs `conway routes explain <role>` (text mode) against `fixture`.
fn explain_text(fixture: &common::Fixture, role: &str) -> String {
    let out = common::run_conway(&["routes", "explain", role], fixture);
    assert!(
        out.status.success(),
        "routes explain must succeed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The `[position] model SELECTED ...` line -- the one line every
/// assertion in this file about the text-mode rendering reads, per the
/// board's own "assert the specific line carrying the claim" instruction.
fn selected_line(text: &str) -> &str {
    text.lines()
        .find(|l| l.contains("SELECTED"))
        .unwrap_or_else(|| panic!("expected a SELECTED line in routes explain output: {text:?}"))
}

/// A fresh `conway.json` naming ONE `openai-compat`/`dialect: "ollama"`
/// backend, with NO `.conway/models.json` at all -- the one shape
/// `common::write_fixture`'s own template can never produce (it always
/// stamps an `Override` entry for its model). `dialect: "ollama"` is
/// deliberate: `crates/conway-plugin-backends/src/profile.rs`'s own
/// `only_openai_declares_its_context_window_verified` test confirms
/// `openai` is the ONLY dialect whose baseline counts as a sourced fact --
/// every other dialect (Ollama included) resolves an unnamed model to
/// `ContextTokensSource::Unverified`, rendered `"floor (assumed)"`.
/// `mcp_server_count` configured `[plugins].mcp[]` entries are added
/// (each costing `first_run::MCP_SERVER_TOOL_SCHEMA_TOKENS_EST_PER_SERVER`
/// = 5,100 estimated tokens) so a caller can deterministically force the
/// runway/fixed-cost warning past its 50%-of-window threshold without
/// depending on the real, compiled `DEFAULT_OPINION_SET`'s own tool-schema
/// size (which this test file cannot see without running the binary):
/// even at zero measured tool-schema tokens, `command_prompt_allowance`
/// alone is a fixed 14,000, and `3 * 5,100 = 15,300` on top of that
/// already exceeds half of the 32,768-token floor (16,384) with room to
/// spare. **That arithmetic is a flat per-CONFIGURED-ENTRY allowance,
/// counted off `conway.config().plugins.mcp.len()` alone (`first_run::
/// default_opinion_set_footprint`'s own signature takes a bare `usize`
/// count, never a live `tools/list` response) -- so each entry only needs
/// to survive `ConwayBuilder::build`'s own install-time handshake, not
/// declare any particular number of tools.** Each entry is therefore
/// `mcp_fixtures::SLEEP_SERVER` (this crate's own established gate-8
/// fixture -- `write_script`/`warm`, reused verbatim rather than a second
/// fixture idiom): a real stdio MCP server that completes `initialize`/
/// `notifications/initialized`/`tools/list` and then idles on stdin,
/// `tools/call` never reached by this test. `["true"]` (an earlier version
/// of this fixture) exits immediately with no stdout at all, so the
/// install step saw every configured server die mid-handshake (`session
/// died: closed stdout (EOF) mid-session`) and failed the whole build --
/// `conway routes explain` never even started.
fn fixture_with_unverified_floor(mcp_server_count: usize) -> common::Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let mcp: Vec<serde_json::Value> = if mcp_server_count == 0 {
        Vec::new()
    } else {
        // A throwaway current-thread-or-multi runtime, scoped to this
        // function alone: `mcp_fixtures::warm` is async (`tokio::process::
        // Command`), but every caller of THIS function that passes a
        // nonzero count is a plain, non-`tokio` `#[test]` -- bridging here,
        // once, is cheaper than converting that test (and its unrelated
        // `mcp_server_count=0` siblings, which never reach this branch at
        // all) to `#[tokio::test]` just to await one warmup call.
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime for MCP script warmup");
        (0..mcp_server_count)
            .map(|i| {
                let id = format!("dogfood-mcp-{i}");
                let script_path = mcp_fixtures::write_script(
                    dir.path(),
                    &format!("{id}.py"),
                    mcp_fixtures::SLEEP_SERVER,
                );
                // Board item 01M09MPZ9C188AHNBKWEJ3CEQA: a freshly-written
                // script's FIRST exec on this OS can block for seconds at
                // ~0% CPU before its own code ever runs -- warmed once,
                // discarded, before `conway`'s own timed install-time
                // handshake below, the identical precedent every other
                // `mcp_fixtures` consumer in this crate already follows.
                rt.block_on(mcp_fixtures::warm(&script_path));
                serde_json::json!({
                    "id": id,
                    "command": [script_path.display().to_string()],
                })
            })
            .collect()
    };
    let config = serde_json::json!({
        "default_role": "default",
        "limits": { "max_steps": 5 },
        "backends": {
            "local": {
                "kind": "openai-compat",
                "base_url": "http://127.0.0.1:1/v1",
                "dialect": "ollama"
            }
        },
        "roles": {
            "default": { "chain": ["local/dogfood-unverified-model"] },
            "coder": { "chain": ["local/dogfood-unverified-model"] }
        },
        "plugins": { "mcp": mcp }
    });
    let config_path = dir.path().join("conway.json");
    std::fs::write(
        &config_path,
        serde_json::to_vec(&config).expect("serialize conway.json"),
    )
    .expect("write conway.json");
    common::Fixture { dir, config_path }
}

/// Splices `tui` into `fixture`'s already-written `conway.json` --
/// `common::write_fixture`'s own template has no `[tui]` slot, and this
/// wave may not edit `common/mod.rs` to add one. `conway::config::merge`
/// strips `"tui"` before `ConwayConfig`'s own `#[serde(deny_unknown_
/// fields)]` deserialize ever sees it (`crates/conway/src/config/merge.rs`
/// line 288), so adding this key never breaks the facade's own config
/// load -- only `crate::tui::config::load`'s separate `[tui]` parse (used
/// only by the TUI binary path) ever reads it.
fn add_tui_section(fixture: &common::Fixture, tui: serde_json::Value) {
    let raw = std::fs::read_to_string(&fixture.config_path).expect("read conway.json");
    let mut doc: serde_json::Value = serde_json::from_str(&raw).expect("parse conway.json");
    doc["tui"] = tui;
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&doc).expect("serialize"),
    )
    .expect("rewrite conway.json with [tui]");
}

// ---------------------------------------------------------------------
// Gate 1: `routes explain` window provenance
// ---------------------------------------------------------------------

/// Bullets: "no candidate conway will route to prints `unknown`" + "every
/// label is one of the four named strings" + "`--json` and text agree on
/// both the number and the source."
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn routes_explain_never_prints_unknown_and_json_and_text_agree_on_window_and_provenance() {
    let mock = MockBackend::start(Script(vec![])).await;
    // `write_fixture` always stamps a `.conway/models.json` `Override`
    // entry for its own mock model -- a real, reachable, fully-resolved
    // candidate, exactly the "a candidate conway will route to" case this
    // bullet is about.
    let fixture = common::write_fixture(&mock, 5);

    let json = explain_json(&fixture, "default");
    let chain = json["chain"].as_array().expect("chain must be an array");
    assert_eq!(
        chain.len(),
        1,
        "this fixture's role has exactly one candidate: {chain:?}"
    );
    let entry = &chain[0];
    let provenance = entry["context_window_source"]
        .as_str()
        .expect("context_window_source must be a string");
    assert_ne!(
        provenance, "unknown",
        "a configured, reachable candidate must never print unknown: {entry:?}"
    );
    assert!(
        ["verified", "models.json", "probed", "floor (assumed)"].contains(&provenance),
        "provenance must be one of the four named labels, got {provenance:?}: {entry:?}"
    );
    let window = entry["context_window_tokens"]
        .as_u64()
        .expect("context_window_tokens must be a number");

    let text = explain_text(&fixture, "default");
    let line = selected_line(&text);
    assert!(
        line.contains(&format!("window: {window} [{provenance}]")),
        "text output must show the SAME window number and provenance the --json output showed: \
         {line:?}"
    );
}

/// The sharp case this gate exists for: `Unverified` (a dialect's own
/// unsourced placeholder floor) must render `"floor (assumed)"`, and must
/// NEVER render `"verified"` -- a floored number dressed up as more
/// certain than it is would be worse than the pre-fix `"unknown"`.
#[test]
fn routes_explain_never_labels_an_unverified_dialect_floor_as_verified() {
    let fixture = fixture_with_unverified_floor(0);

    let json = explain_json(&fixture, "default");
    let entry = &json["chain"][0];
    assert_eq!(
        entry["context_window_source"], "floor (assumed)",
        "{entry:?}"
    );
    assert_eq!(entry["context_window_tokens"], 32_768, "{entry:?}");
    assert_ne!(entry["context_window_source"], "verified", "{entry:?}");

    let text = explain_text(&fixture, "default");
    let line = selected_line(&text);
    assert!(line.contains("[floor (assumed)]"), "{line:?}");
    assert!(!line.contains("[verified]"), "{line:?}");
}

/// "A runway notice fired on an assumed floor says it is assumed."
///
/// `first_run::runway_fixed_cost_warning`'s own wording never names
/// provenance at all (verified by reading it) -- the honesty guarantee
/// this bullet describes holds at the level of `routes explain`'s single
/// rendered line, which prints the fixed-cost flag and the provenance
/// marker TOGETHER (`commands::routes::print_text`'s own format string:
/// `"window: {window} [{provenance}], ..., fixed_cost: {fixed_cost}"`).
/// This test forces the runway warning to fire (three configured MCP
/// servers, guaranteeing the fixed cost exceeds half of the 32,768-token
/// assumed floor regardless of the real, compiled default tool set's own
/// size -- see `fixture_with_unverified_floor`'s own doc for the
/// arithmetic) and confirms both facts land on the SAME line: an operator
/// reading only the "over half your window" flag can never miss that the
/// window number backing it is not even a real fact.
#[test]
fn routes_explain_runway_warning_on_an_assumed_floor_still_names_it_assumed() {
    let fixture = fixture_with_unverified_floor(3);

    let json = explain_json(&fixture, "default");
    let entry = &json["chain"][0];
    let fixed_cost = entry["fixed_cost"]
        .as_str()
        .expect("fixed_cost must be a string");
    assert!(
        fixed_cost.contains("over half"),
        "3 configured MCP servers (15,300 estimated tokens) plus the fixed 14,000-token \
         command-prompt allowance alone already exceed half of the 32,768-token floor \
         (16,384): {fixed_cost:?}"
    );
    assert_eq!(
        entry["context_window_source"], "floor (assumed)",
        "{entry:?}"
    );

    let text = explain_text(&fixture, "default");
    let line = selected_line(&text);
    assert!(line.contains("over half"), "{line:?}");
    assert!(line.contains("floor (assumed)"), "{line:?}");
}

/// The TUI's `ctx` status-line field agrees with `routes explain` on
/// provenance for the assumed-floor case: both derive from the identical
/// `ContextTokensSource` resolved off the same `CapabilityIndex` entry
/// (board item `01M1ZJ796E0YP6Y8QWS8HB0AVB`; `crates/conway-cli/src/tui/
/// state.rs`'s own doc on `model_max_context_source`), so a disagreement
/// here would be a real regression in that shared-source wiring, not two
/// independently-computed opinions that happened to differ.
///
/// **Scope, disclosed rather than silently narrowed:** this checks
/// PROVENANCE agreement (the assumed-floor marker's presence), not a
/// byte-for-byte NUMBER match. `view/status.rs::ctx_label` renders the
/// compact status line as a PERCENTAGE once a window is known
/// (`focused_ctx_tokens * 100 / max`), or a bare spent-token count when it
/// is not -- the raw window number itself is never printed on that line
/// at all, by design (`ctx_label`'s own doc: this is deliberate lossy
/// rendering, not an omission). Reconstructing the window number from a
/// percentage plus the spent-token count would depend on live tokenizer
/// output and would be exactly the kind of fragile, timing-sensitive
/// assertion the board's own flake warning cautions against. The
/// PROVENANCE boundary (assumed-floor marker shown vs. not) is the
/// meaningful, robustly-assertable half of "agrees... on window and
/// provenance" available from the rendered screen alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tui_ctx_field_marks_the_assumed_floor_exactly_when_routes_explain_does() {
    let fixture = fixture_with_unverified_floor(0);

    let cmd = common::pty_command(&[], &fixture);
    let session = PtySession::spawn(cmd, 160, 45);
    session.wait_for(LANDED, Duration::from_secs(15));
    session.wait_for("floor (assumed)", Duration::from_secs(10));

    let json = explain_json(&fixture, "default");
    assert_eq!(
        json["chain"][0]["context_window_source"], "floor (assumed)",
        "the CLI, against the identical fixture, must agree: {json:?}"
    );
}

/// The other half of the same boundary: a `models.json`-backed (`Override`)
/// window must NEVER show the assumed-floor marker in the TUI, matching
/// `routes explain`'s own `"models.json"` (never `"floor (assumed)"`) for
/// the identical fixture.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tui_ctx_field_never_shows_the_assumed_floor_marker_for_a_models_json_override() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = common::write_fixture(&mock, 5);

    let cmd = common::pty_command(&[], &fixture);
    let session = PtySession::spawn(cmd, 160, 45);
    session.wait_for(LANDED, Duration::from_secs(15));
    let screen = session.screen();
    assert!(
        !screen.contains("floor (assumed)"),
        "an Override (models.json) window must never render the assumed-floor marker: {screen}"
    );

    let json = explain_json(&fixture, "default");
    assert_eq!(
        json["chain"][0]["context_window_source"], "models.json",
        "{json:?}"
    );
}

// ---------------------------------------------------------------------
// Gate 2: status line refreshes
// ---------------------------------------------------------------------

/// "Two captures separated by more than the cadence differ." A shell
/// command that writes+reads an incrementing counter file (deterministic,
/// no network, no fixed sleep needed for the ASSERTION itself -- only
/// `PtySession::wait_for_since`'s own poll-with-timeout) proves the TUI's
/// live status line actually re-reads the plugin's contribution on its own
/// cadence, not merely once at startup (the exact gap board item
/// `01M0Y3A8MYKKE0GMYKZE1K0QTD` closed -- `conway-plugin-statusline`'s own
/// module doc's "Addendum").
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_line_command_output_refreshes_in_the_live_tui() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = common::write_fixture(&mock, 5);

    let counter_path = fixture.dir.path().join("dogfood_counter");
    let counter = counter_path.to_str().expect("utf8 tempdir path");
    // Alternates between two tokens of equal length whose every character
    // differs. That is load-bearing, not cosmetic: the pty harness
    // accumulates the raw byte STREAM and strips ANSI from it -- it does not
    // emulate a screen buffer. A terminal redrawing this line rewrites only
    // the cells that changed, so a counter (`1` -> `2`) re-emits the bare
    // digit and the literal string "dogfood: 2" NEVER appears in the stream,
    // while the accumulated capture reads "dogfood: 1" followed later by a
    // run of loose digits. An all-characters-differ token forces the whole
    // value to be rewritten contiguously, so the awaited text really is
    // present. (Found 2026-09-16: the counter form of this test failed for
    // exactly this reason while the feature under test worked correctly.)
    let script = format!(
        "n=$(( $(cat '{counter}' 2>/dev/null || echo 0) + 1 )); echo $n > '{counter}'; \
         if [ $(( n % 2 )) -eq 1 ]; then echo AAAAA; else echo BBBBB; fi"
    );
    add_tui_section(
        &fixture,
        serde_json::json!({
            "status_line_command": {
                "command": ["sh", "-c", script],
                "key": "dogfood",
                "refresh_interval_ms": 1000,
                "timeout_ms": 2000
            }
        }),
    );

    let cmd = common::pty_command(&[], &fixture);
    let session = PtySession::spawn(cmd, 160, 45);
    session.wait_for(LANDED, Duration::from_secs(15));
    // `PLUGIN_STATUS_POLL_TICK` (`tui/app/run.rs`) and the plugin's own
    // refresh floor are both 1000ms; generous timeouts below account for
    // both ticks landing back-to-back in the worst case, without any
    // fixed sleep standing in for the wait itself.
    // Two captures separated by more than the cadence differ -- the gate's
    // own wording. `AAAAA` is the odd-numbered refresh, `BBBBB` the even,
    // so seeing the second strictly after the first proves the status line
    // re-ran its command and re-rendered a CHANGED value, not merely that
    // it rendered once.
    let first = session.wait_for("AAAAA", Duration::from_secs(8));
    session.wait_for_since("BBBBB", first, Duration::from_secs(10));
}

/// "A command that sleeps past its timeout: the UI never blocks -- the
/// prompt still accepts input while it runs." Types a distinctive string
/// right after landing, while a `sleep 30` status-line command (given a
/// generous 10s `timeout_ms`, so it is still genuinely in-flight) is
/// running, and confirms the typed text echoes back almost immediately --
/// proving the render/input loop was never waiting on the subprocess.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_line_command_stuck_past_its_timeout_never_blocks_the_prompt() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = common::write_fixture(&mock, 5);
    add_tui_section(
        &fixture,
        serde_json::json!({
            "status_line_command": {
                "command": ["sleep", "30"],
                "key": "dogfood",
                "refresh_interval_ms": 5000,
                "timeout_ms": 10000
            }
        }),
    );

    let cmd = common::pty_command(&[], &fixture);
    let mut session = PtySession::spawn(cmd, 160, 45);
    let landed = session.wait_for(LANDED, Duration::from_secs(15));
    // 20s, not the 3s this originally used. This bounds how long we wait
    // before declaring the prompt dead; it does NOT race a competing writer,
    // because nothing overwrites the echoed probe text once it lands. So a
    // generous deadline cannot change the answer, only how patiently we wait
    // for it -- unlike a timeout that arbitrates between two events, where a
    // bigger number moves the failure threshold without removing the race.
    // Measured 2026-09-16: 3s failed 1 run in 20 on a cold binary; 20s then
    // failed under full-workspace parallelism, where dozens of test binaries
    // compete for CPU and a TUI render loop is not scheduled promptly. 60s is
    // deliberately far past any plausible healthy latency: the ONLY cost of a
    // generous bound here is how long a genuine hang takes to report, and a
    // hang is what this test exists to catch.
    session.send("dogfood-input-liveness-probe");
    session.wait_for_since(
        "dogfood-input-liveness-probe",
        landed,
        Duration::from_secs(60),
    );
}

/// "A command exiting non-zero renders `failed`, not blank" --
/// `conway_plugin_statusline::run_once`'s own `exit {code}: {stderr_line}`
/// reason text, reaching the real, live status line end to end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_line_command_nonzero_exit_renders_failed_with_reason_not_blank() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = common::write_fixture(&mock, 5);
    add_tui_section(
        &fixture,
        serde_json::json!({
            "status_line_command": {
                "command": ["sh", "-c", "echo boom >&2; exit 3"],
                "key": "dogfood",
                "refresh_interval_ms": 1000,
                "timeout_ms": 2000
            }
        }),
    );

    let cmd = common::pty_command(&[], &fixture);
    let session = PtySession::spawn(cmd, 160, 45);
    session.wait_for(LANDED, Duration::from_secs(15));
    session.wait_for("dogfood: exit 3: boom", Duration::from_secs(8));
}

/// "The permission-mode field keeps its place... must never be crowded
/// out by a refreshed contribution." A failing (and therefore
/// theme.error-styled, non-empty) status-line contribution is live on
/// screen at the same time the operator cycles into `AUTO-ALLOW` -- the
/// one label `view/status.rs`'s own doc calls "a genuine safety signal:
/// an operator who forgets they're in it is the exact failure this
/// guards against." Both must be visible in the same final capture.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_line_command_failure_never_crowds_out_the_permission_mode_field() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = common::write_fixture(&mock, 5);
    add_tui_section(
        &fixture,
        serde_json::json!({
            "status_line_command": {
                "command": ["sh", "-c", "echo boom >&2; exit 3"],
                "key": "dogfood",
                "refresh_interval_ms": 1000,
                "timeout_ms": 2000
            }
        }),
    );

    let cmd = common::pty_command(&[], &fixture);
    let mut session = PtySession::spawn(cmd, 160, 45);
    let landed = session.wait_for(LANDED, Duration::from_secs(15));
    session.wait_for_since("dogfood: exit 3", landed, Duration::from_secs(8));

    // Prompt -> Plan -> AutoAllow: two Shift-Tabs (`tui_permission_mode.rs`'s
    // own precedent for the cycle order). Intermediate "plan" text is
    // deliberately never waited on here -- `tui_guided_setup.rs`'s own
    // 2026-09-16 fix note warns against anchoring `wait_for`/`wait_for_any`
    // on a pattern that can also appear in unrelated help/hint text; going
    // straight for the terminal `AUTO-ALLOW` state avoids that class of
    // flake entirely.
    session.send_shift_tab();
    session.send_shift_tab();
    session.wait_for_since("AUTO-ALLOW", landed, Duration::from_secs(10));

    let screen = session.screen();
    assert!(
        screen.contains("AUTO-ALLOW"),
        "the permission-mode field must remain visible: {screen}"
    );
    assert!(
        screen.contains("dogfood: exit 3"),
        "the failing plugin contribution must ALSO still be visible -- this is what proves \
         AUTO-ALLOW was not merely shown because the contribution had nothing to compete with: \
         {screen}"
    );
}
