//! `FsPlugin`: the file tools (`cd`, `read`, `write`, `edit`, `glob`, `grep`).
//!
//! ## `[S1.5]`, retired-and-closed: `conway.fs` enforces its own root, and
//! the harness-level pre-gate root check is gone
//!
//! `FsPlugin` reads its root from the general per-agent
//! plugin-configuration/narrowing mechanism (`conway_core::ports::
//! Plugin::narrowable_keys`/`PluginConfig::narrow`): it declares one
//! narrowable key, bare name `"root"` (reachable in a caller's
//! `SubagentSpec::plugin_config` map as `"conway.fs.root"` once the host
//! prefixes it with this plugin's own manifest id), and enforces it inside
//! `read`/`write`/`edit`/`cd` (`beneath`) and `glob`/`grep` (`beneath::
//! confine_search_root`) before the underlying I/O runs.
//!
//! **This is now the ONLY confinement enforcement these six tools get.**
//! The harness-level pre-gate root check
//! (`PermissionBroker::check_root`'s per-tool `PathArgs::Named` walk) that
//! used to run ahead of the operator's gate for every tool declaring
//! `PathArgs::Named` is retired -- see `conway_runtime::permission`'s own
//! doc for exactly what remains there (a narrower, differently-named
//! gate-routing policy for `PathArgs::Unconfinable` calls like `bash`'s,
//! which this plugin cannot help with because `bash` is a different
//! plugin's tool). `check_root` (the pre-check-then-separately-open
//! function this module used to export) is gone; `beneath` replaces it with
//! open-relative enforcement, closing the TOCTOU gap the pre-check could
//! never close -- see that module's own doc for the full "why" and the
//! symlink-race test that fails against a reverted, pre-check-shaped build.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use conway_core::containment::{CanonicalRoot, Containment};
use conway_core::ports::{CancellationToken, NarrowingRule, Plugin, PluginManifest, Tool};

// `pub` only under `test-fakes`, exposing `beneath::toctou_probe` (this
// item's own verification anchor, `tests/fs_confinement.rs`, needs it) --
// mirrors `crate::testing`'s identical conditional-visibility pattern.
// `pub(crate)` otherwise: `beneath` itself (`resolve`/`Access`/`open_root`/
// the four confined-operation entry points) is production dispatch
// plumbing, not a public API.
#[cfg(feature = "test-fakes")]
pub mod beneath;
#[cfg(not(feature = "test-fakes"))]
pub(crate) mod beneath;
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
/// `beneath` so the two can never independently drift.
pub const PLUGIN_ID: &str = "conway.fs";

/// The bare per-agent config key this plugin declares narrowable.
pub const ROOT_CONFIG_KEY: &str = "root";

/// The already-prefixed key `conway_core::ports::ToolCtx::config` actually
/// carries this plugin's root under, once the host has applied the prefix
/// [`ROOT_CONFIG_KEY`]'s own doc describes. `conway_runtime`'s
/// `runtime::root`/`subagent` cannot import this constant (crate layering:
/// `conway-runtime -> conway-tools` would be a new, backward dependency
/// edge), so both derive the identical `"conway.fs.root"` string
/// independently, from a `pub(crate)` constant of their own naming this one
/// -- see those modules' own doc for the cross-crate duplication this
/// forces and why it is bounded (a `const`, not restated logic).
pub(crate) const FULL_ROOT_CONFIG_KEY: &str = "conway.fs.root";

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
        // The fs plugin needs nothing the host might lack -- it reads/writes
        // the filesystem through `ToolCtx`, no host-capability gate applies.
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

    // `check_root`'s own coverage (unconfined no-op, allow-inside,
    // deny-outside) moved to `beneath`'s test module along with the function
    // itself -- see that module for the equivalent (and now open-relative,
    // TOCTOU-closed) tests.
}
