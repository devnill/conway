//! `resolve_default_path`: the §2.8 tolerant constructor's orchestrator
//! (DESIGN §2.5, §5, §6).
//!
//! This module reads an owning session's log, finds the HEAD (the latest
//! `ContextPathSet` record), and assembles the default path — either the
//! whole effective transcript (no head) or `expand(selection)` `++` own
//! records from `covers_upto` (head exists). The result is a
//! [`ValidatedPath`] built via [`ValidatedPath::default_path`], which runs
//! the coherence validator in DECLARE mode (orphans are tolerated and
//! recorded as `HarnessDrop`, never refused — DESIGN §4.1).
//!
//! **Not wired into the runtime in D1-3c.** This module exists in isolation
//! with its own tests; D1-3d connects it to the agent loop.
//!
//! # T4 and the prefix expansion
//!
//! The prefix-chain expansion + record resolution logic (expand the
//! `PathSelection`'s prefix chain via the `PathStore`, flatten root-first,
//! resolve each node's record via `TranscriptResolver::resolve_prefix`) is
//! the same logic `conway_session::resolver::resolve_path` implements. The
//! runtime cannot depend on `conway-session` (T4: `conway-runtime` depends on
//! `conway-core` only), so this module inlines the expansion rather than
//! calling across the crate boundary. A future item can deduplicate by
//! moving `resolve_path` into `conway-core` (all its building blocks are
//! already there); until then, the two implementations are deliberately
//! parallel and the test suite here mirrors the one in
//! `conway-session/src/resolver.rs`.

use std::sync::Arc;

use chrono::Utc;

use conway_core::error::{PathStoreError, StoreError};
use conway_core::ids::{LogSeq, SeqRange, SessionId};
use conway_core::log::LogRecord;
use conway_core::path::{
    NodeProvenance, NodeStamp, PathError, PathNode, PathSelection, RecordRef, ResolvedPath,
    Selector, ValidatedPath,
};
use conway_core::ports::{PathStore, SessionStore};
use conway_core::transcript::{TranscriptResolver, MAX_ANCESTRY_DEPTH};

