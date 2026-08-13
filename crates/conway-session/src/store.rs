//! `JsonlSessionStore`: append-only, one-file-per-session backing storage
//! (architecture §4.4, §7 "Module: conway-session").
//!
//! WI-047 implements `open`/`open_with`, the `SessionStore` trait impl
//! (`create`/`append`/`read`/`head`/`meta`), the fsync policy, and
//! crash-tolerant reads. `fork` (WI-048) delegates to
//! `crate::fork::fork_impl`, which in turn calls `create` — so `create` is
//! the single place `children`/`list` updates need to be wired in for both
//! paths. `children`/`list` (WI-050) delegate to `SessionIndex`, an
//! in-memory, no-I/O read; `create` calls `SessionIndex::record_header`
//! after the header write succeeds, and `open_with` builds the index via
//! `SessionIndex::load_or_rebuild`.
//!
//! Layout: `root/<session_id>.jsonl`, one file per session, no
//! subdirectories. `root/index.jsonl` (WI-050) is skipped by every
//! directory scan here (a session id never parses as the literal string
//! `index`, so the skip is implicit in the id-parse step, not a special
//! case).

use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex as AsyncMutex, RwLock as AsyncRwLock};
use tokio::time::Instant;

use chrono::Utc;
use conway_core::error::StoreError;
use conway_core::ids::{LogSeq, SeqRange, SessionId};
use conway_core::log::{LogRecord, SessionFilter, SessionMeta};
use conway_core::ports::{LiveOwner, SessionStore};

use crate::codec;
use crate::index::SessionIndex;

/// Durability policy for `append`. Header writes (`create`, and `fork` once
/// WI-048 lands) always fsync immediately, independent of this policy — see
/// [`JsonlSessionStore::create`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsyncPolicy {
    /// `sync_data()` after every write.
    Always,
    /// `sync_data()` only if at least this much time has elapsed since the
    /// session's last sync, except `agent_result` records, which always
    /// sync immediately regardless of elapsed time.
    Interval(Duration),
    /// Never call `sync_data()` for an appended record, `agent_result`
    /// included. (Header writes are unaffected — see the type-level doc.)
    Never,
}

impl Default for FsyncPolicy {
    fn default() -> Self {
        FsyncPolicy::Interval(Duration::from_millis(200))
    }
}

/// Store-wide configuration. `lru_capacity` is consumed by
/// [`crate::resolver::TranscriptResolver`] (WI-049); `JsonlSessionStore`
/// itself only reads `fsync`.
#[derive(Debug, Clone)]
pub struct StoreConfig {
    pub fsync: FsyncPolicy,
    pub lru_capacity: usize,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            fsync: FsyncPolicy::default(),
            lru_capacity: 64,
        }
    }
}

/// One session's open file plus the in-memory state needed to serve
/// `append`/`read`/`head`/`meta` without re-scanning the file on every
/// call. Guarded by a per-session `Mutex` — never a store-wide lock.
struct SessionFile {
    file: File,
    meta: SessionMeta,
    /// Exclusive upper bound on assigned seqs (= `records.len()`).
    head: LogSeq,
    records: Vec<LogRecord>,
    last_fsync: Instant,
    /// Bytes written since the last `sync_data` — the background flusher
    /// syncs dirty idle handles so a session that stops appending is never
    /// left un-fsynced longer than the interval (WI-047 review S1).
    dirty: bool,
    /// Set by `remove` under this session's own mutex, before the file is
    /// unlinked. An `append` that already cloned the handle `Arc` before
    /// the tombstone was published (and so can still reach this mutex)
    /// checks this flag after acquiring it and fails `NotFound` instead of
    /// writing to the unlinked inode and reporting a record as durably
    /// stored when it was silently discarded (review F-1).
    removed: bool,
}

/// Entry in the `handles` map.
enum Handle {
    /// A live per-session write handle.
    Live(Arc<AsyncMutex<SessionFile>>),
    /// Removal tombstone left by `remove`. A cold-open racing the delete
    /// may have opened the session file just before `remove_file` unlinked
    /// it; without the tombstone its insert below would resurrect a warm
    /// handle for a purged session and later appends would write to the
    /// unlinked inode while returning `Ok` (review F-1). The tombstone
    /// makes every handle acquisition for the purged id fail `NotFound`.
    /// A later `create` of the same id (practically impossible — ids are
    /// ULIDs) simply overwrites it.
    ///
    /// Tombstones accumulate for the store's lifetime: each entry is
    /// `SessionId`-sized and bounded by user purge activity, and they are
    /// deliberately never reaped because a cold-open racing the purge may
    /// still be in flight when `remove` returns.
    Removed,
}

/// A cloned live per-session handle, opaque to callers. Exists only so
/// tests can hold a raw handle Arc across a `remove` and then drive the
/// append path through it (see
/// [`JsonlSessionStore::clone_handle_for_test`] /
/// [`JsonlSessionStore::append_via_raw_handle`]). Test-only
/// instrumentation, not part of the public store contract.
#[doc(hidden)]
pub struct RawSessionHandle(Arc<AsyncMutex<SessionFile>>);

