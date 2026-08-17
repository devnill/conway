//! `FsPathStore`: content-addressed, write-once backing storage for path
//! selections, plus the derived, rebuildable retention reverse index
//! (DESIGN-context-path §2.6, §2.9, §4.4).
//!
//! ## Layout
//!
//! `<root>/paths/<key-hex>` — one file per selection; body = serde `PathSelection`
//! as JSON (pretty-printed for diffability). The filename IS `SelectionKey::as_str`
//! (the lowercase-hex string itself); no new encoding is invented (§2.3).
//!
//! `<root>/paths-index.jsonl` — the reverse index projection, one JSON object
//! per line: `{"session": "<sid>", "key": "<key-hex>"}`, one line per
//! `(session, key)` pair. A selection referencing sessions S1, S2 yields two
//! lines. Mirrors `SessionIndex`'s `index.jsonl` discipline exactly.
//!
//! ## Reverse-index coverage (§4.4)
//!
//! A selection is keyed in the reverse index by the sessions in its OWN nodes
//! ONLY, NOT transitively the prefix's sessions. The prefix is itself a stored
//! selection with its own index lines, so transitive coverage would
//! double-count the prefix's sessions under the child's key. §4.4's rule —
//! "a selection pins every record it references" — is satisfied because the
//! prefix's own index entry already pins the prefix's sessions; a child that
//! references the prefix transitively reaches those records via the prefix's
//! own pinned entry, not via a duplicated line under the child. Own-only.
//!
//! ## Failure policy
//!
//! The reverse index is a cache. `put`'s index append is best-effort: any I/O
//! error is logged at WARN and swallowed, never propagated into `put`'s
//! `Ok(key)` result (mirrors `SessionIndex::record_header`). `load_or_rebuild`
//! treats an absent, corrupt, or disk-inconsistent `paths-index.jsonl` the
//! same way: rebuild by scanning `<root>/paths/*`, logging a WARN whenever
//! the rebuild was triggered by something other than a first-run absence
//! (mirrors `SessionIndex::load_or_rebuild`).
//!
//! ## Write ordering
//!
//! Within `put`, the selection object file is written (and fsynced) BEFORE
//! the reverse-index lines are appended, so an index never points at a
//! missing body (§2.6's "stored before appended," adapted to the file+index
//! pair). A crash in the window leaves an orphaned body with no index lines;
//! the next open's `load_or_rebuild` consistency check (key-set mismatch)
//! self-heals it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::fs::File;
use tokio::io::AsyncWriteExt;

use conway_core::error::PathStoreError;
use conway_core::ids::SessionId;
use conway_core::path::{PathNode, PathSelection, SelectionKey};
use conway_core::ports::PathStore;
use conway_core::transcript::MAX_ANCESTRY_DEPTH;

fn io_err(e: std::io::Error) -> PathStoreError {
    PathStoreError::Io {
        detail: e.to_string(),
    }
}

/// Per-call counter for unique temp filenames in `put` (see the write-once race
/// note on `put`). `std::process::id()` disambiguates across processes; this
/// counter disambiguates concurrent calls within one process.
static PUT_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The on-disk projection of one `(session, selection-key)` reverse-index
/// entry — one line per pair, mirroring `SessionIndex::IndexLine`'s shape.
#[derive(Debug, Serialize, Deserialize)]
struct PathIndexLine {
    session: SessionId,
    key: SelectionKey,
}

/// In-memory reverse-index state: `session → selection keys` (deduped,
/// insertion-ordered). Reads (`selections_referencing`) return a clone of the
/// list; rebuild/load replaces the whole state.
#[derive(Debug, Default)]
struct PathIndexState {
    by_session: HashMap<SessionId, Vec<SelectionKey>>,
}

impl PathIndexState {
    /// Add `key` under `session`, deduped (a re-record of the same pair is a
    /// no-op, mirroring `IndexState::upsert`'s `!list.contains` guard).
    fn upsert(&mut self, session: SessionId, key: SelectionKey) {
        let list = self.by_session.entry(session).or_default();
        if !list.contains(&key) {
            list.push(key);
        }
    }
}

/// Why `try_load` did not produce a usable index — distinguishes a fresh
/// store (no prior `paths-index.jsonl`, not a failure) from a genuinely
/// corrupt or stale one (rebuild, and warn that it happened).
enum LoadOutcome {
    Missing,
    Invalid(String),
}

