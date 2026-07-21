## Size Assessment

**Right size.** The module decomposes into 6 work items along the axis: types/codec → store I/O + durability → fork → transcript resolution → index → provenance persistence. Every file is owned by exactly one item; shared-file edits are eliminated by having WI-046 create the full module skeleton with exact stub signatures that later items fill in.

Assumption stated: the module spec does not name the crate's file layout. I fix it below (`crates/conway-session/src/{meta,codec,store,fork,resolver,index,provenance}.rs`). All `conway-core` types are referenced as external (`MODULE:conway-core`).

---

# WI-046: conway-session crate skeleton, session metadata types, and JSONL line codec

**complexity:** Medium

**scope:**
- `crates/conway-session/Cargo.toml` (create)
- `crates/conway-session/src/lib.rs` (create)
- `crates/conway-session/src/meta.rs` (create)
- `crates/conway-session/src/codec.rs` (create)
- `crates/conway-session/src/store.rs` (create — stub only)
- `crates/conway-session/src/fork.rs` (create — stub only)
- `crates/conway-session/src/resolver.rs` (create — stub only)
- `crates/conway-session/src/index.rs` (create — stub only)
- `crates/conway-session/src/provenance.rs` (create — stub only)
- `crates/conway-session/tests/codec_tests.rs` (create)

**depends:** MODULE:conway-core

**criteria:**
- [machine] `crates/conway-session` is a workspace member; `cargo check -p conway-session` succeeds.
- [machine] `conway_session::{SessionMeta, ForkOrigin, SessionStatus, SessionFilter}` are public, `Serialize + Deserialize + Clone + Debug + PartialEq`.
- [machine] `SessionMeta` has exactly the fields `id: SessionId, agent_id: AgentId, origin: Option<ForkOrigin>, agent_def: Option<String>, role: Option<RoleAlias>, created: DateTime<Utc>, cwd: PathBuf, labels: Vec<String>, status: SessionStatus`.
- [machine] `ForkOrigin` has exactly `parent: SessionId, at_seq: LogSeq, mode: SubagentMode`.
- [machine] Serializing a `SessionMeta` with `origin = Some(...)` via `codec::encode_header` produces a single-line JSON object with `"kind":"header"` and keys `session, agent, created, origin{parent,at_seq,mode}, agent_def, role, cwd, labels, status`; round-tripping through `codec::decode_header` yields an equal value (test asserts equality on the §5.1 example header line verbatim).
- [machine] `codec::encode_record(&LogRecord, seq: LogSeq) -> String` emits exactly one line ending in `\n`, containing no interior `\n`, with `"seq"` and `"kind"` as top-level keys.
- [machine] `codec::decode_line(&str) -> Result<Line, CodecError>` where `Line = Header(SessionMeta) | Record{seq: LogSeq, rec: LogRecord}`; decoding a record line missing `seq` returns `CodecError::MissingSeq`.
- [machine] Property test (proptest, ≥256 cases): `decode_record(encode_record(r, s)) == (s, r)` for arbitrary `LogRecord`.
- [machine] `cargo clippy -p conway-session -- -D warnings` passes.

**notes:**

Objective: Establish the crate, the persisted metadata schema, and the line-level (de)serialization that every other work item in this module depends on. Also lay down the module skeleton with exact stub signatures so no two later work items edit the same file.