/// One `.jsonl`-per-session, append-only session store.
///
/// Each session has its own write-lock (`Arc<Mutex<SessionFile>>`) held in
/// `handles`; the outer `RwLock` is only ever held for the brief
/// map-lookup/insert, never across file I/O, so N sessions append with N
/// independent locks and no store-wide contention.
///
/// ## Lock order
///
/// Outermost first: `lifecycle` → `handles` → the per-session
/// `SessionFile` mutex → `SessionIndex::state` (a `std::sync::RwLock`,
/// never held across an `.await`). No code path holds two of these locks
/// at once out of this order — `SessionIndex::state` is always a leaf,
/// acquired and released synchronously — so there is no lock-ordering
/// inversion.
///
/// `lifecycle` is taken only by `create`/`fork`/`remove`/`set_ephemeral`
/// and is held
/// across remove's guard-check-plus-delete AND across fork's
/// head-check-plus-create (and plain `create`'s file-write-plus-index-
/// record — the spawn path in `conway-runtime` creates children through
/// `create` directly, so fork-only serialization would leave the same
/// orphan window open there) — and across `set_ephemeral`'s guard-check-
/// plus-header-rewrite (so a promote and a purge of the same session
/// linearize: the purge fails `NotFound`, or the promote lands first and
/// the purge fails `NotRemovable` on the flipped header). This closes the TOCTOU in which a remove's
/// children check could miss a concurrently created child, orphaning it
/// with dangling provenance (P-2, review F-1), and serializes
/// `SessionIndex::remove`'s `persist_full` rewrite against a concurrent
/// `record_header` append (review F-2). The hot path (`append`/`read`/
/// `head`/`meta`) never touches `lifecycle`, preserving per-session
/// append concurrency.
pub struct JsonlSessionStore {
    root: PathBuf,
    handles: Arc<AsyncRwLock<HashMap<SessionId, Handle>>>,
    /// Store-wide lifecycle serialization for `create`/`fork`/`remove` —
    /// see the type-level "Lock order" docs. `pub(crate)` so
    /// `crate::fork::fork_impl` can hold it across its head-check plus the
    /// delegated create.
    pub(crate) lifecycle: AsyncMutex<()>,
    fsync: FsyncPolicy,
    fsync_count: Arc<AtomicU64>,
    /// Total lines read across every cold-open full-file scan performed by
    /// [`get_or_open_handle`](Self::get_or_open_handle). A warm (already
    /// cached) handle contributes nothing here — this is what lets WI-048's
    /// fork tests assert `fork` performs zero parent reads when the parent
    /// handle is pre-warmed. Test-only instrumentation via
    /// [`lines_scanned`](Self::lines_scanned), not part of the public store
    /// contract.
    lines_scanned: Arc<AtomicU64>,
    /// Background flusher for `FsyncPolicy::Interval` (None otherwise);
    /// aborted on drop. Holds only a `Weak` to `handles`, so a dropped
    /// store also ends the task naturally.
    flusher: Option<tokio::task::JoinHandle<()>>,
    /// The derived `children`/`list` accelerator (WI-050). Built once at
    /// `open_with` time via rebuild-by-scan or an existing `index.jsonl`;
    /// kept current by `record_header` calls from `create` (which `fork`
    /// also goes through). Never a source of truth.
    index: Arc<SessionIndex>,
}

impl Drop for JsonlSessionStore {
    fn drop(&mut self) {
        if let Some(task) = self.flusher.take() {
            task.abort();
        }
        // Best-effort index durability on shutdown (spec: `flush(root)` on
        // store drop); a lost tail is healed by rebuild-by-scan on next
        // open, so failure here is deliberately ignored.
        let _ = self.index.flush_sync(&self.root);
    }
}

impl std::fmt::Debug for JsonlSessionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonlSessionStore")
            .field("root", &self.root)
            .field("fsync", &self.fsync)
            .finish_non_exhaustive()
    }
}

fn io_err(e: std::io::Error) -> StoreError {
    StoreError::Io {
        detail: e.to_string(),
    }
}

/// Background flusher for `FsyncPolicy::Interval`: ticks at `interval` and
/// `sync_data`s any dirty handle whose last sync is older than `interval`,
/// so an idle session's tail write is never left un-fsynced longer than the
/// interval (WI-047 review S1). Exits when the store is dropped (the
/// `Weak` fails to upgrade) or the task is aborted.
async fn flush_idle_handles(
    handles: std::sync::Weak<AsyncRwLock<HashMap<SessionId, Handle>>>,
    fsync_count: Arc<AtomicU64>,
    interval: Duration,
    index: std::sync::Weak<SessionIndex>,
    root: PathBuf,
) {
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let Some(map) = handles.upgrade() else { return };
        let snapshot: Vec<Arc<AsyncMutex<SessionFile>>> = map
            .read()
            .await
            .values()
            .filter_map(|h| match h {
                Handle::Live(h) => Some(Arc::clone(h)),
                Handle::Removed => None,
            })
            .collect();
        drop(map);
        for handle in snapshot {
            let mut sf = handle.lock().await;
            if sf.dirty && sf.last_fsync.elapsed() >= interval && sf.file.sync_data().await.is_ok()
            {
                fsync_count.fetch_add(1, Ordering::Relaxed);
                sf.last_fsync = Instant::now();
                sf.dirty = false;
            }
        }
        // Index durability rides the same tick (spec: `flush(root)` from
        // the interval flusher). Best-effort — the index is a cache, and a
        // lost tail is healed by rebuild-by-scan on next open.
        if let Some(idx) = index.upgrade() {
            let _ = idx.flush(&root).await;
        }
    }
}

/// One logical line of a session file's raw content, with byte offsets into
/// that content. `end` is the offset immediately after this line's `\n`
/// (or `content.len()` if the line has no trailing `\n`, which is only
/// possible for the very last line).
struct ScannedLine<'a> {
    text: &'a str,
    end: usize,
    has_newline: bool,
}

fn scan_lines(content: &str) -> Vec<ScannedLine<'_>> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let len = content.len();
    while start < len {
        match content[start..].find('\n') {
            Some(rel) => {
                let nl = start + rel;
                lines.push(ScannedLine {
                    text: &content[start..nl],
                    end: nl + 1,
                    has_newline: true,
                });
                start = nl + 1;
            }
            None => {
                lines.push(ScannedLine {
                    text: &content[start..],
                    end: len,
                    has_newline: false,
                });
                start = len;
            }
        }
    }
    lines
}

