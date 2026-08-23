//! [`FsSessionDiscoveryHost`]: the production implementation of
//! `conway_core::ports::SessionDiscoveryHost` (board item
//! `01M0PS8J3AK7Z7253Z3E3RD3GY`) -- built once by [`crate::builder`] and
//! injected into `conway_runtime::runtime::RuntimeDeps::session_discovery`.
//!
//! # Why this lives HERE, not in `conway-runtime`
//!
//! `crates/conway/tests/architecture_invariants.rs`'s T4 pins `conway-runtime`
//! to depend on `conway-core` ALONE -- no adapter edge, `conway-session`
//! included. `SessionSearchScope::AllProjects` genuinely needs adapter
//! machinery (`conway_session::discovery`, opening a `JsonlSessionStore` per
//! sibling project directory) that has no adapter-free equivalent the way
//! `resolve_default_path`/`write_head` do, so the concrete host is built
//! HERE -- this crate already carries the `conway-session` edge, gated by
//! `jsonl-store`, exactly like [`crate::builder::build_default_store`].
//! `RuntimeDeps::session_discovery` takes the finished `Arc<dyn
//! SessionDiscoveryHost>` directly (unlike `context_path_host`, which
//! `Runtime::new` builds internally from adapter-free pieces already in
//! `RuntimeDeps`) -- see that field's own doc.
//!
//! # `SessionSearchScope::CurrentProject` needs no adapter at all
//!
//! Only [`scan_all_projects`] is feature-gated. The `CurrentProject` fast
//! path reuses the already-built `store: Arc<dyn SessionStore>` through the
//! generic port trait alone, so it works identically whether `jsonl-store`
//! is on or an embedder injected their own `SessionStore` via
//! `ConwayBuilder::with_session_store`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use conway_core::error::StoreError;
use conway_core::ids::SeqRange;
use conway_core::log::{SessionFilter, SessionMeta};
use conway_core::ports::{
    MatchedRecord, SessionDiscoveryHost, SessionMatch, SessionSearchQuery, SessionSearchResult,
    SessionSearchScope, SessionStore,
};

/// Mirrors `conway_session::discovery::MAX_SESSIONS_HARD_CAP` -- duplicated
/// rather than referenced under a feature gate so
/// [`FsSessionDiscoveryHost::search_current_project`] (adapter-free) does
/// not need `#[cfg(feature = "jsonl-store")]` of its own just to name a
/// constant.
const MAX_SESSIONS_HARD_CAP: usize = 100;

/// See module doc.
pub(crate) struct FsSessionDiscoveryHost {
    store: Arc<dyn SessionStore>,
    project_key: String,
    central_root: Option<PathBuf>,
}

impl FsSessionDiscoveryHost {
    pub(crate) fn new(
        store: Arc<dyn SessionStore>,
        project_key: String,
        central_root: Option<PathBuf>,
    ) -> Self {
        Self {
            store,
            project_key,
            central_root,
        }
    }

    /// `SessionSearchScope::CurrentProject`: metadata via `self.store.list`
    /// (header-only, no records read), then -- only if `query.text` is
    /// `Some` -- a bounded content scan of the most recent `max_sessions`
    /// candidates via `self.store.read`. Mirrors `conway_session::
    /// discovery::search_all_projects`'s own two-pass shape, over one
    /// project instead of many.
    async fn search_current_project(
        &self,
        query: &SessionSearchQuery,
    ) -> Result<SessionSearchResult, StoreError> {
        let max_sessions = query.max_sessions.clamp(1, MAX_SESSIONS_HARD_CAP);

        let mut metas = self
            .store
            .list(SessionFilter {
                agent_def: query.agent_def.clone(),
                label: query.label.clone(),
                parent: None,
                limit: None,
                include_ephemeral: false,
            })
            .await?;
        metas.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| a.id.cmp(&b.id)));

        let mut result = SessionSearchResult {
            projects_scanned: 1,
            sessions_considered: metas.len(),
            ..Default::default()
        };

        let Some(text) = query.text.as_ref() else {
            result.truncated = metas.len() > max_sessions;
            result.matches = metas
                .into_iter()
                .take(max_sessions)
                .map(|meta| self.session_match(meta, Vec::new()))
                .collect();
            return Ok(result);
        };

        let needle = text.to_lowercase();
        result.truncated = metas.len() > max_sessions;
        let mut matches = Vec::new();
        for meta in metas.into_iter().take(max_sessions) {
            let records = self.store.read(&meta.id, SeqRange::full()).await?;
            result.sessions_content_scanned += 1;
            result.records_scanned += records.len();
            let matched_records = matching_records(&records, &needle);
            if !matched_records.is_empty() {
                matches.push(self.session_match(meta, matched_records));
            }
        }
        matches.sort_by(|a, b| {
            b.created
                .cmp(&a.created)
                .then_with(|| a.session.cmp(&b.session))
        });
        result.matches = matches;
        Ok(result)
    }

    fn session_match(
        &self,
        meta: SessionMeta,
        matched_records: Vec<MatchedRecord>,
    ) -> SessionMatch {
        SessionMatch {
            session: meta.id,
            project_key: self.project_key.clone(),
            cwd: meta.cwd,
            created: meta.created,
            agent_def: meta.agent_def,
            labels: meta.labels,
            matched_records,
        }
    }
}

