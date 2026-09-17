//! Dogfood gates 5-6 (board item `01M2MEZMH2KJ79XSADQWCBFVFH`), driven
//! against the real, compiled `conway` binary via `common::pty` and
//! `common::{run_conway, open_conway}` -- no feature under test is touched
//! by this file; a failing assertion here is a regression to report, not to
//! fix (this file's own header note repeats the item's own rule so a future
//! reader does not have to go find it).
//!
//! # Gate 5 -- cache reporting (`01M1ZJTXRWRA93KRWZSZBA745G`)
//!
//! **Premise check, disclosed up front.** The gate's own text says "'not
//! reported' and 'not supported' are different wordings. They are
//! different facts and collapsing them is the defect." Reading the
//! implementation (`crates/conway-cli/src/tui/usage_format.rs`,
//! `crates/conway-cli/src/tui/state/turn_summary.rs`,
//! `crates/conway-cli/src/commands/routes.rs`) turns up exactly ONE
//! wording, "not reported", used for both the per-backend STATIC
//! capability (`conway::CacheReporting`, what `routes explain` prints) and
//! the per-turn DYNAMIC observation (`conway::CacheAccounting`, what the
//! TUI's cache suffix prints) -- the string "not supported" does not occur
//! anywhere in this tree for cache. Worse: `usage_format::cache_suffix`'s
//! own signature (`fn cache_suffix(usage: &Usage, focused_model: Option<&
//! str>) -> String`) never receives a `CacheReporting` value at all, so it
//! is not merely choosing the same wording for two facts -- it has no way
//! to ask the question. `not_reported_wording_is_identical_regardless_of_
//! backend_capability`, below, pins this precisely and cites the exact
//! lines. This is a genuine finding, reported per the item's own "a failure
//! you find is a finding to report, not to fix" rule -- **not fixed here**.
//!
//! Prefix byte-stability (the OTHER cache-economy claim gate 5's parent
//! item names) already has a dedicated test --
//! `crates/conway-runtime/tests/prefix_stability.rs`'s
//! `three_consecutive_turns_form_a_growing_prefix_with_a_stable_prefix_key`
//! and `a_fork_childs_first_request_extends_the_parents_last_request_as_a_
//! wire_byte_prefix` -- cited here, not duplicated.
//!
//! The Anthropic-real-key half (does a REAL provider actually cache
//! conway's prefix) is explicitly out of scope for this file, per the
//! item's own text; it is the operator's judgement half
//! (`01M1ZJTXRWRA93KRWZSZBA745G`'s own "needs your key" section).
//!
//! # Gate 6 -- fallback explains itself (`01M1ZJVF7RY0BM4235974X53KB`)
//!
//! `after: []` was the original defect (an empty skipped-candidate list on
//! a genuine fallback); `fallback_notice_and_why_name_the_skipped_
//! candidate_with_its_numbers` proves the opposite end to end: a real,
//! two-model chain where the first candidate is skipped for a REAL,
//! numeric reason (its declared `max_context_tokens` cannot fit the
//! request), and the second is selected.
//!
//! "`/agents` shows one lineage rather than three rows" is only PARTLY
//! provable through this harness -- disclosed prominently, not silently
//! dropped, in `three_model_switches_keep_per_turn_attribution_recoverable_
//! via_why`'s own doc comment.
//!
//! Routing POLICY (which candidate a chain resolves to) is asserted to be
//! UNCHANGED by construction: every test in this file reads `conway-
//! routing`/`conway-plugin-routing` and `conway-cli/src/tui` READ-ONLY (this
//! writer's fence has no path under either), and the fallback test's own
//! assertion IS that the long-standing selection rule (skip on failed
//! headroom, select the next candidate that fits) still holds -- a policy
//! change would show up as this test's own failure, which is exactly the
//! regression-report path the item wants, not a silent pass.

mod common;

// `common/mod.rs` is shared with a sibling writer and out of this writer's
// fence, so this new helper cannot be declared as `pub mod cache_mock;`
// there. `#[path]` reaches the same file (`tests/common/cache_mock.rs`,
// this writer's own new file) as an independent top-level module instead
// -- no edit to `common/mod.rs` or `common/pty.rs` anywhere in this crate.
#[path = "common/cache_mock.rs"]
mod cache_mock;

use std::time::Duration;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::pty::PtySession;
use common::{open_conway, run_conway, write_fixture, Fixture};
use conway::SessionFilter;
use serde_json::Value;

