//! The divergence guard: harness gap review 2026-09-01, finding 11.
//!
//! Before this test existed, every default setting was written twice --
//! once as a Rust `impl Default` in `crates/conway/src/config/schema.rs`,
//! and once again as a hand-maintained `serde_json::json!` literal in
//! `crates/conway/src/config/merge.rs::default_document`, the ONLY one of
//! the two a bare `settings.json` actually loads through. Nothing enforced
//! that the two agreed: `LimitsConfig::default()` carried five fields
//! (`max_tool_calls` among them) while the document's own `[limits]` table
//! had four; `RoutingSection::default()` carried `headroom_fraction`
//! alongside `default_headroom_tokens` while the document had only the
//! latter. Commit `a0f560d` (`max_steps`, `40` -> `0`) had to edit both
//! files by hand, and its own commit message says so -- proof this was a
//! standing, silent-drift-shaped footgun, not a hypothetical one.
//!
//! `crate::config::merge::default_document` is now
//! `serde_json::to_value(ConwayConfig::baseline())` -- see that function's
//! own doc comment and [`conway::config::schema::ConwayConfig::baseline`]'s.
//! There is exactly one place left to name a section's default value; this
//! file is what makes that property load-bearing rather than merely
//! asserted in a doc comment.
//!
//! Two proofs, at two different seams:
//!
//! 1. [`empty_settings_file_loads_to_exactly_conwayconfig_baseline`] drives
//!    the real production entry point (`conway::config::load`, the same
//!    function every `settings.json` on a real machine goes through) with a
//!    settings file that names nothing at all, and asserts the resulting
//!    `ConwayConfig` equals `ConwayConfig::baseline()` field by field
//!    (patched at the one field `load` itself deliberately resolves --
//!    `[session].root`, see that test's own comment).
//! 2. [`every_top_level_section_in_the_bare_document_round_trips_its_own_default`]
//!    reads the built-in layer back out through
//!    `conway::config::merged_document` -- the SAME public seam
//!    `conway-cli` itself uses to read a raw layered document, never the
//!    crate-private `default_document` directly -- and, per section,
//!    deserializes it into that section's own type and compares it against
//!    that type's own `Default::default()`.
//!
//! **Why this can no longer go red the way the bug it documents did:**
//! `default_document` and `ConwayConfig::baseline()` are now the same
//! value by construction (`serde_json::to_value` of the same struct), so
//! changing a `Default` impl changes what THIS test compares against too --
//! there is no second, independently-authored value left for either
//! assertion here to disagree with. That is the point: before this item,
//! changing `LimitsConfig::default().max_parallel_tools` alone (leaving
//! `merge.rs`'s old `json!` literal untouched) silently changed nothing a
//! real `settings.json` load would ever see, and no test in this crate
//! caught it -- the drift this file is named for. A reviewer wanting to see
//! that failure mode reproduced can `git stash` this item's `merge.rs`/
//! `schema.rs` changes, apply just this test file plus a one-line edit to
//! `LimitsConfig::default().max_parallel_tools`, and watch assertion 2 fail
//! naming exactly that field -- then restore the real changes and watch it
//! pass. See this crate's own completion report for that exact procedure,
//! run once against this diff.

#[path = "support/mod.rs"]
mod support;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::config::{discovery, load, merged_document, CliOverrides, LoadOptions};

