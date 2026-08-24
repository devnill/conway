//! [`RuntimeContextPathHost`]: the production implementation of
//! `conway_core::ports::ContextPathHost` (DESIGN §2.5, decision
//! `01M0K4QT6MBXPD6PXMBBBD2P7B`) -- the ONE place a `Tool::invoke` reaches
//! [`super::path::resolve_default_path`]/[`super::path::write_head`], the
//! same way `subagent.rs`'s `impl SubagentHost for Runtime` is the one place
//! a tool reaches fork/spawn.
//!
//! Built from the SAME three dependencies `AgentLoop`'s per-turn path
//! assembly already threads to those two functions (`LoopDeps::store`/
//! `path_store`/`resolver`) -- no new store, no new cache, one instance
//! shared across every turn (`Runtime::new` constructs it once).

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;

use conway_core::ids::{LogSeq, SessionId};
use conway_core::log::LogRecord;
use conway_core::path::{PathError, PathNode, PathSelection, RecordRef, ValidatedPath};
use conway_core::ports::{ContextPathHost, PathStore, SessionStore};
use conway_core::transcript::TranscriptResolver;

use super::path::{resolve_default_path, resolve_records, write_head};

/// See module doc.
pub struct RuntimeContextPathHost {
    store: Arc<dyn SessionStore>,
    path_store: Arc<dyn PathStore>,
    resolver: Arc<TranscriptResolver>,
}

impl RuntimeContextPathHost {
    pub fn new(
        store: Arc<dyn SessionStore>,
        path_store: Arc<dyn PathStore>,
        resolver: Arc<TranscriptResolver>,
    ) -> Self {
        Self {
            store,
            path_store,
            resolver,
        }
    }
}

#[async_trait]
impl ContextPathHost for RuntimeContextPathHost {
    /// Delegates straight to [`resolve_default_path`] -- the SAME per-turn
    /// path assembly `agent_loop.rs` runs, so a tool composing against this
    /// base sees exactly what the next turn would otherwise have sent, and
    /// [`derive_with`](ValidatedPath::derive_with)'s "compose from `self`'s
    /// own nodes, THEN `foreign`" resolution finds `session`'s current own
    /// tail among `self`'s own nodes without the caller doing anything
    /// special -- this is what keeps a tool that only ADDS foreign records
    /// from ever hitting the `covers_upto` reset trap (`conway_runtime::
    /// context::path`'s own `covers_upto_for` doc): the base already
    /// carries the tail, so it survives every `derive_with` that does not
    /// explicitly omit it.
    async fn default_path(&self, session: SessionId) -> Result<ValidatedPath, PathError> {
        resolve_default_path(
            &self.resolver,
            self.store.as_ref(),
            self.path_store.as_ref(),
            &session,
        )
        .await
    }

    /// Resolves each ref via the shared, masked `resolve_records` helper
    /// (`super::path::resolve_records`) -- see that function's own doc and
    /// `ContextPathHost::resolve_records`'s own doc for why this is the
    /// same resolution `resolve_default_path` itself uses, not a second,
    /// looser one.
    /// Placeholder `stamp`/`prov` on the throwaway `PathNode`s below are
    /// never read by `resolve_records` (it only reads `.record`) and never
    /// escape this function.
    async fn resolve_records(
        &self,
        refs: &[RecordRef],
    ) -> Result<BTreeMap<RecordRef, Arc<LogRecord>>, PathError> {
        if refs.is_empty() {
            return Ok(BTreeMap::new());
        }
        let placeholder_nodes: Vec<PathNode> = refs
            .iter()
            .map(|r| PathNode {
                record: *r,
                stamp: conway_core::path::NodeStamp::Own,
                prov: conway_core::path::NodeProvenance {
                    selected_by: conway_core::path::Selector::Operator,
                    at: chrono::Utc::now(),
                },
            })
            .collect();
        let resolved =
            resolve_records(&self.resolver, self.store.as_ref(), placeholder_nodes).await?;
        Ok(resolved
            .into_iter()
            .map(|(node, record)| (node.record, record))
            .collect())
    }

