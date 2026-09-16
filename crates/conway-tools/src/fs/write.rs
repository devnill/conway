//! `WriteTool`: the `write` tool — atomic whole-file replacement via a
//! sibling temp file plus rename.
//!
//! This module is also the ONE workspace-wide implementation of the
//! "write to a temp sibling, `fsync` it, rename over the target" durable
//! atomic-write protocol (board item `01M2M5KVC9J7MWY56DQK5YPXNR`,
//! consolidating board item `01M1WVNPYTFF0TEJGHGGRPDF8E`'s partial work).
//!
//! # Why this crate is the canonical home
//!
//! Before this item, the protocol existed twice: once here (async,
//! `tokio::fs`-based) and once in `conway::fs` (sync, `std::fs`-based, its
//! own `tmp_sibling`). Both got the ordering half right (`sync_all` before
//! `rename`), and NEITHER synced the parent directory after the rename --
//! so a crash between the rename and its own directory-entry update
//! reaching disk could still lose the rename, even though the renamed
//! file's bytes were themselves durable (see [`atomic_write`]'s own doc
//! for the full mechanism this closes).
//!
//! [`atomic_write`] (this function, sync, `std::fs`-based) is now the only
//! body that does the actual create-dirs/write/fsync/rename/fsync-parent
//! sequence. Two thin, differently-shaped entry points sit on top of it,
//! for the two contexts that need this primitive and cannot share a
//! calling convention:
//!
//! - `atomic_write_str` (this module, `pub(crate)`, async, returns
//!   `Result<u64, ToolError>`): what `beneath::write_file_atomic`'s
//!   `Access::Unconfined` branch needs to satisfy `Tool::invoke`'s async,
//!   `ToolError`-returning contract. Runs [`atomic_write`] inside
//!   `tokio::task::spawn_blocking`, the same way `beneath`'s OWN
//!   `Access::Confined` branch already runs its sync, `cap_std`-based body
//!   -- so the two branches of that dispatch are now symmetric in shape,
//!   not just in outcome.
//! - `conway::fs::atomic_write` (a different crate, `crates/conway/src/
//!   fs.rs`, sync, returns `io::Result<()>`): the entry point three
//!   sync, non-`Tool` callers use (`conway-cli`'s `session_names::
//!   NamesStore::save` and `tui::history::save`, and
//!   `conway-plugin-names`'s `FsAgentNames::persist` -- none of the three
//!   may depend on this crate directly: see that module's own doc for
//!   why). Calls [`atomic_write`] directly, no async machinery at all --
//!   deliberately: at least one of those three call sites (`conway-cli`'s
//!   `sessions name` command) runs synchronously on a `#[tokio::main]`
//!   worker thread with no `spawn_blocking` wrapper of its own, where
//!   spinning up a nested `tokio::runtime::Runtime` to bridge to an async
//!   implementation would panic ("Cannot start a runtime from within a
//!   runtime"). A plain sync function sidesteps that hazard entirely, and
//!   works identically for an embedder who never starts a runtime at all.
//!   Gated `#[cfg(feature = "builtin-tools")]` there, because this is its
//!   only implementation and that feature is what pulls this crate in.
//!
//! `tmp_sibling` is the one temp-name scheme both entry points share
//! (via [`atomic_write`]) and `beneath::write_file_atomic_confined` reuses
//! directly for its own, `cap_std`-relative temp file.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;

use conway_core::content::{PermissionClass, ToolCall, ToolCategory, ToolSpec, TruncationPolicy};
use conway_core::error::ToolError;
use conway_core::ids::ToolName;
use conway_core::ports::{PathArgs, RenderKind, Tool, ToolCtx, ToolOutput};

use crate::common::{check_cancel, parse_args, resolve_path, text_output};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
}

/// Writes `content` to `path`, creating parent directories as needed and
/// replacing any existing content, atomically (temp file + rename).
#[derive(Debug, Default)]
pub struct WriteTool;