/// The result of scanning one session file's content: the parsed header,
/// the complete records recovered from it, and (if the trailing line was
/// damaged) the byte length to truncate the file to plus how many bytes
/// were dropped.
struct Recovered {
    meta: SessionMeta,
    records: Vec<LogRecord>,
    truncate: Option<(u64, u64)>,
}

/// Parses a session file's full content into a header and its complete
/// trailing records, tolerating a damaged *final* line (repaired by the
/// caller via `set_len` + a `tracing::warn!`). A damaged header, or a
/// damaged non-final record line, is `StoreError::Corrupt`.
fn recover(sid: &SessionId, content: &str) -> Result<Recovered, StoreError> {
    let lines = scan_lines(content);
    let Some(header_line) = lines.first() else {
        return Err(StoreError::Corrupt {
            session: *sid,
            line: 0,
            detail: "empty session file".into(),
        });
    };
    let meta = codec::decode_header(header_line.text).map_err(|e| StoreError::Corrupt {
        session: *sid,
        line: 0,
        detail: e.to_string(),
    })?;

    let mut records = Vec::new();
    let mut expected = 0u64;
    let mut good_end = header_line.end as u64;
    let mut truncate = None;

    for (idx, line) in lines.iter().enumerate().skip(1) {
        let is_last = idx == lines.len() - 1;

        if !line.has_newline {
            // Only the final line can lack a trailing `\n`; a mid-write
            // crash truncated it, complete or not.
            debug_assert!(is_last);
            let dropped = content.len() as u64 - good_end;
            truncate = Some((good_end, dropped));
            break;
        }

        match codec::decode_record(line.text) {
            Ok((seq, rec)) => {
                if seq.0 != expected {
                    return Err(StoreError::Corrupt {
                        session: *sid,
                        line: idx as u64,
                        detail: format!("non-contiguous seq: expected {expected}, got {}", seq.0),
                    });
                }
                records.push(rec);
                expected += 1;
                good_end = line.end as u64;
            }
            Err(e) => {
                if is_last {
                    let dropped = content.len() as u64 - good_end;
                    truncate = Some((good_end, dropped));
                    break;
                }
                return Err(StoreError::Corrupt {
                    session: *sid,
                    line: idx as u64,
                    detail: e.to_string(),
                });
            }
        }
    }

    Ok(Recovered {
        meta,
        records,
        truncate,
    })
}

/// Returns `rec` with its `seq` field overwritten to `seq`, satisfying
/// `codec::encode_record`'s debug-mode invariant that the caller's record
/// already carries the seq being assigned (the store — not the caller — is
/// the seq authority: `append` always assigns the next value regardless of
/// whatever seq the caller's `rec` happened to carry).
fn assign_seq(rec: LogRecord, seq: LogSeq) -> Result<LogRecord, StoreError> {
    let mut value = serde_json::to_value(&rec).map_err(|e| StoreError::Io {
        detail: format!("append: record failed to serialize: {e}"),
    })?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "seq".to_string(),
            serde_json::to_value(seq).expect("LogSeq always serializes to JSON"),
        );
    }
    serde_json::from_value(value).map_err(|e| StoreError::Io {
        detail: format!("append: record failed to deserialize after seq assignment: {e}"),
    })
}

impl JsonlSessionStore {
    /// Opens `root`, creating it recursively if absent. Never reads or
    /// modifies any session file — session state is loaded lazily, per
    /// session, on first access.
    pub async fn open(root: PathBuf) -> Result<Self, StoreError> {
        Self::open_with(root, StoreConfig::default()).await
    }

    /// As [`open`](Self::open), with an explicit [`StoreConfig`].
    pub async fn open_with(root: PathBuf, cfg: StoreConfig) -> Result<Self, StoreError> {
        tokio::fs::create_dir_all(&root).await.map_err(io_err)?;
        let index = Arc::new(SessionIndex::load_or_rebuild(&root).await?);
        let handles: Arc<AsyncRwLock<HashMap<SessionId, Handle>>> =
            Arc::new(AsyncRwLock::new(HashMap::new()));
        let fsync_count = Arc::new(AtomicU64::new(0));
        let flusher = match cfg.fsync {
            FsyncPolicy::Interval(interval) => Some(tokio::spawn(flush_idle_handles(
                Arc::downgrade(&handles),
                Arc::clone(&fsync_count),
                interval,
                Arc::downgrade(&index),
                root.clone(),
            ))),
            _ => None,
        };
        Ok(Self {
            root,
            handles,
            lifecycle: AsyncMutex::new(()),
            fsync: cfg.fsync,
            fsync_count,
            lines_scanned: Arc::new(AtomicU64::new(0)),
            flusher,
            index,
        })
    }

    fn session_path(&self, sid: &SessionId) -> PathBuf {
        self.root.join(format!("{sid}.jsonl"))
    }

    /// Path to the cross-process liveness sidecar (S1 follow-up). Deliberately
    /// NOT `.jsonl`-suffixed so `SessionIndex`'s directory scan (which filters
    /// by `extension == "jsonl"`) never mistakes it for a session file.
    fn live_marker_path(&self) -> PathBuf {
        self.root.join(".conway-live")
    }

    fn map_open_err(&self, e: std::io::Error, sid: &SessionId) -> StoreError {
        if e.kind() == ErrorKind::NotFound {
            StoreError::NotFound { session: *sid }
        } else {
            io_err(e)
        }
    }

