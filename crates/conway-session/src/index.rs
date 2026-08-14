//! `SessionIndex`: the derived, rebuildable index that accelerates `list`,
//! `children`, and tree reconstruction — never a source of truth
//! (architecture §7 "Module: conway-session").
//!
//! ## On-disk form
//!
//! `root/index.jsonl`: one JSON object per line, a projection of the header
//! (never records). The wire schema here adds `cwd` on top of the fields
//! named in the spec prose: `SessionMeta::cwd` is not optional, and
//! without it a `list()`/`children()` result served from a *loaded* (not
//! rebuilt) index would silently return the wrong `cwd` for every session —
//! a real correctness gap the spec's illustrative schema didn't need to
//! call out, since project prose enumerates it loosely ("a projection of
//! the header") rather than as an exhaustive field list.
//!
//! ## In-memory form
//!
//! `IndexState { by_id, children }`, guarded by a `std::sync::RwLock` (not
//! `tokio::sync::RwLock`): `record_header`/`children`/`list` are
//! synchronous per the earlier work-fixed signatures below, so a blocking lock is
//! both correct and simpler — no lock is ever held across an `.await`.
//!
//! ## Failure policy
//!
//! The index is a cache. `record_header`'s `index.jsonl` append is
//! best-effort: any I/O error is logged at WARN and swallowed, never
//! propagated into the caller's `create`/`fork` result (`store.rs` calls
//! `record_header` unconditionally after the session file write has already
//! succeeded). `load_or_rebuild` treats an absent, corrupt, or
//! disk-inconsistent `index.jsonl` the same way: rebuild by scanning
//! `root/*.jsonl` and recover, logging `"index rebuild"` at WARN whenever
//! the rebuild was triggered by something other than a first-run absence.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use conway_core::error::StoreError;
use conway_core::ids::{AgentId, LogSeq, RoleAlias, SessionId};
use conway_core::log::{ForkOrigin, SessionFilter, SessionMeta, SubagentMode};

use crate::codec;

fn io_err(e: std::io::Error) -> StoreError {
    StoreError::Io {
        detail: e.to_string(),
    }
}

/// The on-disk projection of one `SessionMeta` — see the module-level "On-disk
/// form" docs for why `cwd` is included beyond the spec's illustrative schema.
#[derive(Debug, Serialize, Deserialize)]
struct IndexLine {
    session: SessionId,
    agent: AgentId,
    parent: Option<SessionId>,
    at_seq: Option<LogSeq>,
    mode: Option<SubagentMode>,
    created: DateTime<Utc>,
    agent_def: Option<String>,
    role: Option<RoleAlias>,
    cwd: PathBuf,
    #[serde(default)]
    labels: Vec<String>,
    /// Projects `SessionMeta::ephemeral` -- required here (not just on the
    /// session file's own header) so that a `list`/`children` result served
    /// from a *loaded* (not rebuilt) index still hides an ephemeral session
    /// correctly, the same "silently wrong `cwd`" concern the module doc
    /// already calls out for that field.
    #[serde(default)]
    ephemeral: bool,
    /// Projects `SessionMeta::ask_origin` (B5) -- required for the same
    /// reason as `ephemeral` above: the TUI's crash-residue sweep finds
    /// modal-ask leftovers via `list`, and a `list` served from a *loaded*
    /// index that dropped this tag would silently miss every leftover (or,
    /// worse, misclassify one) until the next rebuild.
    #[serde(default)]
    ask_origin: Option<conway_core::log::AskOrigin>,
    /// Projects `SessionMeta::root` (S3) -- required for the same reason as
    /// `ephemeral`/`ask_origin` above: a `list`/`children` result served
    /// from a *loaded* (not rebuilt) index must not silently drop a
    /// session's confinement root.
    ///
    /// **NOT authoritative for any confinement decision.** `#[serde(default)]`
    /// means an `index.jsonl` written before this field existed decodes to
    /// `None` for every session until the next rebuild -- indistinguishable,
    /// here, from a genuinely unconfined session. That staleness window is
    /// the same one `cwd`/`ephemeral`/`ask_origin` already have, but `root`
    /// is security-relevant in a way those are not, so it is called out:
    /// "the index says `None`" must never be read as "this session is
    /// unconfined."
    ///
    /// This is not a live hole. The only places that act on a root --
    /// `SubagentHost::start`'s inheritance algebra and `Runtime::resume_root`
    /// -- read the session's own header via `SessionStore::meta`, never this
    /// projection. A future consumer of `list`/`children` metadata must do
    /// the same rather than trusting this field.
    #[serde(default)]
    root: Option<PathBuf>,
}