const LANDED: &str = "Type a message, or / for commands";

fn ok_script() -> Script {
    Script(vec![vec![Chunk::Text("ok"), Chunk::Finish("stop")]])
}

// ---------------------------------------------------------------------
// Gate 5 -- cache reporting
// ---------------------------------------------------------------------

/// A backend that never sends a `usage` object at all (`common::
/// mock_backend`'s own SSE/JSON writers, unconditionally, this suite's own
/// shared harness -- see `common/mock_backend.rs`'s module doc) must never
/// render a cache PERCENTAGE: `crates/conway-cli/src/tui/usage_format.rs`'s
/// own `CacheAccounting::NotReported` arm renders a bare wording and no
/// number at all. Proven on the REAL turn-end summary line (T4,
/// `turn_summary.rs`), not the status line, since that is the surface
/// `sessions show`'s raw counts are compared against.
///
/// **What this covers that `usage_format.rs`'s own unit tests do not.**
/// Those call `cache_suffix` directly and pin all three wordings and their
/// pairwise distinctness (`the_three_not_reported_wordings_are_pairwise_
/// distinct`). This one proves the end-to-end claim they cannot: that a
/// real turn, through the real compiled binary, against a backend that sent
/// no `usage`, puts no percentage on the rendered summary. A `0% cached`
/// here would be an affirmative lie about a number nobody reported.
///
/// **History, so it is not re-derived.** This test was formerly
/// `not_reported_wording_is_identical_regardless_of_backend_capability` and
/// asserted the DEFECT board item `01M2NS0996E139VN5R8W4PGD8V` names: that
/// "cannot report, ever" and "can report, said nothing this turn" rendered
/// identical text. That item has landed -- `cache_suffix` now takes a
/// `CacheReporting` and there are three distinct wordings -- so the
/// assertion is gone and the name with it. It also carried an unprovable
/// precondition (`routes explain` declaring the backend `reported`), which
/// is what had it `#[ignore]`d against `01M2NRS3FRZNXF138Q0B6RGHXT`: a
/// default install runs on MinimalRouter, where every capability field
/// reads `unknown`. Nothing below needs that precondition, so it is gone
/// too and the test runs again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_with_no_usage_field_never_renders_a_cache_percentage() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);

    let cmd = common::pty_command(&[], &fixture);
    let mut session = PtySession::spawn(cmd, 160, 45);
    let landed = session.wait_for(LANDED, Duration::from_secs(15));

    session.send("hi\r");
    session.wait_for_since("not reported by mock", landed, Duration::from_secs(15));

    let screen = session.screen();
    assert!(
        !screen.contains("(0% cached)"),
        "a backend that never sent usage must never render a percentage, let alone 0%. \
         Screen:\n{screen}"
    );
    assert!(
        !screen.contains("% cached)"),
        "no percentage of any kind belongs on a turn with no usage field at all. \
         Screen:\n{screen}"
    );
}

