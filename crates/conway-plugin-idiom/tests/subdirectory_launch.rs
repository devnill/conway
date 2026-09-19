//! Board item `01M2V6JHRWWEPN690C954R0PYS`: launching conway from a
//! SUBDIRECTORY of a repository must still find the repository root's
//! `AGENTS.md` / `.conway/instructions.md`, and must tell the model a real
//! working directory.
//!
//! **Why this file exists rather than another unit test in `lib.rs`.** This
//! crate's existing walk tests (`walk_up_finds_the_repo_root_dot_conway_
//! file_from_a_nested_cwd` and friends) all hand
//! [`conway_plugin_idiom::project_instructions_path`] an ABSOLUTE
//! `tempfile::tempdir()` path, and they all pass -- the walk itself is
//! correct. The shipped binary never passed an absolute path: it passed
//! `ConwayConfig::cwd`, whose schema default is the literal relative `"."`,
//! and on `"."` the walk is inert (`Path::new(".").parent()` is `Some("")`,
//! whose own `.parent()` is `None` -- two relative misses and it gives up).
//! So every test constructed a state the product never occupied, and a
//! subdirectory launch silently saw no operator instructions at all.
//!
//! These tests therefore drive the REAL composition -- `conway::config::
//! load*` producing the `cwd` that `conway-cli`'s
//! `first_party_plugins::install` hands this crate -- with the relative
//! `"."` in place, both as the schema default and as an explicitly written
//! settings value. They fail against the pre-fix tree.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use conway::config::{load_ignoring_user_config, CliOverrides, LoadOptions};
// `Plugin::instructions` is a trait method -- the fragment surface
// `conway-cli` reads through `ConwayBuilder::with_plugin`.
use conway::plugin::Plugin;

/// A repository (a real `.git` entry -- the walk only checks that
/// *something* is there) holding `AGENTS.md` at its root and an empty
/// `pkg/sub/` underneath: the exact fixture the board item reproduced at
/// the wire. Returns the tempdir (kept alive by the caller), the
/// `AGENTS.md` path, and the subdirectory to launch from.
fn repo_with_root_agents_md() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let repo = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(repo.path().join(".git")).expect("mkdir .git");
    let agents_md = repo.path().join("AGENTS.md");
    std::fs::write(&agents_md, "Repo-root standing instructions.\n").expect("write AGENTS.md");
    let nested = repo.path().join("pkg").join("sub");
    std::fs::create_dir_all(&nested).expect("mkdir pkg/sub");
    (repo, agents_md, nested)
}

/// `LoadOptions` for a launch from `cwd`, with `CONWAY_CONFIG_DIR` pointed
/// at an isolated directory and nothing else in `env` -- never the
/// developer's own real `~/.conway`.
fn launch_from(cwd: &Path, config_dir: &Path) -> LoadOptions {
    let mut env = HashMap::new();
    env.insert(
        "CONWAY_CONFIG_DIR".to_string(),
        config_dir.display().to_string(),
    );
    LoadOptions {
        cwd: cwd.to_path_buf(),
        explicit_path: None,
        env,
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    }
}

/// The defect itself, end to end: with no settings file anywhere, `cwd`
/// comes out of the five-source merge as `schema::default_cwd`'s literal
/// `"."`, and that is the value the CLI hands this crate. Resolved at load,
/// the git-root walk runs and finds the repository root's `AGENTS.md`;
/// unresolved (the pre-fix tree) the walk cannot leave `"."` and this
/// assertion fails.
#[test]
fn a_repo_root_agents_md_is_found_when_conway_is_launched_from_a_subdirectory() {
    let (_repo, agents_md, nested) = repo_with_root_agents_md();
    let config_dir = tempfile::tempdir().expect("tempdir");

    let outcome = load_ignoring_user_config(launch_from(&nested, config_dir.path()))
        .expect("an unconfigured repository must load");
    let cwd = outcome.config.cwd;

    assert_ne!(
        cwd,
        PathBuf::from("."),
        "config.cwd must be a real directory by the time a plugin receives it -- a literal `.` \
         makes every ancestor walk downstream inert"
    );
    assert_eq!(
        conway_plugin_idiom::project_instructions_path(&cwd),
        agents_md,
        "launching from a subdirectory must still read the repository root's AGENTS.md"
    );
}

