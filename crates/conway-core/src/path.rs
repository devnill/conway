//! The first-class context path — pure value types, the model-free
//! `SelectionKey`, the refusing constructors `derive`/`derive_reordered`,
//! and the three-rule coherence validator (DESIGN-context-path §2.1–§2.9,
//! §4.1–§4.2).
//!
//! A **path** is an ordered list of *references* to immutable records plus an
//! optional reference to another path **selection** as its prefix. A
//! **selection** is a frozen path identified by a model-free,
//! content-addressed `SelectionKey` over its (expanded) node list. This module
//! owns the vocabulary and the two refusing constructors; the tolerant
//! constructor (`default_path`), the `PathStore` port, head resolution and
//! assembly are later sub-units (D1-3b..e) and live elsewhere.
//!
//! What is here is pure: no I/O, no store, no policy. `derive`/`derive_reordered`
//! refuse an incoherent candidate with `PathError::WouldOrphan` rather than
//! constructing it; only `default_path` (D1-3c) tolerates and declares harness
//! incoherence. Everything round-trips through serde, and `SelectionKey`'s
//! exclusions are documented at the hash site because they are load-bearing
//! for the "ten heads, one selection" sharing story (§2.3).

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::canon::canonical_json_bytes;
use crate::content::ContentBlock;
use crate::ids::{LogSeq, SessionId};
use crate::log::LogRecord;

// ──────────────────────────────────────────────────────────────────────────
// §2.1  Record = blob
// ──────────────────────────────────────────────────────────────────────────

/// A reference to one immutable record in one session's append-only log.
///
/// `LogRecord` carries no session id (`log.rs`), so `(SessionId, LogSeq)` is
/// the only addressing that exists and the only one needed. Records are never
/// copied, never rewritten (DESIGN §2.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RecordRef {
    pub session: SessionId,
    pub seq: LogSeq,
}

impl fmt::Display for RecordRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `session/seq`, matching how a curator reads a node aloud.
        write!(f, "{}/{}", self.session, self.seq)
    }
}

// ──────────────────────────────────────────────────────────────────────────
// §2.2  Path node
// ──────────────────────────────────────────────────────────────────────────

/// Which of the two existing mapping functions runs for a node (DESIGN §2.2):
///
/// - `Inherited { from }` → `record_role_and_content` + `Provenance::Inherited`;
/// - `Head` → the head-segment mapping that forces `UserPrompt`/`ForkDirective`
///   regardless of stored provenance;
/// - `Own` → `own_segment`.
///
/// The stamp lives on the *node* rather than being derived in the builder so
/// byte-identity is mechanical. It also carries the tier boundary: the frozen
/// portion of a rendering is *statics + `Inherited` nodes*; `Head` and `Own`
/// are volatile. Committing a curated selection so children can share its cache
/// is exactly re-stamping `Head`/`Own` nodes as `Inherited { from: <true owner> }`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeStamp {
    /// Inherited from `from` (the immediate parent at fork depth ≥ 2, per the
    /// legacy carry-over noted in DESIGN §3; a *derived* path stamps the true
    /// owning session instead — the two are honestly different selections).
    Inherited { from: SessionId },
    /// The owning session's own first record (forced `UserPrompt`/`ForkDirective`
    /// mapping regardless of stored provenance).
    Head,
    /// The owning session's own subsequent records.
    Own,
}

/// A string newtype for the operation a curation plugin named (e.g. `"omit"`,
/// `"include"`, `"move"`). Free-form so a plugin can declare its own verbs;
/// the harness never parses it, only records it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OpLabel(pub String);

impl OpLabel {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OpLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Who selected a node (DESIGN §2.2). A selection is model-free but it is not
/// authorless: this records which of the three selection routes put a node on
/// the path. Deliberately *outside* `SelectionKey` (§2.3) — provenance travels
/// with the selection but does not identify it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Selector {
    /// The one built-in constructor `default_path` — includes everything, in
    /// log order, excludes nothing. The identity function over today's
    /// behaviour (DESIGN §5).
    DefaultRule,
    /// A curation plugin with id `id` ran operation `op`.
    Plugin { id: String, op: OpLabel },
    /// The operator, through `conway path` verbs (DESIGN §4.5, §10).
    Operator,
}

/// Why a node is on this path, and when that decision was made (DESIGN §2.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeProvenance {
    pub selected_by: Selector,
    pub at: DateTime<Utc>,
}

/// One node on a path: a record reference, its stamp, and its provenance
/// (DESIGN §2.2). A node is always a record — the static preamble (agent-def
/// name/text, skills, tool-registry hash) belongs to the rendering, not the
/// selection (DESIGN §2.4), so it has no node here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathNode {
    pub record: RecordRef,
    pub stamp: NodeStamp,
    pub prov: NodeProvenance,
}

// ──────────────────────────────────────────────────────────────────────────
// §2.3  Selection = the durable identity
// ──────────────────────────────────────────────────────────────────────────

/// Stable, model-free identity of a path selection: `blake3` hex, transparent
/// serde, same shape as `PrefixKey` (DESIGN §2.3).
///
/// The hash is
/// `blake3("conway.selection.v1" ‖ canonical_json_bytes(projection))`
/// where `projection` is a JSON array, one entry per node **of the fully
/// expanded list** (prefix chains flattened first), each entry being exactly
/// `{ "record": { "session": …, "seq": … }, "stamp": … }`.
///
/// **Deliberately excluded, each for a reason that must survive a later
/// reader** — stated in the style of `prefix.rs`'s own doc, because that doc
/// is why its exclusions have held:
///
/// - **`ModelId`.** A selection is model-free. Switching models invalidates
///   the rendering (a cache, supposed to be invalidated) and leaves the
///   curation untouched. See `from_nodes` for the encoding.
/// - **Everything in the static preamble** — agent-def name and text, skill
///   names and texts, tool-registry hash (DESIGN §2.4). They are rendering
///   inputs, not selection inputs.
/// - **`NodeProvenance` (`selected_by`, `at`).** Who curated, and when, is a
///   fact about the *act*, not about the selection. Two curators reaching the
///   same selection by different routes must hash equal, or the
///   ten-heads-one-selection sharing story fails on the first plugin that
///   stamps its own name. Same class of exclusion, for the same reason, as
///   `prefix_key` excluding per-agent `SegmentId` so siblings hash equal.
/// - **The `incoherence` declaration (§4.1).** Derivable from the referenced
///   content, so including it would change no equality and would make the key
///   depend on reading every record.
/// - **Record content.** A reference names an immutable blob; a git tree names
///   blobs by id and does not re-hash them. Disclosed limit: a selection key
///   is therefore only as trustworthy as the log's immutability — a hand-edited
///   session file silently changes what a stored selection means. That
///   invariant is already load-bearing everywhere else in this tree.
/// - **How the selection was chunked into prefix references.** The projection
///   is over the *expanded* list, so `prefix(A) ++ [n1, n2]` and the flat
///   equivalent hash equal. Without this, sharing would depend on how a
///   curator happened to batch its commits.
///
/// The key is cheap: it hashes references and order, never bytes of content,
/// and never reaches the wire.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SelectionKey(pub String);

impl SelectionKey {
    /// Build a key from a blake3 hash, rendered as lowercase hex — same shape
    /// as `PrefixKey::from_blake3`.
    pub fn from_blake3(hash: blake3::Hash) -> Self {
        Self(hash.to_hex().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Compute the selection key over the **fully expanded** node list
    /// (prefix chains flattened first by the caller; DESIGN §2.3).
    ///
    /// `nodes` is taken in render order. Only `record` and `stamp` are hashed
    /// into the projection — see the type's own doc for every exclusion and
    /// its reason, which are load-bearing.
    pub fn from_nodes(nodes: &[PathNode]) -> Self {
        let projection: Vec<serde_json::Value> = nodes
            .iter()
            .map(|node| {
                serde_json::json!({
                    "record": {
                        "session": &node.record.session,
                        "seq": &node.record.seq,
                    },
                    "stamp": &node.stamp,
                })
            })
            .collect();

        let mut hasher = blake3::Hasher::new();
        hasher.update(b"conway.selection.v1");
        hasher.update(&canonical_json_bytes(&serde_json::Value::Array(projection)));
        Self::from_blake3(hasher.finalize())
    }
}

impl fmt::Display for SelectionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for SelectionKey {
    type Err = crate::error::ConwayError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_string()))
    }
}

// ──────────────────────────────────────────────────────────────────────────
// §2.6  Selection object = commit
// ──────────────────────────────────────────────────────────────────────────

/// One unanswered tool call declared by a path (DESIGN §4.1).
///
/// `default_path` *tolerates and declares* the incoherence a fork cut or a
/// killed session left behind (a result-without-call or a call-without-result
/// on the *default* path is harness-caused, not curator-caused, so it cannot
/// be refused — DESIGN §4.1). The declaration is what distinguishes the
/// tolerant constructor from the refusing one.
///
/// This is the minimal shape: the `call_id` of an unanswered tool call, the
/// one piece of information `drop_unanswered_tool_calls` in
/// `conway-runtime/src/context/builder.rs` returns today. D1-3b/d will
/// reconcile this with the render-time drop (adding the call's `tool`/`seq`
/// when the validator is wired) — that reconciliation is explicitly out of
/// scope for D1-3a, which only fixes the vocabulary and the hash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessDrop {
    /// The `call_id` of a `ContentBlock::ToolUse` with no answering
    /// `ContentBlock::ToolResultBlock` anywhere on the path.
    pub call_id: String,
}

impl HarnessDrop {
    pub fn new(call_id: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
        }
    }
}

/// A frozen path identified by its `SelectionKey` (DESIGN §2.6).
///
/// Immutable, globally readable, freely referenced, stored content-addressed
/// under its `SelectionKey`. Because the key is model-free, ten siblings
/// routing to four models share one stored object, where a model-scoped
/// design would have stored four identical bodies under four keys.
///
/// `prefix` references another *selection*, never a head (DESIGN §2.5). A head
/// is the latest `ContextPathSet` in a session's own log and references a
/// `SelectionKey`; there is no way to spell a head in a path body, so
/// referencing another session's head is unrepresentable rather than refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathSelection {
    /// Another selection this one is prefixed by, or `None`. Transitively
    /// expanded with a depth bound and `PathError::PrefixChainTooDeep`
    /// (DESIGN §2.6); cycles are impossible by construction but a hand-edited
    /// store is not, so the bound stays.
    pub prefix: Option<SelectionKey>,
    /// The node list, in render order. Prefix chains are flattened first
    /// before hashing (DESIGN §2.3).
    pub nodes: Vec<PathNode>,
    /// Incoherence the *default* path tolerated rather than refused (DESIGN
    /// §4.1). Excluded from `SelectionKey` (derivable). Reconciled at render
    /// time against `drop_unanswered_tool_calls`'s own drops — D1-3e.
    pub incoherence: Vec<HarnessDrop>,
}

// ──────────────────────────────────────────────────────────────────────────
// §4.2  Path operations and their cost
// ──────────────────────────────────────────────────────────────────────────

