//! `JsonlSessionStore`: append-only, one-file-per-session backing storage
//! (architecture §4.4, §7 "Module: conway-session").
//!
//! WI-047 implements `open`/`open_with`, the `SessionStore` trait impl
//! (`create`/`append`/`read`/`head`/`meta`), the fsync policy, and
//! crash-tolerant reads. `fork` is a documented `Err` placeholder — WI-048
//! replaces it with `crate::fork::fork_impl`. `children`/`list` are minimal
//! correct header-scan implementations — WI-050 replaces the scan with an
//! accelerated `SessionIndex` without needing to touch this file's public
//! surface.
//!
//! Layout: `root/<session_id>.jsonl`, one file per session, no
//! subdirectories. `root/index.jsonl` is reserved for WI-050 and is skipped
//! by every directory scan here (a session id never parses as the literal
//! string `index`, so the skip is implicit in the id-parse step, not a
//! special case).

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

use conway_core::error::StoreError;
use conway_core::ids::{LogSeq, SeqRange, SessionId};
use conway_core::log::{LogRecord, SessionFilter, SessionMeta};
use conway_core::ports::SessionStore;

use crate::codec;

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
}

/// One `.jsonl`-per-session, append-only session store.
///
/// Each session has its own write-lock (`Arc<Mutex<SessionFile>>`) held in
/// `handles`; the outer `RwLock` is only ever held for the brief
/// map-lookup/insert, never across file I/O, so N sessions append with N
/// independent locks and no store-wide contention.
pub struct JsonlSessionStore {
    root: PathBuf,
    handles: Arc<AsyncRwLock<HashMap<SessionId, Arc<AsyncMutex<SessionFile>>>>>,
    fsync: FsyncPolicy,
    fsync_count: Arc<AtomicU64>,
    /// Background flusher for `FsyncPolicy::Interval` (None otherwise);
    /// aborted on drop. Holds only a `Weak` to `handles`, so a dropped
    /// store also ends the task naturally.
    flusher: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for JsonlSessionStore {
    fn drop(&mut self) {
        if let Some(task) = self.flusher.take() {
            task.abort();
        }
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
    handles: std::sync::Weak<AsyncRwLock<HashMap<SessionId, Arc<AsyncMutex<SessionFile>>>>>,
    fsync_count: Arc<AtomicU64>,
    interval: Duration,
) {
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let Some(map) = handles.upgrade() else { return };
        let snapshot: Vec<Arc<AsyncMutex<SessionFile>>> =
            map.read().await.values().cloned().collect();
        drop(map);
        for handle in snapshot {
            let mut sf = handle.lock().await;
            if sf.dirty && sf.last_fsync.elapsed() >= interval {
                if sf.file.sync_data().await.is_ok() {
                    fsync_count.fetch_add(1, Ordering::Relaxed);
                    sf.last_fsync = Instant::now();
                    sf.dirty = false;
                }
            }
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
        let handles: Arc<AsyncRwLock<HashMap<SessionId, Arc<AsyncMutex<SessionFile>>>>> =
            Arc::new(AsyncRwLock::new(HashMap::new()));
        let fsync_count = Arc::new(AtomicU64::new(0));
        let flusher = match cfg.fsync {
            FsyncPolicy::Interval(interval) => Some(tokio::spawn(flush_idle_handles(
                Arc::downgrade(&handles),
                Arc::clone(&fsync_count),
                interval,
            ))),
            _ => None,
        };
        Ok(Self {
            root,
            handles,
            fsync: cfg.fsync,
            fsync_count,
            flusher,
        })
    }

    fn session_path(&self, sid: &SessionId) -> PathBuf {
        self.root.join(format!("{sid}.jsonl"))
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
        if let Some(h) = self.handles.read().await.get(sid) {
            return Ok(Arc::clone(h));
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
        };
        let arc = Arc::new(AsyncMutex::new(sf));
        let mut handles = self.handles.write().await;
        if let Some(existing) = handles.get(sid) {
            // Lost a cold-open race: use the winner's handle; ours has
            // performed no writes (repair via set_len is idempotent —
            // both racers computed the same recovery from the same bytes).
            return Ok(Arc::clone(existing));
        }
        handles.insert(*sid, Arc::clone(&arc));
        Ok(arc)
    }

    async fn scan_all_headers(&self) -> Result<Vec<SessionMeta>, StoreError> {
        let mut out = Vec::new();
        let mut rd = tokio::fs::read_dir(&self.root).await.map_err(io_err)?;
        while let Some(entry) = rd.next_entry().await.map_err(io_err)? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(sid) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse::<SessionId>().ok())
            else {
                // Not a session-id-shaped filename — includes `index.jsonl`
                // (WI-050), which never parses as a ULID.
                continue;
            };
            if let Ok(m) = self.meta(&sid).await {
                out.push(m);
            }
        }
        Ok(out)
    }

