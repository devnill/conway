# conway-session

`conway-session` implements the [`SessionStore` port](conway-core.md) from
`conway-core`: append-only persistence, ancestry resolution, and
fork-by-reference. See [`/ARCHITECTURE.md`](/ARCHITECTURE.md) for the
whole-system picture.

## Responsibility and boundary

`conway-session` owns:

- **`JsonlSessionStore`** (`store.rs`) — the `SessionStore` implementation:
  one append-only `.jsonl` file per session, crash-tolerant on read, never
  mutated or deleted in place.
- **the JSONL line codec** (`codec.rs`) — header and record
  (de)serialization.
- **`fork.rs`** — O(1) fork-by-reference.
- **`TranscriptResolver`** (`resolver.rs`) — an agent's effective
  transcript, computed by walking the ancestry chain.
- **`SessionIndex`** (`index.rs`) — a derived, rebuildable index that
  accelerates `list`/`children`/tree reconstruction. Never a source of
  truth; the log files are.
- **context-report persistence** (`provenance.rs`) — `append_context_report`
  / `load_context_report`, built on the ordinary append/read path.

It is explicitly **not** responsible for deciding *what* to persist (that's
[`conway-runtime`](conway-runtime.md)), context assembly (also
`conway-runtime`), or in-memory agent state. `meta.rs` re-exports
`conway-core`'s `SessionMeta`/`ForkOrigin`/`SessionStatus`/`SessionFilter`
rather than redefining them — `conway-core` is authoritative for every
persisted type's shape; this crate only owns how those types get written
to and read from disk.

## The append-only session log

Layout: `root/<session_id>.jsonl`, one file per session, no
subdirectories. Line 0 of every session file is the header —
`LogRecord::Header(SessionMeta)`, serialized directly, with `conway-core`'s
`#[serde(tag = "kind")]` supplying `"kind":"header"`. Every subsequent line
is one `LogRecord`; since every non-`Header` variant already carries its
own `seq: LogSeq` field, serializing the record directly already produces
a top-level `"seq"` key alongside `"kind"` via serde's internally-tagged
representation — no separate seq-injection step exists in the wire form.

`JsonlSessionStore` implements the full `SessionStore` port: `create`,
`append`, `read`, `head`, `fork`, `meta`, `children`, `list`. Its fsync
policy (`FsyncPolicy`, re-exported from `conway-core`'s `store.rs`
counterpart) governs write durability; reads tolerate a partially-written
final line (a crash mid-append) by treating it as absent rather than
corrupt.

`root/index.jsonl` is `SessionIndex`'s on-disk form — one JSON object per
line, a projection of each session's header (never records), extended with
`SessionMeta::cwd` so a `list()`/`children()` result served from a
*loaded* (not rebuilt) index doesn't silently return the wrong working
directory. It is skipped by every directory scan (a session id never
parses as the literal string `index`, so the skip falls out of the
id-parsing step). `SessionIndex`'s in-memory state
(`by_id`/`children`, `std::sync::RwLock`) is a synchronous, no-I/O cache
that `open_with` rebuilds by scanning the log directory when the index
file is missing or stale, and that `create`/`fork` both update after their
header write succeeds.

## Fork snapshot semantics

`fork::fork_impl` writes **exactly one header line** for the child and
copies **zero** parent records — this is what makes fork O(1) in parent
transcript size regardless of how many records the parent holds, which is
what makes a tournament pattern (one fork producing N spawned children)
affordable. `JsonlSessionStore::fork` delegates to it verbatim. The only
parent I/O `fork_impl` performs is a `store.head(parent)` call: when the
parent's per-session handle is already warm (the ordinary runtime case — a
fork always follows the parent agent having already appended through the
same live store), this is a mutex lock and an `Arc` clone, not a file
read. A cold parent handle still requires a directory scan to recover its
records and head, but that cost belongs to handle acquisition — an
amortized cost every store method pays once per session — not to `fork`
itself.

## Transcript / prefix resolution

`TranscriptResolver::resolve`/`resolve_prefix` compute an agent's
*effective* transcript by walking the ancestry chain and applying each
`ForkOrigin.at_seq` bound:

```text
prefix(sid, upto_local) =
    (if origin(sid): prefix(origin.parent, origin.at_seq) else [])
    ++ own_records(sid)[0..upto_local]
```

Every bound in this algorithm — `ForkOrigin.at_seq` and each recursion
level's `upto` — is a **local** index into that session's own records (the
same units `store.head` and `fork`'s range check use), never an index into
the effective transcript; conflating the two units was an earlier-cycle
defect (silent truncation of a non-root parent's true tip) that the
current implementation fixes by keeping every bound strictly local. The
inherited prefix always flows through in full — a fork inherits the
forker's entire context up to the fork point, never a partial slice.

`TranscriptResolver` caches resolved prefixes with allocations shared
across siblings: forking multiple children at the same parent sequence
number reuses one memoized `Arc<[LogRecord]>` rather than recomputing (or
re-storing) the same prefix once per child, which is what keeps N-way
fan-out cheap. This resolved prefix is what
[`conway-runtime`](conway-runtime.md)'s `ContextBuilder` replays,
unchanged, into every one of a fork child's turns.

## Context-report persistence

Per-turn context provenance (which segments went into a request, their
`Provenance`, and their estimated token cost) survives a process restart:
`provenance.rs` persists it as an ordinary `LogRecord::
ContextReportRecord` (`kind == "context_report"`) through the same
`store.append`/`store.read` path every other record uses, inheriting fsync
policy, seq assignment, and crash tolerance with no new file format. The
`ContextReport`/`ContextReportEntry` types themselves are defined once, in
`conway-core::provenance` — this module re-exports them rather than
defining a second, differently-shaped copy, since `LogRecord::
ContextReportRecord` already embeds the `conway-core` type by name.

## How it fits the whole

`conway-session` depends only on [`conway-core`](conway-core.md).
[`conway-runtime`](conway-runtime.md) is its primary consumer: the agent
loop appends `LogRecord`s as a turn executes (persist-before-act — the log
is truth, in-memory state is a cache), and `ContextBuilder` calls
`TranscriptResolver` to assemble a fork child's inherited prefix. The
[`conway`](conway.md) facade exposes session listing/inspection
(`SessionHandle`, `conway sessions`) built on `SessionStore::list`/
`children`/`meta`. See [`/ARCHITECTURE.md §3.1–3.2`](/ARCHITECTURE.md) for
the fork/spawn and context-assembly picture this crate underpins.