Implementation Notes:
- `Cargo.toml` dependencies: `conway-core` (path), `serde`, `serde_json`, `tokio` (features `fs`, `sync`, `time`, `rt`), `async-trait`, `thiserror`, `chrono`, `tracing`, `lru`. Dev-dependencies: `proptest`, `tempfile`, `tokio` (`macros`, `rt-multi-thread`).
- `lib.rs` declares, in this order: `pub mod meta; pub mod codec; pub mod store; pub mod fork; pub mod resolver; pub mod index; pub mod provenance;` and re-exports `pub use meta::{SessionMeta, ForkOrigin, SessionStatus, SessionFilter}; pub use store::{JsonlSessionStore, StoreConfig, FsyncPolicy}; pub use resolver::TranscriptResolver; pub use index::SessionIndex; pub use provenance::ContextReport;`.
- `SessionStatus`: `#[non_exhaustive] enum { Running, Completed, Failed, Cancelled }`, serde `snake_case`.
- `SessionFilter`: `{ parent: Option<SessionId>, status: Option<SessionStatus>, label: Option<String>, limit: Option<usize> }`, all `None` = match everything.
- Header wire form (line 0 of every session file) matches §5.1 exactly. Field name mapping via serde rename: `id`→`session`, `agent_id`→`agent`. `origin` is omitted when `None` (`skip_serializing_if`). `kind: "header"` is a serde tag literal.
- Record wire form: `{"seq": u64, "kind": "...", ...}`. `LogRecord` from `conway-core` carries its own `kind` tag; `encode_record` merges `seq` into the serialized object via `serde_json::Value::Object` insertion, then `to_string`. If `LogRecord` does not serialize to a JSON object, return `CodecError::NotAnObject`.
- `CodecError`: `#[derive(thiserror::Error)] { Json(#[from] serde_json::Error), MissingSeq, MissingKind, NotAnObject, WrongLineKind{expected, got} }`. `store.rs` converts it into `conway_core::StoreError::Corrupt{ path, line, source }`.
- Stub files created here contain only module docs plus these exact signatures, bodies `todo!()`, so downstream items fill them without touching each other:
  - `store.rs`: `pub struct JsonlSessionStore; pub struct StoreConfig; pub enum FsyncPolicy; impl JsonlSessionStore { pub async fn open(root: PathBuf) -> Result<Self, StoreError>; pub async fn open_with(root: PathBuf, cfg: StoreConfig) -> Result<Self, StoreError>; }`
  - `fork.rs`: `pub(crate) async fn fork_impl(store: &JsonlSessionStore, parent: &SessionId, at: LogSeq, meta: SessionMeta) -> Result<SessionId, StoreError>;`
  - `resolver.rs`: `pub struct TranscriptResolver; impl TranscriptResolver { pub fn new(capacity: usize) -> Self; pub async fn resolve(&self, store: &JsonlSessionStore, sid: &SessionId) -> Result<Arc<[LogRecord]>, StoreError>; }`
  - `index.rs`: `pub struct SessionIndex; impl SessionIndex { pub(crate) async fn load_or_rebuild(root: &Path) -> Result<Self, StoreError>; pub(crate) fn record_header(&self, meta: &SessionMeta); pub(crate) async fn flush(&self, root: &Path) -> Result<(), StoreError>; pub(crate) fn children(&self, sid: &SessionId) -> Vec<SessionId>; pub(crate) fn list(&self, f: &SessionFilter) -> Vec<SessionMeta>; }`
  - `provenance.rs`: `pub struct ContextReport { pub turn: u32, pub segments: Vec<ContextSegmentEntry> } pub struct ContextSegmentEntry { pub segment: SegmentId, pub provenance: Provenance, pub tokens_est: u32 }` with `todo!()`-free plain data (types are real here; persistence helpers are stubs).
- The store never inspects record fields other than `seq` and `kind`; the codec must therefore not deserialize record bodies into anything more specific than `LogRecord`.

---

# WI-047: JsonlSessionStore — file layout, create/append/read/head/meta, fsync policy, crash-tolerant reads

**complexity:** High

**scope:**
- `crates/conway-session/src/store.rs` (modify)
- `crates/conway-session/tests/store_tests.rs` (create)
- `crates/conway-session/tests/recovery_tests.rs` (create)

**depends:** WI-046