/// Scan `<root>/paths` for selection object files. Each entry's file stem is
/// a `SelectionKey` hex string; files whose stem does not parse as one (a
/// future `.tmp`/sidecar) are skipped. Returns `(key, path)` pairs.
async fn scan_selection_files(root: &Path) -> Result<Vec<(SelectionKey, PathBuf)>, PathStoreError> {
    let paths_dir = root.join("paths");
    let mut out = Vec::new();
    let mut rd = match tokio::fs::read_dir(&paths_dir).await {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err(e)),
    };
    while let Some(entry) = rd.next_entry().await.map_err(io_err)? {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(stem) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // The filename IS the key hex; a 64-char lowercase-hex string. Skip
        // anything that does not look like one (temp files, sidecars). A
        // hand-edited store could name a file anything, but the only files
        // `put` ever writes are valid keys, so this filter is exact for
        // sanctioned writes and lenient (skip+warn) for the rest — rebuild
        // reads each surviving file and warns on a decode failure anyway.
        if stem.len() != 64 || !stem.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        let key = SelectionKey(stem.to_string());
        out.push((key, path));
    }
    Ok(out)
}

/// Read and decode the `PathSelection` body at `path`. `None` on any I/O or
/// decode failure (the caller drops and warns — mirrors `read_header`).
async fn read_selection(path: &Path) -> Option<PathSelection> {
    let bytes = tokio::fs::read(path).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// One logical line of `paths-index.jsonl`'s raw content: `(text,
/// had_trailing_newline)`. A final line lacking its trailing `\n` is a
/// truncated write and must be treated as invalid, not silently accepted
/// (mirrors `index.rs::raw_lines`).
fn raw_lines(content: &str) -> Vec<(&str, bool)> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let len = content.len();
    while start < len {
        match content[start..].find('\n') {
            Some(rel) => {
                out.push((&content[start..start + rel], true));
                start += rel + 1;
            }
            None => {
                out.push((&content[start..], false));
                start = len;
            }
        }
    }
    out
}

/// The derived, rebuildable reverse index for `FsPathStore`. Mirrors
/// `SessionIndex`: a `std::sync::RwLock` (never held across an `.await`),
/// best-effort append on `put`, `load_or_rebuild` on absence/corruption/
/// inconsistency (warn on rebuild). Never a source of truth.
#[derive(Debug)]
pub struct PathIndex {
    state: RwLock<PathIndexState>,
    root: PathBuf,
}

impl PathIndex {
    /// Loads `root/paths-index.jsonl`, or rebuilds it by scanning
    /// `root/paths/*` (deserializing each `PathSelection`) if it is absent,
    /// corrupt, or inconsistent with the selection files on disk.
    ///
    /// Rebuild triggers (any one is sufficient): the file is absent; a line
    /// fails to decode; a line's final byte lacks a trailing newline (a
    /// truncated write); the set of keys referenced in the index disagrees
    /// with the set of key files on disk. A rebuild triggered by anything
    /// other than plain absence logs `tracing::warn!(..., "index rebuild")`.
    pub(crate) async fn load_or_rebuild(root: &Path) -> Result<Self, PathStoreError> {
        let state = match Self::try_load(root).await {
            Ok(state) => {
                return Ok(Self {
                    state: RwLock::new(state),
                    root: root.to_path_buf(),
                });
            }
            Err(LoadOutcome::Missing) => Self::rebuild_scan(root).await?,
            Err(LoadOutcome::Invalid(detail)) => {
                tracing::warn!(root = %root.display(), detail = %detail, "index rebuild");
                Self::rebuild_scan(root).await?
            }
        };

        let index = Self {
            state: RwLock::new(state),
            root: root.to_path_buf(),
        };
        if let Err(e) = index.persist_full().await {
            tracing::warn!(
                error = %e,
                "path index rebuild: failed to persist rebuilt paths-index.jsonl (will be rebuilt again next open)"
            );
        }
        Ok(index)
    }

    /// Attempts to load an existing, internally consistent
    /// `paths-index.jsonl`. Any inconsistency is reported via `LoadOutcome`.
    async fn try_load(root: &Path) -> Result<PathIndexState, LoadOutcome> {
        let path = root.join("paths-index.jsonl");
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(LoadOutcome::Missing),
            Err(e) => {
                return Err(LoadOutcome::Invalid(format!(
                    "paths-index.jsonl unreadable: {e}"
                )))
            }
        };

        let mut state = PathIndexState::default();
        let mut seen_keys: HashSet<SelectionKey> = HashSet::new();
        let lines = raw_lines(&content);
        for (idx, (text, had_newline)) in lines.iter().enumerate() {
            let is_last = idx == lines.len() - 1;
            if !had_newline {
                debug_assert!(is_last, "only the final line can lack a trailing newline");
                return Err(LoadOutcome::Invalid(format!(
                    "paths-index.jsonl line {idx} is truncated (no trailing newline)"
                )));
            }
            let line: PathIndexLine = match serde_json::from_str(text) {
                Ok(l) => l,
                Err(e) => {
                    return Err(LoadOutcome::Invalid(format!(
                        "paths-index.jsonl line {idx} failed to decode: {e}"
                    )))
                }
            };
            seen_keys.insert(line.key.clone());
            state.upsert(line.session, line.key);
        }

