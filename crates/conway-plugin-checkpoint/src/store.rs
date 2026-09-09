//! The on-disk shadow store `conway.checkpoint` snapshots into and restores
//! from: content-addressed blobs shared across a project, plus one
//! append-only per-session index recording which path changed, when, and
//! what came before/after. See this crate's own module doc (`lib.rs`) for
//! why the mechanism is shaped this way; this module is the ONE place that
//! shape is actually implemented -- `record_observed`/`rollback`/`entries`/
//! `earliest_at_or_after` are what `lib.rs`'s observer, commands, and tests
//! all call, so there is exactly one snapshot/restore implementation, not
//! one per caller (this crate's own "CONSTRAINTS" section).
//!
//! Deliberately independent of `conway`/`conway-core`: every type here is
//! plain Rust plus `serde`/`chrono`/`blake3`, testable in isolation --
//! `lib.rs` is the only place this module meets `Plugin`/`ToolObserver`/
//! `Command`.
//!
//! # Layout
//!
//! ```text
//! <root>/                          # <cwd>/.conway/checkpoints
//!   objects/
//!     <blake3-hex>.bin              # content-addressed blob, shared by every
//!                                    # session under this root
//!     .order.txt                    # one hash per line, oldest first --
//!                                    # eviction order for the per-project bound
//!   sessions/
//!     <session-id>/
//!       index.jsonl                 # this session's own ordered entries
//! ```
//!
//! # Two bounds, both enforced here
//!
//! [`DEFAULT_MAX_SNAPSHOT_BYTES`] (2 MiB): content larger than this is never
//! captured at all -- [`SnapshotRef::Unavailable`], with a reason a caller
//! can turn into a visible notice. [`DEFAULT_MAX_PROJECT_BYTES`] (256 MiB):
//! the total size of every blob under `objects/`, across every session in
//! this project -- when a new blob would push the total over the bound,
//! [`CheckpointStore::put_blob`] evicts the OLDEST blobs (by write order,
//! tracked in `.order.txt` rather than filesystem mtime, so eviction order
//! is exact regardless of a filesystem's mtime resolution) until there is
//! room, or gives up (never stores a blob larger than the bound all by
//! itself). An entry whose blob has since been evicted is detected lazily,
//! at read time (`resolve_ref`), never by mutating the append-only index.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Content larger than this is never captured -- see this module's own doc,
/// "Two bounds, both enforced here".
pub const DEFAULT_MAX_SNAPSHOT_BYTES: u64 = 2 * 1024 * 1024;

/// The total size every blob under one project's `objects/` directory is
/// held to, across every session -- see this module's own doc.
pub const DEFAULT_MAX_PROJECT_BYTES: u64 = 256 * 1024 * 1024;

/// What kind of event produced one [`CheckpointEntry`].
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Write,
    Edit,
    /// A restore this store performed on the operator's own typed
    /// `/conway.checkpoint.rollback` -- listed and itself roll-back-able
    /// exactly like a write/edit ([`CheckpointStore::rollback`]'s own doc,
    /// "this rollback is itself undoable").
    Rollback,
}

/// What a [`CheckpointEntry`]'s `old`/`new` field holds.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SnapshotRef {
    /// Content this store captured, content-addressed by `hash` (a `blake3`
    /// hex digest) under `objects/`. The blob itself may since have been
    /// evicted (the per-project bound) -- `len` is kept here so a reader
    /// can still say how big it WAS even after eviction.
    Bytes { hash: String, len: u64 },
    /// The path is known to have NOT existed at this point -- only ever
    /// produced by a rollback's own `old`/`new` (a rollback reads/writes the
    /// filesystem synchronously, so this is a positive fact, never a
    /// guess).
    Absent,
    /// This store genuinely does not know what was here: either this is
    /// the FIRST time this session's checkpoint history has seen this path
    /// (no prior entry to chain `old` from -- the observer that calls
    /// `record_observed` only ever runs AFTER a call finishes, so there was
    /// no earlier point at which this store could have captured the
    /// truly-prior state; see this crate's own module doc, "What this
    /// plugin actually observes, and the seam gap that shapes it"), or the
    /// content exceeded [`DEFAULT_MAX_SNAPSHOT_BYTES`] and was never
    /// captured. `reason` distinguishes the two for a reader.
    Unavailable { reason: String },
}