/// A single structural edit a curator (plugin or operator) proposes against a
/// base path (DESIGN §4.2).
///
/// **There is no "set the node list" operation.** `PathOp` is `Omit` /
/// `Include` / `Move { node, before }` / `Restamp`. You cannot reorder by
/// accident: `derive` refuses any `Move` with
/// `PathError::ReorderRequiresExplicitDerivation`; `derive_reordered` accepts
/// it. The cheap operation gets the short name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PathOp {
    /// Omit `node` from the derived path. `derive`/`derive_reordered` apply
    /// this op; `offers_for` constructs it as a repair suggestion (the harness
    /// offers; it never picks, §4.1). No curator plugin exists yet to PROPOSE
    /// an `Omit` (curators land with the curation capability, D1-8), but the
    /// harness repair-offer path does construct it, so this variant is not on
    /// the construction guard's allowlist.
    Omit { node: RecordRef },
    /// (Re-)include `node` in the derived path. `derive`/`derive_reordered`
    /// apply this op; `offers_for` constructs it as a repair suggestion
    /// (§4.1). No curator plugin exists yet to propose an `Include` (D1-8),
    /// but the harness repair-offer path does construct it, so this variant
    /// is not on the construction guard's allowlist.
    Include { node: RecordRef },
    /// Move `node` to immediately before `before` in the derived path.
    /// Refused by `derive`; accepted by `derive_reordered` (DESIGN §4.2).
    /// `derive`/`derive_reordered` APPLY this op but do not emit it
    /// internally -- a curator proposes a `Move`. The one place core
    /// constructs it is `offers_for` (§4.1), which offers the inverse reorder
    /// as the single-op repair for a rule-3 orphan (result reordered before
    /// its call). So this variant is NOT on the construction guard's
    /// allowlist -- the guard finds it constructed.
    Move { node: RecordRef, before: RecordRef },
    /// Re-stamp a node (e.g. committing a curated selection so children can
    /// share its cache by stamping `Head`/`Own` as `Inherited { from }`).
    /// Not yet implemented in core (§5): curator plugins construct `Restamp`s
    /// and `derive`/`derive_reordered` apply them, but no curator exists yet
    /// (D1-8/conway.memory); conway-core/session/runtime never construct it,
    /// and `offers_for` never offers a restamp.
    Restamp { node: RecordRef, to: NodeStamp },
}

/// How the first divergence between a derived path and its base falls, and
/// whether that divergence is an omission or a reorder (DESIGN §4.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DivergenceKind {
    /// No divergence: the derived path's expanded node list equals the base's.
    #[default]
    None,
    /// The base has a node the derived path omits (the cheap direction —
    /// dropping from the tail is nearly free, §5b).
    Omission,
    /// The derived path reorders nodes relative to the base (the expensive
    /// direction — only available via `derive_reordered`).
    Reorder,
}

/// The structural cost of a derivation, measured at the selection layer where
/// it is model-free and honest (DESIGN §4.2). The price returns with the
/// result, before anything is sent.
///
/// Whether a given structural divergence actually costs a cache hit is a
/// *rendering* question, answered at assembly and reported as
/// `RenderDivergence` — the derivation promises structure, not prices it
/// cannot know.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CostEstimate {
    /// How many leading nodes the derived path shares with its base.
    pub shared_prefix_nodes: u64,
    /// Estimated tokens in the shared prefix (the cacheable portion).
    pub shared_prefix_tokens_est: u64,
    /// Estimated tokens discarded from the base by this derivation (the
    /// un-cacheable tail spent by omitting/reordering).
    pub discarded_prefix_tokens_est: u64,
    /// The first node at which the derived path diverges from the base, or
    /// `None` if they are identical.
    pub first_divergence: Option<RecordRef>,
    /// Whether that first divergence is an omission or a reorder.
    pub divergence_kind: DivergenceKind,
    /// `true` iff `first_divergence` falls at or before the last
    /// `Inherited`-stamped node — a pure function of the node list and the
    /// tier boundary, computable without the model (DESIGN §4.2). That is
    /// §5b's "dropping from the tail is nearly free, dropping from the head
    /// spends everything", mechanical rather than rhetorical.
    pub divergence_inside_frozen_tier: bool,
}

// ──────────────────────────────────────────────────────────────────────────
// §2.7 + §4.1  PathError
// ──────────────────────────────────────────────────────────────────────────

/// One orphan a refused derivation found, and which of the three rules it
/// violated (DESIGN §4.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Orphan {
    /// The `call_id` of the orphaned tool call/result.
    pub call_id: String,
    /// The tool that issued or answered the call.
    pub tool: String,
    /// The node carrying the `ToolUse` half of the pair.
    pub call_node: RecordRef,
    /// The node carrying the `ToolResultBlock` half of the pair.
    pub result_node: RecordRef,
    /// Which coherence rule this orphan violated: `1` = a `ToolUse` with no
    /// answering `ToolResultBlock`; `2` = a `ToolResultBlock` with no
    /// `ToolUse`; `3` = a result appearing before its call. See DESIGN §4.1.
    pub rule: u8,
}

/// Failure modes a path constructor can report (DESIGN §2.7 + §4.1).
///
/// Two kinds, by DESIGN §2.7's discriminator — *refuse when the thing
/// referenced cannot be produced; report when it was produced exactly but
/// costs more*:
///
/// - `UnresolvableNode`, `PrefixChainTooDeep`, `ReorderRequiresExplicitDerivation`,
///   `WouldOrphan` are **refusals** from `derive`/`derive_reordered`.
/// - A cache miss / `RenderDivergence` is the *report* kind and is not a
///   `PathError` at all — it lands on the context report and as an event, loud,
///   free, never fatal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PathError {
    /// A `RecordRef` whose session is gone, or a `SelectionKey` absent from
    /// the store. Retention (DESIGN §4.4) makes this unreachable through
    /// sanctioned operations; a corrupt store is not (DESIGN §2.7).
    UnresolvableNode { record: RecordRef, detail: String },

    /// A `prefix` chain exceeded the depth bound — same shape as
    /// `resolver.rs`'s `MAX_ANCESTRY_DEPTH`. Cycles are impossible by
    /// construction but a hand-edited store is not, so the bound stays
    /// (DESIGN §2.6).
    PrefixChainTooDeep,

    /// `derive` refuses any `PathOp::Move` — reordering is a different
    /// function (`derive_reordered`), and the cheap operation gets the short
    /// name (DESIGN §4.2).
    ReorderRequiresExplicitDerivation,

    /// A derivation would orphan a tool call/result pair. `derive` runs the
    /// three-rule coherence validator over the resolved node list and returns
    /// this rather than constructing an invalid path (DESIGN §4.1).
    ///
    /// `Display` renders the human sentence — *"omitting session 01H…/seq 7
    /// orphans call `tc_3` issued in seq 6; also omit seq 6, or keep seq 7"* —
    /// and `offers` carries both candidate repairs, so a plugin can retry
    /// programmatically. The harness offers; it never picks.
    WouldOrphan {
        orphans: Vec<Orphan>,
        offers: Vec<PathOp>,
    },
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathError::UnresolvableNode { record, detail } => {
                write!(f, "unresolvable node {record}: {detail}")
            }
            PathError::PrefixChainTooDeep => f.write_str("prefix chain too deep"),
            PathError::ReorderRequiresExplicitDerivation => {
                f.write_str("reorder requires explicit derivation (use derive_reordered)")
            }
            // The §4.1 human sentence is derived from the structured `orphans`
            // rather than stored, so a `WouldOrphan` read off disk or built by
            // hand displays exactly what its `orphans` say.
            PathError::WouldOrphan { orphans, .. } => f.write_str(&render_would_orphan(orphans)),
        }
    }
}

impl std::error::Error for PathError {}

impl PathError {
    /// Construct a `WouldOrphan` from its structured parts (DESIGN §4.1).
    /// `Display` renders the human sentence from `orphans`, so it is not
    /// stored. The harness offers; it never picks.
    pub fn would_orphan(orphans: Vec<Orphan>, offers: Vec<PathOp>) -> Self {
        Self::WouldOrphan { orphans, offers }
    }
}

/// Render the §4.1 human sentence for a `WouldOrphan`.
///
/// *"omitting session 01H…/seq 7 orphans call `tc_3` issued in seq 6; also
/// omit seq 6, or keep seq 7"*. The sentence names the omit that caused the
/// orphan (the omitted node, in full `session …/seq N` form), the orphaned
/// `call_id`, where the call was issued (shorthand `seq N`), and the two
/// candidate repairs (omit the other half too, or keep the omitted half).
/// One orphan is the common case; multiple are listed in order, joined by
/// semicolons.
fn render_would_orphan(orphans: &[Orphan]) -> String {
    if orphans.is_empty() {
        return "derivation would orphan a tool call/result pair".to_string();
    }
    let full = |r: &RecordRef| format!("session {}/seq {}", r.session, r.seq);
    let short = |r: &RecordRef| format!("seq {}", r.seq);
    // One sentence per orphan, joined by semicolons — matches the §4.1 example
    // shape ("...; also omit seq 6, or keep seq 7").
    let sentences: Vec<String> = orphans
        .iter()
        .map(|o| match o.rule {
            // Rule 1: a ToolUse whose answering ToolResultBlock is not on the
            // path. The omit that caused it is of the RESULT node; the orphaned
            // thing is the call_id (the ToolUse), issued in the CALL node.
            1 => format!(
                "omitting {result_full} orphans call `{call_id}` issued in {call_short}; \
                 also omit {call_short}, or keep {result_short}",
                call_id = o.call_id,
                result_full = full(&o.result_node),
                call_short = short(&o.call_node),
                result_short = short(&o.result_node),
            ),
            // Rule 2: a ToolResultBlock whose ToolUse is not on the path. The
            // omit is of the CALL node; the orphaned thing is the result, which
            // answered the call in the RESULT node.
            2 => format!(
                "omitting {call_full} orphans result `{call_id}` answered in {result_short}; \
                 also omit {result_short}, or keep {call_short}",
                call_id = o.call_id,
                call_full = full(&o.call_node),
                result_short = short(&o.result_node),
                call_short = short(&o.call_node),
            ),
            // rule 3 (result appears before its call) or any other value: the
            // reorder itself is the defect, state it plainly.
            _ => format!(
                "reordering would place result `{call_id}` ({result_short}) before its call in \
                 {call_short}; keep {call_short} before {result_short}",
                call_id = o.call_id,
                call_short = short(&o.call_node),
                result_short = short(&o.result_node),
            ),
        })
        .collect();
    sentences.join("; ")
}

// ──────────────────────────────────────────────────────────────────────────
// §3  ResolvedPath — the expanded, record-resolved path (D1-3c)
// ──────────────────────────────────────────────────────────────────────────

/// A fully-expanded, record-resolved path: the expanded node list zipped with
/// its already-read `Arc<LogRecord>`s, in render order (DESIGN §3). Produced by
/// `resolve_path` (conway-session); consumed by assembly (D1-3d).
///
/// This is the value `ContextInput` carries in place of the legacy
/// `inherited`/`head`/`own` triple (D1-3d). `Clone` is derived (not a manual
/// impl) because both halves — `PathNode` and `Arc<LogRecord>` — are `Clone`
/// by value; cloning a `ResolvedPath` cheaply clones the `Arc`s (bump refcount,
/// no `LogRecord` copy), which is the shape `ContextInput`'s `Clone` derive
/// needs so the golden/behavioural tests can clone a fixture before mutating
/// a non-path field (e.g. `cache_mode`).
#[derive(Clone)]
pub struct ResolvedPath {
    /// The expanded node list zipped with its already-read records, in render
    /// order. The records are cloned out of the resolver's memoised
    /// `Arc<[LogRecord]>` into fresh `Arc<LogRecord>`s (see `resolve_path`'s
    /// doc: `Arc::ptr_eq` does NOT hold across siblings with the current cache
    /// shape; D1-3d may restructure the cache if assembly needs shared `Arc`s).
    /// Within a single `ResolvedPath`, `derive`/`default_path` reuse these
    /// `Arc`s without cloning (a path family shares them).
    pub nodes: Vec<(PathNode, Arc<LogRecord>)>,
}

impl std::fmt::Debug for ResolvedPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedPath")
            .field("len", &self.nodes.len())
            .finish_non_exhaustive()
    }
}

// ──────────────────────────────────────────────────────────────────────────
// §2.8 + §2.9  ValidatedPath + Derivation
// ──────────────────────────────────────────────────────────────────────────

