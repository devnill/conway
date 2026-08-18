//! `conway.memory`: the cross-tree curator that recalls records from
//! explicitly labelled past sessions into the current turn's assembled
//! context (DESIGN-context-path §11.7, board item 01M090JY3KYHQQMKCZZM1Y6EDZ).
//!
//! # Why this is a curator, not a subsystem
//!
//! §11.7 states the whole design in one sentence: memory "needs no storage
//! of its own, no retrieval semantics of its own, and no new port" --
//! "recall what I learned in a past session" is a cross-session *selection*,
//! and the selection layer already has everything that requires: the §11.5
//! read surface (`CurateCtx::store`/`resolver`, reachable from ANY session,
//! not just the calling one) and, since Unit 3a, a cross-tree
//! [`ValidatedPath::derive_with`] that can `Include` a record the curator
//! resolved from OUTSIDE the calling session's own ancestry. This crate is
//! nothing but policy laid over that seam -- same shape as
//! `conway-plugin-stepguard`'s own "the mechanism moved out, the judgment
//! came with it" framing, one port down.
//!
//! # The load-bearing requirement (INTENT.md §5e)
//!
//! Same-session-only selection is NOT memory -- that is `conway.compaction`.
//! `MemoryCurator::curate` must be able to reach a record OUTSIDE the
//! calling session's own ancestry, and this crate's own end-to-end test
//! (`tests/memory_end_to_end.rs`) proves a foreign record actually reaches
//! an assembled request through a real turn, not merely that the selection
//! type permits it.
//!
//! # Selection policy: an explicit opt-in label (R1)
//!
//! A session is recallable iff its `SessionMeta.labels` contains
//! [`MemoryConfig::label`] (default [`DEFAULT_LABEL`], `"memory"`). This is
//! the only policy that is *explicit*: recency or similarity heuristics
//! would silently drag arbitrary old context into every session -- exactly
//! the "cache-invalidation machine" `PHILOSOPHY.md`'s automatic-compaction
//! refusal warns against, one layer over. A user labels a session (e.g. via
//! a future `/conway.memory.remember` command, out of this item's scope);
//! nothing else is ever recalled. `SessionFilter { label: Some(..), .. }`
//! is the exact mechanism `SessionStore::list` already exposes -- no new
//! primitive.
//!
//! # Configuration is a constructor argument, not `CurateCtx` (R2)
//!
//! [`CurateCtx`] carries no `PluginConfig` (that is a different, narrower
//! per-agent mechanism -- `SessionMeta::plugin_config` -- and threading it
//! onto the curator port is a Unit-2 port change, out of scope here and
//! unnecessary). This plugin follows the established first-party
//! convention instead: [`MemoryPlugin::new`] takes a [`MemoryConfig`]
//! constructor argument, the same shape `SkillsPlugin::new`/
//! `StepGuardPlugin::new`/`McpPlugin::new` already use.
//!
//! # `PathStore` is not needed (R3)
//!
//! Unit 2 deferred wiring `PathStore` onto `CurateCtx` "if the design
//! needs it." This design does not: the whole selection is computed from
//! `ctx.store.list(..)` (find labelled sessions) and `ctx.store.read(..)`
//! (read their records), both already on the §11.5 read surface.
//! `PathStore` exists to recall a *stored, named selection* -- a later
//! feature this crate does not attempt.
//!
//! # Bounded by construction (R4)
//!
//! At most [`MemoryConfig::max_records`] records and at most
//! [`MemoryConfig::max_bytes`] of (conservatively estimated) recalled text,
//! enforced by `MemoryCurator::curate` BEFORE it ever calls `derive_with`
//! -- see that function's own doc for exactly where each cap is checked.
//! Candidate sessions are walked oldest-first (`SessionMeta::created`, tied
//! on `SessionId` for a total order), and each session's own records are
//! walked in their natural (already seq-ordered) `SessionStore::read`
//! order, so the selection is reproducible.
//!
//! # Both halves or neither (R5)
//!
//! Unit 3a's disclosed limit: `derive_with` can only refuse an orphan it
//! can NAME, so a recalled `ToolUse` whose answering `ToolResultBlock` the
//! curator never resolved is not refused, and the derived path would ship
//! an unanswered tool call most providers reject outright. This crate's
//! first cut is the simplest sufficient fix, stated as a deliberate
//! restriction rather than an oversight: it recalls only records that carry
//! NO tool-call/result half at all -- see `carries_tool_block`. A future
//! cut could recall an answered pair as a unit; this one never ships half
//! of one.
//!
//! # Ancestry exclusion
//!
//! Recalling a record already reachable from the calling session's own
//! path would be a same-tree selection, not memory. `MemoryCurator::curate`
//! excludes `ctx.session_id` itself and every session already represented
//! on `base` (`base.nodes()` -- `PathNode::record.session`, the owning
//! session of every node already on the path, `Own`/`Head`/`Inherited`
//! alike).
//!
//! # `Unchanged` is the common path (R6)
//!
//! No labelled session (beyond the calling one's own ancestry) -> the
//! curator never reads beyond the cheap `list` call and returns
//! [`CurateOutcome::Unchanged`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use conway::plugin::{
    async_trait, ContentBlock, CurateCtx, CurateOutcome, Curator, Plugin, PluginManifest, SeqRange,
    Tool,
};
use conway::{
    LogRecord, OpLabel, PathOp, RecordRef, Selector, SessionFilter, SessionId, ValidatedPath,
};