/// A turn whose backend DOES send `usage.prompt_tokens_details.
/// cached_tokens` renders a percentage (`CacheAccounting::Reported`), and
/// that percentage corroborates the raw counts `conway sessions show`
/// prints for the SAME turn -- the gate's own "where a percentage is
/// shown, it corroborates the raw token counts" bullet, proven end to end
/// rather than re-asserting `cache_suffix`'s own arithmetic (already unit
/// tested in `usage_format.rs` -- `reported_renders_nonzero_percent` et
/// al.; this test's job is the WIRING between the wire response, the live
/// TUI render, and the durable session log, which those unit tests cannot
/// see).
///
/// Uses the sibling `cache_mock` module (this writer's own new helper, not
/// the shared `common::mock_backend`) -- see that module's own top doc for
/// why: the shared mock never emits a `usage` object at all, so no test
/// built on it can ever reach the `Reported` branch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reported_percentage_corroborates_raw_counts_in_sessions_show() {
    use cache_mock::{CacheMockBackend, CacheTurn, CacheUsage};

    // prompt_tokens=100, cache_read(cached_tokens)=80, completion=20 ->
    // denom = 100 + 80 + 0(cache_write, always 0 for this dialect -- see
    // `openai_compat::wire::map_usage`'s own doc) = 180; pct = 80*100/180
    // = 44 (integer division) -- `usage_format::cache_suffix`'s own
    // formula, restated here only to predict the ONE number this test
    // waits for, never re-implemented as a second copy of the arithmetic.
    let mock = CacheMockBackend::start(
        "cache-model",
        vec![CacheTurn {
            text: "ok",
            usage: Some(CacheUsage {
                prompt_tokens: 100,
                completion_tokens: 20,
                cached_tokens: Some(80),
            }),
        }],
    )
    .await;
    let fixture = common::write_fixture_with(&mock.base_url, &mock.model, 10);

    let cmd = common::pty_command(&[], &fixture);
    let mut session = PtySession::spawn(cmd, 160, 45);
    let landed = session.wait_for(LANDED, Duration::from_secs(15));

    session.send("hi\r");
    session.wait_for_since("(44% cached)", landed, Duration::from_secs(15));

    // Corroborate against `conway sessions show`'s own raw numbers -- the
    // real CLI subcommand, not a re-read of the same in-memory `Usage` the
    // TUI already rendered from (that would prove the TUI is consistent
    // with itself, not with the durable log).
    let conway = open_conway(&fixture).await;
    let sessions = conway
        .sessions(SessionFilter::default())
        .await
        .expect("list sessions");
    assert_eq!(sessions.len(), 1, "expected exactly one session");
    let sid = sessions[0].id;

    let show = run_conway(&["sessions", "show", &sid.to_string()], &fixture);
    assert!(
        show.status.success(),
        "sessions show must succeed: {}",
        String::from_utf8_lossy(&show.stderr)
    );
    let show_stdout = String::from_utf8_lossy(&show.stdout);
    assert!(
        show_stdout.contains("input_tokens: 100"),
        "sessions show must carry the raw prompt_tokens the TUI's 44% was computed from -- \
         got: {show_stdout}"
    );
    assert!(
        show_stdout.contains("cache_read_tokens: 80"),
        "sessions show must carry the raw cached_tokens the TUI's 44% was computed from -- \
         got: {show_stdout}"
    );
    assert!(
        show_stdout.contains("output_tokens: 20"),
        "sessions show must carry the raw completion_tokens too -- got: {show_stdout}"
    );
}

// ---------------------------------------------------------------------
// Gate 6 -- fallback explains itself
// ---------------------------------------------------------------------

/// A `conway.json` naming TWO models on one mock backend: `tiny` (a real,
/// tiny `max_context_tokens`, so it fails the headroom gate for any
/// ordinary request -- a genuine, numeric skip, never a contrived
/// `CapabilitySkip { missing: ["capabilities: unknown ..."] }` from an
/// absent models.json entry) and `big` (the shared mock's own advertised
/// model, comfortably large). Modeled on `tui_model_and_role.rs`'s own
/// `write_env_only_fixture` -- a bare, hand-built `conway.json` plus its
/// own `.conway/models.json`, since `common::write_fixture`'s template has
/// no slot for a second chain entry.
///
/// **`"plugins": {"install": ["conway.routing"]}` is load-bearing, not
/// decorative.** Established while diagnosing this test's own timeout:
/// `ConwayBuilder::build` (`crates/conway/src/builder.rs`) only installs
/// the real `conway_plugin_routing::DeclarativeRouter` -- the router with
/// per-candidate headroom/health checking and a genuine `after:` skip list
/// -- when a `RouterFactory` naming `conway_plugin_routing::ROUTER_ID`
/// (`"conway.routing"`) is present, either injected directly or (as here)
/// named in `[plugins].install`. Absent that, `build` falls back to
/// `conway_core::routing::MinimalRouter`, which does NOT filter candidates
/// by capability/headroom at all -- `MinimalRouter::reason_for`'s own
/// non-primary-position arm hard-codes `RoutingReason::Fallback { after:
/// Vec::new(), .. }`, an ALWAYS-empty skip list, by construction. Since
/// `fallback_notice_text` (`tui/state.rs`) returns `None` whenever `after`
/// is empty, a fixture on `MinimalRouter` can NEVER produce this gate's
/// own notice, regardless of whether a real runtime skip happens -- not
/// because the notice is broken, but because nothing ever computed a skip
/// to report. `common::write_fixture`'s own shared TEMPLATE has no
/// `[plugins]` section either (confirmed by reading it) and so is subject
/// to the identical limitation -- out of this writer's fence to change,
/// and NOT this test's problem to work around, since this fixture is
/// hand-built specifically to exercise a real `DeclarativeRouter` skip.
fn write_two_model_fixture(base_url: &str, tiny: &str, big: &str) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = serde_json::json!({
        "default_role": "default",
        "limits": { "max_steps": 10 },
        "backends": {
            "mock": { "kind": "openai-compat", "base_url": base_url, "dialect": "openai" }
        },
        "roles": {
            "default": { "chain": [format!("mock/{tiny}"), format!("mock/{big}")] }
        },
        "plugins": { "install": ["conway.routing"] }
    });
    let config_path = dir.path().join("conway.json");
    std::fs::write(
        &config_path,
        serde_json::to_vec(&config).expect("serialize conway.json"),
    )
    .expect("write conway.json");

    let models_dir = dir.path().join(".conway");
    std::fs::create_dir_all(&models_dir).expect("create .conway dir");
    let models_json = serde_json::json!({
        "models": {
            format!("mock/{tiny}"): {
                "max_context_tokens": 50,
                "tool_calling": "streaming_validated",
                "reasoning": false,
                "reliability_tier": "verified",
            },
            format!("mock/{big}"): {
                "max_context_tokens": 128_000,
                "tool_calling": "streaming_validated",
                "reasoning": false,
                "reliability_tier": "verified",
            },
        }
    });
    std::fs::write(
        models_dir.join("models.json"),
        serde_json::to_vec(&models_json).expect("serialize models.json"),
    )
    .expect("write models.json");

    Fixture { dir, config_path }
}