impl IndexLine {
    fn from_meta(meta: &SessionMeta) -> Self {
        let (parent, at_seq, mode) = match &meta.origin {
            Some(o) => (Some(o.parent), Some(o.at_seq), Some(o.mode)),
            None => (None, None, None),
        };
        Self {
            session: meta.id,
            agent: meta.agent_id,
            parent,
            at_seq,
            mode,
            created: meta.created,
            agent_def: meta.agent_def.clone(),
            role: meta.role.clone(),
            cwd: meta.cwd.clone(),
            labels: meta.labels.clone(),
            ephemeral: meta.ephemeral,
            ask_origin: meta.ask_origin,
            root: meta.root.clone(),
        }
    }

    fn into_meta(self) -> SessionMeta {
        let origin = match (self.parent, self.at_seq, self.mode) {
            (Some(parent), Some(at_seq), Some(mode)) => Some(ForkOrigin {
                parent,
                at_seq,
                mode,
            }),
            _ => None,
        };
        SessionMeta {
            id: self.session,
            agent_id: self.agent,
            origin,
            agent_def: self.agent_def,
            role: self.role,
            created: self.created,
            cwd: self.cwd,
            labels: self.labels,
            ephemeral: self.ephemeral,
            ask_origin: self.ask_origin,
            root: self.root,
        }
    }
}

/// In-memory state: every known header, plus a parent → children projection
/// kept up to date incrementally by [`IndexState::upsert`].
#[derive(Debug, Default)]
struct IndexState {
    by_id: HashMap<SessionId, SessionMeta>,
    /// Unsorted membership lists; `children()` sorts by `created` (looked up
    /// via `by_id`) at read time rather than maintaining sort order on
    /// every insert, which would need an O(log n) insertion search per
    /// `upsert` for a query pattern (`children`) that is comparatively rare
    /// and small per session.
    children: HashMap<SessionId, Vec<SessionId>>,
}

impl IndexState {
    fn upsert(&mut self, meta: SessionMeta) {
        // Re-recording an id (not expected in normal operation, but kept
        // correct defensively) must not leave a stale entry under the old
        // parent if the origin ever differs between calls.
        if let Some(old) = self.by_id.remove(&meta.id) {
            if let Some(origin) = &old.origin {
                if let Some(list) = self.children.get_mut(&origin.parent) {
                    list.retain(|c| *c != meta.id);
                }
            }
        }
        if let Some(origin) = &meta.origin {
            let list = self.children.entry(origin.parent).or_default();
            if !list.contains(&meta.id) {
                list.push(meta.id);
            }
        }
        self.by_id.insert(meta.id, meta);
    }

    /// Evicts `sid` from both projections: `by_id`, its entry in its
    /// parent's `children` list, and its own `children` entry (empty under
    /// the store's remove guard matrix, evicted regardless for symmetry
    /// with `upsert`'s defensive re-record handling).
    fn remove(&mut self, sid: &SessionId) {
        if let Some(old) = self.by_id.remove(sid) {
            if let Some(origin) = &old.origin {
                if let Some(list) = self.children.get_mut(&origin.parent) {
                    list.retain(|c| c != sid);
                }
            }
        }
        self.children.remove(sid);
    }
}

/// Why `try_load` did not produce a usable index — distinguishes a fresh
/// store (no prior `index.jsonl`, not a failure) from a genuinely corrupt or
/// stale one (rebuild, and warn that it happened).
enum LoadOutcome {
    Missing,
    Invalid(String),
}

