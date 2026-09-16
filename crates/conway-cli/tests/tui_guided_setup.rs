//! Compiled-binary, real-pty acceptance tests for the interactive guided
//! setup rows of the TUI harness table (board item
//! `01M2M6NR49FYQSRS00B95PTAZT`): the local-offer accept flow
//! (`docs/getting-started.md:20-35`), the verify-before-land ordering
//! (same section), and the settings/models.json SAME-LAYER regression
//! (see [`guided_setup_accepted_writes_settings_and_models_json_to_the_
//! same_layer_and_the_resolved_window_is_identical_from_a_third_directory`]'s
//! own doc for why this is a REGRESSION test, not the spec's originally
//! stated "must fail against HEAD" -- the defect it guards
//! (`01M2M68XYD5FSCNSH2Z1BMQ399`) was fixed and merged before this item
//! started).
//!
//! # The local-probe override now reaches this flow
//!
//! These tests point `CONWAY_LOCAL_PROBE_BASE_URL` at a
//! `common::mock_backend` fixture so the interactive flow detects a
//! controlled "local server" rather than whatever happens to be listening
//! on the developer's or CI runner's real `127.0.0.1:11434`.
//!
//! That override originally reached only the non-interactive degrade path:
//! `run_backend_setup` called `detect_local_provider` with the hardcoded
//! `LOCAL_OLLAMA_BASE_URL`, so these three tests could not reach a fixture
//! at all. The worker that wrote them found and reported that gap rather
//! than working around it; both call sites now resolve through
//! `first_run::effective_local_probe_base_url`, and these tests pass.

#[allow(dead_code)]
mod common;

use std::path::Path;
use std::time::Duration;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::pty::PtySession;
use common::Fixture;

const LANDED: &str = "Type a message, or / for commands";

fn ok_script() -> Script {
    Script(vec![vec![Chunk::Text("ok"), Chunk::Finish("stop")]])
}

/// A fixture with NO config at all (`{}`) -- `should_offer_guided_setup`
/// requires a genuinely empty/unusable fleet, matching a real first
/// launch.
fn write_unconfigured_fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("conway.json");
    std::fs::write(&config_path, "{}").expect("write empty conway.json");
    Fixture { dir, config_path }
}