    /// Returns the per-session handle, opening and (if the trailing line
    /// was damaged by a crash) repairing the file on first access. Only
    /// ever reads/writes the one session's file — never a store-wide scan.
    async fn get_or_open_handle(
        &self,
        sid: &SessionId,
    ) -> Result<Arc<AsyncMutex<SessionFile>>, StoreError> {
        match self.handles.read().await.get(sid) {
            Some(Handle::Live(h)) => return Ok(Arc::clone(h)),
            // Removal tombstone: the session was purged; never resurrect a
            // handle for it (see `Handle::Removed`).
            Some(Handle::Removed) => return Err(StoreError::NotFound { session: *sid }),
            None => {}
        }

        // Cold path: open, read, and (if needed) repair the file WITHOUT
        // holding the map lock — a slow cold-open must never block lookups
        // or cold-opens of unrelated sessions (WI-047 review S2). Two
        // concurrent cold-opens of the same session may both do this work;
        // the insert below is first-wins and the loser's read-only handle
        // is dropped before any write happens through it.
        let path = self.session_path(sid);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .append(true)
            .open(&path)
            .await
            .map_err(|e| self.map_open_err(e, sid))?;

        let mut content = String::new();
        file.read_to_string(&mut content).await.map_err(io_err)?;
        self.lines_scanned
            .fetch_add(scan_lines(&content).len() as u64, Ordering::Relaxed);

        let recovered = recover(sid, &content)?;
        if let Some((new_len, dropped_bytes)) = recovered.truncate {
            file.set_len(new_len).await.map_err(io_err)?;
            tracing::warn!(session = %sid, dropped_bytes, "truncated trailing line");
        }

        let head = LogSeq(recovered.records.len() as u64);
        let sf = SessionFile {
            file,
            meta: recovered.meta,
            head,
            records: recovered.records,
            last_fsync: Instant::now(),
            dirty: false,
            removed: false,
        };
        let arc = Arc::new(AsyncMutex::new(sf));
        let mut handles = self.handles.write().await;
        match handles.get(sid) {
            // Lost a cold-open race: use the winner's handle; ours has
            // performed no writes (repair via set_len is idempotent —
            // both racers computed the same recovery from the same bytes).
            Some(Handle::Live(existing)) => Ok(Arc::clone(existing)),
            // Lost a race with `remove`: the session was purged while we
            // were opening it (possibly before `remove_file` unlinked it).
            // Drop our handle — it points at a dead-or-dying inode.
            Some(Handle::Removed) => Err(StoreError::NotFound { session: *sid }),
            None => {
                handles.insert(*sid, Handle::Live(Arc::clone(&arc)));
                Ok(arc)
            }
        }
    }

    /// The body of [`SessionStore::append`] starting from an already
    /// acquired per-session handle. Factored out so the `removed`-flag
    /// regression tests can drive the append path through a stale,
    /// pre-cloned handle Arc (see [`append_via_raw_handle`]) instead of
    /// relying on the probabilistic barrier race.
    async fn append_with_handle(
        &self,
        sid: &SessionId,
        handle: Arc<AsyncMutex<SessionFile>>,
        rec: LogRecord,
    ) -> Result<LogSeq, StoreError> {
        let mut sf = handle.lock().await;

        // The handle was cloned before `remove` published its tombstone,
        // and `remove` has since marked the session purged under this same
        // mutex (lock order: `handles` → session mutex, type-level docs).
        // Fail rather than write a record to the unlinked inode and report
        // it as stored (review F-1).
        if sf.removed {
            return Err(StoreError::NotFound { session: *sid });
        }

        let seq = sf.head;
        let rec = assign_seq(rec, seq)?;
        let is_agent_result = rec.kind_str() == "agent_result";
        let line = codec::encode_record(&rec, seq);

        sf.file.write_all(line.as_bytes()).await.map_err(io_err)?;

        let should_sync = match self.fsync {
            FsyncPolicy::Always => true,
            FsyncPolicy::Never => false,
            FsyncPolicy::Interval(d) => is_agent_result || sf.last_fsync.elapsed() >= d,
        };
        sf.dirty = true;
        if should_sync {
            sf.file.sync_data().await.map_err(io_err)?;
            sf.last_fsync = Instant::now();
            self.fsync_count.fetch_add(1, Ordering::Relaxed);
            sf.dirty = false;
        } else {
            // `sync_data` (above) implies a flush; when the fsync policy
            // doesn't call for one, still flush so the write has actually
            // reached the OS before `append` returns (`tokio::fs::File`
            // may otherwise leave it in flight on the blocking pool —
            // see `create`'s comment on the same point).
            sf.file.flush().await.map_err(io_err)?;
        }

        sf.records.push(rec);
        sf.head = seq.succ();
        Ok(seq)
    }

    /// Total number of `sync_data()` calls issued by this store so far
    /// (header writes, `append`'s fsync-policy syncs). Test-only
    /// instrumentation, not part of the public store contract.
    #[doc(hidden)]
    pub fn fsync_count(&self) -> u64 {
        self.fsync_count.load(Ordering::Relaxed)
    }

    /// Total lines read across every cold-open full-file scan so far (see
    /// the field doc on [`JsonlSessionStore::lines_scanned`]). WI-048's O(1)
    /// fork tests pre-warm the parent handle, snapshot this counter, call
    /// `fork`, and assert it is unchanged. Test-only instrumentation, not
    /// part of the public store contract.
    #[doc(hidden)]
    pub fn lines_scanned(&self) -> u64 {
        self.lines_scanned.load(Ordering::Relaxed)
    }

