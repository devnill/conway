//! Filesystem/user config discovery of config files. No parsing, no env-var
//! `CONWAY_*` mapping (that lives in `merge.rs`) — just "which paths exist."
//!
//! ## The one-file-two-roles collision, and how [`discover`] closes it
//!
//! `~/.conway/settings.json` is, structurally, TWO different things at
//! once: it is what [`user_config_path`] resolves to whenever
//! `CONWAY_CONFIG_DIR` is unset (the *user* layer), and it is also an
//! ordinary match for [`discover`]'s own upward walk from any working
//! directory beneath `$HOME` (the *project* layer) -- nothing about the walk
//! ever knew those were the same file wearing two hats. Before this module
//! closed the gap (board item `01M0VV6CVSZM4XH8J4G6EBV5E3`), that meant two
//! distinct harms: the redundant-but-harmless case (env unset, the same file
//! merges twice, once as each layer, producing identical output either way)
//! MASKED the harmful case -- `CONWAY_CONFIG_DIR` set to relocate the user
//! layer somewhere isolated (a test fixture, an embedder's own directory)
//! while the *project* walk, which knows nothing of that variable, still
//! reached upward from `cwd` and found the real `~/.conway/settings.json`
//! sitting there unchanged, outranking the isolated layer the operator
//! believed they had switched to (`project` beats `user` in the five-source
//! order). One live run cost the operator two real provider calls on real
//! credentials this way.
//!
//! [`discover`] now takes an explicit `exclude` list -- see
//! `project_discovery_exclusions`, the one function every call site in
//! this crate builds that list with -- and skips (keeps walking past,
//! rather than stopping on) any candidate that names the same underlying
//! file as an excluded path. `project_discovery_exclusions` always includes
//! the literal, override-independent `~/.conway/settings.json`
//! ([`home_settings_path`]) in addition to whatever [`user_config_path`]
//! currently resolves to (which coincide when `CONWAY_CONFIG_DIR` is unset,
//! and diverge -- the case that matters -- when it is set): this is what
//! makes the exclusion work in BOTH the unset case (a harmless dedup, since
//! the file would merge identically either way) and the set case (a real
//! exclusion, since the two paths differ and only the home file is
//! collision-prone).
//!
//! **Why this does not break "a project genuinely lives in `$HOME`" (the
//! case an earlier, simpler fix -- bounding the walk at `$HOME` outright --
//! would have broken).** A project whose own `.conway/settings.json` sits
//! at some ancestor CLOSER than `$HOME` (e.g. `$HOME/work/proj/.conway/`)
//! is returned by `discover` exactly as before: the walk finds it first and
//! never reaches `$HOME` at all. Only the walk's LAST possible candidate --
//! `$HOME/.conway/settings.json` itself -- is ever excluded, and only
//! because it is the literal file [`user_config_path`]'s own fallback
//! branch already reads under a different label. An operator who genuinely
//! keeps `.conway/settings.json` directly in `$HOME` and relies on it being
//! discovered as a *project* config sees no behavioral change when
//! `CONWAY_CONFIG_DIR` is unset (it is applied via the user layer instead,
//! with byte-identical content, so the merge result is unchanged) and,
//! correctly, no longer sees it applied at all once `CONWAY_CONFIG_DIR` IS
//! set -- exactly the isolation that variable advertises.
//!
//! **Extends to `permission_file_paths`/`provider_profile_file_paths` too**
//! (both call `discover` for the same reason and shared the same
//! collision): each passes `project_discovery_exclusions` through.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Walks from `start` up to the filesystem root looking for
/// `<dir>/.conway/settings.json`, returning the *nearest* match (i.e.
/// `start` itself is checked first) that does not name the same underlying
/// file as anything in `exclude` -- a candidate that DOES match `exclude`
/// is skipped, not treated as a stopping point, and the walk continues
/// upward past it. See this module's own doc for the collision `exclude`
/// exists to close, and `project_discovery_exclusions` for how every
/// production call site in this crate builds that list.
///
/// An empty `exclude` slice reproduces this function's pre-item behavior
/// exactly (every unit test below that does not care about the exclusion
/// passes `&[]`).
pub fn discover(start: &Path, exclude: &[PathBuf]) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join(".conway").join("settings.json");
        if candidate.is_file()
            && !exclude
                .iter()
                .any(|excluded| same_settings_file(&candidate, excluded))
        {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Whether `a` and `b` name the same underlying file -- the comparison
/// [`discover`] uses to decide whether a candidate is the collision this
/// module's own doc describes.
///
/// Canonicalizes both sides when possible, so a spelling difference (a
/// symlinked `$HOME` -- macOS's `/var` -> `/private/var` is the same shape
/// one level up the tree, or an operator-managed symlink pointing `.conway`
/// somewhere else) does not defeat the comparison. But canonicalizing a
/// path that does not exist, or that this process cannot stat (a broken
/// symlink, a permission error), is EXPECTED here, not exceptional: the
/// exclusion set commonly names a file that has never been created (e.g. a
/// fixture's own `$CONWAY_CONFIG_DIR/settings.json` before the fixture ever
/// writes one) -- so a side that fails to canonicalize falls back to
/// [`normalize_lexically`] rather than making the whole comparison bail out
/// (fail-loud is wrong here; the correct behavior for "one side is a bare
/// path with nothing there" is simply "compare the paths"). Per P-13 (fail
/// closed): `discover`'s own candidate side is always an existing file
/// (`candidate.is_file()` already gated the call), so it is only ever the
/// EXCLUDE side that can fail to canonicalize -- and falling back to a
/// normalized-but-uncanonicalized comparison on that side never makes this
/// function LESS likely to catch a real collision than skipping
/// canonicalization entirely would have; it only adds symlink-awareness on
/// top when both sides do resolve.
fn same_settings_file(a: &Path, b: &Path) -> bool {
    let resolve = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| normalize_lexically(p));
    resolve(a) == resolve(b)
}