/// A chain naming N models on one mock backend, all comfortably large --
/// [`write_two_model_fixture`]'s sibling for the `/model` switching test,
/// which needs several DISTINCT, individually-nameable models rather than
/// one that must fail headroom.
fn write_multi_model_fixture(base_url: &str, models: &[&str]) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let chain: Vec<String> = models.iter().map(|m| format!("mock/{m}")).collect();
    let config = serde_json::json!({
        "default_role": "default",
        "limits": { "max_steps": 20 },
        "backends": {
            "mock": { "kind": "openai-compat", "base_url": base_url, "dialect": "openai" }
        },
        "roles": {
            "default": { "chain": [chain[0].clone()] }
        },
        // Parity with `write_two_model_fixture`'s own fix (see that
        // function's doc for the full `MinimalRouter`/`DeclarativeRouter`
        // reasoning): `/model <ref>`'s PIN path bypasses the role chain
        // and capability checking under EITHER router (`DeclarativeRouter::
        // evaluate`'s pin arm uses `std::slice::from_ref(pin_ref)`;
        // `MinimalRouter::chain_for`'s pin arm is the identical bypass), so
        // this was not identified as the cause of this test's own timeout
        // -- installed anyway, for the same reason a real operator's
        // session normally has it, and so this fixture's routing behavior
        // is not a second unverified variable alongside the real fix
        // below.
        "plugins": { "install": ["conway.routing"] }
    });
    let config_path = dir.path().join("conway.json");
    std::fs::write(
        &config_path,
        serde_json::to_vec(&config).expect("serialize conway.json"),
    )
    .expect("write conway.json");

    let models_dir = dir.path().join(".conway");
    std::fs::create_dir_all(&models_dir).expect("create .conway dir");
    let mut models_obj = serde_json::Map::new();
    for name in models {
        models_obj.insert(
            format!("mock/{name}"),
            serde_json::json!({
                "max_context_tokens": 128_000,
                "tool_calling": "streaming_validated",
                "reasoning": false,
                "reliability_tier": "verified",
            }),
        );
    }
    let models_json = serde_json::json!({ "models": Value::Object(models_obj) });
    std::fs::write(
        models_dir.join("models.json"),
        serde_json::to_vec(&models_json).expect("serialize models.json"),
    )
    .expect("write models.json");

    Fixture { dir, config_path }
}