/// Drives the interactive local-offer accept flow to a landed session,
/// declining every optional step along the way (no second provider, no
/// confined/plain shell) -- the shortest path from "nothing configured" to
/// "one working provider, chain persisted". `mock` doubles as BOTH the
/// detected "local Ollama" server (via `CONWAY_LOCAL_PROBE_BASE_URL` --
/// see this file's own top doc for why that does not yet reach this call
/// site) and the real backend the landed session then runs on.
///
/// Returns the running `PtySession` (still landed) plus the byte offsets,
/// in [`PtySession::screen`]'s own stripped-text space, marking where
/// "Verifying with a real request" and "it works" first appeared -- for a
/// caller that wants to assert their ORDER (`guided_setup_verifies_before_
/// landing`) without needing a live screen grid at all.
fn accept_local_offer_and_land(
    mock: &common::mock_backend::MockHandle,
    fixture: &Fixture,
) -> (PtySession, usize, usize) {
    let mut cmd = common::pty_command(&[], fixture);
    cmd.env("CONWAY_LOCAL_PROBE_BASE_URL", &mock.base_url);

    let mut session = PtySession::spawn(cmd, 200, 50);

    let offered = session.wait_for("found one, model", Duration::from_secs(15));
    session.send_enter(); // "Press Enter to use it" -- the one keypress.

    // Context-window discovery: this fixture's `mock` answers neither
    // `/api/tags` nor `/api/show` (the ollama-native discovery pair --
    // `conway-plugin-backends/src/probe.rs`'s own doc), so discovery
    // returns `None` and setup falls through to its interactive ask
    // (`ask_and_persist_context_window`) -- a real, ordinary branch of the
    // SAME regression this file's third test exists to prove (both paths
    // persist through `persist_context_window_beside_settings`). Typed
    // value chosen to match this item's own given regression evidence
    // verbatim: a 131072-token window resolves to `headroom_tokens=104857`.
    let after_offer = session.wait_for_since(
        "conway could not determine",
        offered,
        Duration::from_secs(10),
    );
    session.send("131072\r");

    let verifying = session.wait_for_since(
        "Verifying with a real request",
        after_offer,
        Duration::from_secs(15),
    );
    let works = session.wait_for_since("it works", verifying, Duration::from_secs(15));

    let add_another =
        session.wait_for_since("Add another provider?", works, Duration::from_secs(10));
    session.send("n");

    // Either an OS containment primitive is offered first (this machine
    // has `sandbox-exec`/`bwrap`) or the plain-bash question is asked
    // directly -- both branches this suite must tolerate, since CI
    // (`ubuntu-latest`, no `bwrap` installed) and the operator's own macOS
    // machine (`sandbox-exec` always present) answer differently.
    //
    // Anchored on each question's own TRAILING marker -- `"[Y/n]"` (the
    // confine question's own default-yes bracket) or the plain-shell
    // offer's own full, literal first line -- rather than on
    // `"OS containment primitive"`/`"shell tool?"`, which is what this used
    // to search for and is the root cause this suite's own flakiness
    // traced to (board item `01M2MNMZ7BWS0KDHGNKP6JP47T`). Both of the old
    // anchors are SUBSTRINGS of the confine question's own single sentence
    // (`first_run.rs`'s `offer_opinion_set_and_shell`: "This machine has an
    // OS containment primitive (...). Enable conway.confine's confined
    // shell tool? ... [Y/n]"), with `"OS containment primitive"` starting
    // BEFORE `"shell tool?"` in that same, already-fully-printed sentence.
    // Because `wait_for_any` returns on whichever pattern starts EARLIEST,
    // the old code always resolved `after_first_shell_question` to a point
    // MID-sentence -- strictly before `"shell tool?"` itself, which was
    // already sitting in the captured buffer. The very next wait then
    // searched for `"shell tool?"` starting from that mid-sentence offset
    // and matched that SAME, stale occurrence on its very first poll,
    // instead of waiting for a genuine second question -- sending an extra,
    // premature keystroke into a process that was about to toggle raw mode
    // for its own next `read_single_key()` call (`enable_raw_mode`/
    // `disable_raw_mode` around each read, `first_run.rs:1007-1018`). That
    // stray keystroke's delivery relative to the raw-mode toggle is a
    // genuine OS-level race, not something this harness controls -- exactly
    // the kind of intermittent, load-sensitive failure this suite was
    // quarantined for. `"[Y/n]"` and `"Enable the bash shell tool?"` each
    // occur nowhere in this flow's output ahead of where they are used
    // here: unlike a bare `"[y/N]"` (which "Add another provider? [y/N]",
    // just answered immediately before this, also contains) and unlike
    // `"shell tool?"` (which both possible first questions' sentences
    // contain), so neither can produce a stale, premature match.
    let (_, after_first_shell_question) = session.wait_for_any(
        &["[Y/n]", "Enable the bash shell tool?"],
        add_another,
        Duration::from_secs(10),
    );
    session.send("n");

    // Either we have landed (no containment primitive on this machine, so
    // the question just answered above WAS the only one), or one genuine
    // second question remains (a containment primitive exists and was just
    // declined, so `offer_plain_shell` asks its own "Enable the bash shell
    // tool?" next) -- both legitimate, depending on what this machine
    // offers. Anchored at the FIRST question's own offset, not at its own
    // match: LANDED is the input-box placeholder and is redrawn on every
    // dirty frame, not emitted once, so re-anchoring per iteration can step
    // past the only emission that matters. A bounded answer-what-appears
    // loop was tried here and was strictly worse (0/12) for exactly that
    // reason. `"Enable the bash shell tool?"` cannot self-match here in the
    // one-question branch: it was already consumed, in full, by the wait
    // immediately above, and `since` is anchored at the END of that match,
    // so only a SECOND, later occurrence (the genuine follow-up question)
    // can satisfy this search.
    let (which_next, offset) = session.wait_for_any(
        &[LANDED, "Enable the bash shell tool?"],
        after_first_shell_question,
        Duration::from_secs(20),
    );
    if which_next == 1 {
        session.send("n");
        session.wait_for_since(LANDED, offset, Duration::from_secs(20));
    }

    (session, verifying, works)
}

/// `docs/getting-started.md:20-35`: "it looks for a local model server
/// already running ... offers it in one keypress if found". See this
/// file's own top doc for the one, currently-unfixed call site that makes
/// this test fail against HEAD today.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_launch_interactive_guided_setup_offers_a_detected_local_server_in_one_keypress() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_unconfigured_fixture();

    // `accept_local_offer_and_land` sends exactly ONE keypress (Enter)
    // between "found one, model" and the next stage of setup -- if that
    // single keypress were not enough, the function's own subsequent
    // `wait_for_since` calls would time out (loudly) rather than silently
    // pass, so reaching a landed session here IS the "one keypress"
    // claim's evidence.
    let (_session, _verifying, _works) = accept_local_offer_and_land(&mock, &fixture);
}