/// The set of `settings.json` paths [`discover`] must refuse to return as a
/// *project* config, because each one plays a second, `user`-layer role
/// too -- see this module's own doc for the collision this closes. Two
/// entries, not one, and DELIBERATELY not deduplicated down to whichever
/// one [`user_config_path`] currently returns:
///
/// - `user_config_path(env)`: wherever THIS invocation's user layer
///   actually reads from right now (`$CONWAY_CONFIG_DIR/settings.json` when
///   set, `~/.conway/settings.json` otherwise).
/// - [`home_settings_path`]: the raw, override-INdependent
///   `~/.conway/settings.json` -- included unconditionally, even when
///   `CONWAY_CONFIG_DIR` is set and the entry above names a different path
///   entirely.
///
/// When `CONWAY_CONFIG_DIR` is unset the two entries coincide (one path
/// after `discover`'s own dedup). When it IS set they diverge, and it is
/// the SECOND entry that does the isolating work this item exists for: the
/// real home file is excluded from the project walk even though the user
/// layer itself has moved elsewhere -- which is exactly the case an
/// operator setting `CONWAY_CONFIG_DIR` for isolation (a test fixture, an
/// embedder, a hand demo) needs closed, and exactly the case a plain
/// "exclude whatever `user_config_path` resolves to today" rule would have
/// missed.
pub(crate) fn project_discovery_exclusions(env: &HashMap<String, String>) -> Vec<PathBuf> {
    let mut exclude = Vec::new();
    if let Some(path) = user_config_path(env) {
        exclude.push(path);
    }
    if let Some(path) = home_settings_path() {
        if !exclude.contains(&path) {
            exclude.push(path);
        }
    }
    exclude
}