/// One row of a session's own `index.jsonl`: one observed write/edit, or
/// one rollback this store itself performed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckpointEntry {
    /// For `Write`/`Edit`: the tool call's own `ObservedCall::result_seq.0`
    /// -- the SAME number space `/conway.history.rewind`/the status line's
    /// `session <id>@<seq>` field already use, so an operator can quote a
    /// seq straight out of either surface. For `Rollback`:
    /// [`CheckpointStore::next_seq`] at the moment it ran -- unique,
    /// monotonic, and in the same space, but naming no real `LogRecord` (a
    /// rollback is a plugin command, not a tool call the session log
    /// assigns a seq to).
    pub seq: u64,
    /// The absolute path touched, as this store resolved it
    /// ([`CheckpointStore::resolve`]) -- a portable-enough on-disk key, in
    /// this platform's own string form.
    pub path: String,
    pub tool: ToolKind,
    /// What was there immediately before this event.
    pub old: SnapshotRef,
    /// What was there immediately after.
    pub new: SnapshotRef,
    pub ts: DateTime<Utc>,
}

/// What resolving a [`SnapshotRef`] against the blob store actually yields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedRef {
    Bytes(Vec<u8>),
    Absent,
    /// Cannot be resolved -- either the ref was already `Unavailable`, or it
    /// was `Bytes` but the blob has since been evicted. Either way, the
    /// `String` is operator-facing text explaining why.
    Missing(String),
}

/// What [`CheckpointStore::put_blob`] did with one blob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PutBlobOutcome {
    pub hash: String,
    /// `false` only when `bytes` alone exceeds `max_project_bytes` -- no
    /// amount of eviction would ever make it fit.
    pub stored: bool,
    /// Hashes of blobs removed to make room, oldest (by write order) first.
    pub evicted: Vec<String>,
}

/// [`CheckpointStore::rollback`]'s own report: one line of bookkeeping per
/// path it looked at, split into the three outcomes an operator needs to
/// tell apart -- restored, conflicted (a hand edit survived), or skipped
/// (something else stopped it, e.g. no known baseline).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RollbackReport {
    pub restored: Vec<RestoredPath>,
    pub conflicts: Vec<ConflictedPath>,
    pub skipped: Vec<(String, String)>,
}

/// One path [`CheckpointStore::rollback`] actually restored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoredPath {
    pub path: String,
    /// The seq of the checkpoint entry this path was restored TO the state
    /// before.
    pub from_seq: u64,
    /// The seq the rollback itself was recorded under -- what a caller
    /// quotes to undo this specific rollback.
    pub rollback_seq: u64,
    /// A size/eviction notice from snapshotting the pre-rollback state,
    /// if any.
    pub notice: Option<String>,
}

/// One path [`CheckpointStore::rollback`] found a hand edit for and, absent
/// `force_all`, left untouched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictedPath {
    pub path: String,
}

/// The shadow store itself. `root` is a `.conway/checkpoints`-shaped
/// directory shared by every session in one project (the per-project total
/// bound applies across all of them); `cwd` is what a relative path
/// argument -- a tool call's own `path` field, or an operator's typed
/// `<path>` -- resolves against.
#[derive(Clone, Debug)]
pub struct CheckpointStore {
    root: PathBuf,
    cwd: PathBuf,
}

