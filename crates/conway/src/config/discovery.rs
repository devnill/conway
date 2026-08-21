//! Filesystem/user config discovery of config files. No parsing, no env-var
//! `CONWAY_*` mapping (that lives in `merge.rs`) — just "which paths exist."

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Walks from `start` up to the filesystem root looking for
/// `<dir>/.conway/settings.json`, returning the *nearest* match (i.e. `start`
/// itself is checked first).
pub fn discover(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join(".conway").join("settings.json");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// The user-scoped config path for `conway`: `~/.conway/settings.json`,
/// or `$CONWAY_CONFIG_DIR/settings.json` when `CONWAY_CONFIG_DIR` is set (and
/// non-empty) in `env`.
///
/// **`~/.conway/` unconditionally, and no `CONWAY_CONFIG_DIR` branch** (INTENT.md
/// §7b, "familiarity is the on-ramp"). Two reasons, and they agree: a
/// dot-directory in the home folder is immediately apparent to someone who does
/// not already know a desktop standard, and it matches the PROJECT layer, which
/// is already `.conway/` (`discover`, above). One story — `.conway/` here,
/// `~/.conway/` there.
///
/// `CONWAY_CONFIG_DIR` names conway's config directory *directly*, so
/// `settings.json` sits at its root rather than under a `conway/` subdirectory
/// the way the user config convention required. It mirrors `CLAUDE_CONFIG_DIR` so a
/// switcher recognises it, and it is what every test uses to stay hermetic —
/// see `crates/conway/tests/config_isolation_guard.rs` for the ambient-read bug
/// that isolation exists to prevent.
///
/// Takes an explicit `env` map (rather than reading `std::env` directly) so
/// callers can inject it via `LoadOptions.env` and keep precedence tests
/// parallel-safe.
pub fn user_config_path(env: &HashMap<String, String>) -> Option<PathBuf> {
    if let Some(dir) = env.get("CONWAY_CONFIG_DIR") {
        if !dir.is_empty() {
            return Some(Path::new(dir).join("settings.json"));
        }
    }
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".conway").join("settings.json"))
}

/// The TUI's persisted input-history file path (T8): alongside the
/// user-scoped global config -- i.e. the same directory
/// [`user_config_path`] resolves `settings.json` into, just with the
/// filename `history` instead. Deliberately NOT the project-scoped
/// `.conway/` directory `discover` looks in: history follows the user
/// across every project, not the checkout. `conway-cli` has no direct
/// `directories` dependency of its own (no new dependencies), so it
/// reaches this resolution through the facade the same way it already
/// reaches `user_config_path`'s directory choice.
pub fn history_file_path(env: &HashMap<String, String>) -> Option<PathBuf> {
    user_config_path(env).and_then(|settings| settings.parent().map(|dir| dir.join("history")))
}

/// V2: the persisted permission-rules file, resolved project-first then
/// global — the same precedence `discover`/`user_config_path` already
/// establish for `settings.json`.
///
/// Project-first is deliberate: a grant like "allow `cargo test`" is
/// almost always about *this* checkout, and a project-scoped file can be
/// reviewed in a diff alongside the code it authorizes. The global file is
/// the fallback for grants that genuinely follow the operator.
///
/// Returns both candidates in precedence order so the caller can load and
/// merge them; a missing file at either level is not an error.
pub fn permission_file_paths(cwd: &std::path::Path, env: &HashMap<String, String>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    // Project scope: alongside the nearest `.conway/settings.json`, or the
    // cwd's own `.conway/` if no ancestor config exists yet.
    if let Some(project) = discover(cwd) {
        if let Some(dir) = project.parent() {
            paths.push(dir.join("permissions.json"));
        }
    } else {
        paths.push(cwd.join(".conway").join("permissions.json"));
    }
    // Global scope, alongside the resolved global settings.
    if let Some(global) =
        user_config_path(env).and_then(|s| s.parent().map(|d| d.join("permissions.json")))
    {
        if !paths.contains(&global) {
            paths.push(global);
        }
    }
    paths
}