    /// Flattens `path` into a fresh, prefix-less `PathSelection` (see
    /// `ContextPathHost::set_head`'s own doc for why) and calls
    /// [`write_head`] -- the ONE writer this whole port exists to reach.
    async fn set_head(&self, session: SessionId, path: ValidatedPath) -> Result<LogSeq, PathError> {
        let incoherence = path.incoherence().to_vec();
        let nodes: Vec<PathNode> = path.into_nodes().into_iter().map(|(n, _)| n).collect();
        let selection = PathSelection {
            prefix: None,
            nodes,
            incoherence,
        };
        write_head(
            self.store.as_ref(),
            self.path_store.as_ref(),
            &session,
            selection,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway_core::log::SessionMeta;
    use conway_core::path::NodeStamp;
    use conway_core::ports::PluginConfig;
    use conway_testkit::FakeStore;
    use std::path::PathBuf;

    /// A minimal in-memory `PathStore`, the same shape `context::path`'s own
    /// test module uses.
    #[derive(Debug, Default)]
    struct MemPathStore {
        selections: std::sync::RwLock<
            std::collections::HashMap<conway_core::path::SelectionKey, PathSelection>,
        >,
    }

    #[async_trait]
    impl PathStore for MemPathStore {
        async fn put(
            &self,
            selection: PathSelection,
        ) -> Result<conway_core::path::SelectionKey, conway_core::error::PathStoreError> {
            let key = conway_core::path::SelectionKey::from_nodes(&selection.nodes);
            self.selections
                .write()
                .unwrap()
                .insert(key.clone(), selection);
            Ok(key)
        }
        async fn get(
            &self,
            key: &conway_core::path::SelectionKey,
        ) -> Result<PathSelection, conway_core::error::PathStoreError> {
            self.selections
                .read()
                .unwrap()
                .get(key)
                .cloned()
                .ok_or_else(|| conway_core::error::PathStoreError::NotFound { key: key.clone() })
        }
        async fn selections_referencing(
            &self,
            _sid: &SessionId,
        ) -> Result<Vec<conway_core::path::SelectionKey>, conway_core::error::PathStoreError>
        {
            Ok(Vec::new())
        }
    }

    fn meta(session: SessionId) -> SessionMeta {
        SessionMeta {
            id: session,
            agent_id: conway_core::ids::AgentId::new(),
            origin: None,
            agent_def: None,
            role: None,
            created: chrono::Utc::now(),
            cwd: PathBuf::from("/tmp"),
            labels: vec![],
            ephemeral: false,
            ask_origin: None,
            root: None,
            plugin_config: PluginConfig::default(),
        }
    }

    fn user_turn(seq: u64, text: &str) -> LogRecord {
        LogRecord::UserTurn {
            seq: LogSeq(seq),
            ts: chrono::Utc::now(),
            text: text.into(),
            prov: conway_core::provenance::Provenance::UserPrompt,
        }
    }

    fn host() -> (RuntimeContextPathHost, Arc<FakeStore>, Arc<MemPathStore>) {
        let store = Arc::new(FakeStore::new());
        let path_store = Arc::new(MemPathStore::default());
        let resolver = Arc::new(TranscriptResolver::new(64));
        let host = RuntimeContextPathHost::new(store.clone(), path_store.clone(), resolver);
        (host, store, path_store)
    }

    /// `default_path` on a fresh session with no head returns the whole own
    /// transcript -- proof this delegates to the real `resolve_default_path`,
    /// not a stub.
    #[tokio::test]
    async fn default_path_of_a_fresh_session_is_its_whole_own_log() {
        let (host, store, _path_store) = host();
        let s = SessionId::new();
        store.create(meta(s)).await.unwrap();
        store.append(&s, user_turn(0, "hello")).await.unwrap();
        store.append(&s, user_turn(1, "again")).await.unwrap();

        let path = host.default_path(s).await.unwrap();
        assert_eq!(path.nodes().count(), 2);
    }

    /// `resolve_records` of an empty slice succeeds with an empty map
    /// without touching the store at all (the `RuntimeContextPathHost`-level
    /// mirror of the port's own contract).
    #[tokio::test]
    async fn resolve_records_of_empty_slice_is_empty() {
        let (host, _store, _path_store) = host();
        let map = host.resolve_records(&[]).await.unwrap();
        assert!(map.is_empty());
    }

    /// `resolve_records` resolves a real record from ANY session -- the
    /// deliberately wide read surface (module doc).
    #[tokio::test]
    async fn resolve_records_resolves_a_real_foreign_record() {
        let (host, store, _path_store) = host();
        let s = SessionId::new();
        store.create(meta(s)).await.unwrap();
        store
            .append(&s, user_turn(0, "foreign content"))
            .await
            .unwrap();

        let want = RecordRef {
            session: s,
            seq: LogSeq(0),
        };
        let map = host.resolve_records(&[want]).await.unwrap();
        let record = map.get(&want).expect("resolved");
        match record.as_ref() {
            LogRecord::UserTurn { text, .. } => assert_eq!(text, "foreign content"),
            other => panic!("expected UserTurn, got {other:?}"),
        }
    }

    /// `resolve_records` of an UNRESOLVABLE ref (unknown session) omits it
    /// from the map rather than partially constructing an entry -- proven by
    /// the fact this returns `Err`, matching `resolve_records`'s own
    /// documented failure mode for a session that does not exist.
    #[tokio::test]
    async fn resolve_records_of_an_unknown_session_refuses() {
        let (host, _store, _path_store) = host();
        let ghost = RecordRef {
            session: SessionId::new(),
            seq: LogSeq(0),
        };
        let err = host.resolve_records(&[ghost]).await.unwrap_err();
        assert!(matches!(err, PathError::UnresolvableNode { .. }));
    }

    /// `set_head` round-trips through the real reader
    /// (`resolve_default_path`) -- the same proof `write_head`'s own test
    /// module runs, but through THIS port's own call shape (a `ValidatedPath`
    /// in, flattened to a prefix-less `PathSelection`).
    #[tokio::test]
    async fn set_head_round_trips_through_default_path() {
        let (host, store, _path_store) = host();
        let s = SessionId::new();
        store.create(meta(s)).await.unwrap();
        store.append(&s, user_turn(0, "first")).await.unwrap();
        store.append(&s, user_turn(1, "second")).await.unwrap();

        let base = host.default_path(s).await.unwrap();
        assert_eq!(base.nodes().count(), 2);

        // Freeze the base unchanged as the new head.
        host.set_head(s, base).await.unwrap();

        let after = host.default_path(s).await.unwrap();
        let nodes: Vec<_> = after.nodes().collect();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].0.stamp, NodeStamp::Inherited { from: s });
        assert_eq!(nodes[1].0.stamp, NodeStamp::Inherited { from: s });
    }
}