impl WriteTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for WriteTool {
    /// `WriteArgs::path` is the only path argument (`content` is file data,
    /// not a path). Note this is the case that forbids a plain
    /// `fs::canonicalize` containment check: a write target legitimately
    /// does not exist yet.
    fn path_args(&self) -> PathArgs {
        PathArgs::Named(&["path"])
    }

    /// `write` never overrides `render`, so its rendering is always the
    /// trait's own default JSON dump -- never a shell command.
    ///.
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("write"),
            description: "Write a file's contents, replacing it if it exists".into(),
            schema: schemars::schema_for!(WriteArgs),
            category: ToolCategory::Edit,
            permission: PermissionClass::RequiresApproval,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        check_cancel(&ctx)?;
        let args: WriteArgs = parse_args(&call)?;
        let path = resolve_path(&ctx, &args.path)?;

        // `[S1.5]`/(retirement): open-relative, so the
        // containment check and the actual write are one step -- see
        // `crate::fs::beneath`'s own doc. Unconfined (no root configured)
        // delegates straight to `atomic_write_str` below, byte-for-byte the
        // pre-existing behavior.
        let bytes = crate::fs::beneath::write_file_atomic(&ctx, &path, &args.content).await?;

        Ok(text_output(
            format!("wrote {bytes} bytes to {}", path.display()),
            TruncationPolicy::None,
        ))
    }
}

/// Durably writes `bytes` to `path`: creates `path`'s parent directories as
/// needed, writes `bytes` to a sibling temp file in the SAME directory (so
/// the final `rename` stays on one filesystem and is therefore atomic --
/// no cross-device copy), `sync_all`s that temp file (an `fsync`, flushing
/// it to the underlying storage device) BEFORE renaming it over `path`,
/// then `sync_all`s `path`'s PARENT DIRECTORY too (a second `fsync`,
/// flushing the directory-entry update the rename itself performed) before
/// returning -- and removes the temp file on any failure along the way
/// (best effort -- a failed cleanup is not itself reported, since the
/// original error is the one the caller needs).
///
/// The `fsync`-before-`rename` ordering is what keeps a crash from ever
/// exposing `path` pointing at incomplete or garbage content: without it,
/// the temp file's own dirty pages might still be sitting in the OS page
/// cache, unflushed, when the rename is observed by a reader after a
/// restart. The parent-directory `sync_all` AFTER the rename is the other
/// half of the same guarantee, for the rename itself: a `rename` is only
/// durable once the directory entry it updates has itself reached the
/// underlying device -- until then, a crash can still leave the OLD
/// directory entry in place even though the new file's bytes were fully
/// flushed, silently undoing an apparently-successful write. See
/// `tests::sync_all_happens_before_rename` and
/// `tests::parent_directory_synced_after_rename` for mechanical proof of
/// each ordering, not just that every step happens -- both are pinned as
/// CALL ORDER, which is everything user-space can assert; neither is nor
/// claims to be a crash/power-loss simulation (not portably falsifiable
/// from user space at all).
///
/// A reader can never observe a partially-written `path`: it is either
/// still the complete old file (nothing here touched it yet, or the
/// write/sync/rename failed before `rename` completed) or already the
/// complete new one (rename succeeded, which -- on every platform this
/// workspace targets -- replaces the destination as a single filesystem
/// operation).
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    atomic_write_with(
        path,
        bytes,
        |p| std::fs::File::create(p),
        |p| std::fs::File::open(p),
    )
}