**criteria:**
- [machine] `JsonlSessionStore::open(root)` creates `root` recursively if absent and returns `Ok`; opening an existing root does not modify any session file (byte-compare before/after).
- [machine] `create(meta)` writes the file `root/<meta.id>.jsonl` containing exactly one line (the header) and returns `meta.id`; `head()` on it returns `LogSeq(0)` meaning "no records yet".
- [machine] `create` with an id whose file already exists returns `StoreError::AlreadyExists`.
- [machine] `append(sid, rec)` assigns the next seq (first record = 0, strictly +1 thereafter), returns it, and appends exactly one line; after N appends the file has N+1 lines and seqs `0..N` in order.
- [machine] `read(sid, SeqRange::Full)` returns all records in seq order and excludes the header; `read(sid, 2..5)` returns exactly seqs 2,3,4; a range beyond head returns the available subset without error.
- [machine] `meta(sid)` returns the header parsed from line 0; a missing file returns `StoreError::NotFound`.
- [machine] Recovery test: a session file with a trailing line truncated mid-JSON is readable — `read` returns all complete records, the file is truncated to the end of the last complete line (`set_len`), a `tracing` WARN containing `"truncated trailing line"` is emitted (captured via `tracing-test`), and no error is returned.
- [machine] Recovery test: a file whose *header* line is incomplete returns `StoreError::Corrupt` and is not truncated.
- [machine] Recovery test: after truncate-and-warn, the next `append` writes seq = last_complete_seq + 1 and the file re-reads cleanly.
- [machine] Fsync test: with `FsyncPolicy::Always`, an instrumented counter shows one fsync per `append`; with `Never`, zero; with `Interval(200ms)`, header writes and records whose `kind == "agent_result"` still fsync immediately (counter asserts ≥1 for those two cases).
- [machine] Concurrency test: 10 concurrent `append` tasks across 10 distinct sessions complete with no lock contention path taken (assert per-session handles are distinct) and all 10 files re-read with contiguous seqs.
- [machine] Concurrency test: 100 concurrent `append` calls to the *same* session produce seqs `0..100` with no duplicates and no interleaved partial lines.
- [machine] No public function mutates or rewrites an existing record line (assert: after any append sequence, the byte prefix of the file equals its previous full contents).

**notes:**

Objective: Implement the durable, append-only, one-file-per-session backing storage and the `SessionStore` methods that do not require ancestry or index knowledge.

Implementation Notes:
- Layout: `root/<session_id>.jsonl`, one file per session, ULID filename, no subdirectories. Index file `root/index.jsonl` is reserved for WI-050 and must be skipped by any directory scan here.
- `StoreConfig { fsync: FsyncPolicy, lru_capacity: usize }`; `Default` = `{ fsync: FsyncPolicy::Interval(Duration::from_millis(200)), lru_capacity: 64 }`. `FsyncPolicy { Always, Interval(Duration), Never }`, serde `snake_case` with `interval` carrying a humantime duration string.
- Per-session writer state: `DashMap<SessionId, Arc<Mutex<SessionFile>>>` where `SessionFile { file: tokio::fs::File, head: LogSeq, last_fsync: Instant, dirty: bool }`. The mutex is per session only — never a global write lock (boundary rule: N siblings write N files with no shared lock).
- `append` sequence: serialize line (WI-046 codec) → `write_all` → apply fsync policy → update `head` → return seq. Persist-before-act means `append` must not return until the durability policy for that record has been satisfied.
- Fsync rule: `Always` ⇒ `sync_data()` every write. `Interval(d)` ⇒ `sync_data()` if `last_fsync.elapsed() >= d`, plus unconditionally when the line is a header or when the record's `kind` is `"agent_result"`. `Never` ⇒ never call `sync_data`. A background flusher task per store ticks at `d` and syncs dirty handles so idle sessions are not left unsynced longer than `d`.
- Reads use a buffered line reader over the whole file (no seq→offset index in MVP; the range filter is applied after decode). Record `seq` from the line is authoritative; the store must not assume `line_number - 1 == seq`, and a non-contiguous seq mid-file is `StoreError::Corrupt{ line }` (only *trailing* damage is tolerated).
- Truncate-and-warn: while reading, if the final line fails to decode OR does not end with `\n`, discard it, `file.set_len(offset_of_last_good_line_end)`, `tracing::warn!(session=%sid, dropped_bytes, "truncated trailing line")`, and continue. Applies only to the last line; a decode failure at any earlier line is `Corrupt`.
- `head(sid)` returns `last_seq + 1` (i.e., the count of records, the exclusive upper bound), which is the value passed as `at` to `fork`. Document this on the method; WI-048 and WI-049 rely on it.
- Trait wiring: `impl SessionStore for JsonlSessionStore` lives here. `fork` delegates verbatim to `crate::fork::fork_impl`; `children`/`list` delegate verbatim to `crate::index::SessionIndex`. Do not implement those bodies in this item.
- Errors map to `conway_core::StoreError::{NotFound, AlreadyExists, Corrupt, Io}`. The store must never interpret a record beyond `seq` and `kind`.

