//! `FsPlugin`: the file tools (`cd`, `read`, `write`, `edit`, `glob`, `grep`).
//!
//! ## `[S1.5]`: `conway.fs`'s root, read from per-agent state
//!
//! `FsPlugin` is the proving consumer of the general per-agent
//! plugin-configuration/narrowing mechanism (`conway_core::ports::
//! Plugin::narrowable_keys`/`PluginConfig::narrow`): it declares one
//! narrowable key, bare name `"root"` (reachable in a caller's
//! `SubagentSpec::plugin_config` map as `"conway.fs.root"` once the host
//! prefixes it with this plugin's own manifest id), and `check_root`
//! enforces it inside `read`/`write`/`cd` before the underlying I/O runs.
//!
//! **This is additive, not a replacement.** The harness-level confinement
//! root (`SubagentSpec::root`/`--root`, enforced by `PermissionBroker`
//! ahead of the gate) is untouched by this item -- see its own board item
//! for the eventual retirement. Both mechanisms can be in effect
//! simultaneously; a call denied by either is denied.
//!
//! **Not itself TOCTOU-closed.** This checks `candidate` (an already-
//! resolved path) and then a caller performs the real filesystem operation
//! as a separate step -- the SAME check-then-open shape the harness-level
//! root has today. Closing that gap with open-relative operations (so the
//! check and the use are one step) is explicitly the FOLLOW-ON item's job,
//! not this one's.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use conway_core::containment::{CanonicalRoot, Containment};
use conway_core::error::ToolError;
use conway_core::ports::{CancellationToken, NarrowingRule, Plugin, PluginManifest, Tool, ToolCtx};

pub mod cd;
pub mod edit;
pub mod glob;
pub mod grep;
pub mod read;
pub mod write;

pub use cd::CdTool;
pub use edit::EditTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use read::ReadTool;
pub use write::WriteTool;

/// This plugin's manifest id -- the namespace prefix
/// `conway_core::ports::Plugin::narrowable_keys`'s own doc says the HOST
/// applies to [`ROOT_CONFIG_KEY`] before it is ever reachable in a caller's
/// `SubagentSpec::plugin_config` map. Shared by [`FsPlugin::manifest`] and
/// `check_root` so the two can never independently drift.
pub const PLUGIN_ID: &str = "conway.fs";

/// The bare per-agent config key this plugin declares narrowable.
pub const ROOT_CONFIG_KEY: &str = "root";

/// The already-prefixed key [`ToolCtx::config`] actually carries this
/// plugin's root under, once the host has applied the prefix
/// [`ROOT_CONFIG_KEY`]'s own doc describes.
const FULL_ROOT_CONFIG_KEY: &str = "conway.fs.root";

/// `[S1.5]`'s narrowing rule for [`ROOT_CONFIG_KEY`]: `child` narrows
/// `parent` iff both are strings naming a real, canonicalizable filesystem
/// location and `child` resolves inside `parent`
/// (`conway_core::containment::CanonicalRoot`, the SAME symlink-aware
/// containment check the harness-level root already uses). Fails closed
/// (`false`) on any non-string value or any path that does not
/// canonicalize -- "can't check" is never "allow", the same discipline
/// [`Containment`]'s own doc establishes.
fn root_narrows(parent: &serde_json::Value, child: &serde_json::Value) -> bool {
    let (Some(parent_str), Some(child_str)) = (parent.as_str(), child.as_str()) else {
        return false;
    };
    let Ok(parent_root) = CanonicalRoot::new(Path::new(parent_str)) else {
        return false;
    };
    let Ok(child_root) = CanonicalRoot::new(Path::new(child_str)) else {
        return false;
    };
    matches!(
        parent_root.contains(child_root.as_path()),
        Containment::Inside
    )
}