#[async_trait]
impl SessionDiscoveryHost for FsSessionDiscoveryHost {
    async fn search(&self, query: SessionSearchQuery) -> Result<SessionSearchResult, StoreError> {
        match query.scope {
            SessionSearchScope::CurrentProject => self.search_current_project(&query).await,
            SessionSearchScope::AllProjects => match &self.central_root {
                Some(root) => scan_all_projects(root, &query).await,
                // No central sessions root resolvable (no home directory
                // AND no `CONWAY_CONFIG_DIR` -- `config::discovery::
                // session_root`'s own extreme-edge-case fallback). Nothing
                // to scan; an honest empty bill of costs, not an error.
                None => Ok(SessionSearchResult::default()),
            },
        }
    }
}

/// Extracted so only THIS function (not the whole struct) needs the
/// `jsonl-store` feature split -- see the module doc.
#[cfg(feature = "jsonl-store")]
async fn scan_all_projects(
    root: &std::path::Path,
    query: &SessionSearchQuery,
) -> Result<SessionSearchResult, StoreError> {
    conway_session::discovery::search_all_projects(root, query).await
}

/// The `jsonl-store`-off arm: `conway-session` is unlinked entirely in this
/// configuration, so there is no adapter left to scan sibling project
/// directories with. An honest empty bill of costs, matching
/// [`FsSessionDiscoveryHost::search`]'s own "no central root resolvable"
/// fallback immediately above -- never an error, since a caller merely
/// asked a wider question than this build can answer, not an invalid one.
#[cfg(not(feature = "jsonl-store"))]
async fn scan_all_projects(
    _root: &std::path::Path,
    _query: &SessionSearchQuery,
) -> Result<SessionSearchResult, StoreError> {
    Ok(SessionSearchResult::default())
}

/// Case-insensitive substring match against each record's own searchable
/// text -- the SAME logic `conway_session::discovery::matching_records`
/// implements (that module's own doc explains the snippet/boundary
/// details). Reimplemented here, not called through a feature gate, so
/// `search_current_project` above stays usable with `jsonl-store` off
/// (an embedder-injected `SessionStore` has nothing to do with which
/// session-log ADAPTER is linked).
fn matching_records(records: &[conway_core::log::LogRecord], needle: &str) -> Vec<MatchedRecord> {
    if needle.is_empty() {
        return Vec::new();
    }
    records
        .iter()
        .filter_map(|record| {
            let seq = record.seq()?;
            let text = record_text(record);
            let lower = text.to_lowercase();
            let pos = lower.find(needle)?;
            Some(MatchedRecord {
                seq,
                snippet: snippet_around(&lower, pos, needle.len()),
            })
        })
        .collect()
}

const SNIPPET_RADIUS: usize = 60;

