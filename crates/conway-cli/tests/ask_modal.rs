//! B5 removal pin: the retired pre-modal `/ask` machinery
//! (`Entry::EphemeralAsk`, `push_ephemeral_ask`/`resolve_ephemeral_ask`,
//! the `AskResult`/`ask_tx` channel, and the transcript's
//! `[ephemeral ask]`/`[ephemeral reply]` rendering arms) must not creep
//! back -- the modal (`Mode::AskModal` + `commands::apply_ask_fate`) is the
//! only `/ask` UI surface. A source-level grep, in the spirit of
//! `cli_surface.rs`'s manifest checks: the compile already guarantees the
//! variant is gone, this pins the TEXT of the retired identifiers so a
//! future re-introduction trips loudly at review time.

/// Reads one TUI source file from the crate under test.
fn tui_src(rel: &str) -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/tui/");
    std::fs::read_to_string(format!("{path}{rel}")).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

#[test]
fn no_ephemeral_ask_entry_or_channel_machinery_remains() {
    // Identifiers of the retired design. Note: "run_ask" is checked as
    // "run_ask(" and the channel fields with a leading space, to avoid
    // matching the legitimate `run_modal_ask`/`modal_ask_tx`/
    // `modal_ask_rx` of the modal design.
    const RETIRED: &[&str] = &[
        "EphemeralAsk",
        "push_ephemeral_ask",
        "resolve_ephemeral_ask",
        "AskResult",
        " ask_tx",
        " ask_rx",
        "run_ask(",
        "ephemeral_ask_lines",
        "[ephemeral ask]",
        "[ephemeral reply]",
    ];
    for rel in [
        "app.rs",
        "state.rs",
        "input.rs",
        "commands.rs",
        "view/transcript.rs",
        "view/mod.rs",
    ] {
        let src = tui_src(rel);
        for ident in RETIRED {
            assert!(
                !src.contains(ident),
                "retired pre-modal /ask machinery `{ident}` found in tui/{rel}"
            );
        }
    }
}

#[test]
fn the_modal_surface_is_present() {
    // The positive half of the pin: the modal and its three fates exist.
    let state = tui_src("state.rs");
    assert!(state.contains("AskModal"), "Mode::AskModal must exist");
    assert!(state.contains("AskFate"), "AskFate must exist");
    let commands = tui_src("commands.rs");
    assert!(
        commands.contains("apply_ask_fate"),
        "the fate dispatcher must exist"
    );
    let view = tui_src("view/mod.rs");
    assert!(
        view.contains("draw_ask_modal"),
        "the modal renderer must exist"
    );
    assert!(
        view.contains("[p] pull in  [f] fork  [esc] discard"),
        "the modal footer must name the three fates"
    );
}
