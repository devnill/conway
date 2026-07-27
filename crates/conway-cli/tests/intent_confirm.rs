//! C2 pin: the NL intent confirmation card surface
//! (`Mode::IntentConfirm`, `draw_intent_confirm`, `IntentChoice`, and the
//! `Action::IntentConfirm`/`commands::execute_intent_confirm` identifiers)
//! must stay present -- a source-level grep, in the spirit of
//! `ask_modal.rs`'s modal surface pin: the compile already guarantees the
//! variant exists, this pins the TEXT of the identifiers and the footer
//! hint so a future re-introduction (or accidental rename) trips loudly at
//! review time.

/// Reads one TUI source file from the crate under test.
fn tui_src(rel: &str) -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/tui/");
    std::fs::read_to_string(format!("{path}{rel}"))
        .unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

#[test]
fn the_intent_confirm_surface_is_present() {
    let state = tui_src("state.rs");
    assert!(state.contains("Mode::IntentConfirm"), "Mode::IntentConfirm must exist");
    assert!(
        state.contains("pub struct IntentConfirm"),
        "IntentConfirm state struct must exist"
    );
    assert!(
        state.contains("pub enum IntentChoice"),
        "IntentChoice enum must exist"
    );
    assert!(
        state.contains("pub fn offer_intent_confirm"),
        "offer_intent_confirm lifecycle method must exist"
    );
    assert!(
        state.contains("pub fn close_intent_confirm"),
        "close_intent_confirm lifecycle method must exist"
    );
    assert!(
        state.contains("pub fn begin_intent_confirm_edit"),
        "begin_intent_confirm_edit lifecycle method must exist"
    );

    let input = tui_src("input.rs");
    assert!(
        input.contains("IntentConfirm(IntentChoice)"),
        "Action::IntentConfirm variant must exist"
    );
    assert!(
        input.contains("fn handle_intent_confirm_key"),
        "the card's key router must exist"
    );

    let commands = tui_src("commands.rs");
    assert!(
        commands.contains("pub async fn execute_intent_confirm"),
        "the choice dispatcher must exist"
    );
    assert!(
        commands.contains("async fn classify_agent_intent"),
        "Host::classify_agent_intent seam must exist"
    );

    let view = tui_src("view/mod.rs");
    assert!(
        view.contains("fn draw_intent_confirm"),
        "the card renderer must exist"
    );
    assert!(
        view.contains("[enter] confirm  [e] edit  [esc] manual"),
        "the card footer must name the three choices in the spec's exact text"
    );
}