/// Scans `root` for session files (`<ulid>.jsonl`, excluding `index.jsonl`
/// and `index.jsonl.tmp` — neither has a `.jsonl`-final extension with a
/// ULID stem, so no special-casing is needed beyond the parse check).
async fn scan_session_files(root: &Path) -> Result<Vec<(SessionId, PathBuf)>, StoreError> {
    let mut out = Vec::new();
    let mut rd = tokio::fs::read_dir(root).await.map_err(io_err)?;
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
            continue;
        };
        out.push((sid, path));
    }
    Ok(out)
}

/// Reads only line 0 of a session file and decodes it as a header —
/// `None` on any I/O or decode failure (the caller drops and warns).
async fn read_header(path: &Path) -> Option<SessionMeta> {
    let file = tokio::fs::File::open(path).await.ok()?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    reader.read_line(&mut line).await.ok()?;
    if line.trim().is_empty() {
        return None;
    }
    codec::decode_header(&line).ok()
}

/// One logical line of `index.jsonl`'s raw content: `(text, had_trailing_newline)`.
/// A final line lacking its trailing `\n` is a truncated write and must be
/// treated as invalid, not silently accepted (`str::lines` alone can't tell
/// the difference, since it yields a trailing partial segment identically
/// to a complete one).
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

/// The derived, rebuildable session index. Implemented by earlier work.
#[derive(Debug)]
pub struct SessionIndex {
    state: RwLock<IndexState>,
    root: PathBuf,
}