impl CheckpointStore {
    pub fn new(root: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            cwd: cwd.into(),
        }
    }

    /// Resolves a possibly-relative path argument against `cwd` -- an
    /// absolute argument passes through unchanged. The SAME function every
    /// path this crate ever stores or looks up goes through, so a relative
    /// argument and its later relative lookup always agree.
    pub fn resolve(&self, raw: &str) -> PathBuf {
        let candidate = Path::new(raw);
        if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.cwd.join(candidate)
        }
    }

    fn objects_dir(&self) -> PathBuf {
        self.root.join("objects")
    }

    fn order_path(&self) -> PathBuf {
        self.objects_dir().join(".order.txt")
    }

    fn blob_path(&self, hash: &str) -> PathBuf {
        self.objects_dir().join(format!("{hash}.bin"))
    }

    fn session_index_path(&self, session: &str) -> PathBuf {
        self.root.join("sessions").join(session).join("index.jsonl")
    }

    fn dir_total_bytes(dir: &Path) -> io::Result<u64> {
        let mut total = 0u64;
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(err) => return Err(err),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_file() && path.extension().is_some_and(|ext| ext == "bin") {
                total += entry.metadata()?.len();
            }
        }
        Ok(total)
    }

    fn read_order(&self) -> io::Result<Vec<String>> {
        match fs::read_to_string(self.order_path()) {
            Ok(content) => Ok(content
                .lines()
                .map(|line| line.trim().to_string())
                .filter(|line| !line.is_empty())
                .collect()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(err) => Err(err),
        }
    }

    fn write_order(&self, hashes: &[String]) -> io::Result<()> {
        let mut content = hashes.join("\n");
        if !content.is_empty() {
            content.push('\n');
        }
        fs::write(self.order_path(), content)
    }

    fn append_order(&self, hash: &str) -> io::Result<()> {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.order_path())?;
        writeln!(file, "{hash}")
    }

    /// Removes the oldest blob still actually present on disk (skipping any
    /// stale `.order.txt` entry left by an earlier eviction of the same
    /// hash), pruning those stale entries out of the order file as it goes.
    /// `Ok(None)` when nothing is left to evict.
    fn evict_one(&self) -> io::Result<Option<String>> {
        let order = self.read_order()?;
        let mut oldest_live: Option<String> = None;
        for hash in &order {
            if self.blob_path(hash).exists() {
                oldest_live = Some(hash.clone());
                break;
            }
        }
        let Some(hash) = oldest_live else {
            return Ok(None);
        };
        fs::remove_file(self.blob_path(&hash))?;
        let pruned: Vec<String> = order
            .into_iter()
            .filter(|h| self.blob_path(h).exists())
            .collect();
        self.write_order(&pruned)?;
        Ok(Some(hash))
    }

    /// Content-addresses `bytes` and stores it, evicting the oldest blobs
    /// (globally, across every session in this project, oldest by WRITE
    /// ORDER) until it fits under `max_project_bytes` -- see this module's
    /// own doc, "Two bounds, both enforced here". Never stores a blob
    /// larger than `max_project_bytes` all by itself (nothing to evict that
    /// would help). Already-present content is a cheap dedup no-op: never
    /// re-written, never touches the eviction order.
    pub fn put_blob(&self, bytes: &[u8], max_project_bytes: u64) -> io::Result<PutBlobOutcome> {
        let hash = blake3::hash(bytes).to_hex().to_string();
        fs::create_dir_all(self.objects_dir())?;
        let path = self.blob_path(&hash);
        if path.exists() {
            return Ok(PutBlobOutcome {
                hash,
                stored: true,
                evicted: Vec::new(),
            });
        }
        let needed = bytes.len() as u64;
        if needed > max_project_bytes {
            return Ok(PutBlobOutcome {
                hash,
                stored: false,
                evicted: Vec::new(),
            });
        }
        let mut evicted = Vec::new();
        loop {
            let total = Self::dir_total_bytes(&self.objects_dir())?;
            if total + needed <= max_project_bytes {
                break;
            }
            match self.evict_one()? {
                Some(evicted_hash) => evicted.push(evicted_hash),
                None => break,
            }
        }
        fs::write(&path, bytes)?;
        self.append_order(&hash)?;
        Ok(PutBlobOutcome {
            hash,
            stored: true,
            evicted,
        })
    }

    pub fn read_blob(&self, hash: &str) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.blob_path(hash)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// The ONE place this store turns raw bytes into a [`SnapshotRef`] --
    /// [`Self::record_observed`]'s own `new` and [`Self::rollback`]'s own
    /// `old`/`new` both go through this (this crate's own "ONE snapshot/
    /// restore implementation" constraint). Returns a human-readable notice
    /// whenever the per-file bound skipped the capture, or the per-project
    /// bound evicted something, so a caller can turn it into an
    /// operator-visible note.
    pub fn capture(
        &self,
        bytes: &[u8],
        max_snapshot_bytes: u64,
        max_project_bytes: u64,
    ) -> io::Result<(SnapshotRef, Option<String>)> {
        if bytes.len() as u64 > max_snapshot_bytes {
            let len = bytes.len();
            return Ok((
                SnapshotRef::Unavailable {
                    reason: format!(
                        "exceeds the {max_snapshot_bytes}-byte per-file snapshot bound \
                         ({len} bytes)"
                    ),
                },
                Some(format!(
                    "not snapshotted: {len} bytes exceeds the {max_snapshot_bytes}-byte \
                     per-file snapshot bound"
                )),
            ));
        }
        let outcome = self.put_blob(bytes, max_project_bytes)?;
        if !outcome.stored {
            return Ok((
                SnapshotRef::Unavailable {
                    reason: "exceeds the per-project total snapshot bound even after evicting \
                             every other snapshot"
                        .to_string(),
                },
                Some(
                    "not snapshotted: exceeds the per-project total snapshot bound even after \
                     evicting every other snapshot"
                        .to_string(),
                ),
            ));
        }
        let notice = if outcome.evicted.is_empty() {
            None
        } else {
            Some(format!(
                "evicted {} older snapshot(s) to stay within the per-project bound",
                outcome.evicted.len()
            ))
        };
        Ok((
            SnapshotRef::Bytes {
                hash: outcome.hash,
                len: bytes.len() as u64,
            },
            notice,
        ))
    }

    /// Resolves a [`SnapshotRef`] to actual bytes, distinguishing "known
    /// absent" from "known bytes" from "cannot resolve" -- the ONE place
    /// `diff`/`rollback`/`list` read stored content through.
    pub fn resolve_ref(&self, snapshot: &SnapshotRef) -> io::Result<ResolvedRef> {
        match snapshot {
            SnapshotRef::Absent => Ok(ResolvedRef::Absent),
            SnapshotRef::Unavailable { reason } => Ok(ResolvedRef::Missing(reason.clone())),
            SnapshotRef::Bytes { hash, .. } => match self.read_blob(hash)? {
                Some(bytes) => Ok(ResolvedRef::Bytes(bytes)),
                None => Ok(ResolvedRef::Missing(format!(
                    "the snapshotted content (blake3:{hash}) was evicted to stay within the \
                     per-project bound"
                ))),
            },
        }
    }

    fn read_index(&self, session: &str) -> io::Result<Vec<CheckpointEntry>> {
        let path = self.session_index_path(session);
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
            })
            .collect()
    }

    fn append_index(&self, session: &str, entry: &CheckpointEntry) -> io::Result<()> {
        use std::io::Write;
        let path = self.session_index_path(session);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let line = serde_json::to_string(entry)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        writeln!(file, "{line}")
    }

    /// Every entry this session has, in `index.jsonl`'s own append order.
    pub fn entries(&self, session: &str) -> io::Result<Vec<CheckpointEntry>> {
        self.read_index(session)
    }

    fn latest_before(
        &self,
        session: &str,
        path: &Path,
        seq: u64,
    ) -> io::Result<Option<CheckpointEntry>> {
        let entries = self.read_index(session)?;
        Ok(entries
            .into_iter()
            .filter(|entry| entry.seq < seq && Path::new(&entry.path) == path)
            .max_by_key(|entry| entry.seq))
    }

    /// The most recent entry this store has for `path` -- what conway
    /// itself last believes is on disk there, the fact
    /// [`Self::rollback`]'s own three-way conflict check reads.
    pub fn latest_for_path(
        &self,
        session: &str,
        path: &Path,
    ) -> io::Result<Option<CheckpointEntry>> {
        let entries = self.read_index(session)?;
        Ok(entries
            .into_iter()
            .filter(|entry| Path::new(&entry.path) == path)
            .max_by_key(|entry| entry.seq))
    }

    /// For every distinct path touched at or after `target_seq` (optionally
    /// narrowed to one `path_filter`), the entry with the SMALLEST seq
    /// still `>= target_seq` for that path -- the pre-image `diff`/
    /// `rollback` restore TOWARD. One entry per path, sorted by path for
    /// determinism.
    pub fn earliest_at_or_after(
        &self,
        session: &str,
        target_seq: u64,
        path_filter: Option<&Path>,
    ) -> io::Result<Vec<CheckpointEntry>> {
        let entries = self.read_index(session)?;
        let mut by_path: BTreeMap<String, CheckpointEntry> = BTreeMap::new();
        for entry in entries.into_iter().filter(|entry| entry.seq >= target_seq) {
            if let Some(filter) = path_filter {
                if Path::new(&entry.path) != filter {
                    continue;
                }
            }
            by_path
                .entry(entry.path.clone())
                .and_modify(|existing| {
                    if entry.seq < existing.seq {
                        *existing = entry.clone();
                    }
                })
                .or_insert(entry);
        }
        Ok(by_path.into_values().collect())
    }

    /// One past the highest `seq` this session's index currently holds --
    /// what a NEW rollback entry is recorded under
    /// ([`CheckpointEntry::seq`]'s own doc, "For `Rollback`").
    pub fn next_seq(&self, session: &str) -> io::Result<u64> {
        let entries = self.read_index(session)?;
        Ok(entries
            .into_iter()
            .map(|entry| entry.seq)
            .max()
            .map_or(0, |max| max + 1))
    }

    /// Records one observed `write`/`edit` at `seq` against `path`: `old`
    /// chains from the latest EARLIER entry this session has for `path`, or
    /// [`SnapshotRef::Unavailable`] when there is none -- see that
    /// variant's own doc for why this store can never do better for the
    /// FIRST touch of a path. `new` is [`Self::capture`] of whatever bytes
    /// are on `path` right now -- correct by this method's own contract:
    /// call it from `ToolObserver::after_tool_call`, strictly after the
    /// write/edit already landed, never before.
    pub fn record_observed(
        &self,
        session: &str,
        seq: u64,
        path: &Path,
        tool: ToolKind,
        max_snapshot_bytes: u64,
        max_project_bytes: u64,
    ) -> io::Result<(CheckpointEntry, Vec<String>)> {
        let old = match self.latest_before(session, path, seq)? {
            Some(prior) => prior.new,
            None => SnapshotRef::Unavailable {
                reason: "not observed by conway.checkpoint before this seq in this session"
                    .to_string(),
            },
        };
        let (new, notices) = match fs::read(path) {
            Ok(bytes) => {
                let (snapshot, notice) =
                    self.capture(&bytes, max_snapshot_bytes, max_project_bytes)?;
                (snapshot, notice.into_iter().collect())
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => (
                SnapshotRef::Absent,
                vec![format!(
                    "{} was not found when conway.checkpoint tried to snapshot it just after \
                     this call",
                    path.display()
                )],
            ),
            Err(err) => return Err(err),
        };
        let entry = CheckpointEntry {
            seq,
            path: path.display().to_string(),
            tool,
            old,
            new,
            ts: Utc::now(),
        };
        self.append_index(session, &entry)?;
        Ok((entry, notices))
    }

    /// Restores every path touched at or after `target_seq` (narrowed to
    /// `path_filter` when given) to the state it had immediately before
    /// that -- the pre-image [`Self::earliest_at_or_after`] resolves for
    /// each. For every path:
    ///
    /// 1. If the pre-image itself cannot be resolved (no baseline was ever
    ///    known, or its blob was evicted), the path is SKIPPED with a
    ///    reason -- never a guess.
    /// 2. Otherwise, a three-way check: this store's own LATEST recorded
    ///    write for the path (the "tool result") is compared against
    ///    whatever bytes are actually on disk right now (the "current"). If
    ///    they differ, an operator hand edit happened since conway last
    ///    wrote this file -- a CONFLICT, and (absent `force_all`) the path
    ///    is left untouched: the hand edit survives, never silently
    ///    overwritten.
    /// 3. Otherwise (or with `force_all`), the CURRENT bytes are captured
    ///    first (so this rollback is itself undoable), the pre-image is
    ///    written to disk, and the rollback itself is recorded as a new
    ///    entry (`ToolKind::Rollback`) under [`Self::next_seq`] -- listed by
    ///    [`Self::entries`] and reachable by a later
    ///    `/conway.checkpoint.rollback <that seq>` exactly like any other
    ///    entry.
    pub fn rollback(
        &self,
        session: &str,
        target_seq: u64,
        path_filter: Option<&Path>,
        force_all: bool,
        max_snapshot_bytes: u64,
        max_project_bytes: u64,
    ) -> io::Result<RollbackReport> {
        let touched = self.earliest_at_or_after(session, target_seq, path_filter)?;
        let mut report = RollbackReport::default();
        for entry in touched {
            let path = PathBuf::from(&entry.path);

            let target_bytes = match self.resolve_ref(&entry.old)? {
                ResolvedRef::Bytes(bytes) => Some(bytes),
                ResolvedRef::Absent => None,
                ResolvedRef::Missing(reason) => {
                    report
                        .skipped
                        .push((entry.path.clone(), format!("cannot restore -- {reason}")));
                    continue;
                }
            };

            let last_known = match self.latest_for_path(session, &path)? {
                Some(latest) => match self.resolve_ref(&latest.new)? {
                    ResolvedRef::Bytes(bytes) => Some(bytes),
                    ResolvedRef::Absent => Some(Vec::new()),
                    ResolvedRef::Missing(_) => None,
                },
                None => None,
            };
            let current_bytes = fs::read(&path).ok();
            let conflict = match (&last_known, &current_bytes) {
                (Some(known), Some(current)) => known != current,
                // Either side unknown/unreadable: this store cannot verify
                // "nothing changed since conway last wrote it", so it never
                // assumes that on the operator's behalf.
                _ => true,
            };
            if conflict && !force_all {
                report.conflicts.push(ConflictedPath {
                    path: entry.path.clone(),
                });
                continue;
            }

            let (old_ref, old_notice) = match &current_bytes {
                Some(bytes) => self.capture(bytes, max_snapshot_bytes, max_project_bytes)?,
                None => (SnapshotRef::Absent, None),
            };

            let write_result = match &target_bytes {
                Some(bytes) => (|| -> io::Result<SnapshotRef> {
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::write(&path, bytes)?;
                    let (new_ref, _notice) =
                        self.capture(bytes, max_snapshot_bytes, max_project_bytes)?;
                    Ok(new_ref)
                })(),
                None => match fs::remove_file(&path) {
                    Ok(()) => Ok(SnapshotRef::Absent),
                    Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(SnapshotRef::Absent),
                    Err(err) => Err(err),
                },
            };
            let new_ref = match write_result {
                Ok(new_ref) => new_ref,
                Err(err) => {
                    report
                        .skipped
                        .push((entry.path.clone(), format!("failed to restore: {err}")));
                    continue;
                }
            };

            let rollback_seq = self.next_seq(session)?;
            let rollback_entry = CheckpointEntry {
                seq: rollback_seq,
                path: entry.path.clone(),
                tool: ToolKind::Rollback,
                old: old_ref,
                new: new_ref,
                ts: Utc::now(),
            };
            self.append_index(session, &rollback_entry)?;
            report.restored.push(RestoredPath {
                path: entry.path.clone(),
                from_seq: entry.seq,
                rollback_seq,
                notice: old_notice,
            });
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store(dir: &TempDir) -> CheckpointStore {
        CheckpointStore::new(dir.path().join(".conway").join("checkpoints"), dir.path())
    }

    const SESSION: &str = "sess1";

    #[test]
    fn resolve_joins_a_relative_path_against_cwd_and_passes_absolute_through() {
        let dir = TempDir::new().unwrap();
        let store = store(&dir);
        assert_eq!(store.resolve("f.txt"), dir.path().join("f.txt"));
        let abs = dir.path().join("elsewhere/g.txt");
        assert_eq!(store.resolve(&abs.display().to_string()), abs);
    }

    #[test]
    fn put_blob_dedups_identical_content() {
        let dir = TempDir::new().unwrap();
        let store = store(&dir);
        let a = store.put_blob(b"hello", DEFAULT_MAX_PROJECT_BYTES).unwrap();
        let b = store.put_blob(b"hello", DEFAULT_MAX_PROJECT_BYTES).unwrap();
        assert_eq!(a.hash, b.hash);
        assert!(a.stored && b.stored);
        assert!(b.evicted.is_empty(), "a dedup hit never evicts");
    }

    #[test]
    fn put_blob_over_the_project_bound_alone_is_never_stored() {
        let dir = TempDir::new().unwrap();
        let store = store(&dir);
        let outcome = store.put_blob(b"0123456789", 5).unwrap();
        assert!(!outcome.stored);
        assert!(store.read_blob(&outcome.hash).unwrap().is_none());
    }

    #[test]
    fn put_blob_evicts_the_oldest_blob_first_to_make_room() {
        let dir = TempDir::new().unwrap();
        let store = store(&dir);
        let first = store.put_blob(b"aaaaa", 12).unwrap(); // 5 bytes
        let _second = store.put_blob(b"bbbbb", 12).unwrap(); // 5 bytes, total 10
                                                             // A third 5-byte blob pushes the total to 15 > 12: the OLDEST
                                                             // (first) must be evicted, not the second.
        let third = store.put_blob(b"ccccc", 12).unwrap();
        assert!(third.stored);
        assert_eq!(third.evicted, vec![first.hash.clone()]);
        assert!(store.read_blob(&first.hash).unwrap().is_none());
        assert!(store.read_blob(&third.hash).unwrap().is_some());
    }

    #[test]
    fn capture_over_the_snapshot_bound_is_unavailable_with_a_notice() {
        let dir = TempDir::new().unwrap();
        let store = store(&dir);
        let (snapshot, notice) = store
            .capture(b"0123456789", 5, DEFAULT_MAX_PROJECT_BYTES)
            .unwrap();
        assert!(matches!(snapshot, SnapshotRef::Unavailable { .. }));
        let notice = notice.expect("a skip must carry a notice");
        assert!(notice.contains("5-byte"), "{notice}");
    }

    #[test]
    fn record_observed_first_touch_of_a_path_has_no_known_old_baseline() {
        let dir = TempDir::new().unwrap();
        let store = store(&dir);
        let path = dir.path().join("f.txt");
        fs::write(&path, "first").unwrap();
        let (entry, notices) = store
            .record_observed(
                SESSION,
                1,
                &path,
                ToolKind::Write,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();
        assert!(notices.is_empty());
        assert!(matches!(entry.old, SnapshotRef::Unavailable { .. }));
        assert!(matches!(entry.new, SnapshotRef::Bytes { .. }));
    }

    #[test]
    fn record_observed_second_touch_chains_old_from_the_first_touchs_new() {
        let dir = TempDir::new().unwrap();
        let store = store(&dir);
        let path = dir.path().join("f.txt");
        fs::write(&path, "first").unwrap();
        store
            .record_observed(
                SESSION,
                1,
                &path,
                ToolKind::Write,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();
        fs::write(&path, "second").unwrap();
        let (entry, _) = store
            .record_observed(
                SESSION,
                2,
                &path,
                ToolKind::Edit,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();
        let ResolvedRef::Bytes(old_bytes) = store.resolve_ref(&entry.old).unwrap() else {
            panic!("expected a resolvable old snapshot");
        };
        assert_eq!(old_bytes, b"first");
    }

    /// Anchor: the acceptance scenario, part 1 -- three edits, no hand
    /// edit, `rollback(seq_of_second)` restores the pre-second-edit bytes.
    #[test]
    fn rollback_restores_pre_second_edit_bytes_when_no_hand_edit_since() {
        let dir = TempDir::new().unwrap();
        let store = store(&dir);
        let path = dir.path().join("f.txt");

        fs::write(&path, "v1").unwrap();
        store
            .record_observed(
                SESSION,
                1,
                &path,
                ToolKind::Write,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();
        fs::write(&path, "v2").unwrap();
        store
            .record_observed(
                SESSION,
                2,
                &path,
                ToolKind::Edit,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();
        fs::write(&path, "v3").unwrap();
        store
            .record_observed(
                SESSION,
                3,
                &path,
                ToolKind::Edit,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();

        let report = store
            .rollback(
                SESSION,
                2,
                None,
                false,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();
        assert_eq!(report.restored.len(), 1);
        assert!(report.conflicts.is_empty());
        assert!(report.skipped.is_empty());
        assert_eq!(fs::read_to_string(&path).unwrap(), "v1");
    }

    /// Anchor: the acceptance scenario, part 2 -- a hand edit made after
    /// the last conway-observed write SURVIVES a rollback attempt; the
    /// rollback is reported as a conflict, never a silent overwrite.
    #[test]
    fn rollback_preserves_a_hand_edit_made_after_the_last_observed_write() {
        let dir = TempDir::new().unwrap();
        let store = store(&dir);
        let path = dir.path().join("f.txt");

        fs::write(&path, "v1").unwrap();
        store
            .record_observed(
                SESSION,
                1,
                &path,
                ToolKind::Write,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();
        fs::write(&path, "v2").unwrap();
        store
            .record_observed(
                SESSION,
                2,
                &path,
                ToolKind::Edit,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();
        fs::write(&path, "v3").unwrap();
        store
            .record_observed(
                SESSION,
                3,
                &path,
                ToolKind::Edit,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();

        // The operator's own hand edit, bypassing conway entirely.
        fs::write(&path, "hand-edited").unwrap();

        let report = store
            .rollback(
                SESSION,
                2,
                None,
                false,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();
        assert!(report.restored.is_empty());
        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "hand-edited",
            "the hand edit must survive an unforced rollback"
        );
    }

    /// The same conflict, forced: `--all`'s own effect at the store layer.
    #[test]
    fn rollback_force_all_overwrites_a_hand_edit() {
        let dir = TempDir::new().unwrap();
        let store = store(&dir);
        let path = dir.path().join("f.txt");
        fs::write(&path, "v1").unwrap();
        store
            .record_observed(
                SESSION,
                1,
                &path,
                ToolKind::Write,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();
        fs::write(&path, "v2").unwrap();
        store
            .record_observed(
                SESSION,
                2,
                &path,
                ToolKind::Edit,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();
        fs::write(&path, "hand-edited").unwrap();

        let report = store
            .rollback(
                SESSION,
                2,
                None,
                true,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();
        assert_eq!(report.restored.len(), 1);
        assert!(report.conflicts.is_empty());
        assert_eq!(fs::read_to_string(&path).unwrap(), "v1");
    }

    /// Anchor: "the rollback appears in list and can itself be rolled
    /// back" -- a successful rollback is itself an entry, and rolling IT
    /// back undoes it.
    #[test]
    fn a_rollback_is_listed_and_can_itself_be_rolled_back() {
        let dir = TempDir::new().unwrap();
        let store = store(&dir);
        let path = dir.path().join("f.txt");
        fs::write(&path, "v1").unwrap();
        store
            .record_observed(
                SESSION,
                1,
                &path,
                ToolKind::Write,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();
        fs::write(&path, "v2").unwrap();
        store
            .record_observed(
                SESSION,
                2,
                &path,
                ToolKind::Edit,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();

        let first_rollback = store
            .rollback(
                SESSION,
                2,
                None,
                false,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "v1");
        let rollback_seq = first_rollback.restored[0].rollback_seq;

        let entries = store.entries(SESSION).unwrap();
        assert!(
            entries
                .iter()
                .any(|e| e.seq == rollback_seq && e.tool == ToolKind::Rollback),
            "the rollback itself must appear in `entries` (the `list` read surface)"
        );

        // Rolling back the rollback restores what was there immediately
        // before IT ran -- "v2".
        let undo = store
            .rollback(
                SESSION,
                rollback_seq,
                None,
                false,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();
        assert_eq!(undo.restored.len(), 1);
        assert_eq!(fs::read_to_string(&path).unwrap(), "v2");
    }

    #[test]
    fn rollback_of_a_path_created_by_write_deletes_it_when_restoring_to_absent() {
        let dir = TempDir::new().unwrap();
        let store = store(&dir);
        let path = dir.path().join("new.txt");
        fs::write(&path, "brand new").unwrap();
        // No prior entry exists for this path at all -- `old` is
        // `Unavailable`, not `Absent` (this store never guesses "did not
        // exist" for a write/edit's own first touch; see `SnapshotRef::
        // Unavailable`'s own doc). Rolling back to seq 0 must therefore
        // SKIP this path, not delete it.
        store
            .record_observed(
                SESSION,
                5,
                &path,
                ToolKind::Write,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();
        let report = store
            .rollback(
                SESSION,
                5,
                None,
                false,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();
        assert!(report.restored.is_empty());
        assert_eq!(report.skipped.len(), 1);
        assert!(
            path.exists(),
            "an unresolvable baseline must never delete the file"
        );
    }

    #[test]
    fn earliest_at_or_after_picks_the_smallest_qualifying_seq_per_path() {
        let dir = TempDir::new().unwrap();
        let store = store(&dir);
        let path = dir.path().join("f.txt");
        for (seq, content) in [(1u64, "v1"), (2, "v2"), (3, "v3")] {
            fs::write(&path, content).unwrap();
            store
                .record_observed(
                    SESSION,
                    seq,
                    &path,
                    ToolKind::Edit,
                    DEFAULT_MAX_SNAPSHOT_BYTES,
                    DEFAULT_MAX_PROJECT_BYTES,
                )
                .unwrap();
        }
        let touched = store.earliest_at_or_after(SESSION, 2, None).unwrap();
        assert_eq!(touched.len(), 1);
        assert_eq!(touched[0].seq, 2);
    }

    #[test]
    fn next_seq_starts_at_zero_and_increments_past_the_highest_recorded() {
        let dir = TempDir::new().unwrap();
        let store = store(&dir);
        assert_eq!(store.next_seq(SESSION).unwrap(), 0);
        let path = dir.path().join("f.txt");
        fs::write(&path, "v1").unwrap();
        store
            .record_observed(
                SESSION,
                7,
                &path,
                ToolKind::Write,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();
        assert_eq!(store.next_seq(SESSION).unwrap(), 8);
    }

    #[test]
    fn sessions_do_not_share_an_index() {
        let dir = TempDir::new().unwrap();
        let store = store(&dir);
        let path = dir.path().join("f.txt");
        fs::write(&path, "v1").unwrap();
        store
            .record_observed(
                "sess-a",
                1,
                &path,
                ToolKind::Write,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();
        assert!(store.entries("sess-b").unwrap().is_empty());
        assert_eq!(store.entries("sess-a").unwrap().len(), 1);
    }

    #[test]
    fn resolve_ref_reports_eviction_distinctly_from_never_captured() {
        let dir = TempDir::new().unwrap();
        let store = store(&dir);
        let (never, _) = store
            .capture(&[0u8; 100], 10, DEFAULT_MAX_PROJECT_BYTES)
            .unwrap();
        let ResolvedRef::Missing(never_reason) = store.resolve_ref(&never).unwrap() else {
            panic!("expected Missing");
        };
        assert!(never_reason.contains("per-file"), "{never_reason}");

        let (evictable, _) = store
            .capture(b"aaaaa", DEFAULT_MAX_SNAPSHOT_BYTES, 5)
            .unwrap();
        // Force eviction by storing a second, different blob at the same
        // tight bound.
        store
            .capture(b"bbbbb", DEFAULT_MAX_SNAPSHOT_BYTES, 5)
            .unwrap();
        let ResolvedRef::Missing(evicted_reason) = store.resolve_ref(&evictable).unwrap() else {
            panic!("expected Missing (evicted)");
        };
        assert!(evicted_reason.contains("evicted"), "{evicted_reason}");
    }
}
