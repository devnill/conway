//! `TranscriptResolver` re-export + `resolve_path` (DESIGN §2.6, §2.7, §2.9,
//! §3).
//!
//! The resolver moved to `conway_core::transcript` (board item
//! 01KZVYVTVWRH20R6VJ6G3SWTJ6, "Stage 1a"): it is pure logic over the
//! `SessionStore` *port*, not over `JsonlSessionStore` specifically, so it
//! belongs beside the contract rather than inside this one adapter. This
//! module re-exports the type unchanged so existing callers of
//! `conway_session::TranscriptResolver` (and `conway_session::resolver::
//! TranscriptResolver`) keep compiling without edits.
//!
//! D1-3c adds [`resolve_path`], the seam that expands a `PathSelection`'s
//! prefix chain (via the `PathStore` port), flattens the fully-expanded node
//! list, and resolves each node's `RecordRef` to its `Arc<LogRecord>` by
//! reusing `TranscriptResolver::resolve_prefix` — the SAME memoised ancestry
//! walk the runtime uses today (cache-hit behaviour is preserved: two forks
//! resolving the same parent prefix hit the same `(SessionId, LogSeq)` cache
//! entry).
//!
//! **`Arc::ptr_eq` does NOT hold across siblings.** `resolve_records` clones
//! each record out of the resolver's `Arc<[LogRecord]>` into a fresh
//! `Arc<LogRecord>` (`Arc::new(record.clone())`), because the records are
//! inline in the slice — you cannot hand out an `Arc<LogRecord>` from an
//! `Arc<[LogRecord]>` without restructuring the cache. Two `resolve_path`
//! calls therefore get distinct `Arc`s. This is not correctness-bearing
//! (`SelectionKey` is content-addressed over `record`+`stamp`, not `Arc`
//! identity, so siblings still hash equal and produce byte-identical wire
//! prefixes); it is a performance/identity property D1-3d may need. If
//! D1-3d's assembly relies on sibling `Arc::ptr_eq` (to avoid cloning across
//! a deep fork tree, or to compare record identity by pointer), it must
//! restructure the resolver cache to hand out per-record `Arc<LogRecord>`s.

use std::sync::Arc;

use conway_core::error::{PathStoreError, StoreError};
use conway_core::ids::{LogSeq, SessionId};
use conway_core::path::{PathError, PathNode, PathSelection, RecordRef, ResolvedPath};
use conway_core::ports::{PathStore, SessionStore};
use conway_core::transcript::MAX_ANCESTRY_DEPTH;

pub use conway_core::transcript::TranscriptResolver;