impl SessionIndex {
    /// Loads `root/index.jsonl`, or rebuilds it by scanning `root/*.jsonl`
    /// (excluding `index.jsonl`) if it is absent, corrupt, or inconsistent
    /// with the session files on disk.
    ///
    /// Rebuild triggers (any one is sufficient): the file is absent; a line
    /// fails to decode; a line's final byte lacks a trailing newline (a
    /// truncated write); a duplicate `session` id appears; an entry names a
    /// session file absent from `root`; or a session file present in `root`
    /// has no entry. A rebuild triggered by anything other than plain
    /// absence logs `tracing::warn!(..., "index rebuild")`.
    pub(crate) async fn load_or_rebuild(root: &Path) -> Result<Self, StoreError> {
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
                "index rebuild: failed to persist rebuilt index.jsonl (will be rebuilt again next open)"
            );
        }
        Ok(index)
    }

    /// Attempts to load an existing, internally consistent `index.jsonl`.
    /// Any inconsistency is reported via `LoadOutcome`, never `panic!` or a
    /// silently wrong index.
    async fn try_load(root: &Path) -> Result<IndexState, LoadOutcome> {
        let path = root.join("index.jsonl");
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(LoadOutcome::Missing),
            Err(e) => return Err(LoadOutcome::Invalid(format!("index.jsonl unreadable: {e}"))),
        };

        let mut state = IndexState::default();
        let mut seen: HashSet<SessionId> = HashSet::new();
        let lines = raw_lines(&content);
        for (idx, (text, had_newline)) in lines.iter().enumerate() {
            let is_last = idx == lines.len() - 1;
            if !had_newline {
                debug_assert!(is_last, "only the final line can lack a trailing newline");
                return Err(LoadOutcome::Invalid(format!(
                    "index.jsonl line {idx} is truncated (no trailing newline)"
                )));
            }
            let line: IndexLine = match serde_json::from_str(text) {
                Ok(l) => l,
                Err(e) => {
                    return Err(LoadOutcome::Invalid(format!(
                        "index.jsonl line {idx} failed to decode: {e}"
                    )));
                }
            };
            if !seen.insert(line.session) {
                return Err(LoadOutcome::Invalid(format!(
                    "index.jsonl has a duplicate entry for session {}",
                    line.session
                )));
            }
            state.upsert(line.into_meta());
        }

        let files = scan_session_files(root)
            .await
            .map_err(|e| LoadOutcome::Invalid(format!("directory scan failed: {e}")))?;
        let disk_ids: HashSet<SessionId> = files.iter().map(|(sid, _)| *sid).collect();
        let index_ids: HashSet<SessionId> = state.by_id.keys().copied().collect();
        if disk_ids != index_ids {
            return Err(LoadOutcome::Invalid(format!(
                "index.jsonl disagrees with disk: {} indexed session(s), {} file(s) on disk",
                index_ids.len(),
                disk_ids.len()
            )));
        }

        Ok(state)
    }

    /// Rebuild-by-scan: read line 0 of every session file on disk, dropping
    /// (and warning about) any whose header can't be decoded.
    async fn rebuild_scan(root: &Path) -> Result<IndexState, StoreError> {
        let files = scan_session_files(root).await?;
        let mut state = IndexState::default();
        for (sid, path) in files {
            match read_header(&path).await {
                Some(meta) => state.upsert(meta),
                None => tracing::warn!(
                    session = %sid,
                    path = %path.display(),
                    "index rebuild: dropping session with an unreadable header"
                ),
            }
        }
        Ok(state)
    }

    /// Atomically rewrites `index.jsonl` from the current in-memory state:
    /// write `index.jsonl.tmp`, fsync, rename over the real file. The
    /// rename is the only non-append write this module ever performs, and
    /// it targets only the derived file, never a session file.
    ///
    /// ORDERING REQUIREMENT: the snapshot above and the rename below are
    /// deliberately NOT covered by `self.state` (a `std::sync::RwLock` —
    /// this module's invariant is that it is never held across an
    /// `.await`, and `record_header`'s blocking `write()` must never
    /// stall a runtime worker behind this rewrite's async file I/O).
    /// Instead the caller must hold the store's `lifecycle` mutex (see
    /// `JsonlSessionStore`'s lock-order docs), which serializes this
    /// snapshot-and-rename against `record_header`'s upsert-plus-append:
    /// without it, a `record_header` interleaved between the snapshot and
    /// the rename would have its appended line destroyed by the rename,
    /// leaving `index.jsonl` one entry short of the in-memory state (the
    /// next open then WARN-rebuilds — self-healed, but spurious; review
    /// F-2). Today's only callers satisfy this: `load_or_rebuild` runs
    /// before the store accepts calls, and `SessionStore::remove` holds
    /// `lifecycle` (as do `create`/`fork`, the only `record_header`
    /// callers).
    async fn persist_full(&self) -> Result<(), StoreError> {
        let metas: Vec<SessionMeta> = {
            let state = self.state.read().unwrap();
            state.by_id.values().cloned().collect()
        };
        let mut buf = String::new();
        for meta in &metas {
            let line = IndexLine::from_meta(meta);
            buf.push_str(&serde_json::to_string(&line).expect("IndexLine always serializes"));
            buf.push('\n');
        }

        let tmp_path = self.root.join("index.jsonl.tmp");
        let final_path = self.root.join("index.jsonl");
        let mut file = tokio::fs::File::create(&tmp_path).await.map_err(io_err)?;
        file.write_all(buf.as_bytes()).await.map_err(io_err)?;
        file.sync_data().await.map_err(io_err)?;
        drop(file);
        tokio::fs::rename(&tmp_path, &final_path)
            .await
            .map_err(io_err)?;
        Ok(())
    }

    /// Records a newly written header in the in-memory index and appends
    /// one line to `index.jsonl` (best-effort — index I/O errors are
    /// logged at WARN and never propagate; see the module-level failure
    /// policy).
    ///
    /// Synchronous by the earlier work-fixed signature: `store.rs` calls this
    /// inline from `create` (which `fork` also delegates to), not awaited.
    /// The `index.jsonl` append below therefore uses blocking `std::fs`
    /// rather than `tokio::fs` — acceptable because it is one small,
    /// best-effort line write, never on `conway-session`'s durability
    /// contract (`index.jsonl` is a cache, not the source of truth).
    pub(crate) fn record_header(&self, meta: &SessionMeta) {
        {
            let mut state = self.state.write().unwrap();
            state.upsert(meta.clone());
        }
        if let Err(e) = self.append_line_sync(meta) {
            tracing::warn!(
                session = %meta.id,
                error = %e,
                "index append failed; will be reconciled by rebuild-by-scan on next open"
            );
        }
    }

    fn append_line_sync(&self, meta: &SessionMeta) -> std::io::Result<()> {
        use std::io::Write;
        let line = IndexLine::from_meta(meta);
        let mut json = serde_json::to_string(&line).expect("IndexLine always serializes to JSON");
        json.push('\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("index.jsonl"))?;
        file.write_all(json.as_bytes())
    }

    /// Fsyncs `index.jsonl`. Called by the interval flusher's tick loop
    /// (the caller's wiring); a missing file is not an error (nothing has been
    /// appended yet).
    pub(crate) async fn flush(&self, root: &Path) -> Result<(), StoreError> {
        let path = root.join("index.jsonl");
        match tokio::fs::OpenOptions::new().write(true).open(&path).await {
            Ok(file) => file.sync_data().await.map_err(io_err),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err(e)),
        }
    }

    /// Synchronous [`flush`](Self::flush) for `Drop`, which cannot await.
    /// Same missing-file tolerance; best-effort at the call site (a lost
    /// tail is healed by `load_or_rebuild` on next open).
    pub(crate) fn flush_sync(&self, root: &Path) -> Result<(), StoreError> {
        let path = root.join("index.jsonl");
        match std::fs::OpenOptions::new().write(true).open(&path) {
            Ok(file) => file.sync_data().map_err(io_err),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err(e)),
        }
    }

    /// Evicts `sid` from the in-memory index and rewrites `index.jsonl`
    /// via [`persist_full`](Self::persist_full). A full rewrite is the only
    /// option: `index.jsonl` is append-only, so a line naming a deleted
    /// session cannot be retracted any other way. Without this eviction +
    /// persist, every subsequent store open would see the index disagree
    /// with disk (`try_load`'s disk-ids-vs-index-ids check) and WARN-rebuild.
    ///
    /// Must be called with the store's `lifecycle` mutex held — see
    /// [`persist_full`](Self::persist_full)'s ordering requirement (review
    /// F-2).
    pub(crate) async fn remove(&self, sid: &SessionId) -> Result<(), StoreError> {
        {
            let mut state = self.state.write().unwrap();
            state.remove(sid);
        }
        self.persist_full().await
    }

    /// Re-records an EXISTING header after an in-place meta mutation — the
    /// store's `set_ephemeral` promote path, the only header mutation the
    /// store supports. `upsert` handles the re-record (replacing the stale
    /// entry under the same id; `IndexState::upsert`'s defensive
    /// old-parent eviction covers any origin change, which promote never
    /// makes), and `persist_full` rewrites `index.jsonl` — a full rewrite
    /// is the only option, exactly as [`remove`](Self::remove)'s doc
    /// explains, since an appended line cannot retract the stale
    /// `ephemeral: true` projection the file already carries.
    ///
    /// Must be called with the store's `lifecycle` mutex held — see
    /// [`persist_full`](Self::persist_full)'s ordering requirement (review
    /// F-2).
    ///
    /// Failure policy (deviates deliberately from `remove`'s
    /// warn-and-swallow): a failed `persist_full` here is NOT self-healing
    /// the way `remove`'s is. `try_load`'s disk-consistency check compares
    /// only the ID SET of index entries against session files, never their
    /// content — so a stale `ephemeral: true` line surviving on disk would
    /// load cleanly on next open and mis-hide the promoted session
    /// indefinitely, silently. Instead, a persist failure deletes
    /// `index.jsonl` outright (best-effort), which forces the next open's
    /// `load_or_rebuild` down the rebuild-by-scan path — and the scan reads
    /// the session files' own (already flipped) headers, so the rebuilt
    /// index is correct. The in-memory upsert above is unaffected by every
    /// failure mode here, so the running store is always immediately
    /// correct; only cross-restart staleness is at stake.
    pub(crate) async fn update_header(&self, meta: &SessionMeta) {
        {
            let mut state = self.state.write().unwrap();
            state.upsert(meta.clone());
        }
        if let Err(e) = self.persist_full().await {
            tracing::warn!(
                session = %meta.id,
                error = %e,
                "index persist after header update failed; deleting stale index.jsonl to force a rebuild-by-scan on next open"
            );
            if let Err(e) = std::fs::remove_file(self.root.join("index.jsonl")) {
                // A missing file is the desired end state anyway; only a
                // real deletion failure leaves the stale index in place
                // (next open loads it, content-stale — logged here, and
                // the in-memory state remains correct for this process).
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        error = %e,
                        "stale index.jsonl could not be deleted; next open may load a content-stale index"
                    );
                }
            }
        }
    }

    /// Delete the persisted `index.jsonl`, tolerating its absence. Called
    /// by `JsonlSessionStore::set_ephemeral` BEFORE the session-header
    /// rename so a crash at ANY point in the promote leaves a self-healing
    /// absence (rebuild-by-scan reads the session files' own headers) rather
    /// than a loadable-but-stale index — `try_load` compares only id SETS,
    /// so a stale `ephemeral: true` line would otherwise load cleanly and
    /// mis-hide the promoted session forever (a mid-remove crash produces
    /// an id-set MISMATCH and self-heals; a mid-promote crash produces
    /// matching sets with stale content and never does — cycle-5 B3 review).
    pub(crate) async fn invalidate_persisted(&self) {
        if let Err(e) = tokio::fs::remove_file(self.root.join("index.jsonl")).await {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    error = %e,
                    "index.jsonl could not be deleted before a promote; a crash before the next persist may load a content-stale index"
                );
            }
        }
    }

    /// Sessions whose header `origin.parent == sid`, ascending `created`
    /// order (ties broken by ascending `id` for determinism). Reads only
    /// in-memory state — no file I/O on the hot path.
    ///
    /// Hides ephemeral children unconditionally: this method takes no
    /// `SessionFilter`, so there is no `include_ephemeral` opt-in to thread
    /// through it (extending the signature would ripple into every
    /// `SessionStore::children` caller across the workspace for a query
    /// surface `list` already covers). A caller that needs a parent's
    /// ephemeral children too uses `list(SessionFilter{parent: Some(sid),
    /// include_ephemeral: true, ..})` instead, which returns full
    /// `SessionMeta`s (`.id` gives the same `SessionId`s this method would).
    pub(crate) fn children(&self, sid: &SessionId) -> Vec<SessionId> {
        let state = self.state.read().unwrap();
        let mut kids: Vec<SessionId> = state
            .children
            .get(sid)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|kid| !state.by_id.get(kid).is_some_and(|m| m.ephemeral))
            .collect();
        kids.sort_by(|a, b| {
            let ca = state.by_id.get(a).map(|m| m.created);
            let cb = state.by_id.get(b).map(|m| m.created);
            ca.cmp(&cb).then_with(|| a.cmp(b))
        });
        kids
    }

    /// Sessions matching `f`, AND-composed across `agent_def`/`parent`/
    /// `label`/`include_ephemeral`, ordered descending `created`
    /// with ties broken by ascending `id`, `limit` applied after filtering
    /// and ordering. Reads only in-memory state — no file I/O on the hot
    /// path.
    pub(crate) fn list(&self, f: &SessionFilter) -> Vec<SessionMeta> {
        let state = self.state.read().unwrap();
        let mut metas: Vec<SessionMeta> = state
            .by_id
            .values()
            .filter(|m| {
                f.agent_def
                    .as_ref()
                    .is_none_or(|v| m.agent_def.as_deref() == Some(v.as_str()))
                    && f.label
                        .as_ref()
                        .is_none_or(|v| m.labels.iter().any(|l| l == v))
                    && f.parent
                        .as_ref()
                        .is_none_or(|p| m.origin.as_ref().is_some_and(|o| o.parent == *p))
                    && (f.include_ephemeral || !m.ephemeral)
            })
            .cloned()
            .collect();
        metas.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| a.id.cmp(&b.id)));
        if let Some(limit) = f.limit {
            metas.truncate(limit);
        }
        metas
    }
}