    /// Whether `a` and `b` currently have distinct in-memory per-session
    /// write handles — proves `append` never funnels through one
    /// store-wide lock. Test-only, not part of the public store contract.
    #[doc(hidden)]
    pub async fn distinct_handles(&self, a: &SessionId, b: &SessionId) -> bool {
        let handles = self.handles.read().await;
        match (handles.get(a), handles.get(b)) {
            (Some(Handle::Live(ha)), Some(Handle::Live(hb))) => !Arc::ptr_eq(ha, hb),
            _ => false,
        }
    }

    /// Whether `sid`'s handles-map entry is currently a removal tombstone
    /// (see [`Handle::Removed`]). Test-only instrumentation for the F-1
    /// regression tests, not part of the public store contract.
    #[doc(hidden)]
    pub async fn is_removal_tombstoned(&self, sid: &SessionId) -> bool {
        matches!(self.handles.read().await.get(sid), Some(Handle::Removed))
    }

    /// Clones `sid`'s live per-session handle Arc, if one is currently in
    /// the map. Test-only instrumentation for the deterministic F-1
    /// stale-Arc regression test (pair with [`append_via_raw_handle`]),
    /// not part of the public store contract.
    #[doc(hidden)]
    pub async fn clone_handle_for_test(&self, sid: &SessionId) -> Option<RawSessionHandle> {
        match self.handles.read().await.get(sid) {
            Some(Handle::Live(h)) => Some(RawSessionHandle(Arc::clone(h))),
            _ => None,
        }
    }

    /// Performs `append` through a previously cloned [`RawSessionHandle`],
    /// bypassing handle acquisition — deterministically exercising the
    /// `SessionFile::removed` flag check that refuses stale-Arc appends
    /// after a removal (review F-1). Test-only instrumentation, not part
    /// of the public store contract.
    #[doc(hidden)]
    pub async fn append_via_raw_handle(
        &self,
        sid: &SessionId,
        handle: RawSessionHandle,
        rec: LogRecord,
    ) -> Result<LogSeq, StoreError> {
        self.append_with_handle(sid, handle.0, rec).await
    }

    /// The body of [`SessionStore::create`], factored out so
    /// `crate::fork::fork_impl` can call it while already holding
    /// `lifecycle` (taking the lock here too would self-deadlock — a
    /// `tokio::sync::Mutex` is not reentrant). MUST only be called with
    /// `lifecycle` held: the lock is what serializes the file-write +
    /// `record_header` below against a concurrent `remove`'s
    /// guard-check-plus-delete and its `persist_full` rewrite of
    /// `index.jsonl` (review F-1/F-2; lock order on the type docs).
    pub(crate) async fn create_inner(&self, meta: SessionMeta) -> Result<SessionId, StoreError> {
        let sid = meta.id;
        let path = self.session_path(&sid);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .append(true)
            .create_new(true)
            .open(&path)
            .await
            .map_err(|e| {
                if e.kind() == ErrorKind::AlreadyExists {
                    StoreError::AlreadyExists { session: sid }
                } else {
                    io_err(e)
                }
            })?;

        let line = codec::encode_header(&meta);
        file.write_all(line.as_bytes()).await.map_err(io_err)?;
        // `tokio::fs::File` only guarantees a write has reached the OS once
        // the handle is flushed/synced (a bare `write_all().await` may
        // still have the write in flight on the blocking pool). Headers
        // always fsync, regardless of policy, which subsumes the flush.
        file.sync_data().await.map_err(io_err)?;
        self.fsync_count.fetch_add(1, Ordering::Relaxed);

        // Single wiring point for the index (WI-050): `fork` delegates to
        // `create` (see `crate::fork::fork_impl`), so recording here covers
        // both paths without any edit to `fork.rs`. Best-effort — index
        // I/O errors are logged and swallowed inside `record_header`,
        // never surfaced as a `create`/`fork` failure. Ordered against
        // `SessionIndex::remove`'s `persist_full` rewrite by `lifecycle`
        // (held by every caller — see the signature docs), so a full
        // rewrite can never rename over and destroy this appended line
        // (review F-2).
        self.index.record_header(&meta);

        let sf = SessionFile {
            file,
            meta,
            head: LogSeq(0),
            records: Vec::new(),
            last_fsync: Instant::now(),
            dirty: false,
            removed: false,
        };
        // Overwrites a `Handle::Removed` tombstone in the practically
        // impossible case of an id being reused after a purge.
        self.handles
            .write()
            .await
            .insert(sid, Handle::Live(Arc::new(AsyncMutex::new(sf))));
        Ok(sid)
    }
}

#[async_trait]
impl SessionStore for JsonlSessionStore {
    async fn create(&self, meta: SessionMeta) -> Result<SessionId, StoreError> {
        // Lock order (type-level docs): `lifecycle` is the outermost lock.
        // Held across the whole create so a concurrent `remove` of this
        // session's parent either completes first (and, for `fork`, fails
        // the head check) or runs afterwards and sees this child in its
        // guard — and so `record_header` can never interleave with
        // `SessionIndex::remove`'s `persist_full` (review F-1/F-2).
        let _lifecycle = self.lifecycle.lock().await;
        self.create_inner(meta).await
    }

    async fn append(&self, sid: &SessionId, rec: LogRecord) -> Result<LogSeq, StoreError> {
        if matches!(rec, LogRecord::Header(_)) {
            return Err(StoreError::Io {
                detail: "append: cannot append a Header record (use create/fork)".into(),
            });
        }

        let handle = self.get_or_open_handle(sid).await?;
        self.append_with_handle(sid, handle, rec).await
    }

