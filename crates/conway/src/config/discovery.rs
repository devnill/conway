//! Filesystem/XDG discovery of config files. No parsing, no env-var
//! `CONWAY_*` mapping (that lives in `merge.rs`) — just "which paths exist."

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Walks from `start` up to the filesystem root looking for
/// `<dir>/.conway/conway.json`, returning the *nearest* match (i.e. `start`
/// itself is checked first).
pub fn discover(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join(".conway").join("conway.json");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// The user-scoped config path for `conway`: `$XDG_CONFIG_HOME/conway/conway.json`
/// when `XDG_CONFIG_HOME` is set (and non-empty) in `env`, otherwise the
/// home-directory default `~/.conway/conway.json` — mirroring the
/// project-scoped `.conway/conway.json` convention so a user's global config
/// lives in `.conway/` under `$HOME` just as a project's does under its root.
///
/// Takes an explicit `env` map (rather than reading `std::env` directly) so
/// callers can inject it via `LoadOptions.env` and keep precedence tests
/// parallel-safe.
pub fn xdg_config_path(env: &HashMap<String, String>) -> Option<PathBuf> {
    if let Some(xdg) = env.get("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(Path::new(xdg).join("conway").join("conway.json"));
        }
    }
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".conway").join("conway.json"))
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
        fs::write(root_conf.join("conway.json"), "").unwrap();

        let nested = tmp.join("a").join("b").join("c");
        fs::create_dir_all(&nested).unwrap();
        let nested_conf_dir = tmp.join("a").join("b").join(".conway");
        fs::create_dir_all(&nested_conf_dir).unwrap();
        fs::write(nested_conf_dir.join("conway.json"), "").unwrap();

        let found = discover(&nested).unwrap();
        assert_eq!(found, nested_conf_dir.join("conway.json"));
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
        assert_eq!(path, PathBuf::from("/custom/xdg/conway/conway.json"));
    }

    #[test]
    fn xdg_config_path_falls_back_to_home_dot_conway_when_unset() {
        let env = HashMap::new();
        // Home-directory default `~/.conway/conway.json` — just assert it
        // resolves to *something* ending in the expected suffix.
        let path = xdg_config_path(&env);
        if let Some(path) = path {
            assert!(path.ends_with(".conway/conway.json"));
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