/// This plugin's published manifest id -- a config author (or a first-party
/// bundle's own linking module) resolves `[plugins].install` entries
/// against this constant.
pub const PLUGIN_ID: &str = "conway.memory";

/// The default `SessionMeta.labels` entry that makes a session recallable
/// (R1). Not (yet) attachable through any built-in surface -- see the
/// module doc's "a future `/conway.memory.remember` command" note; a
/// session gets this label today only however the embedder chooses to
/// stamp `SessionMeta::labels` at creation/append time.
pub const DEFAULT_LABEL: &str = "memory";

/// The default cap on recalled records (R4).
pub const DEFAULT_MAX_RECORDS: usize = 8;

/// The default cap on recalled bytes (R4), measured as documented on
/// `record_byte_estimate`.
pub const DEFAULT_MAX_BYTES: usize = 8192;

/// Constructor configuration for [`MemoryPlugin`] (R2): arrives as an
/// ordinary constructor argument, never through `CurateCtx`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryConfig {
    /// The `SessionMeta.labels` entry a session must carry to be
    /// recallable (R1).
    pub label: String,
    /// The maximum number of records this curator will recall in one
    /// derivation (R4).
    pub max_records: usize,
    /// The maximum total (estimated) bytes of recalled record content in
    /// one derivation (R4).
    pub max_bytes: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            label: DEFAULT_LABEL.to_string(),
            max_records: DEFAULT_MAX_RECORDS,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// The `conway.memory` plugin: contributes exactly one curator
/// (`MemoryCurator`) and no tools, no commands, no host capabilities.
/// Installs through the SAME `Plugin::curators`/`with_plugin` surface every
/// other curation capability uses (GP-03) -- no privileged first-party
/// channel.
pub struct MemoryPlugin {
    config: MemoryConfig,
}

impl MemoryPlugin {
    pub fn new(config: MemoryConfig) -> Self {
        Self { config }
    }
}

impl Plugin for MemoryPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PLUGIN_ID.to_string(),
            version: "0.1.0".to_string(),
            tools: vec![],
            required_host_caps: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![]
    }

    fn curators(&self) -> Vec<Arc<dyn Curator>> {
        vec![Arc::new(MemoryCurator {
            config: self.config.clone(),
        })]
    }
}

/// The curator itself. See the module doc for the full policy (R1-R6); this
/// type's own [`curate`](Curator::curate) doc walks the mechanical steps in
/// order.
struct MemoryCurator {
    config: MemoryConfig,
}

impl MemoryCurator {
    /// Every session in `from`'s ancestry, walking the `SessionMeta.origin`
    /// parent chain upward. Does NOT include `from` itself (the caller adds
    /// it).
    ///
    /// This is the TRUE ancestry, independent of what the resolved path
    /// happens to show: a masked-out ancestor still counts as an ancestor
    /// (see the exclusion comment in `curate`). The walk mirrors the one
    /// `TranscriptResolver` already performs, and is bounded by the same
    /// `MAX_ANCESTRY_DEPTH` value (256) that `conway_core::transcript`
    /// publishes -- restated here as a literal because the constant is not
    /// re-exported through the `conway` facade, and this crate holds the
    /// facade-only discipline. The bound also makes a cyclic `origin` chain
    /// (corrupt data) terminate rather than hang; a `visited` set makes it
    /// terminate immediately in the common cyclic case.
    ///
    /// A metadata read that fails simply stops the walk: exclusion is a
    /// safety filter, and a partial ancestry can only ever exclude FEWER
    /// sessions, never recall something the operator did not label.
    async fn ancestry_of(&self, ctx: &CurateCtx, from: SessionId) -> BTreeSet<SessionId> {
        const MAX_ANCESTRY_DEPTH: usize = 256;
        let mut seen: BTreeSet<SessionId> = BTreeSet::new();
        let mut current = from;
        for _ in 0..MAX_ANCESTRY_DEPTH {
            let Ok(meta) = ctx.store.meta(&current).await else {
                break;
            };
            let Some(origin) = meta.origin else {
                break;
            };
            if !seen.insert(origin.parent) {
                break;
            }
            current = origin.parent;
        }
        seen
    }
}

