//! Acceptance coverage for board item `01M11XWB4T8ZADNDB4M8R482MA` (the
//! settings menu's providers section) that drives the REAL COMPILED
//! `conway` binary rather than `App`'s internal methods directly.
//!
//! **Why this file cannot drive the TUI's own `/settings` screen
//! interactively.** `App`'s fields (`state`, `conway`, ...) are private to
//! `crates/conway-cli/src/tui/app.rs` and its own submodules; a `tests/*.rs`
//! file compiles as a SEPARATE crate linking against `conway_cli` as an
//! external dependency, so it can only reach `pub` items -- exactly the
//! same constraint `tests/tui_model_pin.rs`'s own module doc names for why
//! it drives `App::session_spec` (a pure, `pub` associated function) rather
//! than introspecting a constructed `App`'s own state. Interactively
//! driving `/settings`' key handling additionally needs a real terminal
//! (`crossterm` raw mode); no pty-driving dependency exists in this crate
//! and none is added here (C-04). **The white-box coverage of `App`'s own
//! add/remove/status-refresh methods and the settings menu's rendered rows
//! lives in this crate's own `#[cfg(test)]` modules instead** --
//! `src/tui/app/provider_manage.rs`, `src/tui/app/provider_status.rs`, and
//! `src/tui/view/settings.rs` -- which, being compiled AS PART OF the same
//! crate, can see everything this file cannot.
//!
//! **What this file proves instead, against the real binary:** the ONE
//! writer `App::apply_add_provider_choice`/`apply_add_provider_credential`/
//! `apply_remove_provider` actually call in production
//! (`conway::config::set_backend_provider`) produces a config the real
//! compiled binary can genuinely start a session from -- not merely a
//! config this crate's own in-process fixtures accept. A round trip:
//! remove the only backend a role's chain names (the real binary must then
//! fail to start a session), then add it back (the real binary must work
//! again) -- proving both directions of the write reach the real
//! production entry point, GP-14's own "nothing may claim to be reached
//! that isn't" applied to this item's write path specifically.

mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{command, write_fixture};

fn ok_script() -> Script {
    Script(vec![vec![Chunk::Text("ok"), Chunk::Finish("stop")]])
}

/// The round trip itself. Uses `fixture.config_path` (a project-scoped
/// file, for ordinary test-harness convenience -- `write_fixture`'s own
/// template) rather than the user-scoped `settings.json`
/// `App`'s production callers target; `set_backend_provider` itself is
/// agnostic to which file it is pointed at (its own doc: the USER-SCOPE
/// discipline is enforced by the CALLER's choice of path, not by the
/// function), so this remains a faithful proof of the writer itself.
// `flavor = "multi_thread"`: this test calls the SYNCHRONOUS, blocking
// `Command::output()` three times against the real subprocess, which would
// otherwise starve the single worker thread `MockBackend`'s own listener
// needs polled to answer each request -- the exact deadlock this crate's
// own `oneshot.rs` suite already avoids the identical way, for the
// identical reason (see that file's own `#[tokio::test(flavor =
// "multi_thread", worker_threads = 2)]` attributes).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_backend_provider_add_and_remove_round_trip_against_the_real_compiled_binary() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 4);

    // Baseline: the generated fixture already runs a one-shot turn
    // successfully -- establishes the harness itself is sound before this
    // test's own writes are ever attempted.
    let out = command(&["-p", "hi"], &fixture)
        .output()
        .expect("run conway binary");
    assert!(
        out.status.success(),
        "baseline fixture must work before this test touches it: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // ---- Remove: the SAME writer `App::apply_remove_provider` calls. ----
    let wrote = conway::config::set_backend_provider(&fixture.config_path, "mock", "{}", false)
        .expect("remove must succeed against a well-formed file");
    assert!(wrote, "the remove must actually perform a write");

    let text_after_remove =
        std::fs::read_to_string(&fixture.config_path).expect("config file must still exist");
    assert!(
        !text_after_remove.contains("\"mock\""),
        "the mock backend must actually be gone from the file: {text_after_remove}"
    );
    // Acceptance 7's own words, checked here at the real-file level too:
    // everything else the template wrote survives.
    assert!(
        text_after_remove.contains("\"default\""),
        "{text_after_remove}"
    );
    assert!(
        text_after_remove.contains("\"coder\""),
        "{text_after_remove}"
    );

    let out = command(&["-p", "hi"], &fixture)
        .output()
        .expect("run conway binary");
    assert!(
        !out.status.success(),
        "removing the only backend a role's chain names must break the REAL binary's own \
         startup -- proving the write genuinely took effect, not merely that a file changed: \
         stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // ---- Add it back: the SAME writer `App::apply_add_provider_choice`/
    // `apply_add_provider_credential` call, producing exactly the entry a
    // hand-edit would (acceptance 5's own words). ----
    let entry_json = serde_json::json!({
        "kind": "openai-compat",
        "dialect": "openai",
        "base_url": mock.base_url,
    })
    .to_string();
    let wrote =
        conway::config::set_backend_provider(&fixture.config_path, "mock", &entry_json, true)
            .expect("add must succeed against a well-formed file");
    assert!(wrote, "the add must actually perform a write");

    let out = command(&["-p", "hi"], &fixture)
        .output()
        .expect("run conway binary");
    assert!(
        out.status.success(),
        "re-adding the provider via set_backend_provider must make the REAL binary work again, \
         with no restart of anything beyond this one process invocation: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