---

# WI-048: O(1) fork-by-reference

**complexity:** Medium

**scope:**
- `crates/conway-session/src/fork.rs` (modify)
- `crates/conway-session/tests/fork_tests.rs` (create)

**depends:** WI-047

**criteria:**
- [machine] `fork(parent, at, meta)` creates `root/<child>.jsonl` whose total line count is exactly 1.
- [machine] The child's header `origin` equals `Some(ForkOrigin{ parent: parent.clone(), at_seq: at, mode: meta.origin.mode })`; if the caller passes `meta.origin == None`, the function fills it in from its arguments rather than erroring.
- [machine] Zero-copy assertion: after forking a parent with 10,000 records, `child_file_len_bytes < 2_000` and `read(child, Full).len() == 0`.
- [machine] O(1) assertion: fork of a 10-record parent and fork of a 10,000-record parent both write exactly 1 line, and the number of parent lines *read* during `fork` is 0 (instrumented read counter asserts 0 — `fork` may stat the parent but must not scan it).
- [machine] `fork` with `at > store.head(parent)` returns `StoreError::InvalidRange{ at, head }` and creates no file.
- [machine] `fork` with a nonexistent parent returns `StoreError::NotFound` and creates no file.
- [machine] Parent-immutability property test (proptest, ≥128 cases): for arbitrary parent record sequences and arbitrary fork points, the parent file's bytes are unchanged by `fork`, and appending M further records to the parent after the fork leaves `TranscriptResolver` output for the child identical to the pre-append result (test may assert the weaker byte-level invariant if run before WI-049 lands; the resolver assertion is added by WI-049's suite).
- [machine] Sibling test: 10 forks of the same parent at the same `at` produce 10 distinct session ids, 10 distinct files, and the parent is byte-identical afterward.
- [machine] The child header line is fsynced before `fork` returns, under all three `FsyncPolicy` values.

**notes:**

Objective: Implement `SessionStore::fork` as a single header write that references the parent by `(parent, at_seq, mode)` and copies nothing.

Implementation Notes:
- Signature is fixed by WI-046's stub: `pub(crate) async fn fork_impl(store: &JsonlSessionStore, parent: &SessionId, at: LogSeq, meta: SessionMeta) -> Result<SessionId, StoreError>`.
- Procedure: (1) validate parent exists (`meta(parent)` reads only line 0, or a file-exists check plus header read — never a full scan); (2) validate `at <= head(parent)`; (3) normalize `meta.origin` to `Some(ForkOrigin{parent, at_seq: at, mode})`, preserving the caller-supplied `mode`; (4) delegate to the same header-writing path `create` uses, which fsyncs headers unconditionally; (5) return `meta.id`; (6) notify `SessionIndex` via `record_header` (the index call is a no-op stub until WI-050 — call it here so WI-050 needs no edit to this file).
- Cost contract: the only parent I/O permitted is reading line 0 and the cached `head`. Implement `head` lookup from the in-memory `SessionFile` state when present, falling back to a tail scan only for sessions not currently open. Document that a cold-parent fork is O(parent) in *bytes read for head discovery only if head is unknown*; to satisfy the read-count assertion, tests fork from a parent that is open in the store, which is the runtime's actual usage.
- Immutability semantics to encode in doc comments and tests: records at `seq < at` are frozen from the child's perspective; parent appends at `seq >= at` are invisible to the child forever. A fork is a snapshot, not a live view.
- `mode` is `conway_core::SubagentMode` (`Fork | Spawn`). `Spawn` children also get an `origin` (the tree link is real) — the difference in what is *inherited* is a `conway-runtime` context-assembly concern, not a storage concern. Storage records the link identically for both modes.

