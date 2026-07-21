//! `FsPlugin`: the file tools (`read`, `write`, `edit`, `glob`, `grep`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use conway_core::ports::{CancellationToken, Plugin, PluginManifest, Tool};

pub mod edit;
pub mod glob;
pub mod grep;
pub mod read;
pub mod write;

pub use edit::EditTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use read::ReadTool;
pub use write::WriteTool;

/// One file entry produced by [`walk_files`]: its path relative to the
/// search root (used for pattern matching and rendered output) and its
/// absolute path (used to open/stat it).
pub(crate) struct WalkEntry {
    pub relative: PathBuf,
    pub absolute: PathBuf,
}

/// Walks `root` gitignore-aware and single-threaded, in the `ignore` crate's
/// deterministic depth-first order, yielding files only (directories are
/// skipped, `.git` is never descended into). Polls `cancel` every 64 entries
/// visited and aborts with `Err(ToolError::Cancelled)`; any other walk
/// failure (e.g. an unreadable directory) is a host-level
/// `Err(ToolError::Io)`.
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

/// The `fs` plugin: `read`, `write`, `edit`, `glob`, `grep`.
pub struct FsPlugin {
    tools: Vec<Arc<dyn Tool>>,
}

impl FsPlugin {
    pub fn new() -> Self {
        let tools: Vec<Arc<dyn Tool>> = vec![
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
            id: "conway.fs".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            tools: self.tools.iter().map(|t| t.spec().name).collect(),
            required_host_caps: Vec::new(),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
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
        assert_eq!(names, vec!["edit", "glob", "grep", "read", "write"]);
    }
}
