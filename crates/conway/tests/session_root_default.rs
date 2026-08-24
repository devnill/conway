//! `[session].root`'s central-default resolution (board item
//! `01M0QK9GRM8HSNWRAR414TCX42`, decision `01M0QK8J757ZH6R06WYJ0PQGEM`): an
//! unconfigured root resolves to a central, project-keyed location under
//! `CONWAY_CONFIG_DIR`/`~/.conway` rather than the old
//! `<cwd>/.conway/sessions`; an explicitly-configured one keeps its old,
//! direct meaning unchanged.
//!
//! **No `LegacyProjectSessionsNotMigrated` warning any more** (decision
//! `01M0RW05G3Y81AZW96NVKTY1RV`, board item `01M0RW17KQNTQNQ4M939V0BTES`):
//! the "leave and point" resolution above still never reads, moves, or
//! deletes an old project-local `.conway/sessions` directory, but conway no
//! longer repeats a ~90-word notice about it on every run to describe a
//! one-time fact -- pre-1.0, that cost outweighed what it bought. The
//! `WarningCode` variant itself is gone (nothing outside this crate's own
//! emit site and these tests ever matched on it), so the differential
//! "warns here, not there" tests this file used to carry
//! (`empty_legacy_directory_does_not_warn`,
//! `absent_legacy_directory_does_not_warn`,
//! `explicit_root_never_warns_about_an_untouched_legacy_directory`) no
//! longer have anything to differentiate and are removed rather than left
//! pinning behavior that no longer exists; the one still-load-bearing
//! property -- an old directory survives untouched -- is proven below by
//! `legacy_nonempty_project_sessions_directory_is_left_untouched_and_produces_no_warning`,
//! now against a fixture whose content could actually detect a regression.

#[path = "support/mod.rs"]
mod support;

use std::collections::HashMap;

use conway::config::{load, CliOverrides, LoadOptions};

fn env_with_config_dir(config_dir: &std::path::Path) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert(
        "CONWAY_CONFIG_DIR".to_string(),
        config_dir.to_string_lossy().to_string(),
    );
    env
}

fn opts(cwd: std::path::PathBuf, env: HashMap<String, String>) -> LoadOptions {
    LoadOptions {
        cwd,
        explicit_path: None,
        env,
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    }
}

#[test]
fn unconfigured_root_resolves_under_conway_config_dir_project_keyed() {
    let config_dir = support::unique_temp_dir("session-default-config-dir");
    let project_dir = support::unique_temp_dir("session-default-project");

    let outcome = load(opts(project_dir.clone(), env_with_config_dir(&config_dir))).unwrap();

    let root = outcome.config.session.root.expect("root must be resolved");
    assert!(
        root.starts_with(config_dir.join("sessions")),
        "expected root under {:?}/sessions, got {root:?}",
        config_dir
    );
    assert_ne!(
        root,
        project_dir.join(".conway").join("sessions"),
        "must not silently keep the old project-local default"
    );
    // A fresh run creates no `.conway/sessions` in the project -- `load`
    // itself never creates ANY directory (it only reads and computes
    // paths); confirmed here as a structural guarantee, not an accident.
    assert!(!project_dir.join(".conway").join("sessions").exists());
}

#[test]
fn two_different_projects_get_two_different_session_roots_sharing_the_same_parent() {
    let config_dir = support::unique_temp_dir("session-default-config-dir-shared");
    let project_a = support::unique_temp_dir("session-default-project-a");
    let project_b = support::unique_temp_dir("session-default-project-b");

    let outcome_a = load(opts(project_a, env_with_config_dir(&config_dir))).unwrap();
    let outcome_b = load(opts(project_b, env_with_config_dir(&config_dir))).unwrap();

    let root_a = outcome_a.config.session.root.unwrap();
    let root_b = outcome_b.config.session.root.unwrap();
    assert_ne!(root_a, root_b);
    assert_eq!(root_a.parent(), root_b.parent());
}

#[test]
fn conway_config_dir_redirects_the_central_root_like_settings_json_and_history() {
    let project_dir = support::unique_temp_dir("session-default-redirect-project");
    let config_dir_one = support::unique_temp_dir("session-default-redirect-one");
    let config_dir_two = support::unique_temp_dir("session-default-redirect-two");

    let outcome_one = load(opts(
        project_dir.clone(),
        env_with_config_dir(&config_dir_one),
    ))
    .unwrap();
    let outcome_two = load(opts(project_dir, env_with_config_dir(&config_dir_two))).unwrap();

    assert!(outcome_one
        .config
        .session
        .root
        .unwrap()
        .starts_with(&config_dir_one));
    assert!(outcome_two
        .config
        .session
        .root
        .unwrap()
        .starts_with(&config_dir_two));
}