    /// Total number of `sync_data()` calls issued by this store so far
    /// (header writes, `append`'s fsync-policy syncs). Test-only
    /// instrumentation, not part of the public store contract.
    #[doc(hidden)]
    pub fn fsync_count(&self) -> u64 {
        self.fsync_count.load(Ordering::Relaxed)
    }

    /// Whether `a` and `b` currently have distinct in-memory per-session
    /// write handles — proves `append` never funnels through one
    /// store-wide lock. Test-only, not part of the public store contract.
    #[doc(hidden)]
    pub async fn distinct_handles(&self, a: &SessionId, b: &SessionId) -> bool {
        let handles = self.handles.read().await;
        match (handles.get(a), handles.get(b)) {
            (Some(ha), Some(hb)) => !Arc::ptr_eq(ha, hb),
            _ => false,
        }
    }
}

#[async_trait]
impl SessionStore for JsonlSessionStore {
    async fn create(&self, meta: SessionMeta) -> Result<SessionId, StoreError> {
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

        let sf = SessionFile {
            file,
            meta,
            head: LogSeq(0),
            records: Vec::new(),
            last_fsync: Instant::now(),
            dirty: false,
        };
        self.handles
            .write()
            .await
            .insert(sid, Arc::new(AsyncMutex::new(sf)));
        Ok(sid)
    }

    async fn append(&self, sid: &SessionId, rec: LogRecord) -> Result<LogSeq, StoreError> {
        if matches!(rec, LogRecord::Header(_)) {
            return Err(StoreError::Io {
                detail: "append: cannot append a Header record (use create/fork)".into(),
            });
        }

        let handle = self.get_or_open_handle(sid).await?;
        let mut sf = handle.lock().await;

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

    /// Not implemented here: WI-048 owns fork semantics and replaces this
    /// with a delegation to `crate::fork::fork_impl`. This placeholder
    /// exists only so the trait is fully implemented in the interim.
    async fn fork(
        &self,
        _parent: &SessionId,
        _at: LogSeq,
        _meta: SessionMeta,
    ) -> Result<SessionId, StoreError> {
        Err(StoreError::Io {
            detail: "fork-by-reference lands in WI-048".into(),
        })
    }

    async fn meta(&self, sid: &SessionId) -> Result<SessionMeta, StoreError> {
        if let Some(h) = self.handles.read().await.get(sid) {
            let sf = h.lock().await;
            return Ok(sf.meta.clone());
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

    /// Minimal correct implementation: scans every `root/*.jsonl` header.
    /// WI-050 replaces this with an accelerated in-memory index.
    async fn children(&self, sid: &SessionId) -> Result<Vec<SessionId>, StoreError> {
        let mut headers = self.scan_all_headers().await?;
        headers.sort_by(|a, b| a.created.cmp(&b.created).then(a.id.cmp(&b.id)));
        Ok(headers
            .into_iter()
            .filter(|m| m.origin.as_ref().is_some_and(|o| o.parent == *sid))
            .map(|m| m.id)
            .collect())
    }

    /// Minimal correct implementation: scans every `root/*.jsonl` header.
    /// WI-050 replaces this with an accelerated in-memory index.
    async fn list(&self, filter: SessionFilter) -> Result<Vec<SessionMeta>, StoreError> {
        let mut metas = self.scan_all_headers().await?;
        metas.retain(|m| {
            filter
                .agent_def
                .as_ref()
                .is_none_or(|v| m.agent_def.as_deref() == Some(v.as_str()))
                && filter
                    .label
                    .as_ref()
                    .is_none_or(|v| m.labels.iter().any(|l| l == v))
                && filter.status.is_none_or(|s| m.status == s)
                && filter
                    .parent
                    .as_ref()
                    .is_none_or(|p| m.origin.as_ref().is_some_and(|o| o.parent == *p))
        });
        metas.sort_by(|a, b| b.created.cmp(&a.created).then(a.id.cmp(&b.id)));
        if let Some(limit) = filter.limit {
            metas.truncate(limit);
        }
        Ok(metas)
    }
}