        // Disk consistency: the set of keys referenced by the index must
        // equal the set of selection files on disk. A mismatch (orphan body,
        // stale line) forces a rebuild — mirroring `SessionIndex::try_load`'s
        // disk-ids-vs-index-ids check.
        let files = scan_selection_files(root)
            .await
            .map_err(|e| LoadOutcome::Invalid(format!("directory scan failed: {e}")))?;
        let disk_keys: HashSet<SelectionKey> = files.iter().map(|(k, _)| k.clone()).collect();
        if disk_keys != seen_keys {
            return Err(LoadOutcome::Invalid(format!(
                "paths-index.jsonl disagrees with disk: {} indexed key(s), {} file(s) on disk",
                seen_keys.len(),
                disk_keys.len()
            )));
        }

        Ok(state)
    }

    /// Rebuild-by-scan: read and decode every selection file under
    /// `root/paths/*`, collecting each distinct `node.record.session` → key.
    /// A file that fails to decode is dropped with a WARN (mirrors
    /// `SessionIndex::rebuild_scan` dropping a session with an unreadable
    /// header).
    async fn rebuild_scan(root: &Path) -> Result<PathIndexState, PathStoreError> {
        let files = scan_selection_files(root).await?;
        let mut state = PathIndexState::default();
        for (key, path) in files {
            match read_selection(&path).await {
                Some(sel) => {
                    // Own-node sessions only (module-level coverage doc).
                    let sessions: HashSet<SessionId> =
                        sel.nodes.iter().map(|n| n.record.session).collect();
                    for sid in sessions {
                        state.upsert(sid, key.clone());
                    }
                }
                None => tracing::warn!(
                    key = %key,
                    path = %path.display(),
                    "path index rebuild: dropping selection with an unreadable body"
                ),
            }
        }
        Ok(state)
    }

    /// Atomically rewrites `paths-index.jsonl` from the current in-memory
    /// state: write `paths-index.jsonl.tmp`, fsync, rename. The only
    /// non-append write this module performs, and it targets only the
    /// derived file, never a selection body. Synchronous snapshot under the
    /// `std::sync::RwLock` read guard (never held across an `.await` — the
    /// snapshot is taken synchronously, the file I/O follows).
    async fn persist_full(&self) -> Result<(), PathStoreError> {
        let entries: Vec<(SessionId, SelectionKey)> = {
            let state = self.state.read().unwrap();
            let mut out: Vec<(SessionId, SelectionKey)> = Vec::new();
            for (sid, keys) in state.by_session.iter() {
                for k in keys {
                    out.push((*sid, k.clone()));
                }
            }
            out
        };

        let mut buf = String::new();
        for (sid, key) in &entries {
            let line = PathIndexLine {
                session: *sid,
                key: key.clone(),
            };
            buf.push_str(&serde_json::to_string(&line).expect("PathIndexLine always serializes"));
            buf.push('\n');
        }

        let tmp_path = self.root.join("paths-index.jsonl.tmp");
        let final_path = self.root.join("paths-index.jsonl");
        let mut file = tokio::fs::File::create(&tmp_path).await.map_err(io_err)?;
        file.write_all(buf.as_bytes()).await.map_err(io_err)?;
        file.sync_data().await.map_err(io_err)?;
        drop(file);
        tokio::fs::rename(&tmp_path, &final_path)
            .await
            .map_err(io_err)?;
        Ok(())
    }

    /// Records a selection's own-node sessions in the in-memory index and
    /// appends one line per distinct session to `paths-index.jsonl`
    /// (best-effort — index I/O errors are logged at WARN and never
    /// propagate; see the module-level failure policy).
    ///
    /// Synchronous append (blocking `std::fs`) — mirrors
    /// `SessionIndex::append_line_sync`'s reasoning: one small, best-effort
    /// line write, never on a durability contract (`paths-index.jsonl` is a
    /// cache, not the source of truth).
    fn record_selection(&self, key: &SelectionKey, sessions: &[SessionId]) {
        {
            let mut state = self.state.write().unwrap();
            for sid in sessions {
                state.upsert(*sid, key.clone());
            }
        }
        if let Err(e) = self.append_lines_sync(key, sessions) {
            tracing::warn!(
                key = %key,
                error = %e,
                "path index append failed; will be reconciled by rebuild-by-scan on next open"
            );
        }
    }

    fn append_lines_sync(&self, key: &SelectionKey, sessions: &[SessionId]) -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("paths-index.jsonl"))?;
        for sid in sessions {
            let line = PathIndexLine {
                session: *sid,
                key: key.clone(),
            };
            let mut json =
                serde_json::to_string(&line).expect("PathIndexLine always serializes to JSON");
            json.push('\n');
            file.write_all(json.as_bytes())?;
        }
        Ok(())
    }

    /// Selection keys whose stored body references any record in `sid`, read
    /// from in-memory state (no file I/O on this path). Ordered by insertion
    /// (the order sessions were first recorded).
    pub(crate) fn selections_referencing(&self, sid: &SessionId) -> Vec<SelectionKey> {
        let state = self.state.read().unwrap();
        state.by_session.get(sid).cloned().unwrap_or_default()
    }
}

