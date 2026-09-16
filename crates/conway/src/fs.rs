//! `atomic_write` is the sync-facing entry point for the one durable
//! "write a small sidecar file" primitive this facade exposes -- board
//! item `01M1WVNPYTFF0TEJGHGGRPDF8E` (architectural review finding F6),
//! consolidated onto its one remaining implementation by board item
//! `01M2M5KVC9J7MWY56DQK5YPXNR` (review finding F13).
//!
//! # Why this module is `#[cfg(feature = "builtin-tools")]`
//!
//! There is exactly one implementation of the "write to a temp sibling,
//! `fsync` it, rename over the target, `fsync` the parent directory too"
//! protocol in this workspace: [`conway_tools::fs::write::atomic_write`].
//! It lives in `conway-tools` (the crate behind this facade's
//! `builtin-tools` feature, default-on but genuinely optional -- see
//! `crates/conway/Cargo.toml`'s `[features]` block) because that is
//! `conway-core`'s own `Tool`/`Plugin` implementation layer, and this
//! primitive already has a real consumer there (`WriteTool`'s own
//! `atomic_write_str`, the async adapter over the same sync core).
//! `conway-core` itself stays I/O-free by design (a separate, standing
//! invariant this item does not reopen), so the implementation cannot live
//! any lower than `conway-tools`, and pulling `conway-tools` in
//! unconditionally would drag `tokio`'s fs/process features, `nix`,
//! `cap-std`, `ignore`, `globset`, and `regex` into every build of this
//! facade, including the two `--no-default-features` legs `conway`'s own
//! CI matrix exercises. This function is therefore gated the same way
//! every other `conway-tools`-sourced item in this crate already is (see
//! `crate::plugin::kill_group`'s own re-export doc in `src/lib.rs` for the
//! identical reasoning) -- `#[cfg(feature = "builtin-tools")]`, no `unix`
//! qualifier, because unlike those process-management re-exports this
//! implementation has no unix-only step.
//!
//! A binary built with `--no-default-features` loses this module
//! entirely, the same way it loses `conway::plugin::kill_group` and every
//! other name this crate re-exports from `conway-tools`: it must bring its
//! own durable-write primitive, or depend on `conway-tools` (or a plugin
//! built on it) directly.
//!
//! # What this does NOT change
//!
//! `crate::config::writer::write_atomically` -- the facade's own
//! `settings.json`/`permissions.json`/`trust.json` writer -- is a
//! deliberately separate, `pub(crate)` primitive with its own documented
//! gap (see that function's own doc: an `fsync` there is left for its own
//! decision, board item `01M12ERK9WSJ10AT87WCJ9ZME9`). This module does
//! not fold into it or change it; the two are independent decisions that
//! happen to share a shape.

use std::io;
use std::path::Path;

/// Durably writes `bytes` to `path`: the sync entry point three non-`Tool`
/// callers use directly (`conway-cli`'s `session_names::NamesStore::save`
/// and `tui::history::save`, and `conway-plugin-names`'s
/// `FsAgentNames::persist` -- none of the three may depend on
/// `conway-tools` directly: `conway-cli`'s own `no_forbidden_deps` test
/// refuses that edge outright, and `conway-plugin-names` is, by decision
/// `01M0TV3ZZBDKSSV7MD0FW3FSY7`, restricted to depending on nothing but
/// this facade).
///
/// Delegates straight to [`conway_tools::fs::write::atomic_write`] -- the
/// one implementation of this protocol in the workspace, including the
/// `fsync`-before-`rename` ordering and the post-rename parent-directory
/// `fsync` that makes the rename itself durable, not just the bytes it
/// points at. No async machinery here: this crate could add one (a
/// per-call `tokio::runtime::Runtime::new().block_on(..)`, say) but at
/// least one real caller (`conway-cli`'s `sessions name` command) invokes
/// this function synchronously from a `#[tokio::main]` worker thread with
/// no `spawn_blocking` wrapper of its own, where bridging to an async
/// implementation that way would panic ("Cannot start a runtime from
/// within a runtime"). A plain sync function avoids that hazard entirely,
/// and works identically for an embedder who never starts a runtime at
/// all -- see `conway_tools::fs::write`'s own module doc for the full
/// three-entry-point picture (this wrapper, the async `Tool`-facing
/// adapter, and the one shared implementation underneath both).
///
/// See [`conway_tools::fs::write::atomic_write`]'s own doc for the full
/// mechanism and its two test-pinned orderings
/// (`sync_all_happens_before_rename`, `parent_directory_synced_after_rename`
/// in that module's own `tests`).
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    conway_tools::fs::write::atomic_write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_bytes_and_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nested/deep/sidecar.json");
        atomic_write(&target, b"hello").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"hello");
    }

    #[test]
    fn second_write_replaces_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sidecar.json");
        atomic_write(&target, b"first").unwrap();
        atomic_write(&target, b"second, longer").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"second, longer");
    }

    #[test]
    fn no_tmp_sibling_remains_after_success() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sidecar.json");
        atomic_write(&target, b"hi").unwrap();
        let leftover: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".conway.tmp"))
            .collect();
        assert!(leftover.is_empty(), "leftover tmp files: {leftover:?}");
    }

    #[test]
    fn failure_to_open_temp_file_returns_err_and_touches_nothing() {
        // A parent that does not exist and cannot be created (its own
        // parent is a FILE, not a directory) makes `create_dir_all` fail
        // before any temp file is even attempted.
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"i am a file, not a directory").unwrap();
        let target = blocker.join("nested/sidecar.json");

        let err = atomic_write(&target, b"hi").unwrap_err();
        assert!(!target.exists());
        // Cross-platform: some form of "not a directory"/"not found"-ish
        // I/O error; the exact `ErrorKind` varies by OS, so this only
        // pins that an `Err` is surfaced, not swallowed.
        let _ = err;
    }
}
