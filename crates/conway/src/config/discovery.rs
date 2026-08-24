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

/// Lexically normalizes `path`: collapses `.`/`..` components without
/// touching the filesystem (never resolves a symlink, never checks
/// existence). Every other function in this module is pure/no-I/O for the
/// same reason this one is -- a caller building a project directory by
/// joining an arbitrary fragment onto an already-absolute base (`config::
/// merge::load_impl` does exactly this for `[session].root`'s central
/// default, joining `options.cwd` -- itself sometimes literally `"."`'s own
/// resolution against `std::env::current_dir()`) wants one clean,
/// deterministic spelling before that path becomes a directory NAME
/// component ([`encode_project_key`]) or gets compared against another
/// path for equality.
pub fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Encodes an absolute project directory into a single, filesystem-safe,
/// human-readable path component -- the project key `session_root`'s
/// central-default branch names its subdirectory with.
///
/// **Deliberately the same scheme Claude Code already uses**
/// (`~/.claude/projects/<encoded-path>/`; this very machine has
/// `~/.claude/projects/-Users-dan-code-conway/`), not a hash: `/` (the
/// platform path separator; `\` too on Windows, where `std::path::
/// is_separator` also matches it) becomes `-`, and `:` (a Windows
/// drive-letter separator; otherwise inert) becomes `_`, every other
/// character passed through unchanged. An operator who `ls`s the sessions
/// root sees their own project paths, not opaque digests -- an unreadable
/// name is worse than a long one (this item's own framing).
///
/// **The three sharp edges this item named, answered:**
/// - Two checkouts of the same repository at different paths get
///   DIFFERENT keys. Correct: they are two genuinely separate working
///   trees, each with its own session history, exactly as running `conway`
///   from two different directories already produced two disjoint
///   `.conway/sessions` stores before this item.
/// - A moved or renamed project gets a NEW key; the old key's directory is
///   left in place, undiscovered automatically. This function does not
///   solve that -- no persistent, rename-surviving project identity exists
///   anywhere in this crate today, and inventing one (a marker file, a
///   registry) is a materially bigger change than "where does a fresh
///   project's sessions default to," the question this item answers. A
///   human-readable key at least makes the abandoned directory
///   recognizable by name if someone goes looking (unlike a hash), which
///   is why this function chose readability over collision-hardening.
/// - Non-filesystem-safe characters: the only two this function knows of
///   across the platforms this crate targets are the path separator itself
///   (a component boundary, and the whole reason encoding is needed at
///   all) and `:` (Windows). A path containing invalid UTF-8 degrades
///   through `Path::to_string_lossy`'s replacement character rather than
///   erroring -- consistent with every other place this crate turns a
///   `Path` into a `String` for display or storage.
///
/// Assumes `path` is already absolute and lexically normalized
/// ([`normalize_lexically`]) -- this function does no normalization of its
/// own, so a caller comparing keys across two different unnormalized
/// spellings of the same directory would see them diverge.
pub fn encode_project_key(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| {
            if std::path::is_separator(c) {
                '-'
            } else if c == ':' {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// The effective `[session].root` directory: `configured`'s value if the
/// operator set one (the field's OLD, direct meaning -- unchanged by this
/// item, resolved against `project_dir` if relative), else the central,
/// project-keyed default under [`user_config_path`]'s own directory (so
/// `CONWAY_CONFIG_DIR` redirects this exactly as it redirects
/// `settings.json`/`history` -- the same directory, just a `sessions/
/// <project-key>/` subtree instead of a bare file).
///
/// Falls back to the OLD, pre-this-item default (`<project_dir>/.conway/
/// sessions`) only if [`user_config_path`] itself returns `None` -- no home
/// directory discoverable AND `CONWAY_CONFIG_DIR` unset, an extreme edge
/// case this function refuses to hard-fail over.
///
/// `project_dir` should already be absolute and normalized
/// ([`normalize_lexically`]) -- callers resolving `[session].root` at
/// `config::load` time have exactly that in `LoadOptions.cwd`.
pub fn session_root(
    project_dir: &Path,
    configured: Option<&Path>,
    env: &HashMap<String, String>,
) -> PathBuf {
    let project_dir = normalize_lexically(project_dir);
    if let Some(root) = configured {
        return if root.is_absolute() {
            root.to_path_buf()
        } else {
            project_dir.join(root)
        };
    }
    match user_config_path(env).and_then(|p| p.parent().map(Path::to_path_buf)) {
        Some(config_dir) => config_dir
            .join("sessions")
            .join(encode_project_key(&project_dir)),
        None => project_dir.join(".conway").join("sessions"),
    }
}

/// The legacy, project-local `.conway/sessions` directory
/// [`session_root`]'s central-default branch stops defaulting to. Exists so
/// `config::merge::load_impl` can check whether one is already there
/// (non-empty) and warn -- see [`crate::config::WarningCode::
/// LegacyProjectSessionsNotMigrated`]'s own doc for the "leave and point"
/// route this exists to support: this function only ever names the path,
/// never reads, moves, or deletes it.
pub fn legacy_project_sessions_dir(project_dir: &Path) -> PathBuf {
    normalize_lexically(project_dir)
        .join(".conway")
        .join("sessions")
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
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            "/custom/config_dir".to_string(),
        );
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
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            "/custom/config_dir".to_string(),
        );
        let path = user_config_path(&env).unwrap();
        assert_eq!(path, PathBuf::from("/custom/config_dir/settings.json"));
    }

    #[test]
    fn history_file_path_sits_alongside_the_resolved_settings_json() {
        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            "/custom/config_dir".to_string(),
        );
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

    #[test]
    fn normalize_lexically_collapses_dot_and_dot_dot() {
        let normalized = normalize_lexically(Path::new("/a/./b/../c"));
        assert_eq!(normalized, PathBuf::from("/a/c"));
    }

    #[test]
    fn normalize_lexically_leaves_an_already_clean_absolute_path_unchanged() {
        let normalized = normalize_lexically(Path::new("/a/b/c"));
        assert_eq!(normalized, PathBuf::from("/a/b/c"));
    }

    #[test]
    fn encode_project_key_replaces_path_separators_with_dashes() {
        assert_eq!(
            encode_project_key(Path::new("/Users/dan/code/conway")),
            "-Users-dan-code-conway"
        );
    }

    #[test]
    fn encode_project_key_replaces_colon_with_underscore() {
        // The Windows drive-letter case -- inert on every other platform,
        // since `:` is not a path separator anywhere this crate targets.
        assert_eq!(
            encode_project_key(Path::new("C:/Users/dan")),
            "C_-Users-dan"
        );
    }

    #[test]
    fn encode_project_key_is_readable_not_hashed() {
        // The whole point named in this function's own doc: a person `ls`ing
        // the sessions root sees their own project path, not a digest.
        let key = encode_project_key(Path::new("/Users/dan/code/conway"));
        assert!(key.contains("conway"));
        assert!(key.contains("dan"));
    }

    #[test]
    fn session_root_configured_relative_resolves_against_project_dir_the_old_direct_way() {
        let env = HashMap::new();
        let resolved = session_root(
            Path::new("/Users/dan/my-project"),
            Some(Path::new("custom-sessions")),
            &env,
        );
        assert_eq!(
            resolved,
            PathBuf::from("/Users/dan/my-project/custom-sessions")
        );
    }

    #[test]
    fn session_root_configured_absolute_is_used_verbatim() {
        let env = HashMap::new();
        let resolved = session_root(
            Path::new("/Users/dan/my-project"),
            Some(Path::new("/elsewhere/sessions")),
            &env,
        );
        assert_eq!(resolved, PathBuf::from("/elsewhere/sessions"));
    }

    #[test]
    fn session_root_unconfigured_is_the_central_project_keyed_default_under_config_dir() {
        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            "/custom/config_dir".to_string(),
        );
        let resolved = session_root(Path::new("/Users/dan/my-project"), None, &env);
        assert_eq!(
            resolved,
            PathBuf::from("/custom/config_dir/sessions/-Users-dan-my-project")
        );
    }

    #[test]
    fn session_root_two_different_projects_get_two_different_roots() {
        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            "/custom/config_dir".to_string(),
        );
        let a = session_root(Path::new("/Users/dan/project-a"), None, &env);
        let b = session_root(Path::new("/Users/dan/project-b"), None, &env);
        assert_ne!(a, b);
        assert_eq!(a.parent(), b.parent(), "both still share the central root");
    }

    #[test]
    fn legacy_project_sessions_dir_names_the_old_default_location() {
        let legacy = legacy_project_sessions_dir(Path::new("/Users/dan/my-project"));
        assert_eq!(
            legacy,
            PathBuf::from("/Users/dan/my-project/.conway/sessions")
        );
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