/// The type that gates assembly — `ContextBuilder::build` (D1-3d) accepts only
/// this, and the only PUBLIC ways in are the two refusing constructors
/// [`derive`](Self::derive) / [`derive_reordered`](Self::derive_reordered)
/// (DESIGN §2.8). `default_path` (D1-3c, not here) is the tolerant constructor
/// that declares harness incoherence rather than refusing.
///
/// Carries the prefix-EXPANDED node list zipped with the already-read records
/// (`derive` never clones a `LogRecord` — it reuses the `Arc<LogRecord>`s it
/// is handed, so a path family shares them). Cross-sibling `Arc::ptr_eq` does
/// NOT hold with the current resolver cache shape (records are cloned out of
/// an `Arc<[LogRecord]>` into fresh `Arc`s at resolve time — see
/// `ResolvedPath`); D1-3d may restructure the cache if assembly needs it.
/// Derived paths are always coherent, so `derive`/`derive_reordered` build
/// with `incoherence: Vec::new()`; only `default_path` declares incoherence.
///
/// `Eq` is manual, not derived: `LogRecord` is `PartialEq` but not `Eq` (some
/// variants carry `serde_json::Value`/`f64`-shaped fields), so `Arc<LogRecord>`
/// is not `Eq` and `#[derive(Eq)]` would not compile. `PartialEq` compares the
/// record contents via `Arc`'s contents-equality, which is the comparison a
/// reader expects; `Eq` just marks it reflexive, which it is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ValidatedPath {
    /// The expanded node list zipped with its already-read records, in render
    /// order. Same `Arc`s the resolver handed us — `derive` reuses them.
    nodes: Vec<(PathNode, Arc<LogRecord>)>,
    /// Incoherence the constructor tolerated rather than refused (DESIGN
    /// §4.1). Empty for derived paths; `default_path` (D1-3c) declares here.
    incoherence: Vec<HarnessDrop>,
}

impl Eq for ValidatedPath {}

impl ValidatedPath {
    /// The internal builder the public constructors share. `pub(crate)`, not
    /// `pub` — it is not a "constructor" in the §2.8 sense; the invariant is
    /// *no public third way in*, not no internal helper. Public construction
    /// goes through `derive`/`derive_reordered` (and later `default_path`),
    /// which run validation and refuse; this fn just assembles already-valid
    /// pieces, so the public constructors can share the zipping/assembly
    /// without re-exposing a way to bypass the coherence check.
    pub(crate) fn from_resolved(
        nodes: Vec<(PathNode, Arc<LogRecord>)>,
        incoherence: Vec<HarnessDrop>,
    ) -> Self {
        Self { nodes, incoherence }
    }

    /// The expanded node list zipped with its records, in render order.
    pub fn nodes(&self) -> impl Iterator<Item = (&PathNode, &Arc<LogRecord>)> {
        self.nodes.iter().map(|(n, r)| (n, r))
    }

    /// Incoherence the constructor tolerated rather than refused (DESIGN
    /// §4.1). Empty for derived paths; `default_path` declares here. Render-
    /// time repair (D1-3e) reconciles `drop_unanswered_tool_calls`'s drops
    /// against this declaration.
    pub fn incoherence(&self) -> &[HarnessDrop] {
        &self.incoherence
    }

    /// The model-free identity of this path's selection —
    /// `SelectionKey::from_nodes` over the expanded node list (DESIGN §2.3).
    /// Equal expanded node lists hash equal, regardless of how they were
    /// chunked into prefix references or who curated them.
    pub fn key(&self) -> SelectionKey {
        let nodes: Vec<PathNode> = self.nodes.iter().map(|(n, _)| n.clone()).collect();
        SelectionKey::from_nodes(&nodes)
    }

    /// Derive a new path from `ops` applied to `self`, refusing any
    /// `PathOp::Move` (reordering is [`derive_reordered`](Self::derive_reordered))
    /// and refusing any candidate that would orphan a tool call/result pair
    /// (DESIGN §4.1, §4.2). The cheap operation gets the short name.
    pub fn derive(&self, ops: &[PathOp]) -> Result<Derivation, PathError> {
        self.apply(ops, false)
    }

    /// Derive a new path from `ops` applied to `self`, accepting
    /// `PathOp::Move` (the expensive direction — DESIGN §4.2). Still refuses
    /// an orphaning candidate.
    pub fn derive_reordered(&self, ops: &[PathOp]) -> Result<Derivation, PathError> {
        self.apply(ops, true)
    }

    /// The §2.8 constructor that includes everything: runs the three-rule
    /// coherence validator in DECLARE mode — orphans are tolerated and recorded
    /// as `HarnessDrop` in the path's `incoherence`, never refused (DESIGN §4.1).
    /// This is the ONLY way to build a `ValidatedPath` that may carry declared
    /// incoherence; `derive`/`derive_reordered` refuse it.
    ///
    /// Whatever incoherence is present was caused by the harness (a fork cut
    /// mid-batch; a session killed between an assistant append and its
    /// results), not by a curator, so it cannot be refused. The declaration is
    /// what distinguishes the tolerant constructor from the refusing one, and
    /// is what render-time repair (D1-3e) reconciles against.
    pub fn default_path(nodes: Vec<(PathNode, Arc<LogRecord>)>) -> Self {
        let incoherence = declare_incoherence(&nodes);
        Self::from_resolved(nodes, incoherence)
    }

    /// Shared core of [`derive`](Self::derive) / [`derive_reordered`](Self::derive_reordered).
    /// `allow_move` is the only difference between the two: `derive` refuses a
    /// `Move`, `derive_reordered` accepts it.
    fn apply(&self, ops: &[PathOp], allow_move: bool) -> Result<Derivation, PathError> {
        // §4.2: `derive` refuses any `Move`. Checked before any mutation so a
        // refusing `derive` never produces a partial candidate.
        if !allow_move {
            for op in ops {
                if let PathOp::Move { .. } = op {
                    return Err(PathError::ReorderRequiresExplicitDerivation);
                }
            }
        }

        let mut candidate: Vec<PathNode> = self.nodes().map(|(n, _)| n.clone()).collect();
        let mut moved = false;
        for op in ops {
            match op {
                // Omit: remove the node with that RecordRef. Absent → no-op
                // (idempotent), so `Omit` of an already-omitted node is harmless.
                PathOp::Omit { node } => {
                    candidate.retain(|n| n.record != *node);
                }
                // (Re-)include a base node, restoring it to its ORIGINAL base
                // position -- not the tail. This is what makes a "keep" repair
                // offer validate (DESIGN §4.1): a curator that omitted a CALL
                // (which precedes its result) and is offered "keep the call"
                // must get `[call, result]`, not `[result, call]` -- a
                // tail-append would manufacture a rule-3 orphan. The op
                // carries only a `RecordRef`, so the stamp/provenance come from
                // `self`'s node with that record. A foreign cross-tree
                // `Include` (memory) would need a read surface to resolve the
                // record and stamp from, which does not exist yet (D1-8); the
                // `UnresolvableNode` refusal is the honest placeholder until
                // then. A curator that wants a node elsewhere still uses `Move`.
                PathOp::Include { node } => {
                    let base: Vec<PathNode> = self.nodes().map(|(n, _)| n.clone()).collect();
                    let Some(bi) = base.iter().position(|n| n.record == *node) else {
                        return Err(PathError::UnresolvableNode {
                            record: *node,
                            detail: "foreign Include requires the read surface (D1-8)".to_string(),
                        });
                    };
                    let pn = base[bi].clone();
                    let base_refs: Vec<RecordRef> = base.iter().map(|n| n.record).collect();
                    // Insert before the first remaining candidate node whose
                    // own base index is greater than `bi` -- i.e. the first
                    // node that originally followed the re-included one. If
                    // none (the node was last in the base, or everything after
                    // it was omitted), append at the tail.
                    let mut insert_at = candidate.len();
                    for (i, cn) in candidate.iter().enumerate() {
                        if let Some(ci) = base_refs.iter().position(|r| *r == cn.record) {
                            if ci > bi {
                                insert_at = i;
                                break;
                            }
                        }
                    }
                    candidate.insert(insert_at, pn);
                }
                // Move: reorder `node` to immediately before `before`. Both
                // must be present in `candidate`; either absent → refusal.
                PathOp::Move { node, before } => {
                    moved = true;
                    let node_pos = candidate.iter().position(|n| n.record == *node);
                    let before_pos = candidate.iter().position(|n| n.record == *before);
                    match (node_pos, before_pos) {
                        (Some(np), Some(bp)) => {
                            let item = candidate.remove(np);
                            // After removing `np`, the `before` index shifts
                            // down by one only if `before` was after `node`.
                            let bp2 = if bp > np { bp - 1 } else { bp };
                            candidate.insert(bp2, item);
                        }
                        (None, _) => {
                            return Err(PathError::UnresolvableNode {
                                record: *node,
                                detail: "Move targets a node not on the path".to_string(),
                            })
                        }
                        (_, None) => {
                            return Err(PathError::UnresolvableNode {
                                record: *before,
                                detail: "Move targets a node not on the path".to_string(),
                            })
                        }
                    }
                }
                // Restamp: change the stamp of the node with that RecordRef.
                // Absent → refusal (cannot restamp a node not on the path).
                PathOp::Restamp { node, to } => {
                    let pos = candidate.iter().position(|n| n.record == *node);
                    match pos {
                        Some(p) => candidate[p].stamp = *to,
                        None => {
                            return Err(PathError::UnresolvableNode {
                                record: *node,
                                detail: "Restamp targets a node not on the path".to_string(),
                            })
                        }
                    }
                }
            }
        }

        let orphans = validate_coherence(self, &candidate);
        if !orphans.is_empty() {
            let offers = offers_for(&orphans);
            return Err(PathError::would_orphan(orphans, offers));
        }

        let cost = cost_estimate(self, &candidate, moved);

        // Zip each candidate `PathNode` with `self`'s `Arc<LogRecord>` for that
        // `RecordRef` (same Arc — preserves sibling sharing; do NOT clone the
        // LogRecord). Every candidate node's record was resolved from `self`
        // (foreign `Include` was refused above), so the lookup always succeeds.
        let rec_by_ref: HashMap<RecordRef, Arc<LogRecord>> = self
            .nodes()
            .map(|(n, r)| (n.record, Arc::clone(r)))
            .collect();
        let zipped: Vec<(PathNode, Arc<LogRecord>)> = candidate
            .iter()
            .map(|n| {
                let r = rec_by_ref
                    .get(&n.record)
                    .cloned()
                    .expect("candidate record resolved from base (foreign Include refused)");
                (n.clone(), r)
            })
            .collect();
        Ok(Derivation {
            path: ValidatedPath::from_resolved(zipped, Vec::new()),
            cost,
        })
    }
}

/// The price returns with the result (DESIGN §4.2): a derivation carries the
/// validated path AND its structural cost, measured at the selection layer
/// where it is model-free and honest. A curator decides whether the price is
/// worth paying before anything is sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Derivation {
    /// The validated path. Coherent (derived paths never declare incoherence;
    /// only `default_path` does, DESIGN §4.1).
    pub path: ValidatedPath,
    /// The structural cost of the derivation (§4.2). Whether it actually costs
    /// a cache hit is a rendering question answered at assembly as
    /// `RenderDivergence`; this promises structure, not prices it cannot know.
    pub cost: CostEstimate,
}

// ──────────────────────────────────────────────────────────────────────────
// §4.1  The three-rule coherence validator (record-layer, model-free)
// ──────────────────────────────────────────────────────────────────────────

/// Which half of a tool call/result pair an extracted `call_id` came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Half {
    Call,
    Result,
}

/// One extracted `(call_id, tool, half)` from a record. The `node_record` is
/// carried by the caller (it is the `PathNode`'s `RecordRef`, not anything the
/// `LogRecord` itself can name — `LogRecord` carries no session id, §2.1).
struct Extracted {
    call_id: String,
    tool: String,
    half: Half,
}

/// Record-layer call/result extraction, mirroring the render layer's
/// `tool_use_call_ids` / `tool_result_call_ids` (`conway-runtime`'s
/// `context/builder.rs`) but over `LogRecord`s instead of rendered segments.
///
/// - `LogRecord::Assistant { content, .. }` → one **Call** per
///   `ContentBlock::ToolUse { call_id, name, .. }` in `content`.
/// - `LogRecord::ToolResultRecord { result, .. }` → one **Result** (using
///   `result.call_id` / `result.tool`).
/// - `ChildResultRecord`/`Header`/`UserTurn`/`ForkDirective`/`ParentSteer`
///   contribute neither (verified against `log.rs`).
fn extract_calls_results(record: &LogRecord) -> Vec<Extracted> {
    match record {
        LogRecord::Assistant { content, .. } => content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { call_id, name, .. } => Some(Extracted {
                    call_id: call_id.clone(),
                    tool: name.as_str().to_owned(),
                    half: Half::Call,
                }),
                _ => None,
            })
            .collect(),
        LogRecord::ToolResultRecord { result, .. } => vec![Extracted {
            call_id: result.call_id.clone(),
            tool: result.tool.as_str().to_owned(),
            half: Half::Result,
        }],
        _ => Vec::new(),
    }
}