/// Resolve the default path for `session` (DESIGN §2.5, §5, §6).
///
/// # Steps
///
/// 1. **Read the owning session's own log** — `SessionStore::read` with a
///    full range.
/// 2. **Find the HEAD** — the latest `ContextPathSet` record by greatest
///    `seq`. Absence of any `ContextPathSet` means the default path (DESIGN
///    §6).
/// 3. **If no head**: the default path is the whole effective transcript.
///    `TranscriptResolver::resolve` returns `Arc<[LogRecord]>`; each content
///    record is zipped into a `PathNode` with `Head` for the first, `Own` for
///    the rest.
///
///    **Identity scope — root sessions only.** This IS byte-identical to
///    today's behaviour for a ROOT session (the whole transcript is the
///    session's own; every `RecordRef { session: root, seq }` is correct).
///    It is NOT byte-identical for a FORK CHILD without a head, in two ways.
///    First, **RecordRef mis-attribution**: `build_transcript_nodes` stamps
///    every record's `RecordRef.session` with the child session, so the
///    parent's records (present in the child's effective transcript via
///    `resolve`) get `RecordRef { child, seq }` — a wrong pointer that would
///    re-resolve to the child's own record at that seq, not the parent's;
///    today's runtime attributes them to the parent (`InheritedPrefix.records
///    = resolve_prefix(&parent, at_seq)`, `runtime/root.rs:728-745`). Second,
///    **Stamp**: the parent's records get `Head`/`Own`, not
///    `Inherited { from: parent }`. Both follow from one root cause: a
///    flattened `Arc<[LogRecord]>` carries no per-record session id, so
///    ancestry cannot be recovered here. Because `NodeStamp` AND `RecordRef`
///    are hashed into `SelectionKey`, and the stamp selects the segment-mapping
///    function (`Inherited` → `record_role_and_content` + `Provenance::Inherited`;
///    `Own` → `own_segment` + a different `Provenance`), this would change
///    segment provenance, `prefix_key`, and wire bytes for fork children.
///    **D1-3d must resolve this before wiring** — either synthesize a default
///    head at fork time, or have this branch walk ancestry per-record via
///    `resolve_prefix(&parent, origin.at_seq)` to split inherited/own,
///    attribute `RecordRef`s to their owning sessions, and stamp `Inherited`.
///    This module is NOT wired into the runtime in D1-3c, so no break today;
///    the test `fork_child_no_head_pins_divergent_stamps` documents the
///    current contract so D1-3d inherits it consciously.
/// 4. **If a head exists**: assembly = `expand(selection)` `++` own records
///    from `covers_upto`. The head's `selection` is looked up in the
///    `PathStore`, its prefix chain is expanded and resolved, and the prefix
///    nodes are re-stamped `Inherited { from: immediate_parent }`. Own
///    records (`read(session, covers_upto..)`) are stamped `Head` for the
///    first, `Own` for the rest.
///
///    **The `covers_upto` gap.** `SeqRange::new(covers_upto, None)` is
///    inclusive at `covers_upto`, so own records are `seq >= covers_upto`.
///    Records with `selection_last_seq < seq < covers_upto` are in NEITHER
///    the frozen selection NOR the own tail — they are silently dropped
///    (the literal DESIGN §2.5 "own records from `covers_upto`" semantic;
///    see `head_covers_upto_excludes_early_own_records`). A well-formed
///    head keeps `covers_upto` consistent with the selection's extent
///    (`covers_upto == selection_last_seq + 1`); D1-3d's head-writer must
///    enforce that, or explicitly justify a skip — otherwise records vanish
///    with no `HarnessDrop`.
/// 5. **Call `ValidatedPath::default_path(nodes)`** — runs the coherence
///    validator in DECLARE mode, recording harness-caused incoherence rather
///    than refusing it.
///
/// # Content vs metadata records
///
/// Only *content* records (UserTurn, Assistant, ToolResultRecord,
/// ForkDirective, ParentSteer, SystemNote, ChildResultRecord) become path
/// nodes. Metadata records (Header, AgentResultRecord, ContextReportRecord,
/// ContextMask, ContextPathSet, ContextPathNamed) are skipped — they are
/// timeline events, not prompt content, matching `record_role_and_content`'s
/// own filtering in `context/builder.rs`.
///
/// # The `Inherited { from }` stamp
///
/// For the "head exists" case, all prefix nodes get a single `from` =
/// `meta.origin.parent` (the immediate parent — "who handed me this context",
/// matching `InheritedPrefix::from`'s semantic, DESIGN §3). If the session
/// is a root (no origin) but has a head with prefix nodes, the fallback is
/// the first prefix node's `record.session` — the closest available
/// representation of the prefix's origin. See `subagent.rs`'s module doc
/// ("`InheritedPrefix::from` at fork depth >= 2") for why a single `from`
/// rather than per-record origin tracking.
pub async fn resolve_default_path<S, P>(
    resolver: &TranscriptResolver,
    session_store: &S,
    path_store: &P,
    session: &SessionId,
) -> Result<ValidatedPath, PathError>
where
    S: SessionStore + ?Sized,
    P: PathStore + ?Sized,
{
    // 1. Read the owning session's own log (full range, including the head
    // record we scan for in step 2).
    let own_log = session_store
        .read(session, SeqRange::full())
        .await
        .map_err(store_err_to_path)?;

    // 2. Find the HEAD = the ContextPathSet record with the greatest `seq`
    //    (the latest head). `SessionStore::read` documents no ordering
    //    guarantee, so scan by max `seq` rather than relying on the vec being
    //    seq-ordered (`FakeStore`/`JsonlSessionStore` happen to return
    //    append-order == seq order, but a future sharded/partial read need not).
    let head = own_log
        .iter()
        .filter_map(|r| match r {
            LogRecord::ContextPathSet {
                seq,
                selection,
                covers_upto,
                ..
            } => Some((*seq, selection.clone(), *covers_upto)),
            _ => None,
        })
        .max_by_key(|(seq, _, _)| *seq)
        .map(|(_, selection, covers_upto)| (selection, covers_upto));

    // Session meta for the immediate parent (the `Inherited { from }`
    // value). Fetched once; both branches may use it.
    let meta = session_store
        .meta(session)
        .await
        .map_err(store_err_to_path)?;
    let immediate_parent = meta.origin.map(|o| o.parent);

    match head {
        None => {
            // 3. No head: default path = whole effective transcript.
            let transcript = resolver
                .resolve(session_store, session)
                .await
                .map_err(store_err_to_path)?;
            let nodes = build_transcript_nodes(&transcript, session);
            Ok(ValidatedPath::default_path(nodes))
        }
        Some((selection_key, covers_upto)) => {
            // 4. Head exists: assembly = expand(selection) ++ own records
            // from covers_upto.
            let selection = path_store.get(&selection_key).await.map_err(|e| {
                let record = RecordRef {
                    session: *session,
                    seq: LogSeq(0),
                };
                match e {
                    PathStoreError::NotFound { key } => PathError::UnresolvableNode {
                        record,
                        detail: format!("head selection {key} not found in path store"),
                    },
                    other => PathError::UnresolvableNode {
                        record,
                        detail: format!("path store error: {other}"),
                    },
                }
            })?;

            // Expand the prefix chain and resolve records. Inlined here
            // (see module doc — T4 prevents depending on conway-session's
            // `resolve_path`).
            let prefix =
                expand_and_resolve(resolver, session_store, path_store, &selection).await?;

            // The `from` for all Inherited-stamped prefix nodes.
            let from = immediate_parent.unwrap_or_else(|| {
                prefix
                    .nodes
                    .first()
                    .map(|(n, _)| n.record.session)
                    .unwrap_or(SessionId::new())
            });

            let mut nodes: Vec<(PathNode, Arc<LogRecord>)> = prefix
                .nodes
                .into_iter()
                .map(|(mut node, record)| {
                    node.stamp = NodeStamp::Inherited { from };
                    (node, record)
                })
                .collect();

            // Own records from covers_upto to head.
            let own_records = session_store
                .read(session, SeqRange::new(covers_upto, None))
                .await
                .map_err(store_err_to_path)?;

            let mut own_first = true;
            for record in own_records {
                if !is_content_record(&record) {
                    continue;
                }
                let seq = record
                    .seq()
                    .expect("content records always carry a seq (Header never reaches here)");
                let stamp = if own_first {
                    own_first = false;
                    NodeStamp::Head
                } else {
                    NodeStamp::Own
                };
                let node = PathNode {
                    record: RecordRef {
                        session: *session,
                        seq,
                    },
                    stamp,
                    prov: NodeProvenance {
                        selected_by: Selector::DefaultRule,
                        at: Utc::now(),
                    },
                };
                nodes.push((node, Arc::new(record)));
            }

            Ok(ValidatedPath::default_path(nodes))
        }
    }
}

