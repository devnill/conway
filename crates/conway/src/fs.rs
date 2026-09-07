//! [`atomic_write`]: the one durable "write a small sidecar file" primitive
//! for consumers of the `conway` facade -- board item
//! `01M1WVNPYTFF0TEJGHGGRPDF8E` (architectural review finding F6).
//!
//! # Why this exists
//!
//! Conway's idiom for persisting a small sidecar JSON/config file is
//! "write to a temp file, then rename it into place" -- the standard way
//! to avoid a torn write if the process dies mid-save. Before this item,
//! five places in the tree reimplemented that pattern independently, and
//! three of them (`conway-cli`'s `session_names::NamesStore::save` and
//! `tui::history::save`, and `conway-plugin-names`'s
//! `FsAgentNames::persist`) forgot the `fsync` step: each wrote the temp
//! file and renamed it, but never called `sync_all` on the temp file
//! handle before the rename, so a crash at exactly the moment the pattern
//! exists to protect against could still lose data (the temp file's bytes
//! might still be sitting in the OS page cache, never flushed to the
//! underlying device, when the rename -- itself durable only once its own
//! directory-entry update reaches disk -- is observed by a reader after a
//! restart). `conway-tools::fs::write::atomic_write` and
//! `fs::beneath::write_file_atomic_confined` got this right (`flush` +
//! `sync_all` before `rename`) but that crate is `conway-core`'s own
//! `Tool`/`Plugin` implementation layer, not something either of the
//! three callers above may depend on: `conway-cli`'s own
//! `no_forbidden_deps` test (`crates/conway-cli/tests/cli_surface.rs`)
//! refuses a direct `conway-tools` dependency outright, and
//! `conway-plugin-names` is, by decision `01M0TV3ZZBDKSSV7MD0FW3FSY7`,
//! restricted to depending on nothing but this facade (see that crate's
//! own `Cargo.toml` comment). Both already depend on `conway`, though --
//! so this module is the shared home: one implementation to get right,
//! reachable from both without a new dependency edge.
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

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Durably writes `bytes` to `path`: creates `path`'s parent directories
/// as needed, writes `bytes` to a sibling temp file in the SAME directory
/// (so the final `rename` stays on one filesystem and is therefore
/// atomic -- no cross-device copy), `sync_all`s that temp file (an
/// `fsync`, flushing it to the underlying storage device) BEFORE renaming
/// it over `path`, then removes the temp file on any failure along the
/// way (best effort -- a failed cleanup is not itself reported, since the
/// original error is the one the caller needs).
///
/// The `fsync`-before-`rename` ordering is what this helper adds over a
/// bare "write then rename": without it, a crash between the rename and
/// the temp file's own dirty pages reaching disk can still leave `path`
/// pointing at incomplete or garbage content, which is exactly the
/// failure mode "write to a temp file, then rename it into place" exists
/// to prevent. See `tests::sync_all_happens_before_rename` for a
/// mechanical proof of the ordering, not just that both steps happen.
///
/// A reader can never observe a partially-written `path`: it is either
/// still the complete old file (nothing here touched it yet, or the
/// write/sync/rename failed before `rename` completed) or already the
/// complete new one (rename succeeded, which -- on every platform this
/// workspace targets -- replaces the destination as a single filesystem
/// operation).
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write_with(path, bytes, |p| fs::File::create(p))
}

/// [`atomic_write`]'s body, generic over how the temp file is opened --
/// production always passes `fs::File::create`; `tests` substitutes a
/// wrapper that records when `write_all`/`sync_all` are called, so the
/// ordering this function promises is checkable rather than merely
/// asserted in prose.
fn atomic_write_with<F: DurableWrite>(
    path: &Path,
    bytes: &[u8],
    open: impl FnOnce(&Path) -> io::Result<F>,
) -> io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }

    let tmp = tmp_sibling(path);

    let write_result: io::Result<()> = (|| {
        let mut file = open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();

    if let Err(err) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }

    if let Err(err) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }

    Ok(())
}

/// A seam over "a writable file that can be durably flushed": implemented
/// for `std::fs::File` in production (`sync_all` delegating to the
/// inherent, real-`fsync` method of the same name) and, in `tests`, for a
/// wrapper that records call order -- `sync_all` is an inherent method on
/// `fs::File`, not part of `io::Write`, so a trait is what makes it
/// substitutable at all.
trait DurableWrite: Write {
    fn sync_all(&self) -> io::Result<()>;
}