/// Declarative provider profiles: the user-supplied `.conway/profiles.toml`
/// file path(s), resolved project-first then global — the identical
/// layering [`permission_file_paths`] already establishes, reused here
/// rather than invented anew (this module's own precedent). Both candidates
/// are returned in precedence order (:
/// copied verbatim onto every `[backends.<id>]` entry's `BackendBuildContext
/// ::profile_file_paths` -- this module still owns discovering WHICH files
/// exist; parsing/merging them is `conway_plugin_backends::profile::
/// ProfileStore::merge_file`'s concern now, a crate this one does not
/// depend on). That function treats a missing file at either level as a
/// no-op, not an error, so the caller can always attempt both without
/// checking existence first.
///
/// Project-first mirrors `permission_file_paths`'s own reasoning: a
/// provider profile override (a local llama.cpp build's actual behavior, a
/// vendor's tweaked dialect) is usually about *this* checkout's toolchain,
/// and a project-scoped file can be reviewed in a diff alongside the code
/// it configures. The global file is the fallback for a profile that
/// genuinely follows the operator across projects.
pub fn provider_profile_file_paths(
    cwd: &std::path::Path,
    env: &HashMap<String, String>,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(project) = discover(cwd) {
        if let Some(dir) = project.parent() {
            paths.push(dir.join("profiles.toml"));
        }
    } else {
        paths.push(cwd.join(".conway").join("profiles.toml"));
    }
    if let Some(global) =
        user_config_path(env).and_then(|s| s.parent().map(|d| d.join("profiles.toml")))
    {
        if !paths.contains(&global) {
            paths.push(global);
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn discover_finds_nearest_ancestor() {
        let tmp = tempfile_dir();
        let root_conf = tmp.join(".conway");
        fs::create_dir_all(&root_conf).unwrap();
        fs::write(root_conf.join("settings.json"), "").unwrap();

        let nested = tmp.join("a").join("b").join("c");
        fs::create_dir_all(&nested).unwrap();
        let nested_conf_dir = tmp.join("a").join("b").join(".conway");
        fs::create_dir_all(&nested_conf_dir).unwrap();
        fs::write(nested_conf_dir.join("settings.json"), "").unwrap();

        let found = discover(&nested).unwrap();
        assert_eq!(found, nested_conf_dir.join("settings.json"));
    }

    #[test]
    fn discover_returns_none_when_absent() {
        let tmp = tempfile_dir();
        let nested = tmp.join("x").join("y");
        fs::create_dir_all(&nested).unwrap();
        assert!(discover(&nested).is_none());
    }

    #[test]
    fn provider_profile_file_paths_is_project_scoped_alongside_discovered_settings() {
        let tmp = tempfile_dir();
        let conf_dir = tmp.join(".conway");
        fs::create_dir_all(&conf_dir).unwrap();
        fs::write(conf_dir.join("settings.json"), "").unwrap();

        let mut env = HashMap::new();
        env.insert("CONWAY_CONFIG_DIR".to_string(), "/custom/config_dir".to_string());
        let paths = provider_profile_file_paths(&tmp, &env);
        assert_eq!(paths[0], conf_dir.join("profiles.toml"));
        assert_eq!(paths[1], PathBuf::from("/custom/config_dir/profiles.toml"));
    }

    #[test]
    fn provider_profile_file_paths_falls_back_to_cwd_dot_conway_when_undiscovered() {
        let tmp = tempfile_dir();
        let nested = tmp.join("x").join("y");
        fs::create_dir_all(&nested).unwrap();
        let env = HashMap::new();
        let paths = provider_profile_file_paths(&nested, &env);
        assert_eq!(paths[0], nested.join(".conway").join("profiles.toml"));
    }

    #[test]
    fn user_config_path_honors_env_var() {
        let mut env = HashMap::new();
        env.insert("CONWAY_CONFIG_DIR".to_string(), "/custom/config_dir".to_string());
        let path = user_config_path(&env).unwrap();
        assert_eq!(path, PathBuf::from("/custom/config_dir/settings.json"));
    }

    #[test]
    fn history_file_path_sits_alongside_the_resolved_settings_json() {
        let mut env = HashMap::new();
        env.insert("CONWAY_CONFIG_DIR".to_string(), "/custom/config_dir".to_string());
        let history = history_file_path(&env).unwrap();
        assert_eq!(history, PathBuf::from("/custom/config_dir/history"));
    }

    #[test]
    fn user_config_path_falls_back_to_home_dot_conway_when_unset() {
        let env = HashMap::new();
        // Home-directory default `~/.conway/settings.json` — just assert it
        // resolves to *something* ending in the expected suffix.
        let path = user_config_path(&env);
        if let Some(path) = path {
            assert!(path.ends_with(".conway/settings.json"));
        }
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "conway-discovery-test-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn unique_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }
}