/// A record kind that becomes a path NODE at all -- the same closed set
/// `conway_runtime::context::path::resolve_default_path`'s own doc names as
/// "content records" (as opposed to metadata records: `Header`,
/// `AgentResultRecord`, `ContextReportRecord`, `ContextMask`,
/// `ContextPathSet`, `ContextPathNamed`, none of which `PathOp::Include`
/// could ever meaningfully name). A curator recalling a metadata record
/// would hand `derive_with` a ref that can never appear on any path, so
/// this filter runs before R5's tool-pairing filter, not after.
fn is_content_record(rec: &LogRecord) -> bool {
    matches!(
        rec,
        LogRecord::UserTurn { .. }
            | LogRecord::Assistant { .. }
            | LogRecord::ToolResultRecord { .. }
            | LogRecord::ForkDirective { .. }
            | LogRecord::ParentSteer { .. }
            | LogRecord::SystemNote { .. }
            | LogRecord::ChildResultRecord { .. }
    )
}

/// R5's filter: does this record carry either half of a tool-call pair?
/// `Assistant` carries the CALL half as a `ContentBlock::ToolUse` in its
/// `content`; `ToolResultRecord` (the log's own name for the RESULT half --
/// see `log.rs`'s `ToolResultRecord` doc, `#[serde(rename = "tool_result")]`)
/// carries the RESULT half in full, whether or not it happens to also
/// nest a `ContentBlock::ToolResultBlock` inside its own `blocks`. A record
/// answering `true` here is never recalled -- see the module doc's "both
/// halves or neither".
fn carries_tool_block(rec: &LogRecord) -> bool {
    match rec {
        LogRecord::Assistant { content, .. } => content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolUse { .. })),
        LogRecord::ToolResultRecord { .. } => true,
        _ => false,
    }
}

/// A conservative estimate of a record's recalled "bytes" for R4's budget:
/// the record's own canonical JSON encoding length. Deliberately NOT a
/// hand-maintained per-variant sum of `text`/`content` fields -- that
/// enumeration would silently drift every time `LogRecord` grows a
/// text-bearing variant (it already has seven, per `is_content_record`),
/// where this is exact-by-construction and, if anything, over-counts (JSON
/// structure/serde tags are not "recalled text" but ARE counted), which is
/// the safe direction for a cap.
/// A record that will not serialize costs `usize::MAX`, not `0`: an
/// unserializable record is one this curator cannot honestly size, and
/// zero-costing it would let it (and everything after it) past a budget
/// this function's whole purpose is to enforce -- the under-counting
/// direction the doc above disclaims. `usize::MAX` fails closed: the cap
/// check rejects it, the walk stops, and the turn proceeds on whatever was
/// already selected. `LogRecord` round-trips through the store on every
/// write, so this is unreachable in practice; it is the fallback's
/// DIRECTION that matters.
fn record_byte_estimate(rec: &LogRecord) -> usize {
    serde_json::to_vec(rec)
        .map(|bytes: Vec<u8>| bytes.len())
        .unwrap_or(usize::MAX)
}