/// `LoadOptions` naming no source beyond the built-in one: an isolated
/// `CONWAY_CONFIG_DIR` (so the user layer resolves to "absent," not
/// whatever `settings.json` happens to sit on the machine running this
/// suite -- see `support::isolated_env`'s own doc), an `explicit_path`
/// pointing at a file that does not exist (so no project layer is read
/// either -- `read_json_layer` treats "not found" as "layer absent," not an
/// error), and no env/CLI overrides.
fn bare_options() -> LoadOptions {
    let dir = support::unique_temp_dir("config-defaults-single-source-bare");
    LoadOptions {
        explicit_path: Some(dir.join("does-not-exist.json")),
        cwd: dir,
        env: support::isolated_env(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    }
}

#[test]
fn empty_settings_file_loads_to_exactly_conwayconfig_baseline() {
    let dir = support::unique_temp_dir("config-defaults-single-source-load");
    let settings_path = dir.join("settings.json");
    std::fs::write(&settings_path, "{}").expect("write an empty settings file");

    let env = support::isolated_env();
    let outcome = load(LoadOptions {
        explicit_path: Some(settings_path),
        cwd: dir.clone(),
        env: env.clone(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    })
    .expect("a settings file naming nothing must still load");

    // `[session].root` is the one field `load` itself resolves, at load
    // time, using `cwd`/`env` it alone has in hand (`SessionConfig`'s own
    // doc comment; `merge::load_impl`'s "central-default resolution" step).
    // `ConwayConfig::baseline()` deliberately leaves it at `None` --
    // "nothing configured" IS `None` -- so a direct, unpatched comparison
    // against `baseline()` would fail on this one field for a reason
    // unrelated to what this test proves. Patched here, explicitly, rather
    // than silently excluded from the comparison.
    let mut expected = ConwayConfig::baseline();
    expected.session.root = Some(discovery::session_root(&dir, None, &env));

    assert_eq!(
        outcome.config, expected,
        "loading a settings file with nothing in it through the production loader must \
         resolve to exactly ConwayConfig::baseline(), modulo the one field `load` itself \
         resolves (`session.root`)"
    );
}

#[test]
fn every_top_level_section_in_the_bare_document_round_trips_its_own_default() {
    let options = bare_options();
    let doc = merged_document(&options)
        .expect("a document naming no layer beyond the built-in default must build");

    macro_rules! assert_section_matches_default {
        ($key:literal, $ty:ty) => {{
            let value = doc.get($key).cloned().unwrap_or_else(|| {
                panic!("the bare document must carry a top-level `{}` key", $key)
            });
            let parsed: $ty = serde_json::from_value(value).unwrap_or_else(|e| {
                panic!("`{}` must deserialize into {}: {e}", $key, stringify!($ty))
            });
            assert_eq!(
                parsed,
                <$ty>::default(),
                "`{}`'s value in the built-in default document no longer matches \
                 {}::default() -- {}'s own `impl Default` in schema.rs is the ONLY place \
                 this value should be edited; `default_document` derives from it \
                 automatically and cannot independently disagree any more",
                $key,
                stringify!($ty),
                stringify!($ty),
            );
        }};
    }

    assert_section_matches_default!("session", SessionConfig);
    assert_section_matches_default!("limits", LimitsConfig);
    assert_section_matches_default!("permissions", PermissionsConfig);
    assert_section_matches_default!("routing", RoutingSection);
    assert_section_matches_default!("health", HealthSection);
    assert_section_matches_default!("agents", AgentsConfig);
    assert_section_matches_default!("models", ModelsConfig);
    assert_section_matches_default!("tools", ToolsConfig);
    assert_section_matches_default!("plugins", PluginsConfig);
    assert_section_matches_default!("hooks", HooksConfig);

    // `backends`/`roles` aren't `Default`-shaped top-level sections the
    // macro above can drive (`backends` is an empty map with no type of
    // its own to compare against; `roles` deliberately carries the one
    // baked-in floor role rather than being empty) -- proven directly
    // instead, matching `ConwayConfig::baseline`'s own construction.
    assert_eq!(
        doc.get("backends").and_then(|v| v.as_object()).map(|m| m.len()),
        Some(0),
        "the built-in default must name no backend"
    );
    let roles = doc
        .get("roles")
        .and_then(|v| v.as_object())
        .expect("the bare document must carry a `roles` table");
    assert_eq!(
        roles.len(),
        1,
        "the built-in default must name exactly the one baked-in baseline role"
    );
    let baseline_entry: RoleEntry = serde_json::from_value(
        roles
            .get("default")
            .expect("the baked-in role must be named \"default\"")
            .clone(),
    )
    .expect("roles.default must deserialize into RoleEntry");
    assert_eq!(
        baseline_entry,
        RoleEntry::default(),
        "the baked-in baseline role must carry no content beyond RoleEntry::default() -- \
         it exists only so an unconfigured default_role still validates"
    );
}