/// The literal `~/.conway/settings.json` path, regardless of whether
/// `CONWAY_CONFIG_DIR` is set in the current invocation -- [`user_config_path`]'s
/// own fallback branch, exposed standalone so `project_discovery_exclusions`
/// can name this file even when `user_config_path` itself currently resolves
/// somewhere else entirely. `None` under the same condition
/// `user_config_path`'s fallback returns `None`: no home directory
/// discoverable on this platform/environment.
pub fn home_settings_path() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".conway").join("settings.json"))
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
/// switcher recognises it, and it is what every IN-PROCESS test uses to stay
/// hermetic — see `crates/conway/tests/config_isolation_guard.rs` for the
/// ambient-read bug that guard exists to prevent (in-process calls into this
/// library only; see that file's own module doc for the scope line).
///
/// **This function alone is not what makes `CONWAY_CONFIG_DIR` mean
/// "isolated," for a compiled-binary invocation.** Relocating the user
/// layer here does nothing about the PROJECT layer `discover` resolves
/// separately -- an unbounded upward walk from `cwd` that, for any `cwd`
/// beneath `$HOME`, used to reach `~/.conway/settings.json` and win, since
/// `project` outranks `user` in the five-source order regardless of what
/// this function returns. That collision (`~/.conway/settings.json` playing
/// BOTH the user role this function resolves and a project role `discover`
/// can independently reach) is now closed at `discover`'s own call sites via
/// `project_discovery_exclusions` -- see this module's own top-of-file doc
/// for the full mechanism and board item `01M0VV6CVSZM4XH8J4G6EBV5E3` for
/// the incident that surfaced it. `crates/conway-cli/tests/
/// config_isolation_binary.rs` is the compiled-binary regression test for
/// that fix; `config_isolation_guard.rs`'s own scope note is why it could
/// never have caught this in the first place.
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
    home_settings_path()
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
    // cwd's own `.conway/` if no ancestor config exists yet. Excludes the
    // same collision-prone paths `discover`'s own doc describes -- without
    // this, `~/.conway/permissions.json` could be returned once here as
    // "project" and again below as "global," un-deduplicated whenever
    // `CONWAY_CONFIG_DIR` relocates the "global scope" push below away from
    // `~/.conway/` (the `!paths.contains(&global)` dedup below only ever
    // catches the CONWAY_CONFIG_DIR-unset case, where the two pushes would
    // already be identical).
    if let Some(project) = discover(cwd, &project_discovery_exclusions(env)) {
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
    // Same exclusion, same reason, as `permission_file_paths`'s own project
    // scope above.
    if let Some(project) = discover(cwd, &project_discovery_exclusions(env)) {
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

        let found = discover(&nested, &[]).unwrap();
        assert_eq!(found, nested_conf_dir.join("settings.json"));
    }

    #[test]
    fn discover_returns_none_when_absent() {
        let tmp = tempfile_dir();
        let nested = tmp.join("x").join("y");
        fs::create_dir_all(&nested).unwrap();
        assert!(discover(&nested, &[]).is_none());
    }

    /// The behavior [`discover`]'s own doc names explicitly: an excluded
    /// candidate is SKIPPED, not treated as "nothing here" -- the walk keeps
    /// going upward past it, rather than stopping (which `discover_returns_
    /// none_when_the_only_match_is_excluded` below distinguishes from
    /// "return None immediately").
    #[test]
    fn discover_skips_an_excluded_candidate_and_returns_the_next_ancestor_match() {
        let tmp = tempfile_dir();
        let root_conf = tmp.join(".conway");
        fs::create_dir_all(&root_conf).unwrap();
        fs::write(root_conf.join("settings.json"), "").unwrap();

        let nested = tmp.join("a").join("b");
        let nested_conf_dir = nested.join(".conway");
        fs::create_dir_all(&nested_conf_dir).unwrap();
        fs::write(nested_conf_dir.join("settings.json"), "").unwrap();

        let excluded = vec![nested_conf_dir.join("settings.json")];
        let found = discover(&nested, &excluded).unwrap();
        assert_eq!(
            found,
            root_conf.join("settings.json"),
            "the nearer match was excluded, so discover must keep walking and \
             return the further ancestor match instead of stopping"
        );
    }

    #[test]
    fn discover_returns_none_when_the_only_match_is_excluded() {
        let tmp = tempfile_dir();
        let conf_dir = tmp.join(".conway");
        fs::create_dir_all(&conf_dir).unwrap();
        fs::write(conf_dir.join("settings.json"), "").unwrap();

        let excluded = vec![conf_dir.join("settings.json")];
        assert!(discover(&tmp, &excluded).is_none());
    }

    /// The "operator genuinely keeps a project in `$HOME`" case this item's
    /// own spec named explicitly: a NEARER, non-excluded `.conway/
    /// settings.json` must win exactly as before, even when an excluded
    /// ancestor ALSO exists further up (standing in for the real
    /// `~/.conway/settings.json` an operator's project directory sits under).
    /// `discover`'s nearest-match-first walk never even reaches the excluded
    /// ancestor here -- this pins that the exclusion mechanism changes
    /// nothing about ordinary, non-colliding project discovery.
    #[test]
    fn discover_returns_a_nearer_non_excluded_match_even_when_an_excluded_ancestor_also_exists() {
        let tmp = tempfile_dir();
        // Stands in for `~/.conway/settings.json`.
        let home_conf = tmp.join(".conway");
        fs::create_dir_all(&home_conf).unwrap();
        fs::write(home_conf.join("settings.json"), "").unwrap();

        // Stands in for `~/work/project/.conway/settings.json` -- a real,
        // ordinary project config nested under that same "home."
        let project = tmp.join("work").join("project");
        let project_conf = project.join(".conway");
        fs::create_dir_all(&project_conf).unwrap();
        fs::write(project_conf.join("settings.json"), "").unwrap();

        let excluded = vec![home_conf.join("settings.json")];
        let found = discover(&project, &excluded).unwrap();
        assert_eq!(found, project_conf.join("settings.json"));
    }

    /// [`same_settings_file`]'s symlink-awareness: a candidate reached via a
    /// symlinked ancestor directory must still be recognized as the same
    /// underlying file as its canonical spelling in the exclude list.
    /// Skipped where the platform/sandbox refuses to create a symlink
    /// (matches this crate's other symlink-dependent tests' own posture)
    /// rather than failing the whole suite over an environment limitation
    /// unrelated to what this test proves.
    #[test]
    fn same_settings_file_recognizes_a_symlinked_spelling_of_the_same_file() {
        let tmp = tempfile_dir();
        let real_dir = tmp.join("real");
        fs::create_dir_all(&real_dir).unwrap();
        let conf_dir = real_dir.join(".conway");
        fs::create_dir_all(&conf_dir).unwrap();
        fs::write(conf_dir.join("settings.json"), "").unwrap();

        let link = tmp.join("linked");
        #[cfg(unix)]
        let symlink_result = std::os::unix::fs::symlink(&real_dir, &link);
        #[cfg(not(unix))]
        let symlink_result: std::io::Result<()> = Err(std::io::Error::other("unsupported"));
        if symlink_result.is_err() {
            eprintln!("skipping: this environment cannot create symlinks");
            return;
        }

        let via_symlink = link.join(".conway").join("settings.json");
        let via_real = conf_dir.join("settings.json");
        assert!(
            same_settings_file(&via_symlink, &via_real),
            "a candidate reached through a symlinked ancestor must still \
             compare equal to the same file's canonical spelling"
        );
    }

    /// [`same_settings_file`]'s fail-closed-toward-comparison fallback: a
    /// path that does not exist (so `fs::canonicalize` errors) must not
    /// abort the comparison -- it falls back to a lexical (uncanonicalized)
    /// comparison instead, per this function's own doc.
    #[test]
    fn same_settings_file_falls_back_to_lexical_comparison_when_one_side_does_not_exist() {
        let tmp = tempfile_dir();
        let nonexistent_a = tmp.join("never-created-a").join("settings.json");
        let nonexistent_b = tmp.join("never-created-b").join("settings.json");

        // Two different nonexistent paths must not spuriously compare equal.
        assert!(!same_settings_file(&nonexistent_a, &nonexistent_b));
        // The identical nonexistent path, spelled byte-for-byte the same
        // both times, must still compare equal via the lexical fallback.
        assert!(same_settings_file(&nonexistent_a, &nonexistent_a.clone()));
    }

    #[test]
    fn project_discovery_exclusions_includes_both_the_relocated_and_the_raw_home_path() {
        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            "/custom/config_dir".to_string(),
        );
        let exclude = project_discovery_exclusions(&env);
        assert!(exclude.contains(&PathBuf::from("/custom/config_dir/settings.json")));
        if let Some(home) = home_settings_path() {
            assert!(
                exclude.contains(&home),
                "the raw home settings path must be excluded even when \
                 CONWAY_CONFIG_DIR relocates the user layer elsewhere"
            );
            assert_eq!(
                exclude.len(),
                2,
                "the two paths differ here, so both must be present, not deduplicated"
            );
        }
    }

    #[test]
    fn project_discovery_exclusions_dedupes_to_one_entry_when_conway_config_dir_is_unset() {
        let env = HashMap::new();
        let exclude = project_discovery_exclusions(&env);
        if let Some(home) = home_settings_path() {
            assert_eq!(
                exclude,
                vec![home],
                "with CONWAY_CONFIG_DIR unset, user_config_path and \
                 home_settings_path coincide -- exactly one entry, not two \
                 identical ones"
            );
        }
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