/// Build path nodes from the full effective transcript (the "no head" case):
/// `Head` for the first content record, `Own` for the rest.
///
/// **Fork-child divergence (D1-3d).** Every record is stamped `Head`/`Own` —
/// none `Inherited` — AND every record's `RecordRef.session` is set to the
/// owning (child) session, because a flattened `Arc<[LogRecord]>` carries no
/// per-record session id, so ancestry cannot be recovered here. For a root
/// session this is byte-identical to today; for a fork child it is NOT (today
/// attributes the parent's records to the parent and stamps them `Inherited {
/// from: parent }`). See `resolve_default_path`'s step-3 doc for the D1-3d
/// resolution; `fork_child_no_head_pins_divergent_stamps` pins the current
/// contract.
fn build_transcript_nodes(
    transcript: &Arc<[LogRecord]>,
    session: &SessionId,
) -> Vec<(PathNode, Arc<LogRecord>)> {
    let mut nodes = Vec::with_capacity(transcript.len());
    let mut first = true;
    for record in transcript.iter() {
        if !is_content_record(record) {
            continue;
        }
        let seq = record
            .seq()
            .expect("content records always carry a seq (Header never reaches here)");
        let stamp = if first {
            first = false;
            NodeStamp::Head
        } else {
            NodeStamp::Own
        };
        let node = PathNode {
            record: RecordRef {
                session: *session,
                seq,
            },
            stamp,
            prov: NodeProvenance {
                selected_by: Selector::DefaultRule,
                at: Utc::now(),
            },
        };
        nodes.push((node, Arc::new(record.clone())));
    }
    nodes
}