/// One file per selection, content-addressed under its expanded
/// `SelectionKey`, plus the derived, rebuildable reverse index. Mirrors
/// `JsonlSessionStore` + `SessionIndex`.
///
/// ## Lock order
///
/// Outermost first: `index.state` (a `std::sync::RwLock`, never held across an
/// `.await`) — always a leaf, acquired and released synchronously. No code
/// path holds a lock across an `.await`. The selection-object file write and
/// the index append are sequential within `put`, never concurrent.
pub struct FsPathStore {
    root: PathBuf,
    index: Arc<PathIndex>,
}

impl FsPathStore {
    /// Opens `root`, creating `root/paths` recursively if absent. Loads (or
    /// rebuilds) the reverse index from `root/paths-index.jsonl`.
    pub async fn open(root: PathBuf) -> Result<Self, PathStoreError> {
        tokio::fs::create_dir_all(&root).await.map_err(io_err)?;
        tokio::fs::create_dir_all(root.join("paths"))
            .await
            .map_err(io_err)?;
        let index = Arc::new(PathIndex::load_or_rebuild(&root).await?);
        Ok(Self { root, index })
    }

    fn selection_path(&self, key: &SelectionKey) -> PathBuf {
        // The filename IS `key.as_str()` — the lowercase-hex string itself.
        self.root.join("paths").join(key.as_str())
    }

    fn map_open_err(&self, e: std::io::Error, key: &SelectionKey) -> PathStoreError {
        if e.kind() == std::io::ErrorKind::NotFound {
            PathStoreError::NotFound { key: key.clone() }
        } else {
            io_err(e)
        }
    }

    /// Walk the prefix chain of `selection` upward, fetching each prefix from
    /// THIS store, and return the fully expanded node list (prefix nodes
    /// first, then `selection`'s own nodes). Bounded by
    /// `MAX_ANCESTRY_DEPTH`; an over-deep chain is
    /// [`PathStoreError::PrefixChainTooDeep`]. An absent prefix surfaces as
    /// [`PathStoreError::NotFound`] (a corrupt/hand-edited store; §2.7).
    async fn expand(&self, selection: &PathSelection) -> Result<Vec<PathNode>, PathStoreError> {
        // Walk upward collecting selections root-first; then flatten.
        let mut chain: Vec<PathSelection> = Vec::new();
        let mut current = selection.clone();
        let mut depth = 0usize;
        loop {
            let prefix_key = match &current.prefix {
                Some(k) => k.clone(),
                None => {
                    chain.push(current);
                    break;
                }
            };
            chain.push(current);
            depth += 1;
            if depth > MAX_ANCESTRY_DEPTH {
                return Err(PathStoreError::PrefixChainTooDeep { depth });
            }
            current = self.get(&prefix_key).await?;
        }
        // `chain` is [selection, prefix1, ..., root]; flatten root-first.
        chain.reverse();
        let mut expanded: Vec<PathNode> = Vec::new();
        for sel in &chain {
            expanded.extend(sel.nodes.iter().cloned());
        }
        Ok(expanded)
    }
}