/// Run the three coherence rules (DESIGN §4.1) over a derived `candidate`
/// against a `base` that names the omitted half of a broken pair.
///
/// **Rule 1** — a `ToolUse` `call_id` with no answering `ToolResultBlock`
/// anywhere in `candidate`: the call is present, the result was omitted. The
/// omitted half's `RecordRef` comes from `base`'s pairing map (the result
/// node that was on the base).
///
/// **Rule 2** — a `ToolResultBlock` `call_id` with no `ToolUse` in `candidate`:
/// the result is present, the call was omitted. The omitted half's `RecordRef`
/// (the call node) comes from `base`.
///
/// **Rule 3** — both halves are present in `candidate`, but the result's
/// candidate order index is before the call's.
///
/// Returns orphans in candidate order (first occurrence of each `call_id`),
/// deduped by `call_id`. An orphan the *base* already had (e.g. a base
/// `default_path` that tolerated a call-without-result) is NOT reported here:
/// `derive` only refuses orphans the *derivation* introduced, so a rule-1 or
/// rule-2 orphan whose omitted half is also absent from `base` is inherited
/// incoherence, not a new one, and is skipped.
pub(crate) fn validate_coherence(base: &ValidatedPath, candidate: &[PathNode]) -> Vec<Orphan> {
    // 1. Build the base pairing map: call_id -> (call_node, result_node), and
    // the first-occurrence index maps the rule-3 base-side skip needs to
    // detect a base's own ri < ci inversion (DESIGN §4.1, D1-3c Part 4).
    let mut base_call: HashMap<String, RecordRef> = HashMap::new();
    let mut base_result: HashMap<String, RecordRef> = HashMap::new();
    let mut base_call_idx: HashMap<String, usize> = HashMap::new();
    let mut base_result_idx: HashMap<String, usize> = HashMap::new();
    for (i, (n, rec)) in base.nodes().enumerate() {
        for ext in extract_calls_results(rec) {
            match ext.half {
                Half::Call => {
                    base_call.insert(ext.call_id.clone(), n.record);
                    base_call_idx.entry(ext.call_id).or_insert(i);
                }
                Half::Result => {
                    base_result.insert(ext.call_id.clone(), n.record);
                    base_result_idx.entry(ext.call_id).or_insert(i);
                }
            }
        }
    }

    // 2. Walk candidate in order; record first-occurrence indices + nodes.
    let rec_by_ref: HashMap<RecordRef, &LogRecord> =
        base.nodes().map(|(n, r)| (n.record, r.as_ref())).collect();
    let mut cand_call_idx: HashMap<String, usize> = HashMap::new();
    let mut cand_result_idx: HashMap<String, usize> = HashMap::new();
    let mut cand_call_node: HashMap<String, RecordRef> = HashMap::new();
    let mut cand_result_node: HashMap<String, RecordRef> = HashMap::new();
    let mut cand_tool: HashMap<String, String> = HashMap::new();
    let mut first_idx: HashMap<String, usize> = HashMap::new();
    for (i, n) in candidate.iter().enumerate() {
        let Some(rec) = rec_by_ref.get(&n.record) else {
            // A foreign node (not resolvable from base) contributes no
            // calls/results. `derive` would have refused it as an
            // `UnresolvableNode` Include before reaching here, so this is
            // purely defensive.
            continue;
        };
        for ext in extract_calls_results(rec) {
            let cid = ext.call_id.clone();
            first_idx.entry(cid.clone()).or_insert(i);
            cand_tool.entry(cid.clone()).or_insert(ext.tool);
            match ext.half {
                Half::Call => {
                    cand_call_idx.entry(cid.clone()).or_insert(i);
                    cand_call_node.entry(cid.clone()).or_insert(n.record);
                }
                Half::Result => {
                    cand_result_idx.entry(cid.clone()).or_insert(i);
                    cand_result_node.entry(cid.clone()).or_insert(n.record);
                }
            }
        }
    }

    // 3-5. Classify each call_id (in candidate order). `cids` is already
    // unique (HashMap keys), so no dedup set is needed here.
    let mut cids: Vec<String> = first_idx.keys().cloned().collect();
    cids.sort_by_key(|c| first_idx[c]);
    let mut orphans: Vec<Orphan> = Vec::new();
    for cid in &cids {
        let call_present = cand_call_idx.contains_key(cid);
        let result_present = cand_result_idx.contains_key(cid);
        let tool = cand_tool.get(cid).cloned().unwrap_or_default();
        if call_present && !result_present {
            // Rule 1: call present, result omitted. The omitted half (result)
            // is named by base. If base also lacks the result, this is the
            // base's declared incoherence, not a derivation-introduced orphan.
            if let Some(&result_node) = base_result.get(cid) {
                let call_node = cand_call_node[cid];
                orphans.push(Orphan {
                    call_id: cid.clone(),
                    tool,
                    call_node,
                    result_node,
                    rule: 1,
                });
            }
        } else if !call_present && result_present {
            // Rule 2: result present, call omitted. The omitted half (call) is
            // named by base. If base also lacks the call, inherited, skip.
            if let Some(&call_node) = base_call.get(cid) {
                let result_node = cand_result_node[cid];
                orphans.push(Orphan {
                    call_id: cid.clone(),
                    tool,
                    call_node,
                    result_node,
                    rule: 2,
                });
            }
        } else if call_present && result_present {
            let ci = cand_call_idx[cid];
            let ri = cand_result_idx[cid];
            if ri < ci {
                // Rule 3: result appears before its call.
                //
                // Base-side skip (mirrors rules 1 and 2 above): rules 1/2 skip
                // an orphan when the base ITSELF lacks the missing half
                // (inherited incoherence the derivation did not introduce).
                // Rule 3's analogue: skip when the base ITSELF has the same
                // `ri < ci` inversion for that `call_id` — the base already
                // declared this rule-3 incoherence (a `default_path` that
                // tolerated a malformed log with a result at a lower seq than
                // its call); the derivation inherited it, did not introduce
                // it. An empty `derive(&[])` on such a base must NOT refuse
                // (DESIGN §4.1, D1-3c Part 4).
                let base_has_inversion = matches!(
                    (base_call_idx.get(cid), base_result_idx.get(cid)),
                    (Some(&bci), Some(&bri)) if bri < bci
                );
                if base_has_inversion {
                    continue;
                }
                let call_node = cand_call_node[cid];
                let result_node = cand_result_node[cid];
                orphans.push(Orphan {
                    call_id: cid.clone(),
                    tool,
                    call_node,
                    result_node,
                    rule: 3,
                });
            }
        }
    }
    orphans
}

/// Run the three coherence rules (DESIGN §4.1) over a single node list in
/// DECLARE mode — the tolerant counterpart of [`validate_coherence`]. Instead
/// of returning [`Orphan`]s for refusal (which `derive` does), this converts
/// each orphan to a [`HarnessDrop`] so [`ValidatedPath::default_path`] can
/// *declare* the incoherence a fork cut or a killed session left behind rather
/// than refusing it (DESIGN §4.1: "whatever incoherence is present was caused
/// by the harness").
///
/// The declared `incoherence` is exactly the set of orphans a `derive(&[])` on
/// this base would otherwise refuse — which is why the rule-3 base-side skip
/// (Part 4) is load-bearing: once `default_path` can declare a rule-3
/// incoherence, `derive(&[])` on that base must NOT refuse, and the skip makes
/// it so.
///
/// All three rules produce a [`HarnessDrop`] keyed by the `call_id` that
/// identifies the broken pair:
/// - Rule 1 (call with no result): the `call_id` comes from the `ToolUse`.
/// - Rule 2 (result with no call): the CALL is absent, but the result's
///   `call_id` field still names the broken pair — that is the `call_id`
///   declared. (There is no call to *drop* at render time, but the declaration
///   records the incoherence so D1-3e's reconciliation can surface it.)
/// - Rule 3 (result before call): the shared `call_id` of the reordered pair.
pub(crate) fn declare_incoherence(nodes: &[(PathNode, Arc<LogRecord>)]) -> Vec<HarnessDrop> {
    // Build call/result first-occurrence index maps, keyed by call_id.
    let mut call_idx: HashMap<String, usize> = HashMap::new();
    let mut result_idx: HashMap<String, usize> = HashMap::new();
    for (i, (_, rec)) in nodes.iter().enumerate() {
        for ext in extract_calls_results(rec) {
            match ext.half {
                Half::Call => {
                    call_idx.entry(ext.call_id).or_insert(i);
                }
                Half::Result => {
                    result_idx.entry(ext.call_id).or_insert(i);
                }
            }
        }
    }

    // All call_ids from either half, deduped and sorted for stable output.
    let mut cids: Vec<String> = call_idx
        .keys()
        .cloned()
        .chain(result_idx.keys().cloned())
        .collect();
    cids.sort();
    cids.dedup();

    let mut drops: Vec<HarnessDrop> = Vec::new();
    for cid in &cids {
        let call_present = call_idx.contains_key(cid);
        let result_present = result_idx.contains_key(cid);
        let is_orphan = if call_present && !result_present {
            // Rule 1: call present, result absent.
            true
        } else if !call_present && result_present {
            // Rule 2: result present, call absent. The call_id comes from the
            // result's own `call_id` field (see the function doc).
            true
        } else if call_present && result_present {
            // Rule 3: both present, result before call.
            result_idx[cid] < call_idx[cid]
        } else {
            false
        };
        if is_orphan {
            drops.push(HarnessDrop::new(cid.clone()));
        }
    }
    drops
}

/// Repair offers for a set of orphans (DESIGN §4.1: "the harness offers; it
/// never picks"). Each emitted offer is a SINGLE `PathOp` that, combined with
/// the curator's original ops and applied to the same base, yields a coherent
/// path -- the property §4.1 contracts ("each, applied to the same base,
/// validates"). These are SUGGESTIONS the harness hands a curator/plugin so
/// it can retry programmatically; the harness never picks.
///
/// - Rule 1 (call present, result omitted): `Omit` the call too, or `Include`
///   the result back at its original position (after the call).
/// - Rule 2 (result present, call omitted): `Omit` the result too, or
///   `Include` the call back at its original position (before the result).
/// - Rule 3 (result before call): the only single-op repair is the inverse
///   reorder -- `Move` the call back before the result. Omitting either half
///   would orphan the other (rule 1/2), so the honest repair IS the reorder.
pub(crate) fn offers_for(orphans: &[Orphan]) -> Vec<PathOp> {
    let mut out = Vec::new();
    for o in orphans {
        match o.rule {
            1 => {
                out.push(PathOp::Omit { node: o.call_node });
                out.push(PathOp::Include {
                    node: o.result_node,
                });
            }
            2 => {
                out.push(PathOp::Omit {
                    node: o.result_node,
                });
                out.push(PathOp::Include { node: o.call_node });
            }
            // Rule 3: a reorder. The only single-op repair is the inverse
            // reorder -- `Move` the call back before the result. Omitting
            // either half would orphan the other (rule 1/2), so this is the
            // honest single offer. `derive_reordered` applies it (rule 3
            // arises only from a prior `Move`, so the curator is already on
            // the `derive_reordered` path).
            _ => {
                out.push(PathOp::Move {
                    node: o.call_node,
                    before: o.result_node,
                });
            }
        }
    }
    out
}

// ──────────────────────────────────────────────────────────────────────────
// §4.2  CostEstimate computation
// ──────────────────────────────────────────────────────────────────────────

/// Rough MODEL-FREE token estimate: ~4 chars per token, the same shape a
/// render-layer estimator would use before a tokenizer is loaded. This is a
/// PLACEHOLDER D1-2 (cost reporting) may refine; the STRUCTURE of
/// `CostEstimate` (shared prefix length, divergence kind/position) is the
/// promise, the token count is an estimate.
fn token_est(record: &LogRecord) -> u64 {
    ((record_content_char_len(record) as u64).saturating_add(3)) / 4
}

