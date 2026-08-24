//! Pin for the `[p]` field-editor surface (`Mode::EditingPattern`,
//! `EditingPatternState`/`PatternField`, `offer_editing_pattern`/
//! `cancel_editing_pattern`/`submit_editing_pattern`, the
//! `Action::GrantPermissionRule` variant, `handle_editing_pattern_key`, and
//! `draw_editing_pattern`). A source-level grep in the spirit of
//! `intent_confirm.rs`'s modal surface pin: the compile already guarantees
//! the variant exists, this pins the TEXT of the identifiers and the
//! footer hint so a future re-introduction (or accidental rename) trips
//! loudly at review time.

/// Reads one TUI source file from the crate under test.
fn tui_src(rel: &str) -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/tui/");
    std::fs::read_to_string(format!("{path}{rel}")).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

#[test]
fn the_editing_pattern_surface_is_present() {
    let state = tui_src("state.rs");
    assert!(
        state.contains("pub struct PatternField"),
        "PatternField struct must exist"
    );
    assert!(
        state.contains("pub struct EditingPatternState"),
        "EditingPatternState struct must exist"
    );
    assert!(
        state.contains("pub fn offer_editing_pattern"),
        "offer_editing_pattern lifecycle method must exist"
    );
    assert!(
        state.contains("pub fn cancel_editing_pattern"),
        "cancel_editing_pattern lifecycle method must exist"
    );
    assert!(
        state.contains("pub fn submit_editing_pattern"),
        "submit_editing_pattern lifecycle method must exist"
    );

    let modal = tui_src("state/modal.rs");
    assert!(
        modal.contains("EditingPattern(EditingPatternState)"),
        "Mode::EditingPattern variant must exist"
    );

    let input = tui_src("input.rs");
    assert!(
        input.contains("GrantPermissionRule(conway::Rule, PermissionScope)"),
        "Action::GrantPermissionRule variant must exist"
    );
    assert!(
        input.contains("fn handle_editing_pattern_key"),
        "the editor's key router must exist"
    );

    let view = tui_src("view/mod.rs");
    assert!(
        view.contains("fn draw_editing_pattern"),
        "the editor renderer must exist"
    );
    assert!(
        view.contains("[space] pin/wildcard  [enter] grant  [s] scope  [esc] cancel"),
        "the editor footer must name the field choices in the spec's exact text"
    );
}