/// Expand a selection's prefix chain, flatten the node list, and resolve each
/// node's record — producing a [`ResolvedPath`] ready for assembly (DESIGN §3).
///
/// # Expansion
///
/// Starting from `selection.prefix`, walks transitively through the
/// [`PathStore`], bounded by [`MAX_ANCESTRY_DEPTH`] (the same constant the
/// transcript resolver uses — referenced by name, never a second 256). An
/// absent prefix key surfaces as [`PathError::UnresolvableNode`] at this seam
/// (the D1-3b deferred note: the port returns [`PathStoreError::NotFound`];
/// the resolver seam translates it). An over-deep chain surfaces as
/// [`PathError::PrefixChainTooDeep`].
///
/// The flattened list = prefix-chain nodes (root-first, matching
/// `FsPathStore::put`'s expansion order) `++ selection.nodes`.
///
/// # Record resolution
///
/// Each `PathNode.record` is a `RecordRef { session, seq }`. To get the
/// `Arc<LogRecord>`, this reuses [`TranscriptResolver::resolve_prefix`] — the
/// memoised ancestry walk keyed by `(SessionId, LogSeq)`. Calling
/// `resolve_prefix(store, &session, seq.succ())` returns the effective
/// transcript ending with the record at local `seq`; the last element is that
/// record (if not masked). This reuse is mandatory for cache-hit parity: two
/// forks resolving the same parent prefix hit the same `(SessionId, LogSeq)`
/// cache entry, so the resolved `LogRecord` *contents* are shared (DESIGN
/// §3). Note this does NOT preserve `Arc::ptr_eq` across siblings — see the
/// module doc: `resolve_records` clones each record into a fresh `Arc`.
pub async fn resolve_path<S, P>(
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
/// Mirrors `FsPathStore::expand`'s order exactly (DESIGN §2.3/§2.6): the
/// `SelectionKey` is computed over the expanded list, and `FsPathStore::put`
/// already expands to compute the key, so the order must match.
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
            // The D1-3b deferred note: the port returns store errors; the
            // resolver seam translates NotFound → UnresolvableNode. The
            // `record` field names the selection's first node (the closest
            // thing to "what was unresolvable"); the detail names the key.
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
/// `TranscriptResolver::resolve_prefix`. Grouping by session and calling with
/// the maximum `upto` per session would reduce resolver calls, but the
/// resolver is memoised — repeated calls for the same `(SessionId, LogSeq)`
/// are cache hits — so per-node calls are simple and correct without the
/// grouping complexity.
async fn resolve_records<S: SessionStore + ?Sized>(
    resolver: &TranscriptResolver,
    session_store: &S,
    nodes: Vec<PathNode>,
) -> Result<Vec<(PathNode, Arc<conway_core::log::LogRecord>)>, PathError> {
    let mut out: Vec<(PathNode, Arc<conway_core::log::LogRecord>)> =
        Vec::with_capacity(nodes.len());
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
        // The record at local `seq` is the last element of the effective
        // transcript up to `seq.succ()` — `resolve_prefix` returns
        // `prefix ++ own[0..upto]`, so `own[seq]` is last (if not masked).
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use conway_core::log::{LogRecord, SessionMeta};
    use conway_core::path::{NodeProvenance, NodeStamp, Selector};
    use conway_testkit::FakeStore;

    use chrono::Utc;

    fn ts() -> chrono::DateTime<chrono::Utc> {
        Utc::now()
    }

    fn node(session: SessionId, seq: u64) -> PathNode {
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

    /// A minimal in-memory `PathStore` for tests (no `PathStore` fake exists in
    /// `conway-testkit`). Keys are pre-computed by the test via
    /// `SelectionKey::from_nodes`.
    #[derive(Debug, Default)]
    struct MemPathStore {
        selections: std::sync::RwLock<HashMap<conway_core::path::SelectionKey, PathSelection>>,
    }

    impl MemPathStore {
        fn insert(&self, key: conway_core::path::SelectionKey, sel: PathSelection) {
            self.selections.write().unwrap().insert(key, sel);
        }
    }

    #[async_trait::async_trait]
    impl PathStore for MemPathStore {
        async fn put(
            &self,
            selection: PathSelection,
        ) -> Result<conway_core::path::SelectionKey, PathStoreError> {
            let nodes: Vec<PathNode> = selection.nodes.clone();
            let key = conway_core::path::SelectionKey::from_nodes(&nodes);
            self.selections
                .write()
                .unwrap()
                .insert(key.clone(), selection);
            Ok(key)
        }

        async fn get(
            &self,
            key: &conway_core::path::SelectionKey,
        ) -> Result<PathSelection, PathStoreError> {
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
        ) -> Result<Vec<conway_core::path::SelectionKey>, PathStoreError> {
            Ok(Vec::new())
        }
    }

    /// Create a session in the store with the given records (appended after a
    /// header).
    async fn make_session(store: &FakeStore, session: SessionId, records: Vec<LogRecord>) {
        let meta = SessionMeta {
            id: session,
            agent_id: conway_core::ids::AgentId::new(),
            origin: None,
            agent_def: None,
            role: None,
            created: ts(),
            cwd: std::path::PathBuf::from("/tmp"),
            labels: vec![],
            ephemeral: false,
            ask_origin: None,
            root: None,
            plugin_config: conway_core::ports::PluginConfig::default(),
        };
        store.create(meta).await.unwrap();
        for rec in records {
            store.append(&session, rec).await.unwrap();
        }
    }

    /// (a) A selection with no prefix resolves to its own nodes zipped with
    /// records.
    #[tokio::test]
    async fn no_prefix_resolves_own_nodes() {
        let store = FakeStore::new();
        let path_store = MemPathStore::default();
        let resolver = TranscriptResolver::new(64);

        let s = SessionId::new();
        make_session(
            &store,
            s,
            vec![
                LogRecord::UserTurn {
                    seq: LogSeq(0),
                    ts: ts(),
                    text: "hello".into(),
                    prov: conway_core::provenance::Provenance::UserPrompt,
                },
                LogRecord::UserTurn {
                    seq: LogSeq(1),
                    ts: ts(),
                    text: "world".into(),
                    prov: conway_core::provenance::Provenance::UserPrompt,
                },
            ],
        )
        .await;

        let sel = PathSelection {
            prefix: None,
            nodes: vec![node(s, 0), node(s, 1)],
            incoherence: vec![],
        };
        let resolved = resolve_path(&resolver, &store, &path_store, &sel)
            .await
            .unwrap();
        assert_eq!(resolved.nodes.len(), 2);
        assert_eq!(
            resolved.nodes[0].0.record,
            RecordRef {
                session: s,
                seq: LogSeq(0)
            }
        );
        assert_eq!(
            resolved.nodes[1].0.record,
            RecordRef {
                session: s,
                seq: LogSeq(1)
            }
        );
        // The records match.
        assert_eq!(resolved.nodes[0].1.seq(), Some(LogSeq(0)));
        assert_eq!(resolved.nodes[1].1.seq(), Some(LogSeq(1)));
    }

    /// (b) A selection with a prefix flattens the prefix chain and resolves
    /// prefix records via the resolver.
    #[tokio::test]
    async fn prefix_flattens_and_resolves() {
        let store = FakeStore::new();
        let path_store = MemPathStore::default();
        let resolver = TranscriptResolver::new(64);

        let parent = SessionId::new();
        let child = SessionId::new();
        make_session(
            &store,
            parent,
            vec![LogRecord::UserTurn {
                seq: LogSeq(0),
                ts: ts(),
                text: "parent turn".into(),
                prov: conway_core::provenance::Provenance::UserPrompt,
            }],
        )
        .await;
        make_session(
            &store,
            child,
            vec![LogRecord::UserTurn {
                seq: LogSeq(0),
                ts: ts(),
                text: "child turn".into(),
                prov: conway_core::provenance::Provenance::UserPrompt,
            }],
        )
        .await;

        // Store a prefix selection over the parent's node.
        let prefix_sel = PathSelection {
            prefix: None,
            nodes: vec![node(parent, 0)],
            incoherence: vec![],
        };
        let prefix_key = conway_core::path::SelectionKey::from_nodes(&prefix_sel.nodes);
        path_store.insert(prefix_key.clone(), prefix_sel);

        // Child selection references the prefix, then its own node.
        let sel = PathSelection {
            prefix: Some(prefix_key),
            nodes: vec![node(child, 0)],
            incoherence: vec![],
        };
        let resolved = resolve_path(&resolver, &store, &path_store, &sel)
            .await
            .unwrap();
        assert_eq!(resolved.nodes.len(), 2);
        // Prefix chain nodes come first.
        assert_eq!(
            resolved.nodes[0].0.record,
            RecordRef {
                session: parent,
                seq: LogSeq(0)
            }
        );
        assert_eq!(
            resolved.nodes[1].0.record,
            RecordRef {
                session: child,
                seq: LogSeq(0)
            }
        );
        // Records resolved correctly.
        assert_eq!(resolved.nodes[0].1.seq(), Some(LogSeq(0)));
        assert_eq!(resolved.nodes[1].1.seq(), Some(LogSeq(0)));
    }

    /// (c) An absent prefix key → PathError::UnresolvableNode.
    #[tokio::test]
    async fn absent_prefix_is_unresolvable() {
        let store = FakeStore::new();
        let path_store = MemPathStore::default();
        let resolver = TranscriptResolver::new(64);

        let s = SessionId::new();
        make_session(
            &store,
            s,
            vec![LogRecord::UserTurn {
                seq: LogSeq(0),
                ts: ts(),
                text: "hi".into(),
                prov: conway_core::provenance::Provenance::UserPrompt,
            }],
        )
        .await;

        let fake_key = conway_core::path::SelectionKey("0".repeat(64));
        let sel = PathSelection {
            prefix: Some(fake_key),
            nodes: vec![node(s, 0)],
            incoherence: vec![],
        };
        let err = resolve_path(&resolver, &store, &path_store, &sel)
            .await
            .unwrap_err();
        assert!(matches!(err, PathError::UnresolvableNode { .. }));
    }

    /// (d) A too-deep prefix chain → PathError::PrefixChainTooDeep.
    #[tokio::test]
    async fn too_deep_prefix_chain() {
        let store = FakeStore::new();
        let path_store = MemPathStore::default();
        let resolver = TranscriptResolver::new(64);

        let s = SessionId::new();
        make_session(
            &store,
            s,
            vec![LogRecord::UserTurn {
                seq: LogSeq(0),
                ts: ts(),
                text: "hi".into(),
                prov: conway_core::provenance::Provenance::UserPrompt,
            }],
        )
        .await;

        // Build a chain deeper than MAX_ANCESTRY_DEPTH.
        let mut prev_key: Option<conway_core::path::SelectionKey> = None;
        for i in 0..=MAX_ANCESTRY_DEPTH {
            let sel = PathSelection {
                prefix: prev_key.clone(),
                nodes: vec![node(s, i as u64)],
                incoherence: vec![],
            };
            let key = conway_core::path::SelectionKey::from_nodes(&sel.nodes);
            path_store.insert(key.clone(), sel);
            prev_key = Some(key);
        }
        // One more prefix level → too deep.
        let too_deep = PathSelection {
            prefix: prev_key,
            nodes: vec![node(s, 999)],
            incoherence: vec![],
        };
        let err = resolve_path(&resolver, &store, &path_store, &too_deep)
            .await
            .unwrap_err();
        assert!(matches!(err, PathError::PrefixChainTooDeep));
    }
}