#[test]
fn explicit_root_passes_through_load_unresolved_exactly_as_before_this_item() {
    // `[session].root`'s explicit branch keeps its OLD, direct meaning --
    // which, before and after this item, means `config::load` carries the
    // value through UNRESOLVED (a relative `PathBuf` exactly as the
    // operator wrote it); `ConwayBuilder::build`'s own `resolve_path(&cwd,
    // root)` (unchanged by this item) is what joins it against `cwd`,
    // deferred to build time -- see `crates/conway/tests/builder.rs`'s
    // `build_constructs_default_jsonl_store_when_none_injected`, which
    // proves that end-to-end resolution still opens a real store at the
    // right place. This test's own job is narrower: prove `load` itself
    // does NOT reach for the central-default machinery (project-key
    // encoding, `CONWAY_CONFIG_DIR`) the moment a value is present, which
    // is the one behavior this item actually changed.
    let config_dir = support::unique_temp_dir("session-default-explicit-config-dir");
    let project_dir = support::unique_temp_dir("session-default-explicit-project");
    std::fs::create_dir_all(project_dir.join(".conway")).unwrap();
    std::fs::write(
        project_dir.join(".conway").join("settings.json"),
        r#"{"session": {"root": "custom-sessions"}}"#,
    )
    .unwrap();

    let outcome = load(opts(project_dir, env_with_config_dir(&config_dir))).unwrap();

    assert_eq!(
        outcome.config.session.root.unwrap(),
        std::path::PathBuf::from("custom-sessions"),
    );
}

#[test]
fn explicit_absolute_root_is_used_verbatim() {
    let config_dir = support::unique_temp_dir("session-default-explicit-abs-config-dir");
    let project_dir = support::unique_temp_dir("session-default-explicit-abs-project");
    let elsewhere = support::unique_temp_dir("session-default-explicit-abs-elsewhere");
    std::fs::create_dir_all(project_dir.join(".conway")).unwrap();
    std::fs::write(
        project_dir.join(".conway").join("settings.json"),
        format!(
            r#"{{"session": {{"root": {:?}}}}}"#,
            elsewhere.to_string_lossy()
        ),
    )
    .unwrap();

    let outcome = load(opts(project_dir, env_with_config_dir(&config_dir))).unwrap();

    assert_eq!(outcome.config.session.root.unwrap(), elsewhere);
}

/// The one property that made removing the warning safe (decision
/// `01M0RW05G3Y81AZW96NVKTY1RV`): a pre-existing, non-empty, project-local
/// `.conway/sessions` is resolved AROUND, never read, moved, or deleted --
/// AND, per acceptance, produces no warning at all any more, not even once.
/// The fixture carries real, distinguishable session content (not an empty
/// directory) so a regression that started reading or mutating it, or that
/// resurrected the old warning, would actually be caught here rather than
/// this test vacuously passing against a fixture with nothing to disturb.
#[test]
fn legacy_nonempty_project_sessions_directory_is_left_untouched_and_produces_no_warning() {
    let config_dir = support::unique_temp_dir("session-default-legacy-config-dir");
    let project_dir = support::unique_temp_dir("session-default-legacy-project");
    let legacy_dir = project_dir.join(".conway").join("sessions");
    std::fs::create_dir_all(&legacy_dir).unwrap();
    let legacy_session = legacy_dir.join("01ABCDEFGH.jsonl");
    std::fs::write(&legacy_session, "{\"kind\":\"header\"}\n").unwrap();
    let bytes_before = std::fs::read(&legacy_session).unwrap();

    let outcome = load(opts(project_dir.clone(), env_with_config_dir(&config_dir))).unwrap();

    assert!(
        outcome.warnings.is_empty(),
        "a legacy project-local .conway/sessions directory must produce no warning \
         at all any more, got: {:?}",
        outcome.warnings
    );
    // "Leave and point" (unchanged by removing the warning): the new
    // central default is what's actually used --
    let root = outcome.config.session.root.unwrap();
    assert_ne!(root, legacy_dir);
    // -- and conway left it alone: the test reads the file back here only
    // to prove conway neither mutated nor deleted it.
    let bytes_after = std::fs::read(&legacy_session).unwrap();
    assert_eq!(
        bytes_before, bytes_after,
        "the legacy session file must be byte-identical"
    );
    assert!(
        legacy_dir.exists(),
        "the legacy directory must not be deleted"
    );
}
