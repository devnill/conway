//! Compiled-binary, real-pty acceptance test for the "first launch,
//! nothing configured, non-interactive" row of the TUI harness table
//! (board item `01M2M6NR49FYQSRS00B95PTAZT`) -- reproduced 2026-09-16.
//!
//! `tests/first_run.rs`'s own
//! `first_run_no_usable_provider_prints_the_guided_setup_message_not_the_old_hard_error`
//! already covers this exact scenario end to end, through `common::
//! run_conway`'s piped `Stdio`. That is a REAL non-interactive run (a pipe
//! is never a terminal), but it cannot distinguish "non-interactive
//! because nothing looks like a terminal" from "non-interactive because
//! `-p` forces it regardless" -- `main.rs`'s own `interactive` computation
//! documents that `-p`/`--print` is ALWAYS treated as non-interactive
//! "regardless of whether stdin happens to be a real terminal", and a
//! piped-`Stdio` test can never exercise the "regardless" half of that
//! claim, because its stdin was never a terminal to begin with. This test
//! is the same claim, driven through a REAL pty (`common::pty`) with `-p`
//! still passed, so it actually proves the `-p` override -- not merely the
//! absence-of-a-terminal fallback `tests/first_run.rs` already covers.

mod common;

use std::time::Duration;

use common::pty::PtySession;
use common::Fixture;
use conway_cli::first_run::GUIDED_SETUP_MARKER;

/// A fixture with `"backends": {}` and an empty default-role chain -- the
/// canonical `NoBackendsConfigured` case, byte-for-byte the same shape
/// `tests/first_run.rs::write_no_backends_fixture` builds (each `tests/
/// *.rs` integration file compiles independently, so this is a deliberate
/// sibling copy, not a shared helper -- see that file's own doc for why an
/// EMPTY chain, not merely an absent backend, is what is needed here).
fn write_no_backends_fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("conway.json");
    std::fs::write(
        &config_path,
        serde_json::json!({
            "default_role": "default",
            "backends": {},
            "roles": { "default": { "chain": [] } },
        })
        .to_string(),
    )
    .expect("write conway.json");
    Fixture { dir, config_path }
}

#[test]
fn first_launch_nothing_configured_non_interactive_refuses_with_the_named_error_under_a_real_pty() {
    let fixture = write_no_backends_fixture();
    let cmd = common::pty_command(&["-p", "hi"], &fixture);
    let mut session = PtySession::spawn(cmd, 120, 40);

    // `-p` forces the non-interactive path even though stdin/stdout ARE a
    // real terminal here (unlike every other suite in this directory) --
    // exactly the claim `main.rs`'s own doc makes and this test exists to
    // prove. The message is the SAME one `tests/first_run.rs` already
    // asserts against the piped-`Stdio` harness; what is new here is that
    // it still appears under a real pty.
    session.wait_for(GUIDED_SETUP_MARKER, Duration::from_secs(10));
    session.wait_for("settings.json", Duration::from_secs(5));
    session.wait_for("ANTHROPIC_API_KEY", Duration::from_secs(5));
    session.wait_for("\"kind\": \"anthropic\"", Duration::from_secs(5));

    let status = session.wait_for_exit(Duration::from_secs(10));
    assert!(
        !status.success(),
        "no provider is configured yet -- this run cannot succeed; screen:\n{}",
        session.screen()
    );

    let screen = session.screen();
    assert!(
        !screen.contains("no backends configured: add a"),
        "the OLD hard error must be replaced, not merely joined; got:\n{screen}"
    );
}
