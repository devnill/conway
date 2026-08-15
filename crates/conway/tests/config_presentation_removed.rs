//! Board item 01KZVYYWZ85D1SYMCSRRZ7RAM3 (Stage 2a): `TuiSection`/
//! `ThemeConfig`/`ThemeStyleConfig`/`StatusLineConfig` no longer live in
//! `conway`'s config schema at all -- moved to `conway-cli`
//! (`crates/conway-cli/src/tui/config.rs`), the one reader that renders
//! them.
//!
//! This file covers the FACADE half of the item's two-part verification
//! anchor: `crates/conway/tests/architecture_invariants.rs`'s
//! `t7_facade_has_no_presentation_types` proves no ratatui-shaped type is
//! reachable from the schema; this file proves what an embedder that is
//! NOT `conway-cli` (the recorded choice for that reader -- see
//! ACCEPTANCE bullet 3) actually observes when it hands `conway::config::
//! load` a `settings.json` that still carries a `[tui]` block: the load
//! SUCCEEDS (not a hard error -- the other sanctioned choice), the rest of
//! the document is honored, and a `ConfigWarning { code:
//! PresentationConfigIgnored, .. }` says so, rather than the block
//! quietly vanishing with no trace at all (explicitly the worst option per
//! the item's own framing).
//!
//! `crates/conway-cli/src/tui/app/startup.rs`'s
//! `a_real_settings_json_with_a_full_theme_block_reaches_a_rendered_session`
//! is the OTHER half of the anchor: the same shape of settings.json,
//! driven through the real CLI, reaching a rendered TUI session.

#[path = "support/mod.rs"]
mod support;

use conway::config::{load, CliOverrides, LoadOptions, WarningCode};

fn write_settings(dir: &std::path::Path, body: serde_json::Value) -> std::path::PathBuf {
    let path = dir.join("settings.json");
    std::fs::write(&path, body.to_string()).expect("write settings.json");
    path
}

/// The core acceptance case: a `[tui]` block that a real user would write
/// (a theme override plus a status-line field list) still loads
/// successfully through the bare facade -- it is not the CLI's job alone
/// to keep this working; `ConwayBuilder::from_config`/`discover` (which
/// every `conway-cli` dispatch target shares) call this exact function.
#[test]
fn a_tui_section_loads_successfully_and_is_reported_as_ignored() {
    let dir = support::unique_temp_dir("tui-section-ignored");
    let path = write_settings(
        &dir,
        serde_json::json!({
            "default_role": "coder",
            "roles": { "coder": { "chain": [] } },
            "tui": {
                "theme": { "user": { "fg": "cyan" } },
                "status_line": { "fields": ["session", "hint"] }
            }
        }),
    );

    let outcome = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(path),
        env: support::isolated_env(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .expect("a settings.json with a [tui] block must still load through the facade");

    // Not silently dropped: a warning names exactly what happened.
    assert!(
        outcome
            .warnings
            .iter()
            .any(|w| w.code == WarningCode::PresentationConfigIgnored),
        "expected a PresentationConfigIgnored warning; got warnings: {:?}",
        outcome.warnings
    );
    let warning = outcome
        .warnings
        .iter()
        .find(|w| w.code == WarningCode::PresentationConfigIgnored)
        .expect("just asserted present");
    assert!(
        warning.message.contains("tui"),
        "the warning message should name [tui] so an operator can act on it: {}",
        warning.message
    );

    // The rest of the document loaded normally -- [tui] being present and
    // ignored must not degrade anything else.
    assert_eq!(outcome.config.default_role.as_str(), "coder");

    // Compile-time enforced already (no `.tui` field exists on
    // `ConwayConfig` at all -- this file would not build otherwise), but
    // reinforced at runtime too: the serialized config carries no trace of
    // the ignored section.
    let value = serde_json::to_value(&outcome.config).expect("serialize");
    assert!(
        value.get("tui").is_none(),
        "the loaded ConwayConfig must not carry a `tui` key anywhere: {value}"
    );
}

/// The negative control: an ordinary settings.json with no `[tui]` key at
/// all produces no such warning -- the warning is conditioned on `[tui]`
/// actually being present, not fired unconditionally.
#[test]
fn no_tui_section_means_no_presentation_config_warning() {
    let dir = support::unique_temp_dir("tui-section-absent");
    let path = write_settings(
        &dir,
        serde_json::json!({
            "default_role": "coder",
            "roles": { "coder": { "chain": [] } }
        }),
    );

    let outcome = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(path),
        env: support::isolated_env(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .expect("load must succeed");

    assert!(
        outcome
            .warnings
            .iter()
            .all(|w| w.code != WarningCode::PresentationConfigIgnored),
        "no [tui] key means no PresentationConfigIgnored warning; got: {:?}",
        outcome.warnings
    );
}

/// The env-var path also counts as "a `[tui]` section is present" --
/// `CONWAY_TUI__*` is documented, reachable config, not a lesser channel
/// than the file-based one.
#[test]
fn a_tui_env_var_also_triggers_the_ignored_warning() {
    let dir = support::unique_temp_dir("tui-section-env-only");
    let path = write_settings(
        &dir,
        serde_json::json!({
            "default_role": "coder",
            "roles": { "coder": { "chain": [] } }
        }),
    );

    let mut env = support::isolated_env();
    env.insert("CONWAY_TUI__HISTORY_SIZE".to_string(), "1000".to_string());

    let outcome = load(LoadOptions {
        cwd: dir,
        explicit_path: Some(path),
        env,
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .expect("load must succeed");

    assert!(
        outcome
            .warnings
            .iter()
            .any(|w| w.code == WarningCode::PresentationConfigIgnored),
        "a CONWAY_TUI__* env var must be treated the same as a [tui] key in the file: {:?}",
        outcome.warnings
    );
}

/// [`conway::config::merged_document`] is the escape hatch a caller that
/// DOES understand `[tui]` (`conway-cli`) uses to read it back out --
/// proven here directly against the facade, independent of `conway-cli`'s
/// own tests, since this function lives in `conway`.
#[test]
fn merged_document_still_carries_the_raw_tui_value_for_a_caller_that_wants_it() {
    let dir = support::unique_temp_dir("tui-section-merged-document");
    let path = write_settings(
        &dir,
        serde_json::json!({
            "default_role": "coder",
            "roles": { "coder": { "chain": [] } },
            "tui": { "status_line": { "fields": ["session"] } }
        }),
    );

    let options = LoadOptions {
        cwd: dir,
        explicit_path: Some(path),
        env: support::isolated_env(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    };

    let merged = conway::config::merged_document(&options).expect("merged_document must succeed");
    assert_eq!(
        merged["tui"]["status_line"]["fields"],
        serde_json::json!(["session"]),
        "merged_document must still carry [tui]'s raw value even though `load` strips it \
         before ConwayConfig's own deserialize: {merged}"
    );
}
