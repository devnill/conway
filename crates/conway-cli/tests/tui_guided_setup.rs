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
//! # A confirmed gap this file's tests are written AGAINST, not around
//!
//! `first_run::run_backend_setup`'s own local-offer detection
//! (`first_run.rs:1604`, `detect_local_provider(env, LOCAL_OLLAMA_BASE_URL)`)
//! calls [`conway_cli::first_run::LOCAL_OLLAMA_BASE_URL`] DIRECTLY -- the
//! hardcoded `"http://127.0.0.1:11434/v1"` constant, never
//! [`conway_cli::first_run::LOCAL_PROBE_BASE_URL_ENV`]
//! (`CONWAY_LOCAL_PROBE_BASE_URL`). That env var IS read, but only by
//! [`conway_cli::first_run::non_interactive_guidance`] (the `-p`/piped
//! degrade path) -- confirmed by reading `first_run.rs` directly, not
//! assumed. So every test in this file that needs the INTERACTIVE flow to
//! detect a fixture-hosted "local" server is, as of this item, unable to
//! reach it: the real `run_backend_setup` always probes the real
//! `127.0.0.1:11434`, which this suite must not depend on (a developer or
//! CI runner's own local state, exactly the flakiness
//! `LOCAL_PROBE_BASE_URL_ENV` exists elsewhere to remove -- this module's
//! own doc on that constant).
//!
//! These three tests are written the way they are INTENDED to work --
//! pointing `CONWAY_LOCAL_PROBE_BASE_URL` at a `common::mock_backend`
//! fixture, exactly as this item's own brief instructed ("that env seam is
//! how your guided-setup tests point at a fixture") -- and are expected to
//! fail at their very first `wait_for` (never finding "found one, model")
//! until `run_backend_setup`'s local-offer call site reads that same env
//! override the way `non_interactive_guidance` already does. That is a
//! `crates/conway-cli/src/first_run.rs` change, outside this item's owned
//! files; see this item's own completion report for the full disclosure.

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
    let (which, after_first_shell_question) = session.wait_for_any(
        &["OS containment primitive", "Enable the bash shell tool?"],
        add_another,
        Duration::from_secs(10),
    );
    session.send("n");
    let after_shell = if which == 0 {
        let (_, offset) = session.wait_for_any(
            &["Enable the bash shell tool?"],
            after_first_shell_question,
            Duration::from_secs(10),
        );
        session.send("n");
        offset
    } else {
        after_first_shell_question
    };

    session.wait_for_since(LANDED, after_shell, Duration::from_secs(15));

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
        assert_eq!(
            headroom,
            "104857",
            "headroom_tokens must be identical (and reflect the real 131072-token window, not \
             the assumed floor) from {}",
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