---

# WI-049: TranscriptResolver with bounded LRU memoization

**complexity:** High

**scope:**
- `crates/conway-session/src/resolver.rs` (modify)
- `crates/conway-session/tests/resolver_tests.rs` (create)

**depends:** WI-048

**criteria:**
- [machine] `TranscriptResolver::resolve(&store, sid)` for a root session (`origin == None`) returns exactly that session's records in seq order.
- [machine] For a child with `origin{parent, at_seq}`, the result equals `resolve(parent)[0..at_seq] ++ own_records`, asserted element-wise on a 3-level ancestry chain (grandparent → parent → child) with distinct records at every level.
- [machine] Transitivity test: a chain of depth 5 with forks at differing `at_seq` values produces the concatenation predicted by an independently-written reference implementation in the test file.
- [machine] Sharing test: two siblings forked from the same parent at the same `at_seq` cause exactly one parent-prefix `Arc` allocation — asserted via `Arc::ptr_eq` on the memoized prefix returned by `resolver.peek_prefix(parent, at_seq)`.
- [machine] Memoization test: resolving the same `(sid, at_seq)` twice performs file reads only on the first call (instrumented read counter on the store asserts no additional reads on the second).
- [machine] Bound test: with `TranscriptResolver::new(2)`, resolving 3 distinct keys then re-resolving the first triggers a re-read (LRU eviction observed).
- [machine] Snapshot test: appending to the parent after a child was forked at `at_seq` does not change `resolve(child)`, before or after cache invalidation of the parent's own full-transcript entry.
- [machine] Cycle test: a hand-crafted pair of headers whose origins reference each other returns `StoreError::CorruptAncestry` rather than looping; depth beyond 256 returns the same error.
- [machine] `resolve` returns `Arc<[LogRecord]>`; the function is `async` and `Send`.
- [machine] Property test (proptest, ≥128 cases): for random fork trees, `resolve(child).len() == at_seq + own_records.len()` and the first `at_seq` elements equal the parent's resolved prefix.

**notes:**

Objective: Compute an agent's effective transcript by walking the ancestry chain, applying `origin.at_seq` truncation, with allocations shared across siblings.

Implementation Notes:
- Memoization key: `(SessionId, LogSeq)` where `LogSeq` is the *exclusive upper bound* of the resolved prefix. Two distinct entry kinds share the map:
  - prefix entry `(sid, at_seq)` → the first `at_seq` records of `sid`'s effective transcript;
  - full entry `(sid, head_at_resolve_time)` → the complete effective transcript.
  Because a full resolve of `sid` at head H is exactly the prefix `(sid, H)`, one keyspace suffices. Never key on "full" as a sentinel — always the concrete bound.
- Cache: `Mutex<lru::LruCache<(SessionId, LogSeq), Arc<[LogRecord]>>>` with capacity from `StoreConfig::lru_capacity` (default 64), constructor `TranscriptResolver::new(capacity)`. Capacity is entry count, not bytes.
- Algorithm (`resolve_prefix(sid, upto: Option<LogSeq>)`):
  1. Read `meta(sid)`. If `upto` is `None`, set `upto = store.head(sid)`.
  2. Cache lookup on `(sid, upto)`; hit → clone the `Arc` and return.
  3. If `origin == Some(o)`, `parent_prefix = resolve_prefix(o.parent, Some(o.at_seq))`; else `parent_prefix = empty`.
  4. `own = store.read(sid, 0..min(upto_own, head))` where `upto_own = upto.saturating_sub(parent_prefix.len())` — i.e. the requested bound is measured over the *effective* transcript, so truncation applies to own records only after the inherited prefix is accounted for. If `upto <= parent_prefix.len()`, take a sub-slice of the parent prefix and skip the own-record read entirely.
  5. Build `Arc<[LogRecord]>` by `parent_prefix.iter().cloned().chain(own).collect()`, insert into the cache, return.
  Recursion is written iteratively (ancestor chain collected first, then folded from root down) to avoid `async fn` recursion; use an explicit `Vec<SessionMeta>` ancestor stack.