/// Expand a selection's prefix chain, flatten the node list, and resolve each
/// node's record — producing a [`ResolvedPath`] (DESIGN §3).
///
/// Mirrors `conway_session::resolver::resolve_path` exactly (see the module
/// doc for why this is inlined rather than imported). The flattened list =
/// prefix-chain nodes (root-first) `++` selection.nodes.
async fn expand_and_resolve<S, P>(
    resolver: &TranscriptResolver,
    session_store: &S,
    path_store: &P,
    selection: &PathSelection,
) -> Result<ResolvedPath, PathError>
where
    S: SessionStore + ?Sized,
    P: PathStore + ?Sized,
{
    // 1. Expand the prefix chain, collecting selections root-first.
    let expanded_nodes = expand_prefix_chain(path_store, selection).await?;

    // 2. Resolve each node's record via the memoised transcript resolver.
    let nodes = resolve_records(resolver, session_store, expanded_nodes).await?;

    Ok(ResolvedPath { nodes })
}

/// Walk the prefix chain upward via the `PathStore`, then flatten root-first.
/// Mirrors `conway_session::resolver::expand_prefix_chain`.
async fn expand_prefix_chain<P: PathStore + ?Sized>(
    path_store: &P,
    selection: &PathSelection,
) -> Result<Vec<PathNode>, PathError> {
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
        chain.push(current.clone());
        depth += 1;
        if depth > MAX_ANCESTRY_DEPTH {
            return Err(PathError::PrefixChainTooDeep);
        }
        current = path_store.get(&prefix_key).await.map_err(|e| {
            let record = current
                .nodes
                .first()
                .map(|n| n.record)
                .unwrap_or(RecordRef {
                    session: SessionId::new(),
                    seq: LogSeq(0),
                });
            match e {
                PathStoreError::NotFound { key } => PathError::UnresolvableNode {
                    record,
                    detail: format!("prefix selection {key} not found in path store"),
                },
                PathStoreError::PrefixChainTooDeep { .. } => PathError::PrefixChainTooDeep,
                other => PathError::UnresolvableNode {
                    record,
                    detail: format!("path store error: {other}"),
                },
            }
        })?;
    }
    // `chain` is [selection, prefix1, ..., root]; flatten root-first.
    chain.reverse();
    let mut expanded: Vec<PathNode> = Vec::new();
    for sel in &chain {
        expanded.extend(sel.nodes.iter().cloned());
    }
    Ok(expanded)
}