impl std::fmt::Debug for FsPathStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsPathStore")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl PathStore for FsPathStore {
    async fn put(&self, selection: PathSelection) -> Result<SelectionKey, PathStoreError> {
        // Expand prefix chains, bounded by MAX_ANCESTRY_DEPTH (§2.6).
        let expanded = self.expand(&selection).await?;
        let key = SelectionKey::from_nodes(&expanded);

        let path = self.selection_path(&key);

        // Write-once: if the body already exists, this is a no-op (same key ⇒
        // same expanded selection ⇒ same content). Do NOT overwrite, do NOT
        // error. The index lines were appended on the first `put`; re-appending
        // would duplicate them (harmless — `upsert` dedupes — but wasteful), so
        // a true no-op is the honest fast path. `try_exists` (not the sync
        // `Path::exists`) keeps the fast path off the runtime thread, consistent
        // with the `tokio::fs` body write below.
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Ok(key);
        }

        // Write the selection object file (the body as given, with its prefix
        // reference intact — content-addressed sharing: `prefix(A) ++ [n1]`
        // is stored once under the expanded key, the body references A by
        // key, never duplicating A's nodes). Pretty-printed for diffability.
        // Atomic: tmp + fsync + rename, so a crash never leaves a half-written
        // body that `get` would later read as corrupt.
        let body = serde_json::to_vec_pretty(&selection).map_err(|e| PathStoreError::Io {
            detail: format!("put: selection failed to serialize: {e}"),
        })?;
        let tmp_path = self.root.join("paths").join(format!(
            "{}.{}.{}.tmp",
            key.as_str(),
            std::process::id(),
            PUT_TMP_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        {
            let mut tmp = File::create(&tmp_path).await.map_err(io_err)?;
            tmp.write_all(&body).await.map_err(io_err)?;
            tmp.sync_data().await.map_err(io_err)?;
            drop(tmp);
        }
        // `rename` over an existing file is an atomic replace; if a concurrent
        // `put` of the same key raced and landed first, this overwrites its
        // identical body (same key ⇒ same content), which is acceptable. The
        // temp file name is UNIQUE per call (pid + counter) so two concurrent
        // same-key puts do NOT race on a shared temp file — without this, one
        // `rename` would observe its temp renamed out from under it and return a
        // spurious `Io` error for an operation that succeeded. The write-once
        // guarantee is against a SECOND distinct body under the same key (a
        // hand-edited store, §2.7), not against a benign same-content race.
        tokio::fs::rename(&tmp_path, &path).await.map_err(io_err)?;

        // The selection object is durably stored BEFORE the reverse-index
        // lines are appended, so an index never points at a missing body
        // (§2.6's "stored before appended," adapted to the file+index pair).
        //
        // Own-node sessions only — see the module-level coverage doc.
        let sessions: Vec<SessionId> = {
            let mut seen: HashSet<SessionId> = HashSet::new();
            let mut out: Vec<SessionId> = Vec::new();
            for n in &selection.nodes {
                if seen.insert(n.record.session) {
                    out.push(n.record.session);
                }
            }
            out
        };
        self.index.record_selection(&key, &sessions);

        Ok(key)
    }

    async fn get(&self, key: &SelectionKey) -> Result<PathSelection, PathStoreError> {
        let path = self.selection_path(key);
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) => return Err(self.map_open_err(e, key)),
        };
        serde_json::from_slice::<PathSelection>(&bytes).map_err(|e| PathStoreError::Corrupt {
            key: key.clone(),
            detail: e.to_string(),
        })
    }

    async fn selections_referencing(
        &self,
        sid: &SessionId,
    ) -> Result<Vec<SelectionKey>, PathStoreError> {
        Ok(self.index.selections_referencing(sid))
    }
}