- Cycle/depth guard: while collecting ancestors, track a `HashSet<SessionId>`; a repeat or depth > 256 ⇒ `StoreError::CorruptAncestry{ chain }`.
- Sibling sharing is a direct consequence of step 3: both siblings request `(parent, at_seq)` and receive the same `Arc`. When their own records are non-empty a new `Arc` is allocated for each child's full transcript, but the parent prefix `Arc` is stored once. `peek_prefix(sid, at_seq) -> Option<Arc<[LogRecord]>>` is a `#[doc(hidden)]` test hook returning the cache entry without computing it.
- Invalidation: entries are immutable snapshots and are never invalidated on parent append — appending to `sid` only makes new higher-bound keys reachable; existing `(sid, at_seq)` entries stay correct forever. This is what makes the snapshot invariant free.
- `TranscriptResolver` is `Send + Sync` and cheap to clone (`Arc` interior). `JsonlSessionStore` owns one instance and exposes `store.resolver()`.

---

# WI-050: SessionIndex — derived list/children acceleration with rebuild-by-scan

**complexity:** Medium

**scope:**
- `crates/conway-session/src/index.rs` (modify)
- `crates/conway-session/tests/index_tests.rs` (create)

**depends:** WI-048

**criteria:**
- [machine] `SessionIndex::load_or_rebuild(root)` with no `index.jsonl` present scans `root/*.jsonl` (excluding `index.jsonl`), reads line 0 of each, and produces an index whose `list(SessionFilter::default())` returns every session's `SessionMeta`.
- [machine] Rebuild equivalence test: given 50 sessions, deleting `index.jsonl` and calling `load_or_rebuild` yields `list`/`children` results equal (as sets) to the pre-deletion results.
- [machine] Corruption test: an `index.jsonl` with a truncated trailing line, a line referencing a session file that no longer exists, and a duplicate entry all resolve to a full rebuild-by-scan with a `tracing` WARN containing `"index rebuild"`; no error is returned.
- [machine] `children(sid)` returns exactly the sessions whose header `origin.parent == sid`, in ascending `created` order; a session with no children returns an empty vec.
- [machine] `list(filter)` honors `parent`, `status`, `label`, and `limit`; filters compose with AND semantics; `limit` applies after filtering and ordering.
- [machine] `list` ordering is descending `created`, ties broken by ascending `id` (deterministic).
- [machine] `create` and `fork` both cause the new header to appear in `children`/`list` without a rebuild (in-memory update path exercised).
- [machine] `index.jsonl` is append-only: after 100 session creations it contains 100 lines and its byte prefix at each step equals the previous contents.
- [machine] Store never fails a `create`/`fork` because of index I/O — a test making `index.jsonl` read-only asserts `create` still succeeds and emits a WARN.
- [machine] Tree test: a 3-level fork tree is reconstructable purely from `children()` calls starting at the root, matching the constructed shape.

**notes:**

Objective: Provide the derived, rebuildable index that accelerates `list`, `children`, and tree reconstruction, without ever becoming a source of truth.