/// Checks `candidate` (an already-resolved, possibly-not-yet-existing
/// filesystem path -- the same shape `crate::common::resolve_path`
/// produces) against `ctx.config`'s [`FULL_ROOT_CONFIG_KEY`], if this agent
/// has one set. `Ok(())` when unconfined (no key set -- today's
/// pre-existing behavior, unchanged) or when `candidate` resolves inside
/// the configured root; `Err(ToolError::Denied)` (model-recoverable --
/// `ToolRunner`'s `execute_one` turns any `Err` from `Tool::invoke` into a
/// persisted, `is_error: true` `ToolResultRecord`, the observable outcome
/// this item's own acceptance asks tests to assert on) otherwise.
/// `Containment::Undecidable` is treated identically to `Outside`.
///
/// Callers invoke this AFTER resolving the candidate path
/// (`crate::common::resolve_path`) and BEFORE performing the underlying
/// I/O.
pub(crate) fn check_root(ctx: &ToolCtx, candidate: &Path) -> Result<(), ToolError> {
    let Some(configured) = ctx.config.values.get(FULL_ROOT_CONFIG_KEY) else {
        return Ok(());
    };
    let Some(root_str) = configured.as_str() else {
        return Err(ToolError::Denied {
            reason: format!(
                "{FULL_ROOT_CONFIG_KEY} is configured but is not a string ({configured}); \
                 refusing to resolve any path against it"
            ),
        });
    };
    let root = CanonicalRoot::new(Path::new(root_str)).map_err(|err| ToolError::Denied {
        reason: format!(
            "{FULL_ROOT_CONFIG_KEY} ({root_str}) does not canonicalize: {err}; refusing every \
             path under an unresolvable root"
        ),
    })?;
    match root.contains(candidate) {
        Containment::Inside => Ok(()),
        Containment::Outside | Containment::Undecidable => Err(ToolError::Denied {
            reason: format!(
                "{} is outside this agent's {FULL_ROOT_CONFIG_KEY} ({})",
                candidate.display(),
                root.as_path().display()
            ),
        }),
    }
}

/// One file entry produced by [`walk_files`]: its path relative to the
/// search root (used for pattern matching and rendered output) and its
/// absolute path (used to open/stat it).
pub(crate) struct WalkEntry {
    pub relative: PathBuf,
    pub absolute: PathBuf,
}

/// Walks `root` gitignore-aware and single-threaded, depth-first, yielding
/// files only (directories are skipped, `.git` is never descended into).
/// Polls `cancel` every 64 entries visited and aborts with
/// `Err(ToolError::Cancelled)`; any other walk failure (e.g. an unreadable
/// directory) is a host-level `Err(ToolError::Io)`.
///
/// **Order is deterministic because this function sorts, not because the
/// walker does.** `ignore::WalkBuilder` defaults to `sorter: None` and yields
/// entries in `read_dir` order, which is filesystem- and inode-dependent; the
/// explicit `sort_by_file_path` below is what makes repeated walks over the
/// same tree agree. An earlier version of this comment claimed the `ignore`
/// crate's order was itself deterministic. It is not, and the difference was
/// user-visible: `grep` and `glob` output order could change between identical
/// runs, and the test asserting walk order passed locally and on one CI run
/// while failing on another with no code change in between.
///
/// This function is synchronous and does blocking I/O; callers on the async
/// path MUST invoke it inside `tokio::task::spawn_blocking`. Shared by
/// [`GlobTool`] and [`GrepTool`] so both tools walk identically.
pub(crate) fn walk_files(
    root: &Path,
    cancel: &CancellationToken,
) -> Result<Vec<WalkEntry>, conway_core::error::ToolError> {
    use conway_core::error::ToolError;

    let mut entries = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .parents(false)
        .filter_entry(|entry| entry.file_name() != std::ffi::OsStr::new(".git"))
        // Sorts siblings within each directory, which is what makes the whole
        // walk reproducible. By path rather than by file name: both agree for
        // siblings (they share a parent, so the paths differ only in the final
        // component), but comparing the full path is a total order on the thing
        // the doc above actually promises, and does not rely on that
        // equivalence continuing to hold.
        .sort_by_file_path(|a, b| a.cmp(b))
        .build();

    for (i, result) in walker.enumerate() {
        if i % 64 == 0 && cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let entry = result.map_err(|err| ToolError::Io {
            detail: format!("walk error under {}: {err}", root.display()),
        })?;
        if entry.file_type().is_some_and(|ft| ft.is_dir()) {
            continue;
        }
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(path).to_path_buf();
        entries.push(WalkEntry {
            relative,
            absolute: path.to_path_buf(),
        });
    }

    Ok(entries)
}

/// The `fs` plugin: `cd`, `read`, `write`, `edit`, `glob`, `grep`.
pub struct FsPlugin {
    tools: Vec<Arc<dyn Tool>>,
}

impl FsPlugin {
    pub fn new() -> Self {
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(CdTool::new()),
            Arc::new(EditTool::new()),
            Arc::new(GlobTool::new()),
            Arc::new(GrepTool::new()),
            Arc::new(ReadTool::new()),
            Arc::new(WriteTool::new()),
        ];
        Self { tools }
    }
}

