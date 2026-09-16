//! Compiled-binary, real-pty acceptance tests for `/model`/`/role`'s two
//! rows in the TUI harness table (board item `01M2M6NR49FYQSRS00B95PTAZT`),
//! both tracing to `01M24ZJ9ABPP0DGVAA2PS3XVDD` (`docs/vision/
//! STATE-OF-THE-UNION.md`'s own §4 F1: "the second command typed --
//! `/model` -- told the operator no models were configured while the
//! session was answering on one, and `/role` failed the same way").
//!
//! # `/model`'s own reproduction, and why it is EXPECTED to fail today
//!
//! `01M24ZJ9`'s own reproduction is specific: a backend supplied through
//! `CONWAY_BACKENDS__*` env vars plus a `--model` pin, and NO role chain --
//! because the environment cannot create one at all
//! (`crates/conway/src/config/merge.rs`'s `env_to_value`: the only
//! per-role env var is `CONWAY_ROLES__<ALIAS>__HEADROOM_TOKENS`, never
//! `chain`). `AppState::configured_models` (`tui/app/defaults.rs::
//! refresh_default_entries`) is, by its own doc, built ENTIRELY from
//! `[roles].*.chain` entries -- it has no path back to a CLI `--model` pin
//! at all. In this exact scenario that source is empty BY CONSTRUCTION (no
//! chain exists anywhere to read), so bare `/model` is deterministically
//! unable to list anything today, regardless of the session actually
//! running on the pinned model underneath it -- the precise defect §4 F1
//! reports. This test reproduces that reproduction verbatim and asserts
//! the DOCUMENTED-correct outcome (the pinned model appears), which is
//! therefore expected to fail against HEAD until a `tui/` change teaches
//! `configured_models` about a CLI-pinned model too -- see this item's own
//! completion report for the full disclosure; this is not a defect in the
//! harness.
//!
//! # `/role`'s own claim needs no such reproduction
//!
//! Unlike `/model`, `/role <alias>` REQUIRES an alias
//! (`docs/interactive.md:392`; `tui/commands.rs::parse`'s `"/role" =>` arm
//! calls `parse_one_arg`, which has no bare-argument special case the way
//! `/model`'s own parse arm does). "Behaves as documented" for the bare
//! form is exactly that usage refusal, reachable with any ordinary
//! configured session -- no env-only reproduction needed.

mod common;

use std::time::Duration;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::pty::PtySession;
use common::Fixture;

const LANDED: &str = "Type a message, or / for commands";

fn ok_script() -> Script {
    Script(vec![vec![Chunk::Text("ok"), Chunk::Finish("stop")]])
}

/// A fixture with an EMPTY `conway.json` (`{}`) -- config comes entirely
/// from the env vars/CLI flags the test itself adds on top, matching
/// `01M24ZJ9`'s own reproduction (no role chain anywhere, a backend named
/// only through `CONWAY_BACKENDS__*`). Still carries a `.conway/
/// models.json` entry for the pinned `backend/model` pair -- the same
/// requirement `common::write_fixture_with`'s own doc explains (the
/// router/capability index is a wholly separate store from a bare
/// `backends.*` declaration).
fn write_env_only_fixture(backend_id: &str, model: &str) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("conway.json");
    std::fs::write(&config_path, "{}").expect("write empty conway.json");

    let models_dir = dir.path().join(".conway");
    std::fs::create_dir_all(&models_dir).expect("create .conway dir");
    let models_json = serde_json::json!({
        "models": {
            format!("{backend_id}/{model}"): {
                "max_context_tokens": 128_000,
                "tool_calling": "streaming_validated",
                "reasoning": false,
                "reliability_tier": "verified",
            }
        }
    });
    std::fs::write(
        models_dir.join("models.json"),
        serde_json::to_vec(&models_json).expect("serialize models.json"),
    )
    .expect("write models.json");

    Fixture { dir, config_path }
}

/// Traces to `01M24ZJ9ABPP0DGVAA2PS3XVDD` -- see this file's own top doc
/// for why this is a currently-expected-red regression test, not a defect
/// in the harness itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bare_model_lists_the_configured_model_rather_than_claiming_none_exist() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_env_only_fixture("mock", &mock.model);

    let model_flag = format!("mock/{}", mock.model);
    let mut cmd = common::pty_command(&["--model", &model_flag], &fixture);
    cmd.env("CONWAY_BACKENDS__MOCK__KIND", "openai-compat");
    cmd.env("CONWAY_BACKENDS__MOCK__DIALECT", "openai");
    cmd.env("CONWAY_BACKENDS__MOCK__BASE_URL", &mock.base_url);

    let mut session = PtySession::spawn(cmd, 160, 45);
    session.wait_for(LANDED, Duration::from_secs(15));

    session.send("/model\r");
    // The DOCUMENTED-correct outcome: the pinned model actually shows up
    // in the listing bare `/model` opens. See this file's own top doc for
    // why this is expected to time out (loudly, naming the captured
    // screen) against HEAD today.
    session.wait_for(&mock.model, Duration::from_secs(10));

    let screen = session.screen();
    assert!(
        !screen.contains("no models are configured"),
        "the operator IS running on a real, working model (--model {model_flag}); bare /model \
         must not claim otherwise. Screen:\n{screen}"
    );
}

/// Traces to `01M24ZJ9ABPP0DGVAA2PS3XVDD` -- the `/role` half of §4 F1.
/// Unlike `/model` above, this needs no env-only reproduction: `/role`
/// with no alias has always required one (`docs/interactive.md:392`), and
/// this test just proves the TUI actually shows that usage line rather
/// than silently doing nothing or panicking.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bare_role_shows_the_documented_usage_line() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = common::write_fixture(&mock, 10);

    let cmd = common::pty_command(&[], &fixture);
    let mut session = PtySession::spawn(cmd, 160, 45);
    session.wait_for(LANDED, Duration::from_secs(15));

    session.send("/role\r");
    session.wait_for("usage: /role <alias>", Duration::from_secs(10));
}