Implementation Notes:
- On-disk form: `root/index.jsonl`, one JSON object per line: `{"session":..., "agent":..., "parent":<SessionId|null>, "at_seq":<u64|null>, "mode":<"fork"|"spawn"|null>, "created":..., "agent_def":..., "role":..., "status":..., "labels":[...]}`. This is a projection of the header; it stores no records.
- In-memory form: `RwLock<IndexState>` with `by_id: HashMap<SessionId, SessionMeta>` and `children: HashMap<SessionId, Vec<SessionId>>`. `record_header(&meta)` updates both and appends one line to `index.jsonl` (best-effort; see failure policy below).
- `load_or_rebuild(root)` procedure: attempt to read `index.jsonl` line by line; trigger a full rebuild if **any** of: the file is absent; a line fails to decode; an entry names a session file absent from `root`; a session file present in `root` has no entry; a duplicate `session` id appears. Rebuild = list `root/*.jsonl` excluding `index.jsonl`, read line 0 of each via the codec, drop and WARN on files whose header is corrupt, then rewrite `index.jsonl` atomically (write `index.jsonl.tmp`, fsync, rename). The rename is the only non-append write permitted, and it targets the derived file only — never a session file.
- Failure policy: the index is a cache. Any index I/O error during `record_header`/`flush` is logged at WARN with the session id and swallowed; the session write has already succeeded and the index will be reconstructed by scan on the next `load_or_rebuild`. Index errors must never propagate into `StoreError` from `create`/`fork`/`append`.
- `flush(root)` is called on store drop and by the interval flusher from WI-047; it fsyncs `index.jsonl`.
- `list`/`children` read only in-memory state — no file I/O on the hot path.
- The index deliberately does not track `head`; head is a per-file property and staleness there would be a correctness hazard.

---

# WI-051: ContextReport persistence and retrieval

**complexity:** Low

**scope:**
- `crates/conway-session/src/provenance.rs` (modify)
- `crates/conway-session/tests/provenance_tests.rs` (create)

**depends:** WI-047

**criteria:**
- [machine] `ContextReport { turn: u32, segments: Vec<ContextSegmentEntry> }` and `ContextSegmentEntry { segment: SegmentId, provenance: Provenance, tokens_est: u32 }` are public, `Serialize + Deserialize + Clone + Debug + PartialEq`.
- [machine] `provenance::append_context_report(&store, sid, &ContextReport) -> Result<LogSeq, StoreError>` appends exactly one line whose `kind` is `"context_report"` and returns its seq.
- [machine] Round-trip test: append a report with 5 segments spanning 5 distinct `Provenance` variants, then `load_context_report(&store, sid, turn)` returns a value equal to the original.
- [machine] `load_context_report(&store, sid, turn)` returns the report for the requested `turn`; if multiple reports share a turn, the highest-seq one wins; absent turn returns `Ok(None)`.
- [machine] `load_all_context_reports(&store, sid)` returns reports in ascending seq order.
- [machine] Restart test: append reports, drop the store, reopen `JsonlSessionStore::open(root)`, and assert reports are byte-identically recoverable — provenance survives process restart.
- [machine] Reports appear in `read(sid, Full)` as ordinary `LogRecord`s interleaved in seq order with turns (asserted by seq adjacency to the surrounding assistant record) — the store applies no special-casing beyond `kind` matching.
- [machine] A `ContextReport` with zero segments serializes and round-trips without error.

**notes:**

Objective: Persist the per-turn provenance report alongside the turn it describes, so `Runtime::context_report` can answer for historical turns after a restart (GP-10, decision 9).

Implementation Notes:
- The report is persisted as an ordinary `LogRecord` variant with `kind == "context_report"`; it is written through the same `store.append` path as every other record and therefore inherits the fsync policy, seq assignment, and crash tolerance from WI-047. This item adds no new file format and no new durability rule.
- `append_context_report` constructs the `LogRecord` and calls `store.append`; it exists as a typed convenience so callers do not hand-build the record. `load_context_report` scans `store.read(sid, SeqRange::Full)`, filters on `kind == "context_report"`, deserializes the payload, and selects by `turn`. Linear scan is acceptable: reports are read on demand by inspection APIs, never on the agent-loop hot path.
- The store must not interpret the report's contents. Matching on `kind` to filter is the only permitted interpretation; `segments`, `provenance`, and `tokens_est` are opaque payload.
- `tokens_est` is an estimate, per T-9. Do not add a tokenizer dependency here; the field is written by the runtime and stored verbatim.
- Reports are appended *after* the turn's assistant record so a truncated trailing line can lose a report without losing the turn it describes.