impl Default for FsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for FsPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PLUGIN_ID.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            tools: self.tools.iter().map(|t| t.spec().name).collect(),
            required_host_caps: Vec::new(),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }

    /// `[S1.5]`'s proving consumer: this plugin's own confinement root,
    /// declared narrowable so a parent can hand a fork/spawn child a
    /// tighter root than its own -- see this module's own doc.
    fn narrowable_keys(&self) -> Vec<NarrowingRule> {
        vec![NarrowingRule {
            key: ROOT_CONFIG_KEY,
            narrows: root_narrows,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_id_and_tool_names() {
        let plugin = FsPlugin::new();
        let manifest = plugin.manifest();
        assert_eq!(manifest.id, "conway.fs");
        assert!(manifest.required_host_caps.is_empty());

        let mut names: Vec<String> = plugin
            .tools()
            .iter()
            .map(|t| t.spec().name.as_str().to_string())
            .collect();
        names.sort();
        assert_eq!(names, vec!["cd", "edit", "glob", "grep", "read", "write"]);
    }

    #[test]
    fn fs_plugin_declares_root_narrowable() {
        let plugin = FsPlugin::new();
        let rules = plugin.narrowable_keys();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].key, "root");
    }

    // ---- root_narrows ----

    #[test]
    fn root_narrows_accepts_a_subdirectory() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let parent = serde_json::json!(tmp.path().display().to_string());
        let child = serde_json::json!(sub.display().to_string());
        assert!(root_narrows(&parent, &child));
    }

    #[test]
    fn root_narrows_rejects_a_sideways_directory() {
        let tmp = tempfile::TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let parent = serde_json::json!(a.display().to_string());
        let child = serde_json::json!(b.display().to_string());
        assert!(!root_narrows(&parent, &child));
    }

    #[test]
    fn root_narrows_rejects_a_wider_ancestor() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        // Widening: `child` (the ancestor) is NOT inside `parent` (the
        // subdirectory) -- the direction this whole mechanism exists to
        // refuse.
        let parent = serde_json::json!(sub.display().to_string());
        let child = serde_json::json!(tmp.path().display().to_string());
        assert!(!root_narrows(&parent, &child));
    }

    #[test]
    fn root_narrows_rejects_non_string_values() {
        assert!(!root_narrows(
            &serde_json::json!(1),
            &serde_json::json!("x")
        ));
        assert!(!root_narrows(
            &serde_json::json!("x"),
            &serde_json::json!(1)
        ));
    }

    #[test]
    fn root_narrows_rejects_a_nonexistent_child() {
        let tmp = tempfile::TempDir::new().unwrap();
        let parent = serde_json::json!(tmp.path().display().to_string());
        let child = serde_json::json!(tmp.path().join("does-not-exist").display().to_string());
        assert!(!root_narrows(&parent, &child));
    }

    // ---- check_root ----

    #[test]
    fn check_root_is_ok_when_unconfined() {
        let (ctx, _h) = crate::testing::test_ctx(PathBuf::from("/tmp"));
        assert!(check_root(&ctx, Path::new("/anything/at/all")).is_ok());
    }

    #[test]
    fn check_root_allows_a_candidate_inside_the_configured_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (mut ctx, _h) = crate::testing::test_ctx(tmp.path().to_path_buf());
        let mut values = serde_json::Map::new();
        values.insert(
            FULL_ROOT_CONFIG_KEY.to_string(),
            serde_json::json!(tmp.path().display().to_string()),
        );
        ctx.config = Arc::new(conway_core::ports::PluginConfig { values });
        assert!(check_root(&ctx, tmp.path()).is_ok());
    }

    #[test]
    fn check_root_denies_a_candidate_outside_the_configured_root() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root_dir = tmp.path().join("root");
        let outside_dir = tmp.path().join("outside");
        std::fs::create_dir(&root_dir).unwrap();
        std::fs::create_dir(&outside_dir).unwrap();
        let (mut ctx, _h) = crate::testing::test_ctx(root_dir.clone());
        let mut values = serde_json::Map::new();
        values.insert(
            FULL_ROOT_CONFIG_KEY.to_string(),
            serde_json::json!(root_dir.display().to_string()),
        );
        ctx.config = Arc::new(conway_core::ports::PluginConfig { values });
        let err = check_root(&ctx, &outside_dir.join("secret.txt")).unwrap_err();
        assert!(matches!(err, ToolError::Denied { .. }));
    }
}