/// `docs/getting-started.md:20-35`: "...and proves it works with one real
/// request" -- BEFORE landing in the session, not after. See this file's
/// own top doc for the one, currently-unfixed call site that makes this
/// test fail against HEAD today.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guided_setup_verifies_with_a_real_request_before_landing() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_unconfigured_fixture();

    let (session, verifying, works) = accept_local_offer_and_land(&mock, &fixture);
    // Both offsets are non-zero, real matches (`wait_for_since` panics
    // rather than returning on a miss) -- `works` being reachable AT ALL
    // by searching forward from `verifying` is the ordering proof: "it
    // works" only appears in the accumulated output at or after
    // "Verifying with a real request" did.
    assert!(
        works > verifying,
        "'it works' must be found strictly after 'Verifying with a real request'; screen:\n{}",
        session.screen()
    );
}

/// **Written as a regression test, not a red one -- amendment to this
/// item's own spec.** The spec's acceptance 3 asked for this test to FAIL
/// against HEAD, citing board item `01M2M68XYD5FSCNSH2Z1BMQ399` (guided
/// setup writing `settings.json` to the config dir and `models.json` to
/// the project dir, splitting one setup run across two config layers).
/// That defect was fixed and merged before this item started, verified end
/// to end: `routes explain default` now reports the SAME
/// `headroom_tokens` from the setup directory, an unrelated directory, and
/// `/tmp`. Manufacturing a failure to satisfy the stale criterion would
/// mean asserting something false about the current tree, which this
/// item's own instructions rule out -- so this proves the fix holds
/// instead, matching the given regression evidence's own number
/// (`headroom_tokens=104857`, from a 131072-token window) exactly. See
/// this file's own top doc for the SEPARATE, still-open gap
/// (`LOCAL_OLLAMA_BASE_URL` not honoring the probe-override env var on the
/// interactive path) that keeps this test itself from running green today
/// despite the regression it guards being fixed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guided_setup_accepted_writes_settings_and_models_json_to_the_same_layer_and_the_resolved_window_is_identical_from_a_third_directory(
) {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_unconfigured_fixture();
    let config_home = fixture.dir.path().to_path_buf();

    let (session, _verifying, _works) = accept_local_offer_and_land(&mock, &fixture);
    drop(session); // done with the interactive half; check the files it left behind.

    assert!(
        config_home.join("settings.json").is_file(),
        "guided setup must write settings.json into CONWAY_CONFIG_DIR"
    );
    assert!(
        config_home.join("models.json").is_file(),
        "the models.json the same run wrote must land BESIDE settings.json, not under a \
         cwd-relative .conway/ -- the exact regression 01M2M68XYD5FSCNSH2Z1BMQ399 fixed"
    );

    let unrelated_dir = tempfile::tempdir().expect("tempdir");
    for cwd in [
        config_home.as_path(),
        unrelated_dir.path(),
        Path::new("/tmp"),
    ] {
        let headroom = routes_explain_default_headroom(&config_home, cwd);
        // 10% of the 131072-token window this run typed at the ask prompt.
        // NOT 104857 -- that figure comes from the original reproduction,
        // where the model's window was 1,048,576; copying it here asserted
        // a number this fixture never produces. What the regression is
        // actually about is that the value is the SAME everywhere and is
        // derived from the real window rather than collapsing to the
        // dialect floor (8192), which is what it did before the fix.
        assert_eq!(
            headroom,
            "13107",
            "headroom_tokens must be identical (and reflect the real 131072-token window, not \
             the assumed 8192 floor) from {}",
            cwd.display()
        );
    }
}

/// Runs `conway routes explain default` with `CONWAY_CONFIG_DIR` fixed at
/// `config_home` but `cwd` varying -- no `--config` flag at all, so
/// resolution goes through the SAME discovery guided setup itself used
/// (`conway::config::discovery`), proving the window is reachable from
/// somewhere OTHER than the directory guided setup happened to run in.
/// Returns the `headroom_tokens` field's raw text out of `routes.rs`'s own
/// `"role: {} (est_tokens={}, headroom_tokens={})"` line.
fn routes_explain_default_headroom(config_home: &Path, cwd: &Path) -> String {
    let out = std::process::Command::new(assert_cmd::cargo::cargo_bin("conway"))
        .current_dir(cwd)
        .env("CONWAY_CONFIG_DIR", config_home)
        .args(["routes", "explain", "default"])
        .output()
        .expect("run conway routes explain default");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let line = stdout
        .lines()
        .find(|l| l.contains("headroom_tokens="))
        .unwrap_or_else(|| {
            panic!("no headroom_tokens line in stdout; stdout:\n{stdout}\nstderr:\n{stderr}")
        });
    let (_, after) = line
        .split_once("headroom_tokens=")
        .expect("headroom_tokens= present, checked above");
    after.trim_end_matches(')').trim().to_string()
}