impl DurableWrite for fs::File {
    fn sync_all(&self) -> io::Result<()> {
        fs::File::sync_all(self)
    }
}

/// The atomic-write temp sibling for `path`: a hidden dotfile beside it,
/// e.g. `<parent>/.<filename>.conway-atomic-write.tmp`. Same-directory
/// placement keeps the final `rename` on one filesystem (atomic, no
/// cross-device copy) -- the same reasoning
/// `conway_tools::fs::write::tmp_sibling` documents for its own,
/// independent temp-name scheme.
fn tmp_sibling(path: &Path) -> PathBuf {
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    parent.join(format!(".{filename}.conway-atomic-write.tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn writes_bytes_and_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nested/deep/sidecar.json");
        atomic_write(&target, b"hello").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"hello");
    }

    #[test]
    fn second_write_replaces_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sidecar.json");
        atomic_write(&target, b"first").unwrap();
        atomic_write(&target, b"second, longer").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"second, longer");
    }

    #[test]
    fn no_tmp_sibling_remains_after_success() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sidecar.json");
        atomic_write(&target, b"hi").unwrap();
        let leftover: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftover.is_empty(), "leftover tmp files: {leftover:?}");
    }

    /// A `DurableWrite` wrapper around a real temp-file handle that
    /// records, in order, when `write_all` and `sync_all` are called --
    /// and, at the moment `sync_all` runs, whether the FINAL destination
    /// path already exists. If `sync_all` ever observed the destination
    /// already present, that would mean the rename had already happened,
    /// i.e. the implementation renamed before syncing -- exactly the
    /// ordering bug this helper exists to rule out. Recording the
    /// destination's existence from *inside* the wrapped `sync_all` call
    /// (rather than merely asserting call order in the abstract) is what
    /// makes this a mechanical proof of "fsync before rename", not just
    /// "fsync was called somewhere".
    struct RecordingFile {
        inner: fs::File,
        dest: PathBuf,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl Write for RecordingFile {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.events.lock().unwrap().push("write");
            self.inner.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    impl DurableWrite for RecordingFile {
        fn sync_all(&self) -> io::Result<()> {
            // The load-bearing assertion: at the instant `sync_all` is
            // invoked, the destination must NOT exist yet -- proving the
            // rename has not happened. A pre-fsync implementation could
            // still pass a test that only checks CALL ORDER in the
            // abstract; this checks an actual filesystem side effect at
            // the moment of the call.
            assert!(
                !self.dest.exists(),
                "sync_all was called after the destination already existed -- \
                 rename happened before fsync"
            );
            self.events.lock().unwrap().push("sync_all");
            self.inner.sync_all()
        }
    }

    #[test]
    fn sync_all_happens_before_rename() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("sidecar.json");
        let events: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

        let dest_for_open = dest.clone();
        let events_for_open = events.clone();
        let result = atomic_write_with(&dest, b"payload", move |tmp_path| {
            let inner = fs::File::create(tmp_path)?;
            Ok(RecordingFile {
                inner,
                dest: dest_for_open.clone(),
                events: events_for_open.clone(),
            })
        });
        result.unwrap();

        // The destination exists, with the right content, once
        // `atomic_write_with` has returned -- the rename did happen.
        assert_eq!(fs::read(&dest).unwrap(), b"payload");

        // And it happened in the order this function promises: write,
        // then sync, then (implicitly, per the in-callback assertion
        // above) rename.
        assert_eq!(*events.lock().unwrap(), vec!["write", "sync_all"]);
    }

    #[test]
    fn failure_to_open_temp_file_returns_err_and_touches_nothing() {
        // A parent that does not exist and cannot be created (its own
        // parent is a FILE, not a directory) makes `create_dir_all` fail
        // before any temp file is even attempted.
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, b"i am a file, not a directory").unwrap();
        let target = blocker.join("nested/sidecar.json");

        let err = atomic_write(&target, b"hi").unwrap_err();
        assert!(!target.exists());
        // Cross-platform: some form of "not a directory"/"not found"-ish
        // I/O error; the exact `ErrorKind` varies by OS, so this only
        // pins that an `Err` is surfaced, not swallowed.
        let _ = err;
    }
}