---

## Coverage Statement

**Module:** conway-session
**Work items:** WI-046, WI-047, WI-048, WI-049, WI-050, WI-051

**Coverage:** These six work items collectively implement 100% of the module's stated scope — `JsonlSessionStore`, log-record serialization, ancestry resolution, fork-by-reference, resume (satisfied by `open` + `read` + `TranscriptResolver` reconstructing any persisted session from disk with no live state), and the session index. All six boundary rules are covered: append-only (WI-047 prefix-immutability criterion, WI-050 append-only index); O(1) fork with no record copying (WI-048); one file per session with no cross-session lock (WI-047 concurrency criteria); persist-before-act with configurable fsync and `always` for headers and `AgentResult` (WI-047); truncate-and-warn on a partial trailing line (WI-047 recovery suite); and no semantic interpretation beyond `kind`/`seq` (WI-047, WI-051). Nothing in the module scope is intentionally excluded. Fork *context semantics* (what a Fork vs Spawn child inherits into its prompt) is correctly absent — it belongs to `conway-runtime`'s `ContextBuilder` per §11.2; this module records the link identically for both modes.

**Provides implemented by:**
- `JsonlSessionStore::open(root) -> Result<impl SessionStore>` → WI-047 (with `fork` delegated to WI-048, `children`/`list` delegated to WI-050)
- `TranscriptResolver::resolve(&store, sid) -> Result<Arc<[LogRecord]>>`, ancestry walk, `at_seq` truncation, memoized per `(sid, at_seq)` in a bounded LRU with sibling `Arc` sharing → WI-049
- `SessionIndex` (derived, rebuildable, accelerates `list`/`children`/tree reconstruction) → WI-050
- `SessionMeta { id, agent_id, origin, agent_def, role, created, cwd, labels, status }` → WI-046 (types + header codec), WI-047 (write/read)
- `ForkOrigin { parent, at_seq, mode }` → WI-046 (type), WI-048 (population and invariant)
- `provenance::ContextReport { segments: Vec<(SegmentId, Provenance, tokens_est)> }` persisted alongside each turn → WI-051 (types declared in WI-046)

**Requires consumed by:**
- `MODULE:conway-core` — `SessionStore` trait → implemented by WI-047 (all methods), WI-048 (`fork`), WI-050 (`children`, `list`)
- `MODULE:conway-core` — `LogRecord`, `LogSeq`, `SeqRange` → WI-046 (codec), WI-047 (append/read), WI-049 (resolution), WI-051 (report record)
- `MODULE:conway-core` — `SessionId`, `AgentId`, `SegmentId`, `RoleAlias`, `SubagentMode` → WI-046, WI-048, WI-051
- `MODULE:conway-core` — `Provenance` → WI-051
- `MODULE:conway-core` — `StoreError` → WI-046 (codec error mapping), WI-047, WI-048, WI-049, WI-050

**Interface contracts honored (§8):** `fork` POST-conditions (exactly one header line, zero records copied, O(1), `meta.origin == Some(ForkOrigin{parent, at_seq: at, mode})`) are asserted by WI-048's criteria. `resolve` POST-condition (`== concat(resolve(parent)[0..at_seq], own_records)` transitively, shared across siblings) is asserted by WI-049's criteria. Per §9, WI-048 completes fork semantics before the runtime's Group 2 track G depends on it.

**Dependency DAG:** WI-046 → WI-047 → {WI-048, WI-051}; WI-048 → {WI-049, WI-050}. No cycles. No file is listed by two items without a dependency edge; the only files touched twice are the WI-046 stubs (`store.rs`, `fork.rs`, `resolver.rs`, `index.rs`, `provenance.rs`), each modified by exactly one downstream item.