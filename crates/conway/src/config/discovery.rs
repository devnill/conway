//! Filesystem/XDG discovery of config files. No parsing, no env-var
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

/// The user-scoped config path for `conway`: `$XDG_CONFIG_HOME/conway/settings.json`
/// when `XDG_CONFIG_HOME` is set (and non-empty) in `env`, otherwise the
/// home-directory default `~/.conway/settings.json` — mirroring the
/// project-scoped `.conway/settings.json` convention so a user's global config
/// lives in `.conway/` under `$HOME` just as a project's does under its root.
///
/// Takes an explicit `env` map (rather than reading `std::env` directly) so
/// callers can inject it via `LoadOptions.env` and keep precedence tests
/// parallel-safe.
pub fn xdg_config_path(env: &HashMap<String, String>) -> Option<PathBuf> {
    if let Some(xdg) = env.get("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(Path::new(xdg).join("conway").join("settings.json"));
        }
    }
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".conway").join("settings.json"))
}

/// The TUI's persisted input-history file path (T8): alongside the
/// user-scoped global config -- i.e. the same directory
/// [`xdg_config_path`] resolves `settings.json` into, just with the
/// filename `history` instead. Deliberately NOT the project-scoped
/// `.conway/` directory `discover` looks in: history follows the user
/// across every project, not the checkout. `conway-cli` has no direct
/// `directories` dependency of its own (C-04: no new dependencies), so it
/// reaches this resolution through the facade the same way it already
/// reaches `xdg_config_path`'s directory choice.
pub fn history_file_path(env: &HashMap<String, String>) -> Option<PathBuf> {
    xdg_config_path(env).and_then(|settings| settings.parent().map(|dir| dir.join("history")))
}

/// V2: the persisted permission-rules file, resolved project-first then
/// global — the same precedence `discover`/`xdg_config_path` already
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
    if let Some(global) = xdg_config_path(env).and_then(|s| s.parent().map(|d| d.join("permissions.json"))) {
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
    fn xdg_config_path_honors_env_var() {
        let mut env = HashMap::new();
        env.insert("XDG_CONFIG_HOME".to_string(), "/custom/xdg".to_string());
        let path = xdg_config_path(&env).unwrap();
        assert_eq!(path, PathBuf::from("/custom/xdg/conway/settings.json"));
    }

    #[test]
    fn history_file_path_sits_alongside_the_resolved_settings_json() {
        let mut env = HashMap::new();
        env.insert("XDG_CONFIG_HOME".to_string(), "/custom/xdg".to_string());
        let history = history_file_path(&env).unwrap();
        assert_eq!(history, PathBuf::from("/custom/xdg/conway/history"));
    }

    #[test]
    fn xdg_config_path_falls_back_to_home_dot_conway_when_unset() {
        let env = HashMap::new();
        // Home-directory default `~/.conway/settings.json` — just assert it
        // resolves to *something* ending in the expected suffix.
        let path = xdg_config_path(&env);
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