    async fn read(&self, sid: &SessionId, range: SeqRange) -> Result<Vec<LogRecord>, StoreError> {
        let handle = self.get_or_open_handle(sid).await?;
        let sf = handle.lock().await;

        let head = sf.head.0;
        let start = range.start.0.min(head);
        let end = range.end.map(|e| e.0).unwrap_or(head).min(head);
        if start >= end {
            return Ok(Vec::new());
        }
        Ok(sf.records[start as usize..end as usize].to_vec())
    }

    async fn head(&self, sid: &SessionId) -> Result<LogSeq, StoreError> {
        let handle = self.get_or_open_handle(sid).await?;
        let sf = handle.lock().await;
        Ok(sf.head)
    }

    /// Delegates verbatim to [`crate::fork::fork_impl`] (WI-048), which
    /// implements fork-by-reference: a single header write that references
    /// `parent` by `(parent, at_seq, mode)` and copies zero records.
    async fn fork(
        &self,
        parent: &SessionId,
        at: LogSeq,
        meta: SessionMeta,
    ) -> Result<SessionId, StoreError> {
        crate::fork::fork_impl(self, parent, at, meta).await
    }

    async fn meta(&self, sid: &SessionId) -> Result<SessionMeta, StoreError> {
        match self.handles.read().await.get(sid) {
            Some(Handle::Live(h)) => {
                let sf = h.lock().await;
                return Ok(sf.meta.clone());
            }
            Some(Handle::Removed) => return Err(StoreError::NotFound { session: *sid }),
            None => {}
        }

        // Cold path: read only line 0, never the whole file — WI-048's
        // fork relies on this being O(1) in parent size.
        let path = self.session_path(sid);
        let file = File::open(&path)
            .await
            .map_err(|e| self.map_open_err(e, sid))?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        reader.read_line(&mut line).await.map_err(io_err)?;
        if line.trim().is_empty() {
            return Err(StoreError::Corrupt {
                session: *sid,
                line: 0,
                detail: "empty session file".into(),
            });
        }
        codec::decode_header(&line).map_err(|e| StoreError::Corrupt {
            session: *sid,
            line: 0,
            detail: e.to_string(),
        })
    }

    /// Accelerated by `SessionIndex` (WI-050): in-memory lookup, no file
    /// I/O on this path. Hides ephemeral children -- see
    /// `SessionIndex::children`'s doc for why this method has no
    /// `include_ephemeral` opt-in.
    async fn children(&self, sid: &SessionId) -> Result<Vec<SessionId>, StoreError> {
        Ok(self.index.children(sid))
    }

    /// Accelerated by `SessionIndex` (WI-050): in-memory lookup, no file
    /// I/O on this path.
    async fn list(&self, filter: SessionFilter) -> Result<Vec<SessionMeta>, StoreError> {
        Ok(self.index.list(&filter))
    }

    /// Guarded purge — see the trait-level doc for the full guard matrix,
    /// including the facade-layer live-session (`rt.tree()`) check that
    /// deliberately does not live at this layer.
    async fn remove(&self, sid: &SessionId) -> Result<(), StoreError> {
        // Lock order (type-level docs): `lifecycle` is the outermost lock,
        // held across the whole guard-check-plus-delete. A `fork`/`create`
        // racing this remove therefore either completes first (the Guard-2
        // list below sees the new child and refuses) or starts after (for
        // `fork`, the parent head check fails `NotFound` on the tombstone)
        // — the pair can never produce an orphaned child with dangling
        // provenance (P-2, review F-1). The same hold orders
        // `SessionIndex::remove`'s `persist_full` against a concurrent
        // `record_header` append (review F-2).
        let _lifecycle = self.lifecycle.lock().await;

        // Guard 1: purge is for ephemeral sessions only (P-2/GP-10's single
        // explicit exception to mandatory provenance retention).
        let meta = self.meta(sid).await?;
        if !meta.ephemeral {
            return Err(StoreError::NotRemovable {
                session: *sid,
                reason: "session is not ephemeral (purge is only for ephemeral sessions)".into(),
            });
        }

        // Guard 2: ANY children block removal. Queried via `list` with
        // `include_ephemeral: true`, never via `children()` — the latter
        // hides ephemeral children and would orphan them.
        let kids = self
            .list(SessionFilter {
                parent: Some(*sid),
                include_ephemeral: true,
                ..Default::default()
            })
            .await?;
        if !kids.is_empty() {
            return Err(StoreError::NotRemovable {
                session: *sid,
                reason: format!("session has {} child session(s)", kids.len()),
            });
        }

        // Publish the removal tombstone BEFORE touching the file: from
        // this point no new handle can be acquired for `sid` — neither
        // from the map nor from a cold-open that raced the delete and
        // opened the file just before `remove_file` unlinks it (see
        // `Handle::Removed`).
        let prev = self.handles.write().await.insert(*sid, Handle::Removed);

        // An append that cloned the handle Arc before the tombstone can
        // still reach the per-session mutex. Mark the session removed
        // under that mutex: any such append either completed before this
        // point (linearized before the removal — fine, the whole session
        // is being deleted) or acquires the mutex after, sees the flag,
        // and fails `NotFound` instead of writing to the unlinked inode
        // and returning `Ok` for a silently lost record (review F-1).
        //
        // The interval flusher may also hold a snapshot Arc of this handle
        // (see `flush_idle_handles`): a `sync_data` from it still lands on
        // the now-unlinked inode and is harmlessly swallowed by the OS —
        // deliberately no synchronization against that race.
        if let Some(Handle::Live(handle)) = &prev {
            handle.lock().await.removed = true;
        }

        if let Err(e) = tokio::fs::remove_file(self.session_path(sid)).await {
            // ENOENT is NOT an error here: the file already being gone
            // (e.g. deleted externally) is exactly the purge outcome, so
            // fall through to index eviction. Mapping it to `NotFound`
            // would wedge the session — tombstone published (so every
            // data path fails `NotFound`) but the index entry survives
            // (so it stays listed) and a retry hits the tombstone in
            // Guard 1, making it un-removable until restart.
            if e.kind() != ErrorKind::NotFound {
                // Any other io error means the file very likely still
                // exists: roll the removal back so the session stays
                // usable AND removable (a permanent tombstone over a
                // surviving file + index entry is the wedge above).
                // Restoring `prev` wholesale is safe because `lifecycle`
                // is still held — no create can have overwritten the
                // tombstone, and a cold-open that saw it failed
                // `NotFound` without inserting — and the flag is cleared
                // before re-publishing the handle (lock order: `handles`
                // → session mutex, never the reverse).
                if let Some(Handle::Live(handle)) = &prev {
                    handle.lock().await.removed = false;
                }
                let mut handles = self.handles.write().await;
                match prev {
                    Some(h) => {
                        handles.insert(*sid, h);
                    }
                    None => {
                        handles.remove(sid);
                    }
                }
                return Err(io_err(e));
            }
        }

        // Index eviction + persist, best-effort under the same failure
        // policy as `record_header`: a failed persist leaves `index.jsonl`
        // disagreeing with disk, which `load_or_rebuild` self-heals (with
        // a WARN) on next open. Serialized against `record_header` by
        // `lifecycle` (still held), so the `persist_full` rewrite can
        // never clobber a concurrent create's appended line (review F-2).
        if let Err(e) = self.index.remove(sid).await {
            tracing::warn!(
                session = %sid,
                error = %e,
                "index eviction after remove failed to persist; will be reconciled by rebuild-by-scan on next open"
            );
        }
        Ok(())
    }

