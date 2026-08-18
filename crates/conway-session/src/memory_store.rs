//! `FsMemoryStore`: durable, mutable, per-memory-file storage backing the
//! `MemoryStore` port (board item `01M09P2T8E5M292WMSMS64CVC4`).
//!
//! ## Layout
//!
//! `<root>/memories/<id-ulid>.json` — one file per memory; body = serde
//! [`Memory`] as JSON (pretty-printed for diffability). The filename IS the
//! memory's own [`MemoryId`] as its ULID string — no new encoding invented,
//! mirroring `FsPathStore`'s "filename IS the key" discipline.
//!
//! ## Why no reverse index, unlike `FsPathStore`
//!
//! `FsPathStore` maintains `paths-index.jsonl` because retention (§4.4) has
//! to answer "which selections reference session S" cheaply, at a scale
//! (every selection ever stored) where a full scan would not do. Nothing
//! about a `MemoryStore` has an analogous query: [`MemoryStore::list`]
//! returns EVERY memory (the caller, `conway-plugin-memory`'s injection
//! hook, filters/bounds it), so a plain directory scan already answers the
//! only read this port defines. `Memory` count is also structurally small
//! -- this port's own removal-is-first-class design (see that trait's own
//! doc) is precisely what keeps it from becoming the kind of unbounded set
//! an index would be worth building for. Adding one anyway would be
//! machinery with no query it serves — the same trap `MemoryStore`'s own
//! module doc names in `PathStore`'s write-once/content-addressed shape not
//! fitting this port either.
//!
//! ## Mutability, unlike `FsPathStore`'s write-once discipline
//!
//! `put` REFUSES a second write under an id already on disk
//! ([`MemoryStoreError::AlreadyExists`]) — not because the content is
//! immutable (it very much is not; see [`MemoryStore::remove`]), but so a
//! caller who wants to REPLACE a memory's text does so as an explicit
//! remove-then-put pair, never a silent overwrite (see the port's own doc
//! on [`MemoryStore::put`]). `remove` deletes the file outright — the
//! actual mutability `PathStore` deliberately has none of.
//!
//! ## Write ordering
//!
//! Mirrors `FsPathStore::put`: tmp file + `sync_data` + atomic rename, so a
//! crash mid-write never leaves a body `get`/`list` would read as corrupt
//! (an unreadable/partially-written tmp file is simply never renamed into
//! place).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;

use conway_core::error::MemoryStoreError;
use conway_core::ids::MemoryId;
use conway_core::ports::{Memory, MemoryStore};

fn io_err(e: std::io::Error) -> MemoryStoreError {
    MemoryStoreError::Io {
        detail: e.to_string(),
    }
}