/// Total chars of a record's content as rendered, for the token estimate
/// above. `Assistant` sums its `content` blocks; `ToolResultRecord` sums its
/// `result.blocks` plus the tool name; the text-bearing turns use their `text`;
/// everything else (Header, result records, context report, mask) has no
/// rendered content text and contributes 0.
fn record_content_char_len(record: &LogRecord) -> usize {
    match record {
        LogRecord::Assistant { content, .. } => content.iter().map(block_char_len).sum(),
        LogRecord::ToolResultRecord { result, .. } => {
            result.blocks.iter().map(block_char_len).sum::<usize>()
                + result.tool.as_str().chars().count()
        }
        LogRecord::UserTurn { text, .. }
        | LogRecord::ForkDirective { text, .. }
        | LogRecord::ParentSteer { text, .. }
        | LogRecord::SystemNote { text, .. } => text.chars().count(),
        LogRecord::Header(_)
        | LogRecord::AgentResultRecord { .. }
        | LogRecord::ChildResultRecord { .. }
        | LogRecord::ContextReportRecord { .. }
        | LogRecord::ContextMask { .. }
        | LogRecord::ContextPathSet { .. }
        | LogRecord::ContextPathNamed { .. } => 0,
    }
}

/// Per-block char count for `record_content_char_len`. Each variant counts the
/// text it carries; a `ToolResultBlock` recurses into its nested blocks.
fn block_char_len(block: &ContentBlock) -> usize {
    match block {
        ContentBlock::Text { text } => text.chars().count(),
        ContentBlock::Thinking { text, .. } => text.chars().count(),
        ContentBlock::ToolUse {
            call_id,
            name,
            arguments,
        } => {
            call_id.chars().count()
                + name.as_str().chars().count()
                + arguments.to_string().chars().count()
        }
        ContentBlock::ToolResultBlock {
            call_id, blocks, ..
        } => call_id.chars().count() + blocks.iter().map(block_char_len).sum::<usize>(),
        ContentBlock::Image { data_base64, .. } => data_base64.chars().count(),
    }
}