/// A candidate genuinely skipped (headroom) has a non-empty `after:`
/// naming it, with its numbers, per the gate's own text ("`after: []` is
/// the original defect"): the TUI's own one-line notice
/// (`tui/state.rs::fallback_notice_text`) and `/why` (`tui/commands.rs::
/// render_why`/`render_routing_reason`) both surface it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fallback_notice_and_why_name_the_skipped_candidate_with_its_numbers() {
    let mock = MockBackend::start(ok_script()).await;
    // Only ONE real HTTP request happens here: chain evaluation (which
    // candidate is skipped, which is selected) is synchronous, in-process,
    // BEFORE any request is dispatched -- `conway-plugin-routing::router::
    // DeclarativeRouter::evaluate`'s own doc. `ok_script()` alone covers
    // the one request the SELECTED candidate (`big`) actually receives.
    let fixture = write_two_model_fixture(&mock.base_url, "tiny", "big");

    let cmd = common::pty_command(&[], &fixture);
    let mut session = PtySession::spawn(cmd, 160, 45);
    let landed = session.wait_for(LANDED, Duration::from_secs(15));

    session.send("hi\r");
    // The one-line notice: `tui/state.rs::fallback_notice_text`'s exact
    // shape, `"routed to {chosen} — {model} skipped: {error}"`.
    let after_notice =
        session.wait_for_since("routed to mock/big", landed, Duration::from_secs(15));

    let screen = session.screen();
    assert!(
        screen.contains("mock/tiny skipped:"),
        "the notice must name the SKIPPED candidate, not just the chosen one. Screen:\n{screen}"
    );
    assert!(
        screen.contains("capability: context: needs")
            && screen.contains("max_context_tokens is 50"),
        "the skip reason must carry its own numbers (est tokens, headroom, and the \
         candidate's own max_context_tokens=50) -- an `after: []`-shaped empty reason is \
         exactly the original defect. Screen:\n{screen}"
    );

    // `/why` shows more than the last hop: even for a single decision, the
    // reason line alone already carries the fallback position AND the full
    // skipped-candidate detail -- strictly more than "you are on mock/big
    // now", which a bare status line already said.
    session.send("/why\r");
    session.wait_for_since(
        "reason: fallback #1 after:",
        after_notice,
        Duration::from_secs(10),
    );
    let why_screen = session.screen();
    assert!(
        why_screen.contains("mock/tiny") && why_screen.contains("max_context_tokens is 50"),
        "/why's reason line must name the skipped candidate with its numbers too, not just \
         echo the notice. Screen:\n{why_screen}"
    );
}