/// The same for a repo-root `.conway/instructions.md`, which wins over
/// `AGENTS.md`: the board item proved BOTH files were missed from a
/// subdirectory, so the whole walk was inert, not just the fallback.
#[test]
fn a_repo_root_conway_instructions_file_is_found_when_launched_from_a_subdirectory() {
    let (repo, _agents_md, nested) = repo_with_root_agents_md();
    let config_dir = tempfile::tempdir().expect("tempdir");
    let instructions = repo.path().join(".conway").join("instructions.md");
    std::fs::create_dir_all(instructions.parent().expect("parent")).expect("mkdir .conway");
    std::fs::write(&instructions, "Project convention.\n").expect("write instructions.md");

    let outcome = load_ignoring_user_config(launch_from(&nested, config_dir.path()))
        .expect("an unconfigured repository must load");

    assert_eq!(
        conway_plugin_idiom::project_instructions_path(&outcome.config.cwd),
        instructions,
        ".conway/instructions.md at the repository root must win, from a subdirectory too"
    );
}

/// The literal relative `"."` written by hand into a project settings file
/// -- the exact spelling the schema default ships, exercised through the
/// real merge rather than only through the default -- resolves the same
/// way. A settings file that pins `cwd` to a relative value must not
/// re-break the walk.
#[test]
fn an_explicit_relative_dot_cwd_in_settings_still_finds_the_repo_root_file() {
    let (repo, agents_md, nested) = repo_with_root_agents_md();
    let config_dir = tempfile::tempdir().expect("tempdir");
    let dot_conway = repo.path().join(".conway");
    std::fs::create_dir_all(&dot_conway).expect("mkdir .conway");
    std::fs::write(dot_conway.join("settings.json"), r#"{ "cwd": "." }"#)
        .expect("write settings.json");

    let outcome = load_ignoring_user_config(launch_from(&nested, config_dir.path()))
        .expect("a settings file naming a relative cwd must load");

    assert_eq!(
        conway_plugin_idiom::project_instructions_path(&outcome.config.cwd),
        agents_md,
        "an explicitly written relative `.` must be resolved before the walk consumes it"
    );
}

/// The board item's second user-visible symptom: the environment block
/// this crate sends the model used to read `Environment: cwd .; ...`,
/// telling the model its working directory was `.` -- a plausible-looking
/// value it may reason from, and a declaration-honesty defect in its own
/// right. The fragment must now name the directory conway was actually
/// launched in.
#[test]
fn the_environment_fragment_names_a_real_working_directory_not_a_dot() {
    let (_repo, _agents_md, nested) = repo_with_root_agents_md();
    let config_dir = tempfile::tempdir().expect("tempdir");

    let outcome = load_ignoring_user_config(launch_from(&nested, config_dir.path()))
        .expect("an unconfigured repository must load");

    let plugin = conway_plugin_idiom::IdiomPlugin::new(&outcome.config.cwd);
    let environment = plugin
        .instructions()
        .into_iter()
        .find(|f| f.name == conway_plugin_idiom::ENVIRONMENT_INSTRUCTION_NAME)
        .expect("the idiom plugin must contribute an environment fragment");

    assert!(
        environment
            .text
            .contains(&format!("cwd {}", nested.display())),
        "the environment block must report the real working directory, got: {}",
        environment.text
    );
    assert!(
        !environment.text.contains("cwd .;"),
        "the environment block must never tell the model its cwd is `.`, got: {}",
        environment.text
    );
}