/// Compute the structural cost of a derivation (DESIGN §4.2), over
/// `(base = self nodes, derived = candidate, moved)`. Model-free: it measures
/// the shared prefix and where the first divergence falls relative to the
/// frozen tier boundary, never reading the model or the wire.
pub(crate) fn cost_estimate(
    base: &ValidatedPath,
    derived: &[PathNode],
    moved: bool,
) -> CostEstimate {
    let base_nodes: Vec<&PathNode> = base.nodes().map(|(n, _)| n).collect();

    // shared_prefix_nodes: longest leading run equal by (record, stamp).
    let mut sp = 0usize;
    while sp < base_nodes.len() && sp < derived.len() {
        let bn = base_nodes[sp];
        let dn = &derived[sp];
        if bn.record == dn.record && bn.stamp == dn.stamp {
            sp += 1;
        } else {
            break;
        }
    }
    let shared_prefix_nodes = sp as u64;

    // Token estimates over base records (shared prefix + discarded tail).
    let records: Vec<Arc<LogRecord>> = base.nodes().map(|(_, r)| Arc::clone(r)).collect();
    let shared_prefix_tokens_est: u64 = records[..sp].iter().map(|r| token_est(r)).sum();
    let discarded_prefix_tokens_est: u64 = records[sp..].iter().map(|r| token_est(r)).sum();

    let first_divergence = if sp < base_nodes.len() {
        Some(base_nodes[sp].record)
    } else {
        None
    };

    let divergence_kind = if first_divergence.is_none() {
        DivergenceKind::None
    } else if sp >= derived.len() {
        // Derived ran out before base: a base node is absent from derived.
        DivergenceKind::Omission
    } else {
        // Both have a node at sp, they differ. If a Move was applied and the
        // base's node at sp appears later in derived, it is a position swap
        // (Reorder). Otherwise (omission, restamp, or just ambiguous) Omission
        // — the cheap direction, per DESIGN §4.2.
        if moved {
            let base_rec = base_nodes[sp].record;
            if derived[sp + 1..].iter().any(|n| n.record == base_rec) {
                DivergenceKind::Reorder
            } else {
                DivergenceKind::Omission
            }
        } else {
            DivergenceKind::Omission
        }
    };

    // Frozen tier boundary: the last Inherited-stamped node in base.
    // `next_back` rather than `last`: the filter is double-ended, and clippy's
    // `double_ended_iterator_last` points out `last` would walk the whole tail
    // when `next_back` walks only to the last match.
    let last_inherited = base_nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| matches!(n.stamp, NodeStamp::Inherited { .. }))
        .map(|(i, _)| i)
        .next_back();
    let divergence_inside_frozen_tier = match (first_divergence.is_some(), last_inherited) {
        (true, Some(li)) => sp <= li,
        _ => false,
    };

    CostEstimate {
        shared_prefix_nodes,
        shared_prefix_tokens_est,
        discarded_prefix_tokens_est,
        first_divergence,
        divergence_kind,
        divergence_inside_frozen_tier,
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn node(
        session: SessionId,
        seq: u64,
        stamp: NodeStamp,
        selected_by: Selector,
        at: &str,
    ) -> PathNode {
        PathNode {
            record: RecordRef {
                session,
                seq: LogSeq(seq),
            },
            stamp,
            prov: NodeProvenance {
                selected_by,
                at: DateTime::parse_from_rfc3339(at)
                    .unwrap()
                    .with_timezone(&Utc),
            },
        }
    }

    fn at(n: u64) -> String {
        format!("2026-08-14T12:00:{n:02}Z")
    }

    /// §2.3: same expanded node list → same key.
    #[test]
    fn same_nodes_hash_equal() {
        let s = SessionId::new();
        let nodes = vec![
            node(s, 1, NodeStamp::Own, Selector::DefaultRule, &at(0)),
            node(s, 2, NodeStamp::Own, Selector::DefaultRule, &at(1)),
        ];
        let k1 = SelectionKey::from_nodes(&nodes);
        let k2 = SelectionKey::from_nodes(&nodes);
        assert_eq!(k1, k2, "identical node lists must hash equal");
        assert_eq!(k1.as_str().len(), 64, "blake3 hex is 64 chars");
        assert!(
            k1.as_str()
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "key is lowercase hex"
        );
    }

    /// §2.3: different node lists → different keys.
    #[test]
    fn different_nodes_hash_distinct() {
        let s = SessionId::new();
        let a = vec![node(s, 1, NodeStamp::Own, Selector::DefaultRule, &at(0))];
        let b = vec![node(s, 2, NodeStamp::Own, Selector::DefaultRule, &at(1))];
        assert_ne!(SelectionKey::from_nodes(&a), SelectionKey::from_nodes(&b));
    }

    /// §2.3: two selections differing ONLY in `NodeProvenance` (different
    /// `selected_by`/`at`) hash EQUAL. Who curated, and when, is a fact about
    /// the act, not the selection.
    #[test]
    fn provenance_does_not_affect_the_key() {
        let s = SessionId::new();
        let left = vec![node(
            s,
            7,
            NodeStamp::Own,
            Selector::DefaultRule,
            "2026-08-14T12:00:00Z",
        )];
        let right = vec![node(
            s,
            7,
            NodeStamp::Own,
            Selector::Plugin {
                id: "conway.compaction".to_string(),
                op: OpLabel::new("omit"),
            },
            "2026-08-15T09:30:00Z",
        )];
        assert_eq!(
            SelectionKey::from_nodes(&left),
            SelectionKey::from_nodes(&right),
            "differing only in NodeProvenance must hash equal"
        );
    }

    /// §2.3: the key is invariant to model/statics — they are not inputs, so
    /// changing them cannot change the key. This is a structural guarantee
    /// (there is no model field on any type hashed), asserted here so a later
    /// reader who adds one fails loudly.
    #[test]
    fn key_has_no_model_or_statics_input() {
        // The projection is only record + stamp. If someone widens the
        // projection, this test's shape still holds (it hashes the same
        // nodes twice); the real guard is that there is no model parameter to
        // `from_nodes`. Kept as a regression marker.
        let s = SessionId::new();
        let nodes = vec![node(s, 1, NodeStamp::Head, Selector::Operator, &at(0))];
        let k = SelectionKey::from_nodes(&nodes);
        // Re-compute with a different "model" in scope — there is no slot for
        // it, so the key is unchanged by construction.
        assert_eq!(k, SelectionKey::from_nodes(&nodes));
    }

    /// §2.3: `prefix(A) ++ [n1, n2]` hashes equal to the flat equivalent.
    /// Chunking into prefix references is excluded — the projection is over
    /// the expanded list. (Here we model "prefix A" as just more nodes in the
    /// same list; the expansion itself is the caller's job per the design,
    /// but the hash must be invariant to whether a curator committed the
    /// prefix as one selection or two.)
    #[test]
    fn chunking_into_prefix_does_not_change_the_key() {
        let s = SessionId::new();
        let n1 = node(
            s,
            1,
            NodeStamp::Inherited { from: s },
            Selector::DefaultRule,
            &at(0),
        );
        let n2 = node(
            s,
            2,
            NodeStamp::Inherited { from: s },
            Selector::DefaultRule,
            &at(1),
        );
        let n3 = node(s, 3, NodeStamp::Own, Selector::DefaultRule, &at(2));
        let n4 = node(s, 4, NodeStamp::Own, Selector::DefaultRule, &at(3));

        // The "flat equivalent": all four nodes in one list.
        let flat = SelectionKey::from_nodes(&[n1.clone(), n2.clone(), n3.clone(), n4.clone()]);

        // `prefix(A) ++ [n3, n4]`: the caller flattens A first, then appends.
        // The expanded list handed to `from_nodes` is the same four nodes, so
        // the key is identical — which is the whole point of projecting over
        // the expanded list.
        let mut expanded = vec![n1.clone(), n2.clone()];
        expanded.push(n3.clone());
        expanded.push(n4.clone());
        let chunked = SelectionKey::from_nodes(&expanded);

        assert_eq!(
            flat, chunked,
            "chunking into prefix references must not change the key"
        );
    }

    /// A different *stamp* on the same record IS a different selection — the
    /// stamp is inside the hash (DESIGN §3's legacy carry-over note: the two
    /// are honestly different selections because they render differently).
    #[test]
    fn different_stamp_is_a_different_key() {
        let s = SessionId::new();
        let n_own = node(s, 5, NodeStamp::Own, Selector::DefaultRule, &at(0));
        let n_inh = node(
            s,
            5,
            NodeStamp::Inherited { from: s },
            Selector::DefaultRule,
            &at(0),
        );
        assert_ne!(
            SelectionKey::from_nodes(&[n_own]),
            SelectionKey::from_nodes(&[n_inh]),
            "re-stamping changes the key, by design"
        );
    }

    /// A different *order* of the same nodes is a different selection — order
    /// is load-bearing (DESIGN §2.3).
    #[test]
    fn order_is_load_bearing() {
        let s = SessionId::new();
        let n1 = node(s, 1, NodeStamp::Own, Selector::DefaultRule, &at(0));
        let n2 = node(s, 2, NodeStamp::Own, Selector::DefaultRule, &at(1));
        assert_ne!(
            SelectionKey::from_nodes(&[n1.clone(), n2.clone()]),
            SelectionKey::from_nodes(&[n2, n1]),
            "reordering changes the key"
        );
    }

    /// Round-trip: every value type serde round-trips. Follows the design's
    /// serde `tag`/`rename_all` conventions (§2.2 uses `tag = "kind"` for the
    /// stamp/selector enums; `PathOp` uses `tag = "op"`; `PathError` uses
    /// `tag = "kind"`).
    #[test]
    fn all_types_roundtrip() {
        let s = SessionId::new();
        let r = RecordRef {
            session: s,
            seq: LogSeq(7),
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: RecordRef = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);

        // NodeStamp variants use `tag = "kind", rename_all = "snake_case"`.
        for stamp in [
            NodeStamp::Inherited { from: s },
            NodeStamp::Head,
            NodeStamp::Own,
        ] {
            let json = serde_json::to_string(&stamp).unwrap();
            let back: NodeStamp = serde_json::from_str(&json).unwrap();
            assert_eq!(stamp, back);
        }
        // Verify the unit-variant wire shape: under `tag = "kind"` a unit
        // variant serializes as `{"kind":"head"}` (the same convention
        // `Provenance` uses with `tag = "type"`). The `Inherited` variant's
        // shape is checked by round-trip only, since it carries a ULID whose
        // string form is not stable to assert against.
        assert_eq!(
            serde_json::to_string(&NodeStamp::Head).unwrap(),
            r#"{"kind":"head"}"#
        );

        // Selector uses `tag = "kind", rename_all = "snake_case"`.
        let sel = Selector::Plugin {
            id: "conway.compaction".to_string(),
            op: OpLabel::new("omit"),
        };
        let json = serde_json::to_string(&sel).unwrap();
        let back: Selector = serde_json::from_str(&json).unwrap();
        assert_eq!(sel, back);
        assert_eq!(
            serde_json::to_string(&Selector::DefaultRule).unwrap(),
            r#"{"kind":"default_rule"}"#
        );

        // OpLabel is a transparent string newtype.
        let op = OpLabel::new("omit");
        assert_eq!(serde_json::to_string(&op).unwrap(), r#""omit""#);
        let back: OpLabel = serde_json::from_str(r#""omit""#).unwrap();
        assert_eq!(op, back);

        // SelectionKey is a transparent string newtype (same shape as PrefixKey).
        let key =
            SelectionKey::from_nodes(&[node(s, 1, NodeStamp::Own, Selector::DefaultRule, &at(0))]);
        let json = serde_json::to_string(&key).unwrap();
        assert!(json.starts_with('"') && json.ends_with('"'));
        let back: SelectionKey = serde_json::from_str(&json).unwrap();
        assert_eq!(key, back);

        // PathSelection round-trips.
        let sel_obj = PathSelection {
            prefix: Some(key.clone()),
            nodes: vec![node(s, 1, NodeStamp::Own, Selector::DefaultRule, &at(0))],
            incoherence: vec![HarnessDrop::new("tc_3")],
        };
        let json = serde_json::to_string(&sel_obj).unwrap();
        let back: PathSelection = serde_json::from_str(&json).unwrap();
        assert_eq!(sel_obj, back);

        // PathOp uses `tag = "op", rename_all = "snake_case"`.
        let s2 = SessionId::new();
        let ops = [
            PathOp::Omit { node: r },
            PathOp::Include { node: r },
            PathOp::Move {
                node: r,
                before: RecordRef {
                    session: s2,
                    seq: LogSeq(2),
                },
            },
            PathOp::Restamp {
                node: r,
                to: NodeStamp::Inherited { from: s },
            },
        ];
        for op in ops {
            let json = serde_json::to_string(&op).unwrap();
            let back: PathOp = serde_json::from_str(&json).unwrap();
            assert_eq!(op, back);
        }
        // The `omit` variant's wire shape is stable on the `op`/`node` fields
        // (the `session` string inside `node` is a ULID and is checked by
        // round-trip, not by exact string).
        let omit_json = serde_json::to_string(&PathOp::Omit { node: r }).unwrap();
        assert!(
            omit_json.starts_with(r#"{"op":"omit","node":{"session":""#),
            "omit wire shape: {omit_json}"
        );
        assert!(
            omit_json.ends_with(r#","seq":7}}"#),
            "omit wire shape: {omit_json}"
        );

        // PathError uses `tag = "kind", rename_all = "snake_case"`.
        let err = PathError::UnresolvableNode {
            record: r,
            detail: "session gone".to_string(),
        };
        let json = serde_json::to_string(&err).unwrap();
        let back: PathError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);

        let orphan = Orphan {
            call_id: "tc_3".to_string(),
            tool: "read".to_string(),
            call_node: RecordRef {
                session: s,
                seq: LogSeq(6),
            },
            result_node: RecordRef {
                session: s,
                seq: LogSeq(7),
            },
            rule: 1,
        };
        let wo = PathError::would_orphan(
            vec![orphan.clone()],
            vec![PathOp::Omit {
                node: orphan.result_node,
            }],
        );
        let json = serde_json::to_string(&wo).unwrap();
        let back: PathError = serde_json::from_str(&json).unwrap();
        assert_eq!(wo, back);

        // CostEstimate round-trips and defaults to no divergence.
        let cost = CostEstimate::default();
        assert_eq!(cost.divergence_kind, DivergenceKind::None);
        let json = serde_json::to_string(&cost).unwrap();
        let back: CostEstimate = serde_json::from_str(&json).unwrap();
        assert_eq!(cost, back);

        // DivergenceKind round-trips through its snake_case form.
        for k in [
            DivergenceKind::None,
            DivergenceKind::Omission,
            DivergenceKind::Reorder,
        ] {
            let json = serde_json::to_string(&k).unwrap();
            let back: DivergenceKind = serde_json::from_str(&json).unwrap();
            assert_eq!(k, back);
        }
        assert_eq!(
            serde_json::to_string(&DivergenceKind::Omission).unwrap(),
            r#""omission""#
        );
    }

    /// §4.1: `WouldOrphan`'s `Display` renders a human sentence naming the
    /// omit, the orphaned `call_id`, where the call was issued, and the two
    /// candidate repairs.
    #[test]
    fn would_orphan_display_renders_the_human_sentence() {
        let s = SessionId::new();
        let orphan = Orphan {
            call_id: "tc_3".to_string(),
            tool: "read".to_string(),
            call_node: RecordRef {
                session: s,
                seq: LogSeq(6),
            },
            result_node: RecordRef {
                session: s,
                seq: LogSeq(7),
            },
            rule: 1,
        };
        let err = PathError::would_orphan(vec![orphan], vec![]);
        let rendered = err.to_string();
        assert!(
            rendered.contains("tc_3"),
            "Display must name the orphaned call_id: {rendered}"
        );
        assert!(
            rendered.contains("seq 7") && rendered.contains("seq 6"),
            "Display must name both the call's and the result's seqs: {rendered}"
        );
    }

    /// `RecordRef::Display` is `session/seq` — how a curator reads a node.
    #[test]
    fn record_ref_display_is_session_slash_seq() {
        let s = SessionId::new();
        let r = RecordRef {
            session: s,
            seq: LogSeq(7),
        };
        assert!(r.to_string().ends_with("/7"), "got {}", r);
    }

    // ──────────────────────────────────────────────────────────────────────
    // D1-3a part 2: validator, derive/derive_reordered, CostEstimate, serde.
    // ──────────────────────────────────────────────────────────────────────

    use crate::content::{StopReason, ToolResult, Usage};
    use crate::ids::{ModelRef, ToolName};

    fn ts() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-14T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// An `Assistant` record carrying one `ToolUse` (the call half of a pair).
    fn assistant_call(seq: u64, call_id: &str, tool: &str) -> LogRecord {
        LogRecord::Assistant {
            seq: LogSeq(seq),
            ts: ts(),
            content: vec![ContentBlock::ToolUse {
                call_id: call_id.to_string(),
                name: ToolName::new(tool),
                arguments: serde_json::json!({}),
            }],
            model: "anthropic/claude-sonnet-4-6".parse::<ModelRef>().unwrap(),
            route_reason: serde_json::json!({}),
            usage: Usage::default(),
            stop: StopReason::ToolUse,
        }
    }

    /// A plain `Assistant` text record (no tool use — never half of a pair).
    fn assistant_text(seq: u64, text: &str) -> LogRecord {
        LogRecord::Assistant {
            seq: LogSeq(seq),
            ts: ts(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            model: "anthropic/claude-sonnet-4-6".parse::<ModelRef>().unwrap(),
            route_reason: serde_json::json!({}),
            usage: Usage::default(),
            stop: StopReason::EndTurn,
        }
    }

    /// A `ToolResultRecord` (the result half of a pair).
    fn tool_result(seq: u64, call_id: &str, tool: &str) -> LogRecord {
        LogRecord::ToolResultRecord {
            seq: LogSeq(seq),
            ts: ts(),
            result: ToolResult {
                call_id: call_id.to_string(),
                tool: ToolName::new(tool),
                blocks: vec![],
                is_error: false,
                truncated: None,
            },
        }
    }

    /// Build a `ValidatedPath` from `(PathNode, LogRecord)` pairs (coherent,
    /// no declared incoherence) — the internal assembler the public constructors
    /// share, exercised directly here so the tests can build known bases.
    fn vp_with(pairs: Vec<(PathNode, LogRecord)>) -> ValidatedPath {
        let zipped = pairs.into_iter().map(|(n, r)| (n, Arc::new(r))).collect();
        ValidatedPath::from_resolved(zipped, Vec::new())
    }

    /// A known call↔result pair on session `s`: call at seq 6, result at seq 7,
    /// `call_id = "tc_3"`, `tool = "read"`. Returns `(base, call_node,
    /// result_node)`.
    fn pair_base() -> (ValidatedPath, PathNode, PathNode) {
        let s = SessionId::new();
        let call_n = node(s, 6, NodeStamp::Own, Selector::DefaultRule, &at(5));
        let result_n = node(s, 7, NodeStamp::Own, Selector::DefaultRule, &at(6));
        let base = vp_with(vec![
            (call_n.clone(), assistant_call(6, "tc_3", "read")),
            (result_n.clone(), tool_result(7, "tc_3", "read")),
        ]);
        (base, call_n, result_n)
    }

    // -- validate_coherence --------------------------------------------------

    /// §4.1 rule 0: a fully-coherent candidate (call + result, in order) yields
    /// no orphans.
    #[test]
    fn validate_coherence_coherent_candidate_has_no_orphans() {
        let (base, call_n, result_n) = pair_base();
        let orphans = validate_coherence(&base, &[call_n.clone(), result_n.clone()]);
        assert!(orphans.is_empty(), "coherent candidate: {orphans:?}");
    }

    /// §4.1 rule 1: a `ToolUse` with no answering `ToolResultBlock` in the
    /// candidate. The omitted half (the result node) is named by the base.
    #[test]
    fn validate_coherence_rule_1_call_present_result_omitted() {
        let (base, call_n, result_n) = pair_base();
        let orphans = validate_coherence(&base, &[call_n.clone()]);
        assert_eq!(orphans.len(), 1);
        let o = &orphans[0];
        assert_eq!(o.call_id, "tc_3");
        assert_eq!(o.tool, "read");
        assert_eq!(o.rule, 1);
        assert_eq!(
            o.call_node, call_n.record,
            "call_node is the candidate's call node"
        );
        assert_eq!(
            o.result_node, result_n.record,
            "result_node is the base's (omitted) result node"
        );
    }

    /// §4.1 rule 2: a `ToolResultBlock` with no `ToolUse` in the candidate. The
    /// omitted half (the call node) is named by the base.
    #[test]
    fn validate_coherence_rule_2_result_present_call_omitted() {
        let (base, call_n, result_n) = pair_base();
        let orphans = validate_coherence(&base, &[result_n.clone()]);
        assert_eq!(orphans.len(), 1);
        let o = &orphans[0];
        assert_eq!(o.call_id, "tc_3");
        assert_eq!(o.tool, "read");
        assert_eq!(o.rule, 2);
        assert_eq!(
            o.call_node, call_n.record,
            "call_node is the base's (omitted) call node"
        );
        assert_eq!(
            o.result_node, result_n.record,
            "result_node is the candidate's result node"
        );
    }

    /// §4.1 rule 3: both halves present but the result is reordered before the
    /// call. Both nodes come from the candidate.
    #[test]
    fn validate_coherence_rule_3_result_reordered_before_call() {
        let (base, call_n, result_n) = pair_base();
        let orphans = validate_coherence(&base, &[result_n.clone(), call_n.clone()]);
        assert_eq!(orphans.len(), 1);
        let o = &orphans[0];
        assert_eq!(o.call_id, "tc_3");
        assert_eq!(o.tool, "read");
        assert_eq!(o.rule, 3);
        assert_eq!(o.call_node, call_n.record);
        assert_eq!(o.result_node, result_n.record);
    }

    /// §4.1 rule 3 base-side skip (D1-3c Part 4): a base whose OWN node list
    /// has a result-before-call inversion (a `default_path` that declared the
    /// incoherence) must NOT produce a rule-3 orphan for an empty derivation
    /// candidate — the derivation introduced nothing. `derive(&[])` on such a
    /// base succeeds.
    #[test]
    fn validate_coherence_rule_3_inherited_from_base_is_skipped() {
        let s = SessionId::new();
        let call_n = node(s, 6, NodeStamp::Own, Selector::DefaultRule, &at(5));
        let result_n = node(s, 7, NodeStamp::Own, Selector::DefaultRule, &at(6));
        // Base with a rule-3 inversion: result at index 0, call at index 1.
        let base = vp_with(vec![
            (result_n.clone(), tool_result(7, "tc_3", "read")),
            (call_n.clone(), assistant_call(6, "tc_3", "read")),
        ]);
        // Empty derivation: candidate = base's own nodes in the same order.
        let orphans = validate_coherence(&base, &[result_n.clone(), call_n.clone()]);
        assert!(
            orphans.is_empty(),
            "inherited rule-3 inversion must be skipped, got {orphans:?}"
        );
        // And `derive(&[])` must succeed (not refuse).
        let deriv = base
            .derive(&[])
            .expect("empty derive on a rule-3 base must succeed");
        assert_eq!(
            deriv
                .path
                .nodes()
                .map(|(n, _)| n.record)
                .collect::<Vec<_>>(),
            vec![result_n.record, call_n.record]
        );
    }

    /// §4.1: two independent orphaned pairs in one candidate are both reported,
    /// in candidate order, with the right fields per orphan.
    #[test]
    fn validate_coherence_multiple_orphans_in_candidate_order() {
        let s = SessionId::new();
        let call_a = node(s, 1, NodeStamp::Own, Selector::DefaultRule, &at(0));
        let result_a = node(s, 2, NodeStamp::Own, Selector::DefaultRule, &at(1));
        let call_b = node(s, 3, NodeStamp::Own, Selector::DefaultRule, &at(2));
        let result_b = node(s, 4, NodeStamp::Own, Selector::DefaultRule, &at(3));
        let base = vp_with(vec![
            (call_a.clone(), assistant_call(1, "tc_a", "read")),
            (result_a.clone(), tool_result(2, "tc_a", "read")),
            (call_b.clone(), assistant_call(3, "tc_b", "bash")),
            (result_b.clone(), tool_result(4, "tc_b", "bash")),
        ]);
        // Drop result_a (rule 1 for tc_a) and drop call_b (rule 2 for tc_b).
        // Candidate order of first occurrence: tc_a at idx 0 (call), tc_b at
        // idx 2 (result) — so tc_a's orphan comes first.
        let orphans = validate_coherence(&base, &[call_a.clone(), result_b.clone()]);
        assert_eq!(orphans.len(), 2);
        assert_eq!(orphans[0].call_id, "tc_a");
        assert_eq!(orphans[0].rule, 1);
        assert_eq!(orphans[0].tool, "read");
        assert_eq!(orphans[0].call_node, call_a.record);
        assert_eq!(orphans[0].result_node, result_a.record);
        assert_eq!(orphans[1].call_id, "tc_b");
        assert_eq!(orphans[1].rule, 2);
        assert_eq!(orphans[1].tool, "bash");
        assert_eq!(orphans[1].call_node, call_b.record);
        assert_eq!(orphans[1].result_node, result_b.record);
    }

    // -- offers_for ----------------------------------------------------------

    /// §4.1: each rule yields the right two `PathOp` repair offers.
    #[test]
    fn offers_for_each_rule() {
        let s = SessionId::new();
        let call_n = node(s, 6, NodeStamp::Own, Selector::DefaultRule, &at(0));
        let result_n = node(s, 7, NodeStamp::Own, Selector::DefaultRule, &at(1));
        let mk = |rule: u8| Orphan {
            call_id: "tc_3".to_string(),
            tool: "read".to_string(),
            call_node: call_n.record,
            result_node: result_n.record,
            rule,
        };
        // Rule 1: omit the call, or include the result.
        assert_eq!(
            offers_for(&[mk(1)]),
            vec![
                PathOp::Omit {
                    node: call_n.record
                },
                PathOp::Include {
                    node: result_n.record
                },
            ]
        );
        // Rule 2: omit the result, or include the call.
        assert_eq!(
            offers_for(&[mk(2)]),
            vec![
                PathOp::Omit {
                    node: result_n.record
                },
                PathOp::Include {
                    node: call_n.record
                },
            ]
        );
        // Rule 3: the inverse reorder -- Move the call back before the result.
        // (Omitting either half would orphan the other; the honest single-op
        // repair for a reorder is the inverse reorder.)
        assert_eq!(
            offers_for(&[mk(3)]),
            vec![PathOp::Move {
                node: call_n.record,
                before: result_n.record
            }]
        );
    }

    /// §4.1 contract: "each [offer], applied to the same base, validates."
    /// Each emitted offer, combined with the curator's ORIGINAL ops and
    /// applied to the same base, must yield a coherent path (no `WouldOrphan`).
    /// This is the property `offers_for_each_rule` above does NOT check -- it
    /// only asserts the offer shapes -- and the property an earlier cut of
    /// `offers_for` (tail-append `Include`, `Omit`-for-rule-3) silently broke.
    #[test]
    fn offers_for_each_offer_validates() {
        let (base, call_n, result_n) = pair_base();

        // Rule 1: curator omitted the result, orphaning the call.
        let err = base
            .derive(&[PathOp::Omit {
                node: result_n.record,
            }])
            .unwrap_err();
        let PathError::WouldOrphan { offers, .. } = err else {
            panic!("rule 1: expected WouldOrphan");
        };
        for offer in &offers {
            base.derive(&[
                PathOp::Omit {
                    node: result_n.record,
                },
                offer.clone(),
            ])
            .expect("rule 1 offer must validate when combined with the original op");
        }

        // Rule 2: curator omitted the call, orphaning the result. The "keep
        // the call" offer is an `Include` that must re-insert the call at its
        // ORIGINAL position (before the result), not the tail -- a tail-append
        // would produce `[result, call]` and trip rule 3.
        let err = base
            .derive(&[PathOp::Omit {
                node: call_n.record,
            }])
            .unwrap_err();
        let PathError::WouldOrphan { offers, .. } = err else {
            panic!("rule 2: expected WouldOrphan");
        };
        for offer in &offers {
            base.derive(&[
                PathOp::Omit {
                    node: call_n.record,
                },
                offer.clone(),
            ])
            .expect("rule 2 offer must validate when combined with the original op");
        }

        // Rule 3: a reorder (result moved before its call). Only
        // `derive_reordered` produces a rule-3 orphan, so the offer (the inverse
        // `Move`) is applied on the `derive_reordered` path.
        let err = base
            .derive_reordered(&[PathOp::Move {
                node: result_n.record,
                before: call_n.record,
            }])
            .unwrap_err();
        let PathError::WouldOrphan { offers, .. } = err else {
            panic!("rule 3: expected WouldOrphan");
        };
        for offer in &offers {
            base.derive_reordered(&[
                PathOp::Move {
                    node: result_n.record,
                    before: call_n.record,
                },
                offer.clone(),
            ])
            .expect("rule 3 offer must validate when combined with the original op");
        }
    }

    // -- derive / derive_reordered -------------------------------------------

    /// §4.2: `derive` refuses any `PathOp::Move` with
    /// `ReorderRequiresExplicitDerivation` — before any candidate mutation.
    #[test]
    fn derive_refuses_move() {
        let (base, call_n, result_n) = pair_base();
        let err = base
            .derive(&[PathOp::Move {
                node: result_n.record,
                before: call_n.record,
            }])
            .unwrap_err();
        assert!(matches!(err, PathError::ReorderRequiresExplicitDerivation));
    }

    /// §4.1: `derive` refuses an orphaning `Omit` (drop the result of a pair)
    /// with `WouldOrphan { orphans, offers }`, and the offers include both
    /// candidate repairs.
    #[test]
    fn derive_refuses_orphaning_omit() {
        let (base, call_n, result_n) = pair_base();
        let err = base
            .derive(&[PathOp::Omit {
                node: result_n.record,
            }])
            .unwrap_err();
        match err {
            PathError::WouldOrphan { orphans, offers } => {
                assert_eq!(orphans.len(), 1);
                assert_eq!(orphans[0].rule, 1);
                assert_eq!(orphans[0].call_id, "tc_3");
                assert_eq!(orphans[0].call_node, call_n.record);
                assert_eq!(orphans[0].result_node, result_n.record);
                assert_eq!(
                    offers,
                    vec![
                        PathOp::Omit {
                            node: call_n.record
                        },
                        PathOp::Include {
                            node: result_n.record
                        },
                    ]
                );
            }
            other => panic!("expected WouldOrphan, got {other:?}"),
        }
    }

    /// §4.1 + §4.2: `derive` succeeds on a coherent `Omit` (a node that is NOT
    /// half of a tool pair), returning a `Derivation` whose `path` is coherent
    /// (`incoherence` empty — exercised via `from_resolved`'s empty input) and
    /// whose `cost` reports the divergence.
    #[test]
    fn derive_succeeds_on_coherent_omit() {
        let s = SessionId::new();
        let text_n = node(s, 5, NodeStamp::Own, Selector::DefaultRule, &at(0));
        let call_n = node(s, 6, NodeStamp::Own, Selector::DefaultRule, &at(1));
        let result_n = node(s, 7, NodeStamp::Own, Selector::DefaultRule, &at(2));
        let base = vp_with(vec![
            (text_n.clone(), assistant_text(5, "intro")),
            (call_n.clone(), assistant_call(6, "tc_3", "read")),
            (result_n.clone(), tool_result(7, "tc_3", "read")),
        ]);
        let deriv = base
            .derive(&[PathOp::Omit {
                node: text_n.record,
            }])
            .expect("omitting a non-pair node is coherent");
        // Derived path: [call, result] — coherent.
        assert_eq!(
            deriv
                .path
                .nodes()
                .map(|(n, _)| n.record)
                .collect::<Vec<_>>(),
            vec![call_n.record, result_n.record]
        );
        // Cost: shared prefix is 0 (derived[0]=call != base[0]=text).
        let cost = &deriv.cost;
        assert_eq!(cost.shared_prefix_nodes, 0);
        assert_eq!(cost.first_divergence, Some(text_n.record));
        assert_eq!(cost.divergence_kind, DivergenceKind::Omission);
        assert!(!cost.divergence_inside_frozen_tier, "no Inherited nodes");
        // discarded prefix = all of base (nothing shared).
        assert!(
            cost.discarded_prefix_tokens_est > 0,
            "discarded tail has tokens"
        );
    }

    /// §4.2: `derive` succeeds on a coherent `Restamp`; the cost's
    /// `first_divergence` is the restamped node and `divergence_kind` is
    /// `Omission` (ambiguous — a restamp is neither a reorder nor a clean
    /// omission, so it falls to the cheap direction per §4.2).
    #[test]
    fn derive_succeeds_on_coherent_restamp() {
        let (base, call_n, _result_n) = pair_base();
        let s = call_n.record.session;
        let deriv = base
            .derive(&[PathOp::Restamp {
                node: call_n.record,
                to: NodeStamp::Inherited { from: s },
            }])
            .expect("restamp of a present node is coherent");
        let cost = &deriv.cost;
        // Shared prefix breaks at the restamped node (stamp changed).
        assert_eq!(cost.shared_prefix_nodes, 0);
        assert_eq!(cost.first_divergence, Some(call_n.record));
        assert_eq!(cost.divergence_kind, DivergenceKind::Omission);
        // The result node is Inherited? No — only the call was restamped.
        // The derived path keeps the result as Own, so no frozen tier here.
        assert!(!cost.divergence_inside_frozen_tier);
        // The derived node list reflects the new stamp.
        let stamps: Vec<NodeStamp> = deriv.path.nodes().map(|(n, _)| n.stamp).collect();
        assert_eq!(
            stamps,
            vec![NodeStamp::Inherited { from: s }, NodeStamp::Own]
        );
    }

    /// §4.2: `derive` succeeds on a coherent `Include` of a base node
    /// (re-include a node that is on the base). The re-included node is appended
    /// at the tail; if the base was fully shared, the cost reports no
    /// divergence.
    #[test]
    fn derive_succeeds_on_include_of_base_node() {
        let (base, _call_n, result_n) = pair_base();
        // Re-include the result node (already present) — appends a second
        // copy at the tail. Coherent (two results for one call: rule 3 checks
        // ordering, and the first result is still after the call).
        let deriv = base
            .derive(&[PathOp::Include {
                node: result_n.record,
            }])
            .expect("re-include of a base node is coherent");
        let recs: Vec<RecordRef> = deriv.path.nodes().map(|(n, _)| n.record).collect();
        assert_eq!(recs.len(), 3);
        // Full base was shared (3 nodes: call, result, + appended result);
        // shared_prefix_nodes = base.len() = 2, so no divergence.
        assert_eq!(deriv.cost.shared_prefix_nodes, 2);
        assert_eq!(deriv.cost.first_divergence, None);
        assert_eq!(deriv.cost.divergence_kind, DivergenceKind::None);
    }

    /// §2.7: a foreign `Include` (a `RecordRef` not on the base) is refused with
    /// `UnresolvableNode` — the honest placeholder until the cross-tree read
    /// surface lands (D1-8).
    #[test]
    fn derive_refuses_foreign_include() {
        let (base, _call_n, _result_n) = pair_base();
        let foreign = RecordRef {
            session: SessionId::new(),
            seq: LogSeq(99),
        };
        let err = base
            .derive(&[PathOp::Include { node: foreign }])
            .unwrap_err();
        match err {
            PathError::UnresolvableNode { record, detail } => {
                assert_eq!(record, foreign);
                assert!(detail.contains("D1-8"), "detail must name D1-8: {detail}");
            }
            other => panic!("expected UnresolvableNode, got {other:?}"),
        }
    }

    /// §4.2: `derive` refuses a `Move` whose target is not on the path with
    /// `UnresolvableNode`.
    #[test]
    fn derive_refuses_move_targeting_absent_node() {
        let (base, _call_n, _result_n) = pair_base();
        let absent = RecordRef {
            session: base.nodes().next().unwrap().0.record.session,
            seq: LogSeq(99),
        };
        let present = base.nodes().next().unwrap().0.record;
        // `derive` refuses Move outright (ReorderRequiresExplicitDerivation),
        // so use `derive_reordered` to reach the target-presence check.
        let err = base
            .derive_reordered(&[PathOp::Move {
                node: absent,
                before: present,
            }])
            .unwrap_err();
        match err {
            PathError::UnresolvableNode { record, detail } => {
                assert_eq!(record, absent);
                assert!(detail.contains("Move targets a node not on the path"));
            }
            other => panic!("expected UnresolvableNode, got {other:?}"),
        }
    }

    /// §4.2: `derive_reordered` accepts a `Move` that does not break coherence
    /// (moving a non-pair text node) and reports `Reorder` as the divergence
    /// kind, with `moved = true` detected as a position swap.
    #[test]
    fn derive_reordered_accepts_coherent_move() {
        let s = SessionId::new();
        let text1 = node(s, 5, NodeStamp::Own, Selector::DefaultRule, &at(0));
        let text2 = node(s, 8, NodeStamp::Own, Selector::DefaultRule, &at(3));
        let call_n = node(s, 6, NodeStamp::Own, Selector::DefaultRule, &at(1));
        let result_n = node(s, 7, NodeStamp::Own, Selector::DefaultRule, &at(2));
        let base = vp_with(vec![
            (text1.clone(), assistant_text(5, "intro")),
            (text2.clone(), assistant_text(8, "aside")),
            (call_n.clone(), assistant_call(6, "tc_3", "read")),
            (result_n.clone(), tool_result(7, "tc_3", "read")),
        ]);
        // Move text2 to immediately before text1. Call+result stay in order.
        let deriv = base
            .derive_reordered(&[PathOp::Move {
                node: text2.record,
                before: text1.record,
            }])
            .expect("moving a non-pair node before another is coherent");
        let recs: Vec<RecordRef> = deriv.path.nodes().map(|(n, _)| n.record).collect();
        assert_eq!(
            recs,
            vec![text2.record, text1.record, call_n.record, result_n.record]
        );
        // Cost: shared prefix 0 (derived[0]=text2 != base[0]=text1). The base's
        // text1 moved to index 1 in derived, so it is a position swap → Reorder.
        assert_eq!(deriv.cost.shared_prefix_nodes, 0);
        assert_eq!(deriv.cost.first_divergence, Some(text1.record));
        assert_eq!(deriv.cost.divergence_kind, DivergenceKind::Reorder);
    }

    /// §4.2: `derive_reordered` refuses a `Move` that reorders a result before
    /// its call (rule 3), returning `WouldOrphan` — reordering is allowed, but
    /// coherence still holds.
    #[test]
    fn derive_reordered_refuses_incoherent_move() {
        let (base, call_n, result_n) = pair_base();
        let err = base
            .derive_reordered(&[PathOp::Move {
                node: result_n.record,
                before: call_n.record,
            }])
            .unwrap_err();
        match err {
            PathError::WouldOrphan { orphans, .. } => {
                assert_eq!(orphans.len(), 1);
                assert_eq!(orphans[0].rule, 3);
            }
            other => panic!("expected WouldOrphan, got {other:?}"),
        }
    }

    // -- CostEstimate: frozen tier boundary ----------------------------------

    /// §4.2: a divergence at or before the last `Inherited`-stamped node is
    /// INSIDE the frozen tier ("dropping from the head spends everything").
    #[test]
    fn cost_divergence_inside_frozen_tier_when_omitting_an_inherited_node() {
        let s = SessionId::new();
        let inh = node(
            s,
            1,
            NodeStamp::Inherited { from: s },
            Selector::DefaultRule,
            &at(0),
        );
        let call_n = node(s, 2, NodeStamp::Own, Selector::DefaultRule, &at(1));
        let result_n = node(s, 3, NodeStamp::Own, Selector::DefaultRule, &at(2));
        let base = vp_with(vec![
            (inh.clone(), assistant_text(1, "inherited head")),
            (call_n.clone(), assistant_call(2, "tc_1", "read")),
            (result_n.clone(), tool_result(3, "tc_1", "read")),
        ]);
        // Omit the inherited node — divergence at index 0, last Inherited at
        // index 0, so 0 <= 0 → inside the frozen tier.
        let deriv = base.derive(&[PathOp::Omit { node: inh.record }]).unwrap();
        assert_eq!(deriv.cost.first_divergence, Some(inh.record));
        assert!(deriv.cost.divergence_inside_frozen_tier);
        assert_eq!(deriv.cost.divergence_kind, DivergenceKind::Omission);
    }

    /// §4.2: a divergence AFTER the last `Inherited`-stamped node is OUTSIDE
    /// the frozen tier ("dropping from the tail is nearly free").
    #[test]
    fn cost_divergence_outside_frozen_tier_when_omitting_a_tail_node() {
        let s = SessionId::new();
        let inh = node(
            s,
            1,
            NodeStamp::Inherited { from: s },
            Selector::DefaultRule,
            &at(0),
        );
        let text = node(s, 2, NodeStamp::Own, Selector::DefaultRule, &at(1));
        let call_n = node(s, 3, NodeStamp::Own, Selector::DefaultRule, &at(2));
        let result_n = node(s, 4, NodeStamp::Own, Selector::DefaultRule, &at(3));
        let base = vp_with(vec![
            (inh.clone(), assistant_text(1, "inherited head")),
            (text.clone(), assistant_text(2, "tail text")),
            (call_n.clone(), assistant_call(3, "tc_1", "read")),
            (result_n.clone(), tool_result(4, "tc_1", "read")),
        ]);
        // Omit the text node at index 1 — divergence at index 1, last
        // Inherited at index 0, so 1 <= 0 is false → outside the frozen tier.
        let deriv = base.derive(&[PathOp::Omit { node: text.record }]).unwrap();
        assert_eq!(deriv.cost.shared_prefix_nodes, 1);
        assert_eq!(deriv.cost.first_divergence, Some(text.record));
        assert!(!deriv.cost.divergence_inside_frozen_tier);
    }

    // -- ValidatedPath / Derivation serde + key ------------------------------

    /// `ValidatedPath` and `Derivation` serde round-trip.
    #[test]
    fn validated_path_and_derivation_roundtrip() {
        let (base, _call_n, _result_n) = pair_base();
        let deriv = base
            .derive(&[PathOp::Restamp {
                node: base.nodes().next().unwrap().0.record,
                to: NodeStamp::Inherited {
                    from: base.nodes().next().unwrap().0.record.session,
                },
            }])
            .unwrap();
        let json = serde_json::to_string(&deriv).unwrap();
        let back: Derivation = serde_json::from_str(&json).unwrap();
        assert_eq!(deriv, back);
        // ValidatedPath alone round-trips too.
        let pj = serde_json::to_string(&deriv.path).unwrap();
        let pb: ValidatedPath = serde_json::from_str(&pj).unwrap();
        assert_eq!(deriv.path, pb);
    }

    /// `ValidatedPath::key` matches `SelectionKey::from_nodes` over the same
    /// expanded node list (§2.3 + §2.8).
    #[test]
    fn validated_path_key_matches_from_nodes() {
        let (base, _call_n, _result_n) = pair_base();
        let nodes: Vec<PathNode> = base.nodes().map(|(n, _)| n.clone()).collect();
        let expected = SelectionKey::from_nodes(&nodes);
        assert_eq!(base.key(), expected);
    }

    // -- default_path / declare_incoherence (D1-3c Part 3a) -----------------

    /// `default_path` on a coherent node list declares no incoherence — the
    /// identity case over today's behaviour (DESIGN §5).
    #[test]
    fn default_path_on_a_coherent_list_declares_nothing() {
        let (base, _call_n, _result_n) = pair_base();
        let pairs: Vec<(PathNode, Arc<LogRecord>)> = base
            .nodes()
            .map(|(n, r)| (n.clone(), Arc::clone(r)))
            .collect();
        let path = ValidatedPath::default_path(pairs);
        assert!(
            path.incoherence.is_empty(),
            "coherent list declares nothing"
        );
    }

    /// `default_path` on a call-without-result (rule 1) declares exactly one
    /// `HarnessDrop` for the orphaned `call_id`, rather than refusing.
    #[test]
    fn default_path_declares_a_rule_1_orphan_as_incoherence() {
        let s = SessionId::new();
        let call_n = node(s, 6, NodeStamp::Own, Selector::DefaultRule, &at(0));
        let pairs = vec![(call_n.clone(), Arc::new(assistant_call(6, "tc_3", "read")))];
        let path = ValidatedPath::default_path(pairs);
        assert_eq!(path.incoherence, vec![HarnessDrop::new("tc_3")]);
    }

    /// `default_path` on a result-before-call (rule 3) declares the
    /// incoherence, and `derive(&[])` on that base succeeds (the rule-3
    /// base-side skip from Part 4 makes the empty derivation not refuse).
    #[test]
    fn default_path_declares_rule_3_and_empty_derive_succeeds() {
        let s = SessionId::new();
        let call_n = node(s, 6, NodeStamp::Own, Selector::DefaultRule, &at(0));
        let result_n = node(s, 7, NodeStamp::Own, Selector::DefaultRule, &at(1));
        let pairs = vec![
            (result_n.clone(), Arc::new(tool_result(7, "tc_3", "read"))),
            (call_n.clone(), Arc::new(assistant_call(6, "tc_3", "read"))),
        ];
        let path = ValidatedPath::default_path(pairs);
        assert_eq!(path.incoherence, vec![HarnessDrop::new("tc_3")]);
        // Empty derivation does not introduce anything → succeeds.
        path.derive(&[])
            .expect("empty derive on a declared rule-3 base must succeed");
    }
}
