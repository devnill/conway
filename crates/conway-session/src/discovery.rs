//! Cross-project session discovery: the `SessionSearchScope::AllProjects`
//! mechanics behind `conway_core::ports::SessionDiscoveryHost` (board item
//! `01M0PS8J3AK7Z7253Z3E3RD3GY`).
//!
//! # No crawler, no registry
//!
//! Decision `01M0QK8J757ZH6R06WYJ0PQGEM` moved every project's sessions
//! under one central, project-keyed root specifically so this module could
//! be exactly what it is: [`list_project_keys`] does ONE `tokio::fs::
//! read_dir` over that root, never a recursive filesystem walk looking for
//! `.conway/sessions` directories, and nothing here writes a side table
//! anything else must keep in sync. A project whose `[session].root` was
//! explicitly configured away from the central default never appears here
//! -- it never wrote to this root, so there is nothing to list. That is the
//! decision's own disclosed edge, not a new gap.
//!
//! # No new index, no unbounded reads
//!
//! Every candidate session's METADATA is read via the ordinary
//! [`conway_core::ports::SessionStore::list`] surface (which
//! `JsonlSessionStore` already backs with [`crate::index::SessionIndex`] --
//! header-only, no record bodies touched). Only when a caller's
//! [`conway_core::ports::SessionSearchQuery::text`] asks for content search
//! does this module read a session's actual records
//! ([`conway_core::ports::SessionStore::read`]), and only up to
//! `max_sessions` sessions -- see [`search_all_projects`]'s own doc for
//! exactly where that bound is enforced.
//!
//! Each project directory is opened with [`FsyncPolicy::Never`]: every call
//! here is read-only (`list`/`read`), so there is nothing to sync, and
//! `Never` is the one policy that spawns no background flush task -- a
//! search must not leave a task running past its own call.

use std::collections::BTreeSet;
use std::path::Path;

use conway_core::error::StoreError;
use conway_core::ids::SeqRange;
use conway_core::log::{LogRecord, SessionFilter, SessionMeta};
use conway_core::ports::{
    MatchedRecord, SessionMatch, SessionSearchQuery, SessionSearchResult, SessionStore,
};

use crate::store::{FsyncPolicy, JsonlSessionStore, StoreConfig};

/// Hard ceiling on `SessionSearchQuery::max_sessions`, enforced regardless
/// of what a caller asks for -- `conway_core::ports::discovery`'s own doc
/// names this as every implementation's responsibility; this is where it is
/// actually applied for the `AllProjects` scope. `CurrentProject` (the
/// `SessionDiscoveryHost` implementation, not this module) applies the same
/// constant to the identical field.
pub const MAX_SESSIONS_HARD_CAP: usize = 100;

/// How many characters of context a [`MatchedRecord::snippet`] keeps on
/// each side of the match.
const SNIPPET_RADIUS: usize = 60;

fn io_err(e: std::io::Error) -> StoreError {
    StoreError::Io {
        detail: e.to_string(),
    }
}

fn read_only_store_config() -> StoreConfig {
    StoreConfig {
        fsync: FsyncPolicy::Never,
        lru_capacity: 8,
    }
}

/// Lists the project-key subdirectory NAMES immediately under
/// `central_root`, sorted for determinism. An absent `central_root` (no
/// central sessions exist yet on this machine) yields an empty list, not an
/// error -- discovery on a fresh install has nothing to find, which is not
/// a failure.
pub async fn list_project_keys(central_root: &Path) -> Result<Vec<String>, StoreError> {
    let mut out = BTreeSet::new();
    let mut rd = match tokio::fs::read_dir(central_root).await {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err(e)),
    };
    while let Some(entry) = rd.next_entry().await.map_err(io_err)? {
        let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
        if !is_dir {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            out.insert(name.to_string());
        }
    }
    Ok(out.into_iter().collect())
}

