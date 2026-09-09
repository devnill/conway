//! `--model`/`--session`/`--resume`/
//! `--fork-from` were all accepted by the CLI parser and never read by the
//! interactive TUI's own session construction (`tui::app::App::new` built
//! its `SessionSpec` from `role`/`keep_alive`/`tools` only) -- a
//! renderer-only gap: the identical `--model` flag is genuinely wired
//! in one-shot mode (`oneshot::resolve_session`, covered by
//! `tests/oneshot.rs`'s `model_flag_pins_and_overrides_role_chain`).
//!
//! This suite drives the TUI's OWN construction path directly --
//! [`conway_cli::tui::app::App::session_spec`], the exact associated
//! function `App::new`'s flag-free path calls to build the fresh-session
//! `SessionSpec` it passes to `Conway::new_session` -- rather than
//! `oneshot::resolve_session` (which is private to `oneshot.rs`, and is a
//! different code path besides). `model_flag_pins_the_session_spec` below
//! was confirmed to fail before this item's fix: `App::new`'s inline
//! `SessionSpec { .. }` construction never set `model` at all, so
//! `spec.model` was always `None` regardless of `--model`.
//!
//! **Board item `01M1YS4FMJH004D1Y619MTBY7A` superseded this suite's own
//! former claim that `--resume`/`--fork-from` are a decided TUI non-goal.**
//! They are now real, wired continuity flags at TUI startup (alongside a
//! new `--continue`) -- see `tui::app::startup::App::resolve_handle`, the
//! function that superseded `session_spec`'s old blanket refusal of all
//! three. `--session` alone did NOT graduate and stays refused (see its
//! own doc in `cli.rs`); `session_spec` itself keeps guarding exactly that
//! one flag, which is why `--session`'s own rejection is still tested here
//! synchronously, with no live `Conway`. The former `--resume`/
//! `--fork-from` rejection tests are replaced by their opposite: proof
//! that `App::new` genuinely ATTEMPTS them now (an unknown target still
//! errors, since the fixture below never creates a real session for
//! `--resume 01ARZ...` to find -- but it errors via `Conway::resume_with`'s
//! own "no such session" now, not via a blanket startup refusal). The
//! SUCCESS half of `--resume` (a real session, backfilled) is proven in
//! `conway_cli::tui::app::startup`'s own `#[cfg(test)]` suite
//! (`resolve_handle_resume_backfills_history_into_the_transcript`), which
//! -- unlike this file -- can reach `App`'s private fields and this
//! crate's own `fixtures::echo_conway_and_store` to build a real,
//! pre-populated session to resume.
//!
//! `App::new` itself is driven here too now (see the previous paragraph),
//! needing a live `Conway` -- `conway::test_support::build_conway_with_
//! echo_backend` over a fresh `FakeStore`, the same shape this file's
//! sibling suites already use.

mod common;

use std::str::FromStr;

use conway::test_support::{base_config, build_conway_with_echo_backend};
use conway::ModelRef;
use conway_cli::cli::{Cli, OneShotPermissionMode, OutputFormat};
use conway_cli::exit::ExitCode;
use conway_cli::tui::app::App;
use conway_testkit::FakeStore;
use std::sync::Arc;

use common::mock_backend::{MockBackend, Script};
use common::{run_conway, write_fixture};

fn minimal_cli() -> Cli {
    Cli {
        print: None,
        output_format: OutputFormat::Text,
        allowed_tools: Vec::new(),
        deny_tools: Vec::new(),
        permission_mode: OneShotPermissionMode::Allowlist,
        default_permission_mode: None,
        role_override: None,
        model: None,
        agent: None,
        system_prompt: None,
        append_system_prompt: None,
        max_turns: None,
        max_tokens: None,
        max_seconds: None,
        output_schema: None,
        session: None,
        resume: None,
        fork_from: None,
        continue_session: false,
        config: None,
        cwd: None,
        root: None,
        verbose: 0,
        command: None,
    }
}

/// `--model backend/model` must reach `SessionSpec.model` -- the exact
/// field `crates/conway/src/session_handle.rs`'s `SessionSpec` has carried
/// since then, which `App::new`'s own stale doc comment used to (wrongly)
/// claim did not exist.
#[test]
fn model_flag_pins_the_session_spec() {
    let mut cli = minimal_cli();
    cli.model = Some("anthropic/claude-x".to_string());

    let spec = App::session_spec(&cli).expect("a well-formed --model must build a spec");

    assert_eq!(
        spec.model,
        Some(ModelRef::from_str("anthropic/claude-x").expect("valid model ref")),
        "the TUI's own SessionSpec must carry the --model pin, not silently drop it"
    );
}

/// The flag-free default: `SessionSpec.model` stays `None`, exactly as
/// before this item -- `--model`'s absence must not be confused with a pin.
#[test]
fn no_model_flag_leaves_the_pin_unset() {
    let spec = App::session_spec(&minimal_cli()).expect("no --model must still build a spec");
    assert_eq!(spec.model, None);
}