// Re-export so `conway-session`'s lib mirrors its other module exports.
pub use PathIndex as FsPathIndex;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use conway_core::ids::LogSeq;
    use conway_core::path::{NodeStamp, Selector};

    use std::sync::Arc;

    /// A scratch store root shared across one test's calls.
    struct Temp {
        dir: tempfile::TempDir,
    }

    impl Temp {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
            }
        }
        fn root(&self) -> PathBuf {
            self.dir.path().to_path_buf()
        }
    }

    fn node(session: SessionId, seq: u64, stamp: NodeStamp) -> PathNode {
        PathNode {
            record: conway_core::path::RecordRef {
                session,
                seq: LogSeq(seq),
            },
            stamp,
            prov: conway_core::path::NodeProvenance {
                selected_by: Selector::DefaultRule,
                at: Utc::now(),
            },
        }
    }

    fn own(session: SessionId, seq: u64) -> PathNode {
        node(session, seq, NodeStamp::Own)
    }

    /// A `SessionStore`-shaped object-safety check: `Arc<dyn PathStore>`
    /// compiles (mirrors the conway-core port module's own assertion).
    #[allow(dead_code)]
    fn _assert_path_store_object_safe(_: Arc<dyn PathStore>) {}

    /// put→get round-trip (prefix=None): the returned key recomputed from the
    /// same nodes matches `SelectionKey::from_nodes`.
    #[tokio::test]
    async fn put_get_roundtrip_prefix_none() {
        let tmp = Temp::new();
        let store = FsPathStore::open(tmp.root()).await.unwrap();
        let s = SessionId::new();
        let nodes = vec![own(s, 1), own(s, 2)];
        let sel = PathSelection {
            prefix: None,
            nodes: nodes.clone(),
            incoherence: Vec::new(),
        };
        let key = store.put(sel.clone()).await.unwrap();
        assert_eq!(key, SelectionKey::from_nodes(&nodes));
        let got = store.get(&key).await.unwrap();
        assert_eq!(got, sel);
    }

    /// Write-once idempotence: put(K, A); put(K, A again) → Ok, get(K) → A;
    /// the file is not rewritten (mtime/size unchanged).
    #[tokio::test]
    async fn put_is_write_once_idempotent() {
        let tmp = Temp::new();
        let store = FsPathStore::open(tmp.root()).await.unwrap();
        let s = SessionId::new();
        let sel = PathSelection {
            prefix: None,
            nodes: vec![own(s, 1)],
            incoherence: Vec::new(),
        };
        let key = store.put(sel.clone()).await.unwrap();
        let path = tmp.root().join("paths").join(key.as_str());
        let meta1 = std::fs::metadata(&path).unwrap();
        // Second put of the SAME selection: no-op, returns Ok(same key).
        let key2 = store.put(sel.clone()).await.unwrap();
        assert_eq!(key, key2);
        let meta2 = std::fs::metadata(&path).unwrap();
        // File was not rewritten: same length (a rewrite via tmp+rename could
        // preserve size but not mtime; assert both for robustness).
        assert_eq!(meta1.len(), meta2.len());
        // get still returns the original body.
        assert_eq!(store.get(&key).await.unwrap(), sel);
    }

    /// Regression for the write-once race (adversarial review finding #1): N
    /// concurrent `put`s of the SAME expanded selection (same key) must ALL
    /// return `Ok`. A shared deterministic temp filename used to let one
    /// `rename` observe its temp renamed out from under it and return a spurious
    /// `Io` error for an operation that succeeded; the per-call unique temp
    /// name (pid + counter) makes that impossible. Multi-thread runtime to
    /// widen the race window.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_same_key_puts_all_succeed() {
        let tmp = Temp::new();
        let root = tmp.root();
        let store = Arc::new(FsPathStore::open(root.clone()).await.unwrap());
        let s = SessionId::new();
        let sel = PathSelection {
            prefix: None,
            nodes: vec![own(s, 1), own(s, 2), own(s, 3)],
            incoherence: Vec::new(),
        };
        let expected = SelectionKey::from_nodes(&sel.nodes);

        let mut handles = Vec::new();
        for _ in 0..8 {
            let store = store.clone();
            let sel = sel.clone();
            handles.push(tokio::spawn(async move { store.put(sel).await }));
        }
        for h in handles {
            let key = h
                .await
                .expect("concurrent put task joined")
                .expect("concurrent same-key put must not spuriously error");
            assert_eq!(key, expected);
        }
        // Exactly one body on disk, shared by every concurrent put.
        let files = scan_selection_files(&root).await.unwrap();
        assert_eq!(files.len(), 1, "one stored object, shared: {files:?}");
        assert_eq!(store.get(&expected).await.unwrap(), sel);
    }

    /// Content-addressed sharing: two `PathSelection`s that expand to the
    /// same node list → put returns the SAME key → second put is a no-op, get
    /// returns the FIRST body (the §2.6 "ten siblings share one stored object"
    /// property, exercised at the store level).
    #[tokio::test]
    async fn content_addressed_sharing_same_key_for_same_expanded_list() {
        let tmp = Temp::new();
        let store = FsPathStore::open(tmp.root()).await.unwrap();
        let s = SessionId::new();

        // Flat selection: {None, [n1, n2, n3, n4]}.
        let flat = PathSelection {
            prefix: None,
            nodes: vec![own(s, 1), own(s, 2), own(s, 3), own(s, 4)],
            incoherence: Vec::new(),
        };
        let key_flat = store.put(flat.clone()).await.unwrap();

        // First store the prefix selection {None, [n1, n2]}.
        let prefix_sel = PathSelection {
            prefix: None,
            nodes: vec![own(s, 1), own(s, 2)],
            incoherence: Vec::new(),
        };
        let key_p = store.put(prefix_sel).await.unwrap();

        // Chunked selection: {Some(key_p), [n3, n4]} — expands to [n1,n2,n3,n4].
        let chunked = PathSelection {
            prefix: Some(key_p),
            nodes: vec![own(s, 3), own(s, 4)],
            incoherence: Vec::new(),
        };
        let key_chunked = store.put(chunked).await.unwrap();

        // Same expanded node list ⇒ same key.
        assert_eq!(key_flat, key_chunked);
        // The chunked put was a no-op (file already existed from the flat
        // put), so get returns the FIRST body — the flat one.
        assert_eq!(store.get(&key_flat).await.unwrap(), flat);
    }

    /// Prefix expansion: put a prefix=None selection P (key Kp); put
    /// {prefix: Some(Kp), nodes: [extra]}; the second key =
    /// `SelectionKey::from_nodes` over [P.nodes..., extra]; get(that key)
    /// returns the prefixed body; `selections_referencing` covers the
    /// prefixed selection's OWN-node sessions (not the prefix's).
    #[tokio::test]
    async fn prefix_expansion_keys_over_flattened_list() {
        let tmp = Temp::new();
        let store = FsPathStore::open(tmp.root()).await.unwrap();
        let s1 = SessionId::new();
        let s2 = SessionId::new();

        // P: prefix=None, nodes in s1.
        let p = PathSelection {
            prefix: None,
            nodes: vec![own(s1, 1), own(s1, 2)],
            incoherence: Vec::new(),
        };
        let kp = store.put(p.clone()).await.unwrap();

        // Child: prefix=Kp, own nodes in s2.
        let extra = own(s2, 7);
        let child = PathSelection {
            prefix: Some(kp.clone()),
            nodes: vec![extra.clone()],
            incoherence: Vec::new(),
        };
        let kchild = store.put(child.clone()).await.unwrap();

        // The child's key = from_nodes over [p.nodes..., extra].
        let mut expanded = p.nodes.clone();
        expanded.push(extra);
        assert_eq!(kchild, SelectionKey::from_nodes(&expanded));

        // get returns the prefixed body (prefix reference intact).
        let got = store.get(&kchild).await.unwrap();
        assert_eq!(got, child);

        // Reverse index: the child's OWN-node sessions (s2) are covered; the
        // prefix's sessions (s1) are NOT covered under the child's key — they
        // are covered under the prefix's own index entry.
        let refs_s2 = store.selections_referencing(&s2).await.unwrap();
        assert!(refs_s2.contains(&kchild), "child key under s2: {refs_s2:?}");
        let refs_s1 = store.selections_referencing(&s1).await.unwrap();
        assert!(
            !refs_s1.contains(&kchild),
            "prefix sessions must NOT be covered under the child's key (own-only): {refs_s1:?}"
        );
        // The prefix's own entry pins s1.
        assert!(refs_s1.contains(&kp), "prefix key under s1: {refs_s1:?}");
    }

    /// Reverse index: a selection referencing sessions S1, S2 →
    /// `selections_referencing(S1)` and `selections_referencing(S2)` both
    /// include its key; `selections_referencing(absent)` → empty.
    #[tokio::test]
    async fn reverse_index_covers_own_node_sessions() {
        let tmp = Temp::new();
        let store = FsPathStore::open(tmp.root()).await.unwrap();
        let s1 = SessionId::new();
        let s2 = SessionId::new();
        let sel = PathSelection {
            prefix: None,
            nodes: vec![own(s1, 1), own(s2, 2)],
            incoherence: Vec::new(),
        };
        let key = store.put(sel).await.unwrap();

        let refs_s1 = store.selections_referencing(&s1).await.unwrap();
        let refs_s2 = store.selections_referencing(&s2).await.unwrap();
        assert!(refs_s1.contains(&key));
        assert!(refs_s2.contains(&key));

        let absent = SessionId::new();
        let refs_absent = store.selections_referencing(&absent).await.unwrap();
        assert!(refs_absent.is_empty(), "absent session: {refs_absent:?}");
    }

    /// Rebuild: delete `<root>/paths-index.jsonl`; the next
    /// `selections_referencing` call (via a fresh open → `load_or_rebuild`)
    /// triggers rebuild-by-scan → correct results (rebuildable accelerator,
    /// never source of truth).
    #[tokio::test]
    async fn rebuild_after_index_deleted() {
        let tmp = Temp::new();
        let root = tmp.root();

        // First store: put a selection referencing s1.
        let s1 = SessionId::new();
        {
            let store = FsPathStore::open(root.clone()).await.unwrap();
            let sel = PathSelection {
                prefix: None,
                nodes: vec![own(s1, 1)],
                incoherence: Vec::new(),
            };
            let key = store.put(sel).await.unwrap();
            // Sanity: the index file exists.
            assert!(root.join("paths-index.jsonl").exists());
            assert!(store
                .selections_referencing(&s1)
                .await
                .unwrap()
                .contains(&key));
        }

        // Delete the index file.
        std::fs::remove_file(root.join("paths-index.jsonl")).unwrap();

        // Reopen: load_or_rebuild sees absence → rebuild-by-scan → correct.
        let store = FsPathStore::open(root.clone()).await.unwrap();
        let files = scan_selection_files(&root).await.unwrap();
        let key = files[0].0.clone();
        let refs = store.selections_referencing(&s1).await.unwrap();
        assert!(
            refs.contains(&key),
            "rebuild must recover the key: {refs:?}"
        );
        // The rebuilt index was persisted.
        assert!(root.join("paths-index.jsonl").exists());
    }

    /// Rebuild on inconsistency: write a stale `paths-index.jsonl` that
    /// disagrees with disk (names a key with no body) → rebuild.
    #[tokio::test]
    async fn rebuild_on_inconsistency() {
        let tmp = Temp::new();
        let root = tmp.root();
        std::fs::create_dir_all(root.join("paths")).unwrap();
        // A stale index line naming a key with NO body file on disk.
        let fake_key = SelectionKey("a".repeat(64));
        let line = serde_json::to_string(&PathIndexLine {
            session: SessionId::new(),
            key: fake_key,
        })
        .unwrap();
        std::fs::write(root.join("paths-index.jsonl"), format!("{line}\n")).unwrap();

        // Open: try_load sees a key-set mismatch (index has fake_key, disk has
        // zero files) → rebuild → empty index (warned).
        let store = FsPathStore::open(root.clone()).await.unwrap();
        let any = SessionId::new();
        assert!(store.selections_referencing(&any).await.unwrap().is_empty());
    }

    /// Absent get → `NotFound`.
    #[tokio::test]
    async fn get_absent_is_not_found() {
        let tmp = Temp::new();
        let store = FsPathStore::open(tmp.root()).await.unwrap();
        let key = SelectionKey::from_nodes(&[own(SessionId::new(), 1)]);
        let err = store.get(&key).await.unwrap_err();
        assert!(
            matches!(err, PathStoreError::NotFound { .. }),
            "got {err:?}"
        );
    }

    /// Prefix-chain-too-deep: build a chain deeper than `MAX_ANCESTRY_DEPTH`
    /// → `put` returns `PrefixChainTooDeep`.
    #[tokio::test]
    async fn prefix_chain_too_deep() {
        let tmp = Temp::new();
        let store = FsPathStore::open(tmp.root()).await.unwrap();
        let s = SessionId::new();

        // Build a chain K0 ← K1 ← ... ← K_{MAX_ANCESTRY_DEPTH} (257 selections).
        // Putting K_{n} expands n prefix hops; K_{MAX_ANCESTRY_DEPTH} walks
        // exactly MAX_ANCESTRY_DEPTH hops, which is allowed (the resolver's
        // `visited.len() > MAX_ANCESTRY_DEPTH` rule lets a 256-hop chain
        // through). One more selection — prefix K_{MAX_ANCESTRY_DEPTH} — walks
        // MAX_ANCESTRY_DEPTH+1 hops → PrefixChainTooDeep.
        let mut prev_key: Option<SelectionKey> = None;
        for i in 0..=MAX_ANCESTRY_DEPTH {
            let sel = PathSelection {
                prefix: prev_key.clone(),
                nodes: vec![own(s, i as u64)],
                incoherence: Vec::new(),
            };
            prev_key = Some(store.put(sel).await.unwrap());
        }
        // prev_key = K_{MAX_ANCESTRY_DEPTH}. A selection prefixing it walks
        // MAX_ANCESTRY_DEPTH+1 hops → too deep.
        let too_deep = PathSelection {
            prefix: prev_key,
            nodes: vec![own(s, 999)],
            incoherence: Vec::new(),
        };
        let err = store.put(too_deep).await.unwrap_err();
        match err {
            PathStoreError::PrefixChainTooDeep { depth } => {
                assert!(
                    depth > MAX_ANCESTRY_DEPTH,
                    "depth {depth} must exceed {MAX_ANCESTRY_DEPTH}"
                );
            }
            other => panic!("expected PrefixChainTooDeep, got {other:?}"),
        }
    }

    /// `PathIndexLine` round-trips through serde (the on-disk projection
    /// schema), matching `SessionIndex::IndexLine`'s round-trip discipline.
    #[test]
    fn path_index_line_roundtrips() {
        let line = PathIndexLine {
            session: SessionId::new(),
            key: SelectionKey::from_nodes(&[own(SessionId::new(), 1)]),
        };
        let json = serde_json::to_string(&line).unwrap();
        let back: PathIndexLine = serde_json::from_str(&json).unwrap();
        assert_eq!(line.session, back.session);
        assert_eq!(line.key, back.key);
    }

    /// `PathStoreError` round-trips and renders (mirrors error.rs's test
    /// discipline for the other error enums).
    #[test]
    fn path_store_error_roundtrips_and_renders() {
        let key = SelectionKey::from_nodes(&[own(SessionId::new(), 1)]);
        for err in [
            PathStoreError::NotFound { key: key.clone() },
            PathStoreError::Corrupt {
                key: key.clone(),
                detail: "bad json".to_string(),
            },
            PathStoreError::Io {
                detail: "disk gone".to_string(),
            },
            PathStoreError::PrefixChainTooDeep {
                depth: MAX_ANCESTRY_DEPTH + 1,
            },
        ] {
            let json = serde_json::to_string(&err).unwrap();
            let back: PathStoreError = serde_json::from_str(&json).unwrap();
            assert_eq!(err, back);
        }
        assert!(PathStoreError::NotFound { key: key.clone() }
            .to_string()
            .contains(&key.to_string()));
    }
}
