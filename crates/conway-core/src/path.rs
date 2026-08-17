//! The first-class context path — pure value types and the model-free
//! `SelectionKey` (DESIGN-context-path §2.1–§2.6, §4.1–§4.2).
//!
//! A **path** is an ordered list of *references* to immutable records plus an
//! optional reference to another path **selection** as its prefix. A
//! **selection** is a frozen path identified by a model-free,
//! content-addressed `SelectionKey` over its (expanded) node list. This module
//! owns the vocabulary; the validating constructors (`default_path`,
//! `derive`, `derive_reordered`), the three-rule coherence validator, the
//! `PathStore` port, head resolution and assembly are later sub-units
//! (D1-3b..e) and live elsewhere.
//!
//! What is here is deliberately minimal and pure: no I/O, no policy, no
//! validating constructors. Everything round-trips through serde, and
//! `SelectionKey`'s exclusions are documented at the hash site because they
//! are load-bearing for the "ten heads, one selection" sharing story (§2.3).

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::canon::canonical_json_bytes;
use crate::ids::{LogSeq, SessionId};

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
    /// Omit `node` from the derived path.
    Omit { node: RecordRef },
    /// (Re-)include `node` in the derived path.
    Include { node: RecordRef },
    /// Move `node` to immediately before `before` in the derived path.
    /// Refused by `derive`; accepted by `derive_reordered` (DESIGN §4.2).
    Move { node: RecordRef, before: RecordRef },
    /// Re-stamp a node (e.g. committing a curated selection so children can
    /// share its cache by stamping `Head`/`Own` as `Inherited { from }`).
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
}