/// A malformed `--model` must fail the SAME way in both modes -- this item's
/// own binding requirement, verified by comparing the TUI's in-process
/// error (`App::session_spec`) against one-shot's real, compiled-binary
/// error (mirroring `tests/oneshot.rs`'s `exit_2_bad_model_ref`). Both call
/// through `crate::model_pin::parse_model_pin`, the single parser this item
/// introduced specifically so the two could never drift apart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_model_fails_identically_in_both_modes() {
    let mut cli = minimal_cli();
    cli.model = Some("not-a-valid-ref".to_string());
    let tui_err = App::session_spec(&cli)
        .expect_err("a malformed --model must be a usage error, not build a spec")
        .to_string();
    assert_eq!(
        ExitCode::from_error(&conway::FacadeError::Config {
            path: None,
            message: tui_err.clone(),
        }),
        ExitCode::Usage,
        "a malformed --model must classify as a usage error (exit 2) in the TUI too"
    );

    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    let out = run_conway(&["-p", "hi", "--model", "not-a-valid-ref"], &fixture);
    assert_eq!(
        out.status.code(),
        Some(2),
        "one-shot's own exit code for a malformed --model"
    );
    let oneshot_err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        mock.requests().is_empty(),
        "a malformed --model must fail before ever dialing a backend, in either mode"
    );

    let needle = "--model not-a-valid-ref:";
    assert!(
        tui_err.contains(needle),
        "TUI error must name the malformed flag/value the same way one-shot's does, got: \
         {tui_err:?}"
    );
    assert!(
        oneshot_err.contains(needle),
        "one-shot's stderr must name the malformed flag/value, got: {oneshot_err:?}"
    );
}

/// `--session` alone did NOT graduate alongside `--resume`/`--fork-from`/
/// `--continue` (board item `01M1YS4FMJH004D1Y619MTBY7A`) -- see
/// `App::session_spec`'s own doc comment for why: the TUI has no "create
/// with this exact id" use case a script has. `App::session_spec` keeps
/// guarding exactly this one flag, so this stays a synchronous,
/// no-live-`Conway` test.
#[test]
fn session_flag_alone_is_still_rejected_by_session_spec() {
    let mut cli = minimal_cli();
    cli.session = Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string());

    let err = match App::session_spec(&cli) {
        Ok(_) => panic!("--session must still be refused, not silently ignored"),
        Err(e) => e,
    };
    assert_eq!(
        ExitCode::from_error(&err),
        ExitCode::Usage,
        "--session: rejection must classify as a usage error (exit 2)"
    );
    let text = err.to_string();
    assert!(
        text.contains("--session"),
        "the refusal must name the flag it refused, got: {text:?}"
    );
}

/// **Superseded pairing (board item `01M1YS4FMJH004D1Y619MTBY7A`):** this
/// suite used to assert `--resume`/`--fork-from` were flatly refused at TUI
/// startup, the same as `--session` just above. They are not any more --
/// `App::resolve_handle` (`tui::app::startup`) now genuinely ATTEMPTS
/// them. This fixture's `FakeStore` starts empty, so a hand-typed ULID
/// that names no real session still ends in `Err` -- but now via
/// `Conway::resume_with`'s own "no such session" (wrapped as a usage
/// error, matching `oneshot::resolve_session`'s identical convention for
/// the identical failure), not via a blanket "not supported" refusal. The
/// two are told apart below by the error TEXT, not merely the exit code:
/// the old refusal always contained the literal string `"not supported"`;
/// this one never does.
///
/// The SUCCESS half -- a real, pre-existing session, backfilled into the
/// transcript -- is proven in `conway_cli::tui::app::startup`'s own
/// `#[cfg(test)]` suite (`resolve_handle_resume_backfills_history_into_
/// the_transcript`), which can reach `App`'s private fields and this
/// crate's own `fixtures::echo_conway_and_store`; this integration suite
/// cannot reach either.
#[tokio::test]
async fn resume_with_an_unknown_id_still_errors_but_not_as_unsupported() {
    let conway = build_conway_with_echo_backend(base_config(), Arc::new(FakeStore::new()));
    let mut cli = minimal_cli();
    cli.resume = Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string());

    let err = match App::new(&cli, &conway, &[]).await {
        Ok(_) => {
            panic!("an empty FakeStore has no session named 01ARZ3NDEKTSV4RRFFQ69G5FAV to resume")
        }
        Err(e) => e,
    };
    assert_eq!(
        ExitCode::from_error(&err),
        ExitCode::Usage,
        "an unresolvable --resume target is still a usage error, matching one-shot's own \
         `resolve_session` convention"
    );
    let text = err.to_string();
    assert!(
        !text.contains("not supported"),
        "must fail because the session is unknown, not because --resume is unsupported: \
         {text:?}"
    );
}