/// Resolve each node's `RecordRef` to its `Arc<LogRecord>` via the memoised
/// `TranscriptResolver::resolve_prefix`. Mirrors
/// `conway_session::resolver::resolve_records`.
async fn resolve_records<S: SessionStore + ?Sized>(
    resolver: &TranscriptResolver,
    session_store: &S,
    nodes: Vec<PathNode>,
) -> Result<Vec<(PathNode, Arc<LogRecord>)>, PathError> {
    let mut out: Vec<(PathNode, Arc<LogRecord>)> = Vec::with_capacity(nodes.len());
    for node in nodes {
        let upto = node.record.seq.succ();
        let transcript = resolver
            .resolve_prefix(session_store, &node.record.session, upto)
            .await
            .map_err(|e| match e {
                StoreError::NotFound { session } => PathError::UnresolvableNode {
                    record: node.record,
                    detail: format!("session {session} not found"),
                },
                StoreError::SeqOutOfRange { requested, head } => PathError::UnresolvableNode {
                    record: node.record,
                    detail: format!("seq {requested} out of range (head {head})"),
                },
                other => PathError::UnresolvableNode {
                    record: node.record,
                    detail: format!("store error resolving session: {other}"),
                },
            })?;
        let record = transcript
            .last()
            .filter(|r| r.seq() == Some(node.record.seq))
            .ok_or_else(|| PathError::UnresolvableNode {
                record: node.record,
                detail: format!(
                    "record at seq {} not found in session {}'s effective transcript (masked?)",
                    node.record.seq, node.record.session
                ),
            })?;
        out.push((node, Arc::new(record.clone())));
    }
    Ok(out)
}

/// Whether a record is a *content* record (one that becomes a path node) vs a
/// *metadata* record (Header, AgentResultRecord, ContextReportRecord,
/// ContextMask, ContextPathSet, ContextPathNamed). Matches
/// `record_role_and_content`'s filtering in `context/builder.rs`.
fn is_content_record(record: &LogRecord) -> bool {
    matches!(
        record,
        LogRecord::UserTurn { .. }
            | LogRecord::Assistant { .. }
            | LogRecord::ToolResultRecord { .. }
            | LogRecord::ForkDirective { .. }
            | LogRecord::ParentSteer { .. }
            | LogRecord::SystemNote { .. }
            | LogRecord::ChildResultRecord { .. }
    )
}