fn snippet_around(text: &str, byte_pos: usize, needle_len: usize) -> String {
    let start = text[..byte_pos]
        .char_indices()
        .rev()
        .nth(SNIPPET_RADIUS)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let end_min = (byte_pos + needle_len).min(text.len());
    let end = text[end_min..]
        .char_indices()
        .nth(SNIPPET_RADIUS)
        .map(|(i, _)| end_min + i)
        .unwrap_or(text.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.push_str(text[start..end].trim());
    if end < text.len() {
        out.push('…');
    }
    out
}

fn record_text(record: &conway_core::log::LogRecord) -> String {
    use conway_core::content::ContentBlock;
    use conway_core::log::LogRecord;

    fn content_blocks_text(blocks: &[ContentBlock]) -> String {
        let mut out = String::new();
        for block in blocks {
            match block {
                ContentBlock::Text { text } | ContentBlock::Thinking { text, .. } => {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(text);
                }
                ContentBlock::ToolUse { name, .. } => {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(name.as_str());
                }
                ContentBlock::ToolResultBlock { blocks, .. } => {
                    let nested = content_blocks_text(blocks);
                    if !nested.is_empty() {
                        if !out.is_empty() {
                            out.push(' ');
                        }
                        out.push_str(&nested);
                    }
                }
                ContentBlock::Image { .. } => {}
                _ => {}
            }
        }
        out
    }

    match record {
        LogRecord::Header(_) => String::new(),
        LogRecord::UserTurn { text, .. }
        | LogRecord::ForkDirective { text, .. }
        | LogRecord::ParentSteer { text, .. }
        | LogRecord::SystemNote { text, .. } => text.clone(),
        LogRecord::Assistant { content, .. } => content_blocks_text(content),
        LogRecord::ToolResultRecord { result, .. } => content_blocks_text(&result.blocks),
        LogRecord::AgentResultRecord { result, .. }
        | LogRecord::ChildResultRecord { result, .. } => result.summary.clone(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use conway_core::ids::{AgentId, LogSeq};
    use conway_core::log::LogRecord;
    use conway_core::ports::PluginConfig;
    use conway_core::provenance::Provenance;
    use conway_testkit::FakeStore;
    use std::path::PathBuf as StdPathBuf;

    fn meta() -> SessionMeta {
        SessionMeta {
            id: conway_core::ids::SessionId::new(),
            agent_id: AgentId::new(),
            origin: None,
            agent_def: None,
            role: None,
            created: Utc::now(),
            cwd: StdPathBuf::from("/tmp"),
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
            ts: Utc::now(),
            text: text.into(),
            prov: Provenance::UserPrompt,
        }
    }

    #[tokio::test]
    async fn current_project_metadata_search_reads_zero_records() {
        let store = Arc::new(FakeStore::new());
        let sid = store.create(meta()).await.unwrap();
        store.append(&sid, user_turn(0, "hello")).await.unwrap();
        let host = FsSessionDiscoveryHost::new(store, "proj".to_string(), None);

        let result = host.search(SessionSearchQuery::default()).await.unwrap();
        assert_eq!(result.projects_scanned, 1);
        assert_eq!(result.sessions_considered, 1);
        assert_eq!(result.records_scanned, 0);
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].project_key, "proj");
    }

    #[tokio::test]
    async fn current_project_text_search_finds_the_matching_record() {
        let store = Arc::new(FakeStore::new());
        let sid = store.create(meta()).await.unwrap();
        store
            .append(&sid, user_turn(0, "let's discuss retry logic"))
            .await
            .unwrap();
        let host = FsSessionDiscoveryHost::new(store, "proj".to_string(), None);

        let query = SessionSearchQuery {
            text: Some("retry logic".to_string()),
            ..SessionSearchQuery::default()
        };
        let result = host.search(query).await.unwrap();
        assert_eq!(result.sessions_content_scanned, 1);
        assert_eq!(result.records_scanned, 1);
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].matched_records[0].seq, LogSeq(0));
    }

    #[tokio::test]
    async fn all_projects_scope_with_no_central_root_is_an_empty_bill_of_costs_not_an_error() {
        let store = Arc::new(FakeStore::new());
        let host = FsSessionDiscoveryHost::new(store, "proj".to_string(), None);
        let query = SessionSearchQuery {
            scope: SessionSearchScope::AllProjects,
            ..SessionSearchQuery::default()
        };
        let result = host.search(query).await.unwrap();
        assert_eq!(result.projects_scanned, 0);
        assert!(result.matches.is_empty());
    }

    #[tokio::test]
    #[cfg(feature = "jsonl-store")]
    async fn all_projects_scope_scans_a_real_central_root_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let central = tmp.path().join("sessions");
        let disk_store = conway_session::JsonlSessionStore::open(central.join("-Users-dan-other"))
            .await
            .unwrap();
        let sid = disk_store.create(meta()).await.unwrap();
        disk_store
            .append(&sid, user_turn(0, "retry logic notes"))
            .await
            .unwrap();

        let store = Arc::new(FakeStore::new());
        let host = FsSessionDiscoveryHost::new(store, "proj".to_string(), Some(central));
        let query = SessionSearchQuery {
            scope: SessionSearchScope::AllProjects,
            text: Some("retry logic".to_string()),
            ..SessionSearchQuery::default()
        };
        let result = host.search(query).await.unwrap();
        assert_eq!(result.projects_scanned, 1);
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].project_key, "-Users-dan-other");
    }
}