    /// The guarded one-way header flip — see the trait-level doc for the
    /// full guard matrix. Durably rewrites line 0 of the session file with
    /// `ephemeral: false`, updates the in-memory `SessionFile::meta`, and
    /// re-records the header in the index.
    async fn set_ephemeral(&self, sid: &SessionId, ephemeral: bool) -> Result<(), StoreError> {
        // Lock order (type-level docs): `lifecycle` is the outermost lock,
        // held across the whole guard-check-plus-rewrite. A `remove` racing
        // this promote therefore either completes first (the handle
        // acquisition below fails `NotFound` on the tombstone) or runs
        // after and sees the flipped header — its Guard 1 then refuses the
        // purge (`NotRemovable`) because the session is no longer
        // ephemeral. The pair can never both succeed. The same hold
        // serializes the index `update_header`'s `persist_full` rewrite
        // against a concurrent `create`'s `record_header` append, exactly
        // as `remove` does (review F-2).
        let _lifecycle = self.lifecycle.lock().await;

        // Guard 0: demotion (persistent -> ephemeral) does not exist —
        // promotion is one-way, so a persistent record can never silently
        // become purge-eligible scratchpad (P-2).
        if ephemeral {
            return Err(StoreError::NotPromotable {
                session: *sid,
                reason: "demotion (ephemeral false -> true) is not supported; promotion is one-way"
                    .into(),
            });
        }

        let handle = self.get_or_open_handle(sid).await?;
        let mut sf = handle.lock().await;

        // Same stale-Arc refusal as `append_with_handle` — unreachable while
        // `lifecycle` is held (`remove` takes it too), but cheap, and keeps
        // the "never write through a removed handle" invariant local to the
        // handle rather than relying on the caller's lock discipline alone.
        if sf.removed {
            return Err(StoreError::NotFound { session: *sid });
        }

        // Guard 1: only a true -> false flip is a promote. A no-op on a
        // non-ephemeral session would silently mask a double promote or a
        // caller bug.
        if !sf.meta.ephemeral {
            return Err(StoreError::NotPromotable {
                session: *sid,
                reason: "session is not ephemeral".into(),
            });
        }

        let mut new_meta = sf.meta.clone();
        new_meta.ephemeral = false;

        // Crash-window ordering (cycle-5 B3 review): delete the persisted
        // index BEFORE the header rename below. `try_load`'s consistency
        // check compares only id SETS, so a crash between the rename and
        // the index rewrite would otherwise leave a loadable-but-stale
        // `ephemeral: true` line that mis-hides the promoted session
        // forever (a mid-remove crash produces an id-set MISMATCH and
        // self-heals; a mid-promote crash produces matching sets with
        // stale content and never does). With the delete first, a crash at
        // any point leaves NO index on disk, and rebuild-by-scan reads the
        // session files' own headers — old header (the promote didn't
        // happen) or new (it did), both correct.
        self.index.invalidate_persisted().await;

        // The header rewrite, crash-atomic via tmp + fsync + rename — the
        // same discipline `SessionIndex::persist_full` uses for
        // `index.jsonl`. The new line 0 is followed by every record byte
        // copied VERBATIM from the live file (P-2: promotion rewrites
        // nothing except the flag — record lines are not even re-encoded).
        // An in-place overwrite of line 0 is impossible here: the promoted
        // header serializes one byte LONGER than the ephemeral one
        // (`"ephemeral":true` -> `"ephemeral":false`), so it can never fit
        // back into the same span — and a mid-write crash of an in-place
        // rewrite would corrupt record bytes the store's crash recovery
        // (trailing-line truncation only, see `recover`) cannot heal. The
        // rename either happens or it doesn't: a crash before it leaves the
        // original ephemeral header fully intact (the promote simply fails
        // and can be retried), and a stray temp file left behind is skipped
        // by every directory scan (non-`.jsonl` extension) and overwritten
        // by the next promote.
        //
        // Reading the raw bytes under the session mutex is what makes the
        // verbatim copy complete: an `append` only pushes to `sf.records`
        // after its write+flush has fully landed, and no append can be in
        // flight while this mutex is held, so the file on disk is exactly
        // the current header plus every record this store has ever
        // acknowledged.
        let path = self.session_path(sid);
        let tmp_path = self.root.join(format!("{sid}.promote.tmp"));
        let raw = tokio::fs::read(&path).await.map_err(io_err)?;
        let header_len = raw
            .iter()
            .position(|b| *b == b'\n')
            .map(|pos| pos + 1)
            .ok_or_else(|| StoreError::Corrupt {
                session: *sid,
                line: 0,
                detail: "session file has no newline-terminated header".into(),
            })?;

        let rewrite = async {
            let mut tmp = File::create(&tmp_path).await.map_err(io_err)?;
            tmp.write_all(codec::encode_header(&new_meta).as_bytes())
                .await
                .map_err(io_err)?;
            tmp.write_all(&raw[header_len..]).await.map_err(io_err)?;
            // Headers always fsync (same durability class as `create`'s
            // header write, regardless of the append-path fsync policy).
            tmp.sync_data().await.map_err(io_err)?;
            drop(tmp);
            tokio::fs::rename(&tmp_path, &path).await.map_err(io_err)
        };
        if let Err(e) = rewrite.await {
            // Best-effort cleanup; a leftover temp file is harmless (see
            // the comment above) but tidy when the failure is recoverable.
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(e);
        }
        self.fsync_count.fetch_add(1, Ordering::Relaxed);

        // The rename detached the inode this handle's `file` still points
        // at: without this swap, every later `append` would write to the
        // unlinked inode while reporting success — the exact failure mode
        // `Handle::Removed` exists to prevent for purge. Reopen the path
        // (now the rewritten file) in the same append mode `create` uses.
        // The interval flusher cannot be mid-`sync_data` on the old fd: it
        // locks this same session mutex.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .append(true)
            .open(&path)
            .await
            .map_err(io_err)?;
        sf.file = file;
        sf.meta = new_meta.clone();
        sf.last_fsync = Instant::now();
        sf.dirty = false;

        // Index upsert + `persist_full`, under the same `lifecycle` hold as
        // `remove`'s eviction (review F-2). Never fails the promote — the
        // session file (source of truth) is already durably flipped, and
        // `update_header` converts a persist failure into a forced
        // rebuild-by-scan on next open rather than surfacing it here (see
        // its doc for why warn-and-swallow alone would be silently wrong).
        self.index.update_header(&new_meta).await;
        Ok(())
    }