/// `atomic_write`'s body, generic over how the temp file and the parent
/// directory are opened -- production always passes `fs::File::create`/
/// `fs::File::open`; `tests` substitutes wrappers that record when
/// `write_all`/`sync_all` are called (and, for the directory wrapper,
/// whether the final destination already exists at that moment), so the
/// ordering this function promises is checkable rather than merely
/// asserted in prose.
fn atomic_write_with<F: DurableWrite, D: DurableSync>(
    path: &Path,
    bytes: &[u8],
    open_tmp: impl FnOnce(&Path) -> std::io::Result<F>,
    open_dir: impl FnOnce(&Path) -> std::io::Result<D>,
) -> std::io::Result<()> {
    use std::fs;
    use std::io::Write;

    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }

    let tmp = tmp_sibling(path);

    let write_result: std::io::Result<()> = (|| {
        let mut file = open_tmp(&tmp)?;
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

    // The durability half of the protocol the rename alone does not
    // provide: fsync the PARENT directory so the directory-entry update
    // `rename` just performed is itself flushed to the underlying device,
    // not just visible to other processes on this machine. Nothing here
    // needs cleaning up on failure -- the rename already committed, so
    // `path` already has the right content; only the crash-survival
    // guarantee for that fact is what this step is establishing.
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let dir = open_dir(parent)?;
    dir.sync_all()?;

    Ok(())
}

/// A seam over "a writable file that can be durably flushed": implemented
/// for `std::fs::File` in production (`sync_all` delegating to the
/// inherent, real-`fsync` method of the same name) and, in `tests`, for a
/// wrapper that records call order -- `sync_all` is an inherent method on
/// `fs::File`, not part of `io::Write`, so a trait is what makes it
/// substitutable at all.
trait DurableWrite: std::io::Write {
    fn sync_all(&self) -> std::io::Result<()>;
}

impl DurableWrite for std::fs::File {
    fn sync_all(&self) -> std::io::Result<()> {
        std::fs::File::sync_all(self)
    }
}

/// A seam over "a directory handle that can be durably flushed" -- the
/// parent-directory half of `atomic_write_with`'s durability guarantee.
/// Unlike `DurableWrite`, this does not extend `io::Write`: a directory
/// handle is only ever fsynced here, never written to.
trait DurableSync {
    fn sync_all(&self) -> std::io::Result<()>;
}

impl DurableSync for std::fs::File {
    fn sync_all(&self) -> std::io::Result<()> {
        std::fs::File::sync_all(self)
    }
}

/// The Tool-facing entry point `beneath::write_file_atomic`'s
/// `Access::Unconfined` branch uses: adapts `atomic_write` (sync,
/// `io::Result<()>`, byte slice) to the async, `Result<u64, ToolError>`
/// contract `Tool::invoke` needs, by running it inside
/// `tokio::task::spawn_blocking` -- the same technique
/// `beneath::write_file_atomic_confined` already uses for the
/// `Access::Confined` branch, so both branches of that dispatch now share
/// one shape as well as one underlying implementation. Returns the number
/// of bytes written.
pub(crate) async fn atomic_write_str(path: &Path, content: &str) -> Result<u64, ToolError> {
    let path_buf = path.to_path_buf();
    let display = path.display().to_string();
    let content = content.to_string();
    let len = content.len() as u64;

    tokio::task::spawn_blocking(move || atomic_write(&path_buf, content.as_bytes()))
        .await
        .map_err(|err| ToolError::Io {
            detail: format!("write task panicked: {err}"),
        })?
        .map_err(|err| ToolError::Io {
            detail: format!("failed to write {display}: {err}"),
        })?;

    Ok(len)
}

/// The atomic-write temp sibling for `path`:
/// `<parent>/.<filename>.<pid>.<n>.conway.tmp`. The ONE temp-name scheme
/// `atomic_write` and `beneath::write_file_atomic_confined` both use --
/// see this module's own doc for why there is exactly one now, where
/// there used to be two.
///
/// The pid + per-process counter suffix makes concurrent writers to the
/// same target path use distinct temp inodes, so neither can truncate the
/// other's in-flight content — the last `rename` wins whole-file
/// (incremental review S1, cycle 1). Same-directory placement keeps the
/// final `rename` on one filesystem (atomic, no cross-device copy).
pub(crate) fn tmp_sibling(path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    parent.join(format!(
        ".{filename}.{}.{}.conway.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_ctx;
    use std::fs;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    fn call(arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            call_id: "tc_1".into(),
            name: ToolName::new("write"),
            arguments,
        }
    }

    #[test]
    fn spec_has_expected_name_category_permission() {
        let spec = WriteTool::new().spec();
        assert_eq!(spec.name.as_str(), "write");
        assert_eq!(spec.category, ToolCategory::Edit);
        assert_eq!(spec.permission, PermissionClass::RequiresApproval);
    }

    #[test]
    fn schema_required_and_properties() {
        let spec = WriteTool::new().spec();
        let json = serde_json::to_value(&spec.schema).unwrap();
        let mut required: Vec<&str> = json["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        required.sort();
        assert_eq!(required, vec!["content", "path"]);
        assert_eq!(json["additionalProperties"], false);
    }

    #[tokio::test]
    async fn invoke_creates_parents_and_writes() {
        let dir = TempDir::new().unwrap();
        let (ctx, _h) = test_ctx(dir.path().to_path_buf());
        let out = WriteTool::new()
            .invoke(
                call(serde_json::json!({"path": "dir/does/not/exist/f.txt", "content": "hello"})),
                ctx,
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        let target = dir.path().join("dir/does/not/exist/f.txt");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
    }

    #[tokio::test]
    async fn invoke_second_write_replaces_content() {
        let dir = TempDir::new().unwrap();
        let (ctx, _h) = test_ctx(dir.path().to_path_buf());
        WriteTool::new()
            .invoke(
                call(serde_json::json!({"path": "f.txt", "content": "first"})),
                ctx.clone(),
            )
            .await
            .unwrap();
        WriteTool::new()
            .invoke(
                call(serde_json::json!({"path": "f.txt", "content": "second, longer"})),
                ctx,
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "second, longer"
        );
    }

    #[tokio::test]
    async fn invoke_no_tmp_sibling_remains_after_success() {
        let dir = TempDir::new().unwrap();
        let (ctx, _h) = test_ctx(dir.path().to_path_buf());
        WriteTool::new()
            .invoke(
                call(serde_json::json!({"path": "f.txt", "content": "hi"})),
                ctx,
            )
            .await
            .unwrap();
        let leftover: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".conway.tmp"))
            .collect();
        assert!(leftover.is_empty(), "leftover tmp files: {leftover:?}");
    }

    #[tokio::test]
    async fn invoke_pre_cancelled_returns_cancelled_without_touching_fs() {
        let dir = TempDir::new().unwrap();
        let (ctx, handles) = test_ctx(dir.path().to_path_buf());
        handles.cancel.cancel();
        let err = WriteTool::new()
            .invoke(
                call(serde_json::json!({"path": "f.txt", "content": "hi"})),
                ctx,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Cancelled));
        assert!(!dir.path().join("f.txt").exists());
    }

    /// Driven through this tool's
    /// production `invoke` entry point, not `resolve_path` in isolation.
    #[tokio::test]
    async fn invoke_rejects_nul_byte_in_path() {
        let dir = TempDir::new().unwrap();
        let (ctx, _h) = test_ctx(dir.path().to_path_buf());
        let err = WriteTool::new()
            .invoke(
                call(serde_json::json!({"path": "a\0b", "content": "hi"})),
                ctx,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    // ---- atomic_write (the canonical sync core) ----

    #[test]
    fn writes_bytes_and_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("nested/deep/sidecar.json");
        atomic_write(&target, b"hello").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"hello");
    }

    #[test]
    fn atomic_write_second_write_replaces_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sidecar.json");
        atomic_write(&target, b"first").unwrap();
        atomic_write(&target, b"second, longer").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"second, longer");
    }

    #[test]
    fn atomic_write_no_tmp_sibling_remains_after_success() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sidecar.json");
        atomic_write(&target, b"hi").unwrap();
        let leftover: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".conway.tmp"))
            .collect();
        assert!(leftover.is_empty(), "leftover tmp files: {leftover:?}");
    }

    #[test]
    fn atomic_write_failure_to_open_temp_file_returns_err_and_touches_nothing() {
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
    ///
    /// This PINS THE CALL ORDER ONLY. It cannot and does not prove
    /// survival across an actual crash/power-loss -- forcing that from
    /// user space is not portably falsifiable at all.
    struct RecordingFile {
        inner: fs::File,
        dest: PathBuf,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl std::io::Write for RecordingFile {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.events.lock().unwrap().push("write");
            self.inner.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }

    impl DurableWrite for RecordingFile {
        fn sync_all(&self) -> std::io::Result<()> {
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
        let result = atomic_write_with(
            &dest,
            b"payload",
            move |tmp_path| {
                let inner = fs::File::create(tmp_path)?;
                Ok(RecordingFile {
                    inner,
                    dest: dest_for_open.clone(),
                    events: events_for_open.clone(),
                })
            },
            |dir_path| fs::File::open(dir_path),
        );
        result.unwrap();

        // The destination exists, with the right content, once
        // `atomic_write_with` has returned -- the rename did happen.
        assert_eq!(fs::read(&dest).unwrap(), b"payload");

        // And it happened in the order this function promises: write,
        // then sync, then (implicitly, per the in-callback assertion
        // above) rename.
        assert_eq!(*events.lock().unwrap(), vec!["write", "sync_all"]);
    }

    /// A `DurableSync` wrapper around a real directory handle that
    /// records, at the moment `sync_all` is invoked on it, whether the
    /// FINAL destination file already exists -- proving the rename has
    /// already happened by the time the parent directory is fsynced.
    ///
    /// This PINS THE CALL ORDER ONLY (parent-directory `sync_all` runs
    /// after `rename` is observably complete, and `atomic_write_with`
    /// does not return until that `sync_all` has). It does NOT and cannot
    /// prove survival across an actual crash/power-loss -- see
    /// `RecordingFile`'s own doc immediately above for the identical
    /// caveat about the temp-file half of this same function; forcing a
    /// crash from user space is not portably falsifiable at all.
    struct RecordingDir {
        inner: fs::File,
        dest: PathBuf,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl DurableSync for RecordingDir {
        fn sync_all(&self) -> std::io::Result<()> {
            assert!(
                self.dest.exists(),
                "parent directory sync_all was called before the destination existed -- \
                 the rename has not happened yet"
            );
            self.events.lock().unwrap().push("dir_sync_all");
            self.inner.sync_all()
        }
    }

    #[test]
    fn parent_directory_synced_after_rename() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("sidecar.json");
        let events: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

        let dest_for_dir = dest.clone();
        let events_for_dir = events.clone();
        let result = atomic_write_with(
            &dest,
            b"payload",
            |tmp_path| fs::File::create(tmp_path),
            move |dir_path| {
                let inner = fs::File::open(dir_path)?;
                Ok(RecordingDir {
                    inner,
                    dest: dest_for_dir.clone(),
                    events: events_for_dir.clone(),
                })
            },
        );
        result.unwrap();

        assert_eq!(fs::read(&dest).unwrap(), b"payload");
        // The parent directory's `sync_all` ran exactly once, and (per the
        // in-callback assertion above) only after the rename had already
        // made the destination visible -- and since `atomic_write_with`
        // has now returned `Ok`, it ran before this function returned too.
        assert_eq!(*events.lock().unwrap(), vec!["dir_sync_all"]);
    }

    #[test]
    fn directory_sync_failure_surfaces_err_but_rename_already_landed() {
        // If the parent directory cannot be opened for its post-rename
        // sync (e.g. permissions), the error surfaces -- the rename has
        // already happened, so there is no temp file left to clean up,
        // and the destination keeps its new content regardless.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("sidecar.json");
        let result = atomic_write_with(
            &dest,
            b"payload",
            |tmp_path| fs::File::create(tmp_path),
            |_dir_path| {
                Err::<fs::File, _>(std::io::Error::other("simulated directory open failure"))
            },
        );
        assert!(result.is_err());
        assert_eq!(fs::read(&dest).unwrap(), b"payload");
    }
}