/// Per-call counter for unique temp filenames in `put`, mirroring
/// `FsPathStore`'s `PUT_TMP_COUNTER` (see that module's own doc for the
/// concurrent-same-key-write reasoning this disambiguates against, adapted
/// here to concurrent same-id writes).
static PUT_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The filesystem-backed [`MemoryStore`] implementation (board item
/// `01M09P2T8E5M292WMSMS64CVC4`). See this module's own doc for the full
/// layout and the ways it deliberately does NOT mirror `FsPathStore`.
pub struct FsMemoryStore {
    root: PathBuf,
    /// Serialises `put`'s exists-check against its own commit.
    ///
    /// Without this, `put` is a TOCTOU: `try_exists` says "absent", and by
    /// the time `rename` runs another `put` of the SAME id may have created
    /// the file. POSIX `rename` REPLACES its destination silently rather
    /// than failing, so both callers would get `Ok(())` and the later
    /// rename would win -- the earlier caller believing its memory was
    /// stored while it is gone. That directly contradicts this port's
    /// documented contract ("a second `put` reusing an existing id is
    /// `AlreadyExists` -- `put` never silently overwrites").
    ///
    /// `FsPathStore` needs no such lock because it is CONTENT-addressed: a
    /// same-key race there is idempotent by construction, both writers
    /// having identical bytes. A memory is caller-id-addressed with
    /// arbitrary text, so the same race loses data. `InMemoryMemoryStore`
    /// already gets this right by holding its mutex across check-and-insert;
    /// this is the durable store's equivalent.
    ///
    /// `put` is the only writer that needs it: `remove` is a single
    /// `remove_file` (atomic, and losing a race just means `NotFound`),
    /// and `get`/`list` are readers that tolerate a concurrent rename.
    put_lock: Arc<tokio::sync::Mutex<()>>,
}

impl FsMemoryStore {
    /// Opens `root`, creating `root/memories` recursively if absent. No
    /// index to load/rebuild (see this module's own doc).
    pub async fn open(root: PathBuf) -> Result<Self, MemoryStoreError> {
        tokio::fs::create_dir_all(root.join("memories"))
            .await
            .map_err(io_err)?;
        Ok(Self {
            root,
            put_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    fn memory_path(&self, id: &MemoryId) -> PathBuf {
        self.root.join("memories").join(format!("{id}.json"))
    }

    fn map_open_err(&self, e: std::io::Error, id: &MemoryId) -> MemoryStoreError {
        if e.kind() == std::io::ErrorKind::NotFound {
            MemoryStoreError::NotFound { id: *id }
        } else {
            io_err(e)
        }
    }
}

impl std::fmt::Debug for FsMemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsMemoryStore")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

/// Scan `<root>/memories` for memory object files. Each entry's file stem
/// (minus `.json`) is a `MemoryId` ULID string; anything that does not
/// parse as one (a stray `.tmp`, a hand-dropped file) is skipped. Mirrors
/// `FsPathStore`'s `scan_selection_files`.
async fn scan_memory_files(root: &Path) -> Result<Vec<(MemoryId, PathBuf)>, MemoryStoreError> {
    let dir = root.join("memories");
    let mut out = Vec::new();
    let mut rd = match tokio::fs::read_dir(&dir).await {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err(e)),
    };
    while let Some(entry) = rd.next_entry().await.map_err(io_err)? {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(id) = stem.parse::<MemoryId>() else {
            continue;
        };
        out.push((id, path));
    }
    Ok(out)
}

#[async_trait]
impl MemoryStore for FsMemoryStore {
    async fn put(&self, memory: Memory) -> Result<(), MemoryStoreError> {
        let path = self.memory_path(&memory.id);
        // Held across the exists-check AND the rename that commits: see
        // `put_lock`'s own doc for why check-then-rename alone is a TOCTOU
        // that silently violates this port's no-overwrite contract.
        let _commit = self.put_lock.lock().await;
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Err(MemoryStoreError::AlreadyExists { id: memory.id });
        }

        let body = serde_json::to_vec_pretty(&memory).map_err(|e| MemoryStoreError::Io {
            detail: format!("put: memory failed to serialize: {e}"),
        })?;
        let tmp_path = self.root.join("memories").join(format!(
            "{}.{}.{}.tmp",
            memory.id,
            std::process::id(),
            PUT_TMP_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        {
            let mut tmp = File::create(&tmp_path).await.map_err(io_err)?;
            tmp.write_all(&body).await.map_err(io_err)?;
            tmp.sync_data().await.map_err(io_err)?;
            drop(tmp);
        }
        tokio::fs::rename(&tmp_path, &path).await.map_err(io_err)?;
        Ok(())
    }

    async fn get(&self, id: &MemoryId) -> Result<Memory, MemoryStoreError> {
        let path = self.memory_path(id);
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => return Err(self.map_open_err(e, id)),
        };
        serde_json::from_slice(&bytes).map_err(|e| MemoryStoreError::Io {
            detail: format!("memory {id} corrupt: {e}"),
        })
    }

    async fn list(&self) -> Result<Vec<Memory>, MemoryStoreError> {
        let files = scan_memory_files(&self.root).await?;
        let mut out = Vec::with_capacity(files.len());
        for (id, path) in files {
            let bytes = tokio::fs::read(&path).await.map_err(io_err)?;
            match serde_json::from_slice::<Memory>(&bytes) {
                Ok(memory) => out.push(memory),
                Err(e) => {
                    // A single unreadable/corrupt memory must not fail every
                    // OTHER caller's turn -- drop it and warn, mirroring
                    // `FsPathStore`'s rebuild-scan "drop with a WARN" policy
                    // for an unreadable selection body.
                    tracing::warn!(
                        id = %id,
                        path = %path.display(),
                        error = %e,
                        "memory store list: dropping an unreadable memory"
                    );
                }
            }
        }
        Ok(out)
    }

    async fn remove(&self, id: &MemoryId) -> Result<(), MemoryStoreError> {
        let path = self.memory_path(id);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) => Err(self.map_open_err(e, id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway_core::ids::SessionId;
    use conway_core::ports::MemoryProvenance;

    struct Temp {
        dir: tempfile::TempDir,
    }
    impl Temp {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().expect("tempdir"),
            }
        }
        fn root(&self) -> PathBuf {
            self.dir.path().to_path_buf()
        }
    }

    fn memory(text: &str) -> Memory {
        Memory {
            id: MemoryId::new(),
            text: text.to_string(),
            created: chrono::Utc::now(),
            provenance: None,
        }
    }

    /// Concurrent `put`s of the SAME id: exactly one wins, and the loser
    /// is told so.
    ///
    /// The naive implementation — `try_exists`, then write-tmp, then
    /// `rename` — passes every single-threaded test while silently losing
    /// data under concurrency: both callers see "absent", both rename, and
    /// POSIX `rename` REPLACES rather than failing, so both get `Ok(())`
    /// and the last writer wins. The earlier caller believes its memory was
    /// stored. That contradicts the port's documented "never silently
    /// overwrites" contract. Regression test for a review finding; fails
    /// without `put_lock`.
    #[tokio::test]
    async fn concurrent_puts_of_one_id_yield_exactly_one_success() {
        let tmp = Temp::new();
        let store = std::sync::Arc::new(FsMemoryStore::open(tmp.root()).await.unwrap());

        let id = MemoryId::new();
        let mut handles = Vec::new();
        for i in 0..8u32 {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                store
                    .put(Memory {
                        id,
                        text: format!("writer {i}"),
                        created: chrono::Utc::now(),
                        provenance: None,
                    })
                    .await
            }));
        }

        let mut ok = 0usize;
        let mut already = 0usize;
        for h in handles {
            match h.await.unwrap() {
                Ok(()) => ok += 1,
                Err(MemoryStoreError::AlreadyExists { .. }) => already += 1,
                Err(other) => panic!("unexpected error: {other:?}"),
            }
        }
        assert_eq!(ok, 1, "exactly one writer may succeed");
        assert_eq!(
            already, 7,
            "every other writer must be told the id was taken"
        );

        // And the survivor is readable and internally consistent -- not a
        // half-written or interleaved file.
        let back = store.get(&id).await.unwrap();
        assert!(back.text.starts_with("writer "), "got {:?}", back.text);
        assert_eq!(store.list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn put_get_roundtrip() {
        let tmp = Temp::new();
        let store = FsMemoryStore::open(tmp.root()).await.unwrap();
        let m = memory("the deploy secret lives in vault");
        store.put(m.clone()).await.unwrap();
        let back = store.get(&m.id).await.unwrap();
        assert_eq!(back, m);
    }

    #[tokio::test]
    async fn put_roundtrips_provenance() {
        let tmp = Temp::new();
        let store = FsMemoryStore::open(tmp.root()).await.unwrap();
        let mut m = memory("sourced");
        m.provenance = Some(MemoryProvenance {
            session: SessionId::new(),
            range: None,
        });
        store.put(m.clone()).await.unwrap();
        let back = store.get(&m.id).await.unwrap();
        assert_eq!(back.provenance, m.provenance);
    }

    #[tokio::test]
    async fn get_absent_is_not_found() {
        let tmp = Temp::new();
        let store = FsMemoryStore::open(tmp.root()).await.unwrap();
        let err = store.get(&MemoryId::new()).await.unwrap_err();
        assert!(matches!(err, MemoryStoreError::NotFound { .. }));
    }

    #[tokio::test]
    async fn put_a_second_time_under_the_same_id_is_already_exists() {
        let tmp = Temp::new();
        let store = FsMemoryStore::open(tmp.root()).await.unwrap();
        let m = memory("first");
        store.put(m.clone()).await.unwrap();
        let mut second = m.clone();
        second.text = "replacement".to_string();
        let err = store.put(second).await.unwrap_err();
        assert!(matches!(err, MemoryStoreError::AlreadyExists { .. }));
        // The original is untouched -- no silent overwrite.
        assert_eq!(store.get(&m.id).await.unwrap().text, "first");
    }

    #[tokio::test]
    async fn list_returns_every_stored_memory() {
        let tmp = Temp::new();
        let store = FsMemoryStore::open(tmp.root()).await.unwrap();
        let a = memory("a");
        let b = memory("b");
        store.put(a.clone()).await.unwrap();
        store.put(b.clone()).await.unwrap();
        let mut listed = store.list().await.unwrap();
        listed.sort_by_key(|m| m.id);
        let mut expected = vec![a, b];
        expected.sort_by_key(|m| m.id);
        assert_eq!(listed, expected);
    }

    #[tokio::test]
    async fn list_on_an_empty_store_is_empty() {
        let tmp = Temp::new();
        let store = FsMemoryStore::open(tmp.root()).await.unwrap();
        assert!(store.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn remove_retires_a_memory_and_it_stops_appearing() {
        let tmp = Temp::new();
        let store = FsMemoryStore::open(tmp.root()).await.unwrap();
        let m = memory("gone soon");
        store.put(m.clone()).await.unwrap();
        store.remove(&m.id).await.unwrap();
        assert!(matches!(
            store.get(&m.id).await.unwrap_err(),
            MemoryStoreError::NotFound { .. }
        ));
        assert!(store.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn remove_absent_is_not_found() {
        let tmp = Temp::new();
        let store = FsMemoryStore::open(tmp.root()).await.unwrap();
        let err = store.remove(&MemoryId::new()).await.unwrap_err();
        assert!(matches!(err, MemoryStoreError::NotFound { .. }));
    }

    /// A memory naming a source session that no longer exists anywhere is
    /// still fully valid and still returned -- `FsMemoryStore` never
    /// consults `SessionStore` at all (module doc: "provenance is a
    /// reference, not a liveness dependency").
    #[tokio::test]
    async fn a_memory_with_a_dangling_session_reference_is_still_valid_and_listed() {
        let tmp = Temp::new();
        let store = FsMemoryStore::open(tmp.root()).await.unwrap();
        let mut m = memory("purged source");
        // A session id that was never created anywhere -- as good as
        // "purged" from this store's point of view, since it never checks.
        m.provenance = Some(MemoryProvenance {
            session: SessionId::new(),
            range: None,
        });
        store.put(m.clone()).await.unwrap();
        let back = store.get(&m.id).await.unwrap();
        assert_eq!(back, m);
        assert_eq!(store.list().await.unwrap(), vec![m]);
    }

    /// Durability across a reopen -- a real filesystem round trip, not just
    /// an in-process assertion.
    #[tokio::test]
    async fn survives_a_reopen_of_the_same_root() {
        let tmp = Temp::new();
        let m = {
            let store = FsMemoryStore::open(tmp.root()).await.unwrap();
            let m = memory("durable");
            store.put(m.clone()).await.unwrap();
            m
        };
        let reopened = FsMemoryStore::open(tmp.root()).await.unwrap();
        assert_eq!(reopened.get(&m.id).await.unwrap(), m);
    }
}