    // Cross-process liveness sidecar (S1 follow-up to B5). The sweep reads
    // this to avoid reaping another process's open modal-ask child; the TUI
    // refreshes it on a heartbeat and clears it on shutdown. Decoupled from
    // the session files: no `.jsonl` extension (so the dir scan skips it),
    // no header rewrite (so heartbeats are a cheap single-file write, not the
    // O(transcript) crash-atomic rewrite `set_ephemeral` does), and no
    // `lifecycle` lock — the marker never interacts with `create`/`fork`/
    // `remove`/`set_ephemeral` and needs no serialization against them.
    async fn live_owner(&self) -> Result<Option<LiveOwner>, StoreError> {
        let raw = match tokio::fs::read(self.live_marker_path()).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_err(e)),
        };
        // A corrupt or half-written marker decodes to None, not an error:
        // "I can't tell whether anyone is alive" is read as "nobody is" (reap
        // residue, the cold-start behavior). This matches the trait doc and
        // keeps a botched sidecar from wedging the sweep.
        #[derive(serde::Deserialize)]
        struct RawMarker {
            pid: u32,
            heartbeat: chrono::DateTime<chrono::Utc>,
        }
        let marker: RawMarker = match serde_json::from_slice(&raw) {
            Ok(m) => m,
            Err(_) => return Ok(None),
        };
        Ok(Some(LiveOwner {
            pid: marker.pid,
            heartbeat: marker.heartbeat,
        }))
    }

    async fn touch_live_owner(&self, pid: u32) -> Result<(), StoreError> {
        // Atomic write (tmp + fsync + rename) so a crash mid-write never
        // leaves a half-decoded marker that would make `live_owner` spuriously
        // return None — the same discipline `persist_full` and the promote
        // header rewrite use. A missing tmp from a prior failed touch is
        // overwritten harmlessly.
        #[derive(serde::Serialize)]
        struct RawMarker<'a> {
            pid: u32,
            heartbeat: &'a chrono::DateTime<chrono::Utc>,
        }
        let now = Utc::now();
        let path = self.live_marker_path();
        let tmp_path = self.root.join(".conway-live.tmp");
        let body = serde_json::to_vec(&RawMarker {
            pid,
            heartbeat: &now,
        })
        .map_err(|e| StoreError::Io {
            detail: format!("encode live marker: {e}"),
        })?;
        let write = async {
            let mut tmp = File::create(&tmp_path).await.map_err(io_err)?;
            tmp.write_all(&body).await.map_err(io_err)?;
            tmp.sync_data().await.map_err(io_err)?;
            drop(tmp);
            tokio::fs::rename(&tmp_path, &path).await.map_err(io_err)
        };
        if let Err(e) = write.await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(e);
        }
        Ok(())
    }

    async fn clear_live_owner(&self) -> Result<(), StoreError> {
        // ENOENT is Ok — the marker is a liveness cache, not a durable
        // record; its absence is the desired end state (clean shutdown,
        // already cleared, or never written). Any other IO error surfaces.
        if let Err(e) = tokio::fs::remove_file(self.live_marker_path()).await {
            if e.kind() != ErrorKind::NotFound {
                return Err(io_err(e));
            }
        }
        Ok(())
    }
}