#[async_trait]
impl Curator for MemoryCurator {
    /// R1-R6, in the order they are actually checked:
    ///
    /// 1. **R1** -- list sessions carrying `config.label` via
    ///    `ctx.store.list(SessionFilter { label: Some(..), .. })`. Empty ->
    ///    **R6** `Unchanged`, before any further read.
    /// 2. **Ancestry exclusion** -- drop `ctx.session_id` and every session
    ///    already represented on `base` (`base.nodes()`).
    /// 3. Still empty -> **R6** `Unchanged`.
    /// 4. Deterministic order: candidate sessions oldest-first
    ///    (`SessionMeta::created`, ties broken by `SessionId`).
    /// 5. For each candidate session, oldest first, `ctx.store.read` its
    ///    full log in seq order; keep only content records
    ///    (`is_content_record`) that carry neither half of a tool-call pair
    ///    (**R5**, `carries_tool_block`).
    /// 6. **R4** -- stop the walk (across ALL sessions, not per-session) the
    ///    moment either cap would be exceeded: `max_records` records
    ///    selected, or `max_bytes` of `record_byte_estimate` total. Both
    ///    caps are enforced HERE, before `derive_with` ever runs.
    /// 7. Nothing survives -> **R6** `Unchanged`.
    /// 8. Otherwise, one `PathOp::Include` per selected record plus the
    ///    `foreign` map `derive_with` resolves them against, stamped
    ///    `Selector::Plugin { id: PLUGIN_ID, op: "recall" }` -- one curation
    ///    act, one provenance.
    async fn curate(&self, ctx: &CurateCtx, base: &ValidatedPath) -> CurateOutcome {
        let filter = SessionFilter {
            label: Some(self.config.label.clone()),
            include_ephemeral: false,
            ..Default::default()
        };
        let mut candidates = match ctx.store.list(filter).await {
            Ok(sessions) => sessions,
            Err(err) => {
                return CurateOutcome::Failed {
                    reason: format!("could not list labelled sessions: {err}"),
                };
            }
        };
        if candidates.is_empty() {
            return CurateOutcome::Unchanged;
        }

        // Ancestry exclusion: recalling a record already reachable from
        // this session's own path is a same-tree selection, not memory
        // (INTENT.md §5e).
        //
        // Exclusion is keyed on TRUE ancestry -- the `SessionMeta.origin`
        // parent chain -- and not merely on which sessions happen to appear
        // in `base.nodes()`. Those differ, and the difference is a real
        // §5e violation: `apply_context_mask` drops masked records from the
        // effective transcript BEFORE they become path nodes, so an ancestor
        // whose every contributing record was masked contributes no node,
        // would not appear in `base.nodes()`, and would be recalled as
        // though it were an unrelated session. That would both misreport a
        // same-tree selection as memory AND reintroduce, through a different
        // door, exactly the records an operator deliberately masked out.
        // The base-path scan is kept as well: it is cheap, and it also
        // covers a session contributing nodes through a prefix whose header
        // this walk would not otherwise visit.
        let mut excluded: BTreeSet<_> = base.nodes().map(|(node, _)| node.record.session).collect();
        excluded.insert(ctx.session_id);
        excluded.extend(self.ancestry_of(ctx, ctx.session_id).await);
        candidates.retain(|meta| !excluded.contains(&meta.id));
        if candidates.is_empty() {
            return CurateOutcome::Unchanged;
        }

        // Deterministic ordering (R4): oldest session first, ties broken by
        // session id so the order never depends on store-internal ordering.
        candidates.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));

        let mut ops: Vec<PathOp> = Vec::new();
        let mut foreign: BTreeMap<RecordRef, Arc<LogRecord>> = BTreeMap::new();
        let mut bytes_used: usize = 0;

        'sessions: for meta in &candidates {
            let mut records = match ctx.store.read(&meta.id, SeqRange::full()).await {
                Ok(records) => records,
                // A session that vanished (or a transient read error)
                // between `list` and `read` is skipped, not fatal -- other
                // candidates may still be recallable.
                Err(_) => continue,
            };
            // `SessionStore::read` documents NO ordering guarantee. Both
            // shipped stores happen to return append order (== seq order),
            // but relying on that would make R4's determinism claim rest on
            // an implementation accident: a future sharded or partial read
            // could return the same records in a different order, and the
            // cap-stopping walk below would then select a DIFFERENT prefix
            // from identical data -- a different `SelectionKey` each turn,
            // which is exactly the prefix-cache reuse this selection is
            // supposed to preserve. Sort explicitly, mirroring
            // `resolve_default_path`'s own defensive scan-by-max-seq in
            // `conway-runtime`'s `context/path.rs`, which refuses to rely on
            // read order for the same stated reason.
            records.sort_by_key(|rec| rec.seq());
            for rec in records {
                if !is_content_record(&rec) || carries_tool_block(&rec) {
                    continue;
                }
                let Some(seq) = rec.seq() else { continue };
                let size = record_byte_estimate(&rec);
                // `saturating_add`, not `+`: `record_byte_estimate` returns
                // `usize::MAX` for an unserializable record (failing closed),
                // and a plain add would overflow-panic in a debug build
                // rather than trip the cap it is meant to trip.
                if ops.len() >= self.config.max_records
                    || bytes_used.saturating_add(size) > self.config.max_bytes
                {
                    // R4: both caps are checked before any Include is
                    // added. Stopping the whole walk here (rather than
                    // skipping this one record and trying a later, smaller
                    // one) keeps the selection a deterministic prefix of
                    // the oldest-first, seq-ordered candidate stream.
                    break 'sessions;
                }
                let record_ref = RecordRef {
                    session: meta.id,
                    seq,
                };
                bytes_used += size;
                ops.push(PathOp::Include { node: record_ref });
                foreign.insert(record_ref, Arc::new(rec));
            }
        }

        if ops.is_empty() {
            return CurateOutcome::Unchanged;
        }

        match base.derive_with(
            &ops,
            &foreign,
            Selector::Plugin {
                id: PLUGIN_ID.to_string(),
                op: OpLabel::new("recall"),
            },
        ) {
            Ok(derivation) => CurateOutcome::Derived(derivation),
            Err(err) => CurateOutcome::Failed {
                reason: format!("derive_with refused the recalled selection: {err}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use conway_core::content::{StopReason, Usage};
    use conway_core::ids::{AgentId, LogSeq, SessionId};
    use conway_core::log::SessionMeta;
    use conway_core::path::{NodeProvenance, NodeStamp, PathNode};
    use conway_core::ports::{PluginConfig, SessionStore};
    use conway_core::provenance::Provenance;
    use conway_core::transcript::TranscriptResolver;
    use conway_testkit::FakeStore;

    fn agent() -> AgentId {
        AgentId::new()
    }

    fn ts(offset_secs: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::UNIX_EPOCH + chrono::Duration::seconds(offset_secs)
    }

    fn meta(id: SessionId, created_offset: i64, labels: Vec<String>) -> SessionMeta {
        SessionMeta {
            id,
            agent_id: agent(),
            origin: None,
            agent_def: None,
            role: None,
            created: ts(created_offset),
            cwd: PathBuf::from("/tmp"),
            labels,
            ephemeral: false,
            ask_origin: None,
            root: None,
            plugin_config: PluginConfig::default(),
        }
    }

    fn user_turn(seq: LogSeq, text: &str) -> LogRecord {
        LogRecord::UserTurn {
            seq,
            ts: ts(0),
            text: text.to_string(),
            prov: Provenance::UserPrompt,
        }
    }

    fn assistant_text(seq: LogSeq, text: &str) -> LogRecord {
        LogRecord::Assistant {
            seq,
            ts: ts(0),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            model: "anthropic/claude-sonnet-4-6".parse().unwrap(),
            route_reason: serde_json::json!({"AliasPrimary": {"alias": "coder"}}),
            usage: Usage::default(),
            stop: StopReason::EndTurn,
        }
    }

    fn assistant_tool_call(seq: LogSeq, call_id: &str) -> LogRecord {
        LogRecord::Assistant {
            seq,
            ts: ts(0),
            content: vec![ContentBlock::ToolUse {
                call_id: call_id.to_string(),
                name: conway_core::ids::ToolName::new("some_tool"),
                arguments: serde_json::json!({}),
            }],
            model: "anthropic/claude-sonnet-4-6".parse().unwrap(),
            route_reason: serde_json::json!({"AliasPrimary": {"alias": "coder"}}),
            usage: Usage::default(),
            stop: StopReason::ToolUse,
        }
    }

    fn tool_result(seq: LogSeq, call_id: &str) -> LogRecord {
        LogRecord::ToolResultRecord {
            seq,
            ts: ts(0),
            result: conway_core::content::ToolResult {
                call_id: call_id.to_string(),
                tool: conway_core::ids::ToolName::new("some_tool"),
                blocks: vec![ContentBlock::Text {
                    text: "tool output".to_string(),
                }],
                is_error: false,
                truncated: None,
            },
        }
    }

    fn empty_base() -> ValidatedPath {
        ValidatedPath::default_path(vec![])
    }

    fn base_with_own_session(session: SessionId, seq: LogSeq) -> ValidatedPath {
        ValidatedPath::default_path(vec![(
            PathNode {
                record: RecordRef { session, seq },
                stamp: NodeStamp::Head,
                prov: NodeProvenance {
                    selected_by: Selector::DefaultRule,
                    at: ts(0),
                },
            },
            Arc::new(user_turn(seq, "the calling session's own turn")),
        )])
    }

    fn ctx(store: Arc<FakeStore>, session_id: SessionId) -> CurateCtx {
        CurateCtx {
            agent_id: agent(),
            session_id,
            turn: 1,
            model: None,
            store: store as Arc<dyn SessionStore>,
            resolver: Arc::new(TranscriptResolver::new(64)),
        }
    }

    async fn seed(store: &FakeStore, id: SessionId, created_offset: i64, labels: Vec<String>) {
        store
            .create(meta(id, created_offset, labels))
            .await
            .unwrap();
    }

    async fn append(store: &FakeStore, id: SessionId, rec: LogRecord) -> LogSeq {
        store.append(&id, rec).await.unwrap()
    }

    /// Seed a session that is a real fork CHILD of `parent` — an actual
    /// ancestry link via `SessionMeta.origin`, not merely a shared record on
    /// some path.
    async fn seed_child_of(
        store: &FakeStore,
        id: SessionId,
        parent: SessionId,
        created_offset: i64,
        labels: Vec<String>,
    ) {
        let mut m = meta(id, created_offset, labels);
        m.origin = Some(conway_core::log::ForkOrigin {
            parent,
            at_seq: LogSeq(0),
            mode: conway_core::log::SubagentMode::Fork,
        });
        store.create(m).await.unwrap();
    }

    // ------------------------------------------------------------------
    // R6: Unchanged is the overwhelmingly common path.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn r6_no_labelled_session_yields_unchanged() {
        let store = Arc::new(FakeStore::new());
        let calling = SessionId::new();
        seed(&store, calling, 0, vec![]).await;
        // An unlabelled OTHER session exists, but is not recallable.
        let other = SessionId::new();
        seed(&store, other, -100, vec![]).await;

        let curator = MemoryCurator {
            config: MemoryConfig::default(),
        };
        let outcome = curator.curate(&ctx(store, calling), &empty_base()).await;
        assert!(matches!(outcome, CurateOutcome::Unchanged));
    }

    #[tokio::test]
    async fn r6_only_the_calling_sessions_own_ancestry_is_labelled_yields_unchanged() {
        let store = Arc::new(FakeStore::new());
        let calling = SessionId::new();
        seed(&store, calling, 0, vec![DEFAULT_LABEL.to_string()]).await;
        let seq = append(&store, calling, user_turn(LogSeq(0), "hi")).await;

        let curator = MemoryCurator {
            config: MemoryConfig::default(),
        };
        let base = base_with_own_session(calling, seq);
        let outcome = curator.curate(&ctx(store, calling), &base).await;
        assert!(
            matches!(outcome, CurateOutcome::Unchanged),
            "the calling session's own label must not recall itself"
        );
    }

    // ------------------------------------------------------------------
    // R1: label matching.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn r1_only_sessions_carrying_the_configured_label_are_recalled() {
        let store = Arc::new(FakeStore::new());
        let calling = SessionId::new();
        seed(&store, calling, 0, vec![]).await;

        let labelled = SessionId::new();
        seed(&store, labelled, -10, vec![DEFAULT_LABEL.to_string()]).await;
        append(&store, labelled, user_turn(LogSeq(0), "remembered content")).await;

        let unlabelled = SessionId::new();
        seed(&store, unlabelled, -20, vec![]).await;
        append(&store, unlabelled, user_turn(LogSeq(0), "not recallable")).await;

        let differently_labelled = SessionId::new();
        seed(&store, differently_labelled, -30, vec!["other".to_string()]).await;
        append(
            &store,
            differently_labelled,
            user_turn(LogSeq(0), "wrong label"),
        )
        .await;

        let curator = MemoryCurator {
            config: MemoryConfig::default(),
        };
        let outcome = curator.curate(&ctx(store, calling), &empty_base()).await;
        let derivation = match outcome {
            CurateOutcome::Derived(d) => d,
            other => panic!("expected Derived, got {other:?}"),
        };
        let session_ids: BTreeSet<SessionId> = derivation
            .path
            .nodes()
            .map(|(n, _)| n.record.session)
            .collect();
        assert_eq!(
            session_ids,
            BTreeSet::from([labelled]),
            "only the correctly-labelled session's records may be recalled"
        );
    }

    // ------------------------------------------------------------------
    // Ancestry exclusion.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn ancestry_a_session_already_on_the_base_path_is_excluded_even_if_labelled() {
        let store = Arc::new(FakeStore::new());
        let calling = SessionId::new();
        seed(&store, calling, 0, vec![]).await;

        // A session that IS labelled memory, but is already represented on
        // the base path (e.g. an ancestor via prefix inheritance).
        let ancestor = SessionId::new();
        seed(&store, ancestor, -10, vec![DEFAULT_LABEL.to_string()]).await;
        let ancestor_seq = append(&store, ancestor, user_turn(LogSeq(0), "ancestor turn")).await;

        let curator = MemoryCurator {
            config: MemoryConfig::default(),
        };
        let base = base_with_own_session(ancestor, ancestor_seq);
        let outcome = curator.curate(&ctx(store, calling), &base).await;
        assert!(
            matches!(outcome, CurateOutcome::Unchanged),
            "a session already represented on the base path must never be recalled again"
        );
    }

    /// A TRUE ancestor is excluded even when it contributes NO node to the
    /// base path.
    ///
    /// `apply_context_mask` drops masked records from the effective
    /// transcript before they ever become path nodes, so an ancestor whose
    /// every contributing record was masked shows up nowhere in
    /// `base.nodes()`. Excluding only what the base path shows would treat
    /// that ancestor as an unrelated session and recall it — reporting a
    /// same-tree selection as memory (an INTENT.md §5e violation) AND
    /// reintroducing, through a different door, exactly the records the
    /// operator masked out. Exclusion therefore walks the real
    /// `SessionMeta.origin` chain. Regression test for a review finding.
    #[tokio::test]
    async fn a_true_ancestor_is_excluded_even_when_it_contributes_no_base_node() {
        let store = Arc::new(FakeStore::new());

        // A labelled PARENT whose records are all masked out of the child's
        // effective transcript -- so it appears nowhere on the base path.
        let parent = SessionId::new();
        seed(&store, parent, -10, vec![DEFAULT_LABEL.to_string()]).await;
        append(&store, parent, user_turn(LogSeq(0), "masked ancestor turn")).await;

        // The calling session is a real fork child of that parent.
        let calling = SessionId::new();
        seed_child_of(&store, calling, parent, 0, vec![]).await;

        let curator = MemoryCurator {
            config: MemoryConfig::default(),
        };
        // An EMPTY base: the parent contributes no node (everything masked).
        let outcome = curator.curate(&ctx(store, calling), &empty_base()).await;
        assert!(
            matches!(outcome, CurateOutcome::Unchanged),
            "a true ancestor must be excluded by its origin chain, not merely \
             by whether it happens to appear on the base path"
        );
    }

    // ------------------------------------------------------------------
    // R4: bounded by construction.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn r4_max_records_caps_the_selection() {
        let store = Arc::new(FakeStore::new());
        let calling = SessionId::new();
        seed(&store, calling, 0, vec![]).await;

        let labelled = SessionId::new();
        seed(&store, labelled, -10, vec![DEFAULT_LABEL.to_string()]).await;
        for i in 0..5u64 {
            append(&store, labelled, user_turn(LogSeq(i), &format!("turn {i}"))).await;
        }

        let curator = MemoryCurator {
            config: MemoryConfig {
                max_records: 2,
                ..MemoryConfig::default()
            },
        };
        let outcome = curator.curate(&ctx(store, calling), &empty_base()).await;
        let derivation = match outcome {
            CurateOutcome::Derived(d) => d,
            other => panic!("expected Derived, got {other:?}"),
        };
        assert_eq!(
            derivation.path.nodes().count(),
            2,
            "max_records=2 must cap the recalled node count at 2"
        );
    }

    #[tokio::test]
    async fn r4_max_bytes_caps_the_selection() {
        let store = Arc::new(FakeStore::new());
        let calling = SessionId::new();
        seed(&store, calling, 0, vec![]).await;

        let labelled = SessionId::new();
        seed(&store, labelled, -10, vec![DEFAULT_LABEL.to_string()]).await;
        let first = user_turn(LogSeq(0), "short");
        let first_size = record_byte_estimate(&first);
        append(&store, labelled, first).await;
        // A second record whose own size alone exceeds a budget set to
        // "first record's size, plus a sliver" -- it must not fit.
        append(
            &store,
            labelled,
            user_turn(LogSeq(1), &"x".repeat(first_size + 1000)),
        )
        .await;

        let curator = MemoryCurator {
            config: MemoryConfig {
                max_bytes: first_size + 10,
                ..MemoryConfig::default()
            },
        };
        let outcome = curator.curate(&ctx(store, calling), &empty_base()).await;
        let derivation = match outcome {
            CurateOutcome::Derived(d) => d,
            other => panic!("expected Derived, got {other:?}"),
        };
        assert_eq!(
            derivation.path.nodes().count(),
            1,
            "the byte cap must stop the walk before the oversized second record"
        );
    }

    // ------------------------------------------------------------------
    // R5: both halves or neither.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn r5_a_tool_call_pair_is_never_recalled_and_never_orphaned() {
        let store = Arc::new(FakeStore::new());
        let calling = SessionId::new();
        seed(&store, calling, 0, vec![]).await;

        let labelled = SessionId::new();
        seed(&store, labelled, -10, vec![DEFAULT_LABEL.to_string()]).await;
        // A plain text record (recallable) plus a tool-call/result pair
        // (must be excluded, per R5).
        append(
            &store,
            labelled,
            user_turn(LogSeq(0), "plain text, recallable"),
        )
        .await;
        append(&store, labelled, assistant_tool_call(LogSeq(1), "tc_1")).await;
        append(&store, labelled, tool_result(LogSeq(2), "tc_1")).await;

        let curator = MemoryCurator {
            config: MemoryConfig::default(),
        };
        let outcome = curator.curate(&ctx(store, calling), &empty_base()).await;
        let derivation = match outcome {
            CurateOutcome::Derived(d) => d,
            other => panic!("expected Derived, got {other:?}"),
        };
        let seqs: Vec<LogSeq> = derivation.path.nodes().map(|(n, _)| n.record.seq).collect();
        assert_eq!(
            seqs,
            vec![LogSeq(0)],
            "only the plain-text record may be recalled; the tool-call pair must be skipped whole"
        );
        // No ToolUse content block reached the derived path at all -- the
        // property R5 exists to guarantee (an unanswered ToolUse is what
        // would make the derived path unsendable).
        for (_, record) in derivation.path.nodes() {
            if let LogRecord::Assistant { content, .. } = record.as_ref() {
                assert!(
                    !content
                        .iter()
                        .any(|b| matches!(b, ContentBlock::ToolUse { .. })),
                    "no recalled record may carry an unanswered ToolUse"
                );
            }
            assert!(
                !matches!(record.as_ref(), LogRecord::ToolResultRecord { .. }),
                "no recalled record may carry a tool result without its call"
            );
        }
    }

    #[tokio::test]
    async fn r5_a_plain_assistant_text_record_with_no_tool_block_is_recallable() {
        // The positive complement to `r5_a_tool_call_pair_is_never_recalled`:
        // an `Assistant` record whose `content` carries only `Text` (no
        // `ToolUse`) is a content record with no tool half at all, and MUST
        // still be recalled -- R5 excludes tool-call pairs specifically, not
        // `Assistant` records in general.
        let store = Arc::new(FakeStore::new());
        let calling = SessionId::new();
        seed(&store, calling, 0, vec![]).await;

        let labelled = SessionId::new();
        seed(&store, labelled, -10, vec![DEFAULT_LABEL.to_string()]).await;
        append(
            &store,
            labelled,
            assistant_text(LogSeq(0), "a remembered lesson"),
        )
        .await;

        let curator = MemoryCurator {
            config: MemoryConfig::default(),
        };
        let outcome = curator.curate(&ctx(store, calling), &empty_base()).await;
        let derivation = match outcome {
            CurateOutcome::Derived(d) => d,
            other => panic!("expected Derived, got {other:?}"),
        };
        let (_, record) = derivation.path.nodes().next().expect("one node");
        match record.as_ref() {
            LogRecord::Assistant { content, .. } => {
                assert_eq!(
                    content,
                    &vec![ContentBlock::Text {
                        text: "a remembered lesson".to_string()
                    }]
                );
            }
            other => panic!("expected Assistant, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn r5_a_pure_tool_call_session_yields_unchanged() {
        let store = Arc::new(FakeStore::new());
        let calling = SessionId::new();
        seed(&store, calling, 0, vec![]).await;

        let labelled = SessionId::new();
        seed(&store, labelled, -10, vec![DEFAULT_LABEL.to_string()]).await;
        append(&store, labelled, assistant_tool_call(LogSeq(0), "tc_1")).await;
        append(&store, labelled, tool_result(LogSeq(1), "tc_1")).await;

        let curator = MemoryCurator {
            config: MemoryConfig::default(),
        };
        let outcome = curator.curate(&ctx(store, calling), &empty_base()).await;
        assert!(
            matches!(outcome, CurateOutcome::Unchanged),
            "a session with nothing but a tool-call pair has nothing recallable"
        );
    }

    // ------------------------------------------------------------------
    // Deterministic ordering.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn oldest_session_first_ordering_is_deterministic() {
        let store = Arc::new(FakeStore::new());
        let calling = SessionId::new();
        seed(&store, calling, 0, vec![]).await;

        let newer = SessionId::new();
        seed(&store, newer, -5, vec![DEFAULT_LABEL.to_string()]).await;
        append(&store, newer, user_turn(LogSeq(0), "newer")).await;

        let older = SessionId::new();
        seed(&store, older, -50, vec![DEFAULT_LABEL.to_string()]).await;
        append(&store, older, user_turn(LogSeq(0), "older")).await;

        let curator = MemoryCurator {
            config: MemoryConfig {
                max_records: 1,
                ..MemoryConfig::default()
            },
        };
        let outcome = curator.curate(&ctx(store, calling), &empty_base()).await;
        let derivation = match outcome {
            CurateOutcome::Derived(d) => d,
            other => panic!("expected Derived, got {other:?}"),
        };
        let (node, record) = derivation.path.nodes().next().expect("one node");
        assert_eq!(node.record.session, older, "the OLDER session comes first");
        match record.as_ref() {
            LogRecord::UserTurn { text, .. } => assert_eq!(text, "older"),
            other => panic!("expected UserTurn, got {other:?}"),
        }
    }

    // ------------------------------------------------------------------
    // Provenance.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn recalled_nodes_are_stamped_with_this_plugins_selector() {
        let store = Arc::new(FakeStore::new());
        let calling = SessionId::new();
        seed(&store, calling, 0, vec![]).await;

        let labelled = SessionId::new();
        seed(&store, labelled, -10, vec![DEFAULT_LABEL.to_string()]).await;
        append(&store, labelled, user_turn(LogSeq(0), "remembered")).await;

        let curator = MemoryCurator {
            config: MemoryConfig::default(),
        };
        let outcome = curator.curate(&ctx(store, calling), &empty_base()).await;
        let derivation = match outcome {
            CurateOutcome::Derived(d) => d,
            other => panic!("expected Derived, got {other:?}"),
        };
        let (node, _) = derivation.path.nodes().next().expect("one node");
        assert_eq!(
            node.prov.selected_by,
            Selector::Plugin {
                id: PLUGIN_ID.to_string(),
                op: OpLabel::new("recall"),
            }
        );
        assert_eq!(node.stamp, NodeStamp::Own);
    }

    // ------------------------------------------------------------------
    // Plugin surface.
    // ------------------------------------------------------------------

    #[test]
    fn manifest_id_matches_the_published_constant() {
        let plugin = MemoryPlugin::new(MemoryConfig::default());
        assert_eq!(plugin.manifest().id, PLUGIN_ID);
        assert!(plugin.tools().is_empty());
        assert_eq!(plugin.curators().len(), 1);
    }
}