/// Translate a `StoreError` into the closest `PathError`. Session-log reads
/// that fail (session not found, seq out of range) surface as
/// `UnresolvableNode` at this seam, the same translation
/// `conway_session::resolver` uses.
fn store_err_to_path(e: StoreError) -> PathError {
    match e {
        StoreError::NotFound { session } => PathError::UnresolvableNode {
            record: RecordRef {
                session,
                seq: LogSeq(0),
            },
            detail: format!("session {session} not found"),
        },
        StoreError::SeqOutOfRange { requested, head } => PathError::UnresolvableNode {
            record: RecordRef {
                session: SessionId::new(),
                seq: requested,
            },
            detail: format!("seq {requested} out of range (head {head})"),
        },
        other => PathError::UnresolvableNode {
            record: RecordRef {
                session: SessionId::new(),
                seq: LogSeq(0),
            },
            detail: format!("store error: {other}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use conway_core::content::{StopReason, Usage};
    use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef};
    use conway_core::log::SessionMeta;
    use conway_core::path::SelectionKey;
    use conway_core::ports::PluginConfig;
    use conway_testkit::FakeStore;

    fn ts() -> chrono::DateTime<chrono::Utc> {
        Utc::now()
    }

    fn user_turn(seq: u64, text: &str) -> LogRecord {
        LogRecord::UserTurn {
            seq: LogSeq(seq),
            ts: ts(),
            text: text.into(),
            prov: conway_core::provenance::Provenance::UserPrompt,
        }
    }

    fn assistant_turn(seq: u64) -> LogRecord {
        LogRecord::Assistant {
            seq: LogSeq(seq),
            ts: ts(),
            content: vec![],
            model: ModelRef {
                backend: BackendId::new("test"),
                model: ModelId::new("test-model"),
            },
            route_reason: serde_json::json!({}),
            usage: Usage::default(),
            stop: StopReason::EndTurn,
        }
    }

    fn context_path_set(seq: u64, selection: SelectionKey, covers_upto: u64) -> LogRecord {
        LogRecord::ContextPathSet {
            seq: LogSeq(seq),
            ts: ts(),
            selection,
            covers_upto: LogSeq(covers_upto),
        }
    }

    fn make_meta(session: SessionId, origin: Option<conway_core::log::ForkOrigin>) -> SessionMeta {
        SessionMeta {
            id: session,
            agent_id: AgentId::new(),
            origin,
            agent_def: None,
            role: None,
            created: ts(),
            cwd: std::path::PathBuf::from("/tmp"),
            labels: vec![],
            ephemeral: false,
            ask_origin: None,
            root: None,
            plugin_config: PluginConfig::default(),
        }
    }

    async fn make_session(store: &FakeStore, meta: SessionMeta, records: Vec<LogRecord>) {
        let sid = meta.id;
        store.create(meta).await.unwrap();
        for rec in records {
            store.append(&sid, rec).await.unwrap();
        }
    }

    /// A minimal in-memory `PathStore` for tests (same shape as the one in
    /// `conway_session::resolver`'s tests).
    #[derive(Debug, Default)]
    struct MemPathStore {
        selections: std::sync::RwLock<HashMap<SelectionKey, PathSelection>>,
    }

    impl MemPathStore {
        fn insert(&self, key: SelectionKey, sel: PathSelection) {
            self.selections.write().unwrap().insert(key, sel);
        }
    }

    #[async_trait::async_trait]
    impl PathStore for MemPathStore {
        async fn put(&self, selection: PathSelection) -> Result<SelectionKey, PathStoreError> {
            let nodes: Vec<PathNode> = selection.nodes.clone();
            let key = SelectionKey::from_nodes(&nodes);
            self.selections
                .write()
                .unwrap()
                .insert(key.clone(), selection);
            Ok(key)
        }

        async fn get(&self, key: &SelectionKey) -> Result<PathSelection, PathStoreError> {
            self.selections
                .read()
                .unwrap()
                .get(key)
                .cloned()
                .ok_or_else(|| PathStoreError::NotFound { key: key.clone() })
        }

        async fn selections_referencing(
            &self,
            _sid: &SessionId,
        ) -> Result<Vec<SelectionKey>, PathStoreError> {
            Ok(Vec::new())
        }
    }

    fn own_node(session: SessionId, seq: u64) -> PathNode {
        PathNode {
            record: RecordRef {
                session,
                seq: LogSeq(seq),
            },
            stamp: NodeStamp::Own,
            prov: NodeProvenance {
                selected_by: Selector::DefaultRule,
                at: ts(),
            },
        }
    }

    /// (a) A root session with no `ContextPathSet` — the default path is the
    /// whole effective transcript, stamped `Head`/`Own`.
    #[tokio::test]
    async fn root_session_no_head_is_whole_transcript() {
        let store = FakeStore::new();
        let path_store = MemPathStore::default();
        let resolver = TranscriptResolver::new(64);

        let s = SessionId::new();
        make_session(
            &store,
            make_meta(s, None),
            vec![
                user_turn(0, "hello"),
                assistant_turn(1),
                user_turn(2, "again"),
            ],
        )
        .await;

        let path = resolve_default_path(&resolver, &store, &path_store, &s)
            .await
            .unwrap();

        // Three content records, all included.
        let nodes: Vec<_> = path.nodes().collect();
        assert_eq!(nodes.len(), 3);
        // First is Head, rest are Own.
        assert_eq!(nodes[0].0.stamp, NodeStamp::Head);
        assert_eq!(nodes[1].0.stamp, NodeStamp::Own);
        assert_eq!(nodes[2].0.stamp, NodeStamp::Own);
        // Records match.
        assert_eq!(nodes[0].1.seq(), Some(LogSeq(0)));
        assert_eq!(nodes[1].1.seq(), Some(LogSeq(1)));
        assert_eq!(nodes[2].1.seq(), Some(LogSeq(2)));
        // No incoherence (coherent transcript).
        assert!(path.incoherence().is_empty());
    }

    /// (b) A fork child with a head — prefix nodes get `Inherited { from:
    /// parent }`, own records get `Head`/`Own`.
    #[tokio::test]
    async fn fork_child_with_head_prefixes_inherited_own_head() {
        let store = FakeStore::new();
        let path_store = MemPathStore::default();
        let resolver = TranscriptResolver::new(64);

        let parent = SessionId::new();
        let child = SessionId::new();

        // Parent has one user turn.
        make_session(
            &store,
            make_meta(parent, None),
            vec![user_turn(0, "parent")],
        )
        .await;

        // Store a selection over the parent's record.
        let prefix_sel = PathSelection {
            prefix: None,
            nodes: vec![own_node(parent, 0)],
            incoherence: vec![],
        };
        let prefix_key = SelectionKey::from_nodes(&prefix_sel.nodes);
        path_store.insert(prefix_key.clone(), prefix_sel);

        // Child forks from parent at at_seq=1 (after parent's record 0).
        let origin = conway_core::log::ForkOrigin {
            parent,
            at_seq: LogSeq(1),
            mode: conway_core::log::SubagentMode::Fork,
        };
        make_session(
            &store,
            make_meta(child, Some(origin)),
            vec![
                user_turn(0, "child turn 1"),
                user_turn(1, "child turn 2"),
                context_path_set(2, prefix_key, LogSeq(0).0),
            ],
        )
        .await;

        let path = resolve_default_path(&resolver, &store, &path_store, &child)
            .await
            .unwrap();

        let nodes: Vec<_> = path.nodes().collect();
        // prefix (1 node) ++ own (2 records, covers_upto=0 → own from seq 0)
        assert_eq!(nodes.len(), 3);
        // Prefix node: Inherited { from: parent }.
        assert_eq!(nodes[0].0.stamp, NodeStamp::Inherited { from: parent });
        assert_eq!(
            nodes[0].0.record,
            RecordRef {
                session: parent,
                seq: LogSeq(0)
            }
        );
        // First own: Head.
        assert_eq!(nodes[1].0.stamp, NodeStamp::Head);
        assert_eq!(
            nodes[1].0.record,
            RecordRef {
                session: child,
                seq: LogSeq(0)
            }
        );
        // Second own: Own.
        assert_eq!(nodes[2].0.stamp, NodeStamp::Own);
        assert_eq!(
            nodes[2].0.record,
            RecordRef {
                session: child,
                seq: LogSeq(1)
            }
        );
    }

    /// (c) The head's `covers_upto` excludes records — own records start from
    /// `covers_upto`, not from seq 0.
    #[tokio::test]
    async fn head_covers_upto_excludes_early_own_records() {
        let store = FakeStore::new();
        let path_store = MemPathStore::default();
        let resolver = TranscriptResolver::new(64);

        let s = SessionId::new();
        make_session(
            &store,
            make_meta(s, None),
            vec![
                user_turn(0, "first"),
                user_turn(1, "second"),
                user_turn(2, "third"),
                user_turn(3, "fourth"),
            ],
        )
        .await;

        // Store a selection with no prefix and one node (s/0).
        let sel = PathSelection {
            prefix: None,
            nodes: vec![own_node(s, 0)],
            incoherence: vec![],
        };
        let sel_key = SelectionKey::from_nodes(&sel.nodes);
        path_store.insert(sel_key.clone(), sel);

        // Append a ContextPathSet at seq 4, covering up to seq 2.
        store
            .append(&s, context_path_set(4, sel_key, 2))
            .await
            .unwrap();

        let path = resolve_default_path(&resolver, &store, &path_store, &s)
            .await
            .unwrap();

        let nodes: Vec<_> = path.nodes().collect();
        // prefix (1 node: s/0) ++ own from covers_upto=2 (records 2, 3).
        // Record 1 is between the prefix and covers_upto — excluded.
        assert_eq!(nodes.len(), 3);
        // Prefix: s/0.
        assert_eq!(
            nodes[0].0.record,
            RecordRef {
                session: s,
                seq: LogSeq(0)
            }
        );
        // Own: s/2 (Head), s/3 (Own). Record 1 is skipped.
        assert_eq!(
            nodes[1].0.record,
            RecordRef {
                session: s,
                seq: LogSeq(2)
            }
        );
        assert_eq!(nodes[1].0.stamp, NodeStamp::Head);
        assert_eq!(
            nodes[2].0.record,
            RecordRef {
                session: s,
                seq: LogSeq(3)
            }
        );
        assert_eq!(nodes[2].0.stamp, NodeStamp::Own);
    }

    /// (d) **D1-3d carryover — a fork child with NO head pins the current
    /// divergent stamps.** Today's runtime splits a fork child's effective
    /// transcript into the parent's portion (`InheritedPrefix { from: parent,
    /// records: resolve_prefix(&parent, origin.at_seq) }`, stamped
    /// `Inherited { from: parent }`) and the child's own (Head/Own) — see
    /// `runtime/root.rs:728-745`. `resolve_default_path`'s no-head branch
    /// cannot recover per-record ancestry from a flattened `Arc<[LogRecord]>`
    /// (it carries no session id), so it stamps EVERY record `Head`/`Own` and
    /// attributes EVERY `RecordRef.session` to the child — divergent from today
    /// for the parent's records. This test PINS that divergent behaviour so
    /// D1-3d inherits a documented contract, not a silent one: when D1-3d
    /// makes the no-head fork-child case byte-identical to today (by walking
    /// ancestry per-record via `resolve_prefix(&parent, origin.at_seq)` or
    /// synthesizing a default head), this test will FAIL and must be updated
    /// to assert `Inherited { from: parent }` on the parent's record and
    /// `RecordRef { session: parent, .. }`.
    #[tokio::test]
    async fn fork_child_no_head_pins_divergent_stamps() {
        let store = FakeStore::new();
        let path_store = MemPathStore::default();
        let resolver = TranscriptResolver::new(64);

        let parent = SessionId::new();
        let child = SessionId::new();
        // Parent has one user turn (seq 0).
        make_session(
            &store,
            make_meta(parent, None),
            vec![user_turn(0, "parent")],
        )
        .await;
        // Child forks from parent at at_seq=1 (after parent's record 0), has
        // one own turn (seq 0 in the child's log), and NO ContextPathSet.
        let origin = conway_core::log::ForkOrigin {
            parent,
            at_seq: LogSeq(1),
            mode: conway_core::log::SubagentMode::Fork,
        };
        make_session(
            &store,
            make_meta(child, Some(origin)),
            vec![user_turn(0, "child turn")],
        )
        .await;

        let path = resolve_default_path(&resolver, &store, &path_store, &child)
            .await
            .unwrap();

        let nodes: Vec<_> = path.nodes().collect();
        // Effective transcript = [parent's record, child's record] (2 content
        // records; Headers filtered by `is_content_record`).
        assert_eq!(nodes.len(), 2);
        // The first is the parent's record (text "parent")...
        let parent_text = match &**nodes[0].1 {
            LogRecord::UserTurn { text, .. } => text.as_str(),
            other => panic!("expected parent UserTurn, got {other:?}"),
        };
        assert_eq!(parent_text, "parent");
        // ...but it is stamped `Head` (NOT `Inherited { from: parent }`) ...
        assert_eq!(nodes[0].0.stamp, NodeStamp::Head);
        // ... and mis-attributed to the CHILD (`RecordRef { child, 0 }`, not
        // `{ parent, 0 }`) — the D1-3d fix point.
        assert_eq!(
            nodes[0].0.record,
            RecordRef {
                session: child,
                seq: LogSeq(0)
            }
        );
        // The second is the child's own record, stamped `Own`.
        let child_text = match &**nodes[1].1 {
            LogRecord::UserTurn { text, .. } => text.as_str(),
            other => panic!("expected child UserTurn, got {other:?}"),
        };
        assert_eq!(child_text, "child turn");
        assert_eq!(nodes[1].0.stamp, NodeStamp::Own);
        assert_eq!(
            nodes[1].0.record,
            RecordRef {
                session: child,
                seq: LogSeq(0)
            }
        );
    }
}