/// The `SessionSearchScope::AllProjects` implementation: metadata across
/// every project directory under `central_root`, then (only if
/// `query.text` is `Some`) a bounded content scan of the most recent
/// `query.max_sessions` candidates.
///
/// `query.max_sessions` is clamped into `1..=`[`MAX_SESSIONS_HARD_CAP`]
/// here -- never trusted verbatim (`conway_core::ports::discovery`'s own
/// doc states every implementation must do this).
pub async fn search_all_projects(
    central_root: &Path,
    query: &SessionSearchQuery,
) -> Result<SessionSearchResult, StoreError> {
    let project_keys = list_project_keys(central_root).await?;
    let max_sessions = query.max_sessions.clamp(1, MAX_SESSIONS_HARD_CAP);

    // Metadata pass: one store per project directory, `list` only -- zero
    // record bodies read here regardless of `query.text`.
    let mut candidates: Vec<(String, SessionMeta)> = Vec::new();
    for key in &project_keys {
        let dir = central_root.join(key);
        let store = JsonlSessionStore::open_with(dir, read_only_store_config()).await?;
        let metas = store
            .list(SessionFilter {
                agent_def: query.agent_def.clone(),
                label: query.label.clone(),
                parent: None,
                limit: None,
                include_ephemeral: false,
            })
            .await?;
        for meta in metas {
            candidates.push((key.clone(), meta));
        }
    }
    let sessions_considered = candidates.len();
    // Most-recent-first, ties broken by id, matching `SessionIndex::list`'s
    // own ordering convention.
    candidates.sort_by(|a, b| {
        b.1.created
            .cmp(&a.1.created)
            .then_with(|| a.1.id.cmp(&b.1.id))
    });

    let mut result = SessionSearchResult {
        projects_scanned: project_keys.len(),
        sessions_considered,
        ..Default::default()
    };

    let Some(text) = query.text.as_ref() else {
        // Metadata-only mode: every candidate (up to the cap) IS a match --
        // zero records ever read.
        result.truncated = candidates.len() > max_sessions;
        result.matches = candidates
            .into_iter()
            .take(max_sessions)
            .map(|(project_key, meta)| session_match(project_key, meta, Vec::new()))
            .collect();
        return Ok(result);
    };

    let needle = text.to_lowercase();
    result.truncated = candidates.len() > max_sessions;
    let mut matches = Vec::new();
    for (project_key, meta) in candidates.into_iter().take(max_sessions) {
        let dir = central_root.join(&project_key);
        let store = JsonlSessionStore::open_with(dir, read_only_store_config()).await?;
        let records = store.read(&meta.id, SeqRange::full()).await?;
        result.sessions_content_scanned += 1;
        result.records_scanned += records.len();
        let matched_records = matching_records(&records, &needle);
        if !matched_records.is_empty() {
            matches.push(session_match(project_key, meta, matched_records));
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
    project_key: String,
    meta: SessionMeta,
    matched_records: Vec<MatchedRecord>,
) -> SessionMatch {
    SessionMatch {
        session: meta.id,
        project_key,
        cwd: meta.cwd,
        created: meta.created,
        agent_def: meta.agent_def,
        labels: meta.labels,
        matched_records,
    }
}

/// Finds every record in `records` whose own searchable text contains
/// `needle` (already-lowercased), case-insensitively. Shared by
/// [`search_all_projects`] and by the `SessionDiscoveryHost`
/// `CurrentProject` implementation, which reuses this against its own
/// already-open `SessionStore` rather than duplicating the match logic.
pub fn matching_records(records: &[LogRecord], needle: &str) -> Vec<MatchedRecord> {
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

/// A short, human-readable excerpt centered on `byte_pos..byte_pos+needle_len`
/// within `text` -- both must already be one consistent, valid UTF-8
/// string (this function slices `text` itself, at char boundaries found via
/// `char_indices`, never at a raw byte offset that could split a
/// multi-byte character).
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

/// Extracts one record's own searchable text -- text content only. A
/// structural record with nothing a person wrote or read (`ContextMask`,
/// `ContextPathSet`, `ContextPathNamed`, `ContextReportRecord`) yields an
/// empty string, which never matches a non-empty needle.
fn record_text(record: &LogRecord) -> String {
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
        LogRecord::ContextReportRecord { .. }
        | LogRecord::ContextMask { .. }
        | LogRecord::ContextPathSet { .. }
        | LogRecord::ContextPathNamed { .. } => String::new(),
        // `LogRecord` is `#[non_exhaustive]`: a future variant this module
        // has not been taught about yet is simply not searchable rather
        // than a compile break or a panic.
        _ => String::new(),
    }
}

fn content_blocks_text(blocks: &[conway_core::content::ContentBlock]) -> String {
    use conway_core::content::ContentBlock;
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
            // `ContentBlock` is `#[non_exhaustive]` too -- same policy as
            // `record_text`'s own wildcard immediately above.
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use conway_core::ids::{AgentId, LogSeq};
    use conway_core::ports::PluginConfig;
    use conway_core::provenance::Provenance;
    use std::path::PathBuf;

    fn meta(cwd: &str) -> SessionMeta {
        SessionMeta {
            id: conway_core::ids::SessionId::new(),
            agent_id: AgentId::new(),
            origin: None,
            agent_def: None,
            role: None,
            created: Utc::now(),
            cwd: PathBuf::from(cwd),
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
    async fn list_project_keys_of_a_missing_root_is_empty_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let keys = list_project_keys(&tmp.path().join("does-not-exist"))
            .await
            .unwrap();
        assert!(keys.is_empty());
    }

    #[tokio::test]
    async fn list_project_keys_lists_only_directories_sorted() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(tmp.path().join("-Users-dan-b"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(tmp.path().join("-Users-dan-a"))
            .await
            .unwrap();
        tokio::fs::write(tmp.path().join("stray-file"), b"x")
            .await
            .unwrap();
        let keys = list_project_keys(tmp.path()).await.unwrap();
        assert_eq!(keys, vec!["-Users-dan-a", "-Users-dan-b"]);
    }

    #[tokio::test]
    async fn search_all_projects_of_a_missing_root_returns_an_empty_bill_of_costs() {
        let tmp = tempfile::tempdir().unwrap();
        let result = search_all_projects(
            &tmp.path().join("no-sessions-yet"),
            &SessionSearchQuery::default(),
        )
        .await
        .unwrap();
        assert_eq!(result.projects_scanned, 0);
        assert_eq!(result.sessions_considered, 0);
        assert_eq!(result.records_scanned, 0);
        assert!(result.matches.is_empty());
        assert!(!result.truncated);
    }

    #[tokio::test]
    async fn search_all_projects_metadata_mode_never_reads_a_record() {
        let tmp = tempfile::tempdir().unwrap();
        let central = tmp.path().join("sessions");
        let project_dir = central.join("-Users-dan-proj");
        let store = JsonlSessionStore::open(project_dir.clone()).await.unwrap();
        let sid = store.create(meta("/Users/dan/proj")).await.unwrap();
        store
            .append(&sid, user_turn(0, "let's talk about retry logic"))
            .await
            .unwrap();

        let result = search_all_projects(&central, &SessionSearchQuery::default())
            .await
            .unwrap();
        assert_eq!(result.projects_scanned, 1);
        assert_eq!(result.sessions_considered, 1);
        assert_eq!(result.sessions_content_scanned, 0);
        assert_eq!(result.records_scanned, 0);
        assert_eq!(result.matches.len(), 1);
        assert!(result.matches[0].matched_records.is_empty());
    }

    #[tokio::test]
    async fn search_all_projects_text_mode_finds_the_matching_record_across_projects() {
        let tmp = tempfile::tempdir().unwrap();
        let central = tmp.path().join("sessions");

        let store_a = JsonlSessionStore::open(central.join("-Users-dan-a"))
            .await
            .unwrap();
        let sid_a = store_a.create(meta("/Users/dan/a")).await.unwrap();
        store_a
            .append(&sid_a, user_turn(0, "nothing interesting here"))
            .await
            .unwrap();

        let store_b = JsonlSessionStore::open(central.join("-Users-dan-b"))
            .await
            .unwrap();
        let sid_b = store_b.create(meta("/Users/dan/b")).await.unwrap();
        store_b
            .append(
                &sid_b,
                user_turn(0, "let's talk about RETRY LOGIC tomorrow"),
            )
            .await
            .unwrap();

        let query = SessionSearchQuery {
            text: Some("retry logic".to_string()),
            ..SessionSearchQuery::default()
        };
        let result = search_all_projects(&central, &query).await.unwrap();
        assert_eq!(result.projects_scanned, 2);
        assert_eq!(result.sessions_considered, 2);
        assert_eq!(result.sessions_content_scanned, 2);
        assert_eq!(result.records_scanned, 2);
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].session, sid_b);
        assert_eq!(result.matches[0].matched_records.len(), 1);
        assert_eq!(result.matches[0].matched_records[0].seq, LogSeq(0));
        assert!(result.matches[0].matched_records[0]
            .snippet
            .to_lowercase()
            .contains("retry logic"));
    }

    #[tokio::test]
    async fn search_all_projects_truncates_at_max_sessions_and_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let central = tmp.path().join("sessions");
        for i in 0..3 {
            let dir = central.join(format!("-Users-dan-p{i}"));
            let store = JsonlSessionStore::open(dir).await.unwrap();
            store.create(meta("/Users/dan/p")).await.unwrap();
        }
        let query = SessionSearchQuery {
            max_sessions: 2,
            ..SessionSearchQuery::default()
        };
        let result = search_all_projects(&central, &query).await.unwrap();
        assert_eq!(result.sessions_considered, 3);
        assert_eq!(result.matches.len(), 2);
        assert!(result.truncated);
    }

    #[test]
    fn matching_records_is_case_insensitive_and_skips_structural_records() {
        let records = vec![
            user_turn(0, "the RETRY logic needs work"),
            LogRecord::ContextMask {
                seq: LogSeq(1),
                ts: Utc::now(),
                target_seq: LogSeq(0),
                excluded: true,
            },
        ];
        let matches = matching_records(&records, "retry logic");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].seq, LogSeq(0));
    }

    #[test]
    fn matching_records_of_an_empty_needle_matches_nothing() {
        let records = vec![user_turn(0, "anything at all")];
        assert!(matching_records(&records, "").is_empty());
    }
}