/// After three `/model` switches, per-turn model attribution is still
/// recoverable via `/why`'s bounded session history (`AppState::
/// model_decision_history`, `render_why`'s `history.len() > 2` branch) --
/// the gate's own "which model served which turn is still recoverable"
/// half.
///
/// **The OTHER half of this gate -- "`/agents` shows ONE lineage rather
/// than three rows" -- is deliberately NOT re-proven here as a live
/// row-count, and that omission is intentional, not silent.** The
/// mechanical collapse itself already has a precise, dedicated unit test:
/// `crates/conway-cli/src/tui/state/agent_panel.rs::
/// three_consecutive_switches_collapse_to_one_visible_row` builds the
/// identical root -> a -> b -> c switch-lineage shape by hand and asserts
/// `visible_agent_nodes()` yields exactly the tip. This file cannot
/// re-assert the LIVE-COUNT claim through the pty harness for a structural
/// reason stated in `common/pty.rs`'s own top doc: `PtySession::screen()`
/// is a CUMULATIVE buffer of everything ever printed, "not a live,
/// cursor-addressed grid" -- text from an earlier `/agents` frame is still
/// present after a later redraw would have visually replaced it on a real
/// terminal. A "count N rows right now" assertion built on that buffer
/// would either (a) only ever grow across repeated captures, so it cannot
/// tell "hidden" from "still shown a moment ago", or (b) require re-
/// implementing a VT100 cursor-addressed grid, which this same module's
/// own doc explains was deliberately rejected as a second dependency this
/// item's budget does not allow. Nor does the panel print any row-count
/// text a caller could grep (`agents ({visibility_label} · ...)` -- the
/// title shows the visibility filter, never a count). What IS safely
/// provable through this harness -- ordering/presence-ever, its actual
/// design point -- is that a real, end-to-end run through the compiled
/// binary genuinely populates the switch-lineage/decision-history
/// machinery the cited unit test exercises by hand; that is what this test
/// proves instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_model_switches_keep_per_turn_attribution_recoverable_via_why() {
    // Replies that share no character in any column, for the same
    // partial-redraw reason the model names below do: `on-b` -> `on-c`
    // would differ in exactly one cell, and a terminal re-emits only the
    // cells that changed.
    let mock = MockBackend::start(Script(vec![
        vec![Chunk::Text("AAAA"), Chunk::Finish("stop")],
        vec![Chunk::Text("BBBB"), Chunk::Finish("stop")],
        vec![Chunk::Text("CCCC"), Chunk::Finish("stop")],
        vec![Chunk::Text("DDDD"), Chunk::Finish("stop")],
    ]))
    .await;
    // Model names that share NO characters in the same column. This is
    // load-bearing, and the pty I/O journal is what proved it: with
    // `model-a`/`model-b`/... the notice DOES render, but the terminal
    // redraws only the cells that changed, so switching b -> c emits
    // literally one byte --
    //
    //     RX <- \e[1;30H \e[38;5;6;49m c \e[1;41H K6XDQSHQ4MS7VMF22H ...
    //
    // -- a cursor move to column 30 and the single character `c`, because
    // `switched model to mock/model-` was already on screen. The awaited
    // string `"switched model to mock/model-c"` therefore never appears
    // contiguously in the byte stream, and no timeout could ever have
    // fixed that. All-different names force the whole NAME to be
    // rewritten, exactly as `dogfood_routes_and_status.rs`'s own
    // AAAAA/BBBBB status tokens do for the same reason.
    //
    // They do not, however, make the whole NOTICE contiguous: the journal
    // showed the b -> c redraw as `\e[1;24H\e[38;5;6;49mcccccc\e[1;40H...`,
    // i.e. the constant prefix `switched model to mock/` is still skipped
    // because it has not changed. The bare name is therefore the longest
    // token this harness can ever await for a repeat switch, and that is
    // what the loop below waits on.
    let fixture =
        write_multi_model_fixture(&mock.base_url, &["aaaaaa", "bbbbbb", "cccccc", "dddddd"]);

    let cmd = common::pty_command(&[], &fixture);
    let mut session = PtySession::spawn(cmd, 160, 45);
    let mut since = session.wait_for(LANDED, Duration::from_secs(15));

    session.send("hi\r");
    since = session.wait_for_since("AAAA", since, Duration::from_secs(15));
    // Wait for the ACTIVITY field to return to `idle` before sending the
    // next command -- an earlier version of this test sent `/model`
    // immediately once the reply TEXT landed on screen, which races
    // `/model`'s own keypress against the tail of this turn's own
    // finish-housekeeping (the reply's content delta streams in and is
    // drawn BEFORE `Event::TurnFinished` is fully processed --
    // `view/status.rs::tokens_label`'s own doc: the activity field shows a
    // spinner+word ladder "while active", "just idle while idle"). Not
    // `" tok"` (this file's first attempt at a settle marker): that text is
    // the STATUS LINE's own `tokens` field too (`tokens_label`, ALWAYS
    // shown, from session start, not only after a turn finishes), so it is
    // already on screen well before any turn completes and proves nothing.
    // `idle` only appears once the busy -> idle transition has actually
    // happened, which is what genuinely indicates the turn -- and every
    // event it caused -- has settled.
    since = session.wait_for_since("idle", since, Duration::from_secs(15));

    for (to, reply) in [("bbbbbb", "BBBB"), ("cccccc", "CCCC"), ("dddddd", "DDDD")] {
        session.send(&format!("/model mock/{to}\r"));
        // The bare name, not `switched model to mock/{to}` -- see the
        // partial-redraw note on the fixture above. The name alone is
        // still specific to this switch: no earlier frame can have
        // contained it, because `to` is a model this session has not used
        // yet.
        since = session.wait_for_since(to, since, Duration::from_secs(15));
        session.send("hi\r");
        since = session.wait_for_since(reply, since, Duration::from_secs(15));
        since = session.wait_for_since("idle", since, Duration::from_secs(15));
    }

    // Four decisions this session: the initial primary selection plus
    // three explicit `/model` switches -- `render_why`'s `history.len() >
    // 2` branch, proven by its own distinctive count line.
    session.send("/why\r");
    let why_from = since;
    since = session.wait_for_since(
        "routing history (4 decisions this session):",
        since,
        Duration::from_secs(10),
    );
    let _ = since;
    // Only the bytes emitted AFTER `/why` was sent. `screen()` is the whole
    // accumulated stream, and every one of these names has already crossed
    // it once (each was the active model for a turn), so asserting against
    // the full buffer would pass no matter what `/why` printed.
    let screen = session.screen();
    let why_output = &screen[why_from.min(screen.len())..];
    for model in ["aaaaaa", "bbbbbb", "cccccc", "dddddd"] {
        assert!(
            why_output.contains(model),
            "/why's history must still name every model this session actually ran on, \
             including the ones later switches replaced -- missing {model}. \
             /why output:\n{why_output}"
        );
    }
}
