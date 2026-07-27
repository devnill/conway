//! Integration tests for `SessionIndex` (WI-050 criteria), exercised
//! entirely through the public `JsonlSessionStore`/`SessionStore` surface —
//! `SessionIndex`'s own methods are `pub(crate)` by design (architecture:
//! the index is a store-internal accelerator, never a source of truth
//! reachable by other crates), so every scenario below drives it via
//! `store.create`/`store.fork`/`store.children`/`store.list`, exactly as a
//! real caller would.
//!
//! WARN capture approach: `tracing-test` is not a dependency of this crate
//! (see `recovery_tests.rs`), so these tests reuse the same minimal
//! `tracing::Subscriber` capture harness.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use chrono::{DateTime, Utc};

use conway_core::ids::{AgentId, LogSeq, SessionId};
use conway_core::log::{ForkOrigin, SessionStatus, SubagentMode};
use conway_core::ports::SessionStore;
use conway_session::{JsonlSessionStore, SessionFilter, SessionMeta};

fn ts() -> DateTime<Utc> {
    "2026-07-20T00:00:00Z".parse().unwrap()
}

fn ts_plus(secs: i64) -> DateTime<Utc> {
    ts() + chrono::Duration::seconds(secs)
}

fn meta_full(id: SessionId, created: DateTime<Utc>, origin: Option<ForkOrigin>) -> SessionMeta {
    SessionMeta {
        id,
        agent_id: AgentId::new(),
        origin,
        agent_def: None,
        role: None,
        created,
        cwd: PathBuf::from("/tmp/project"),
        labels: vec![],
        status: SessionStatus::Active,
        ephemeral: false,
        ask_origin: None,
    }
}

fn meta_for(id: SessionId) -> SessionMeta {
    meta_full(id, ts(), None)
}

fn fork_origin(parent: SessionId) -> ForkOrigin {
    ForkOrigin {
        parent,
        at_seq: LogSeq(0),
        mode: SubagentMode::Fork,
    }
}

// ---------------------------------------------------------------------
// Minimal tracing WARN capture (no tracing-subscriber dependency) — see
// recovery_tests.rs for the identical pattern and rationale.
// ---------------------------------------------------------------------

#[derive(Clone, Default)]
struct CaptureLog {
    entries: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl CaptureLog {
    fn contains(&self, needle: &str) -> bool {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .any(|m| m.contains(needle))
    }
}

struct CaptureSubscriber {
    log: CaptureLog,
}

struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

impl tracing::Subscriber for CaptureSubscriber {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);
        self.log.entries.lock().unwrap().push(visitor.0);
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

fn install_capture() -> (CaptureLog, tracing::subscriber::DefaultGuard) {
    let log = CaptureLog::default();
    let guard = tracing::subscriber::set_default(CaptureSubscriber { log: log.clone() });
    (log, guard)
}

// ---------------------------------------------------------------------
// load_or_rebuild: absent index.jsonl
// ---------------------------------------------------------------------

#[tokio::test]
async fn rebuild_when_index_absent_scans_headers_and_lists_everything() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();

    let mut created = HashSet::new();
    {
        let store = JsonlSessionStore::open(root.clone()).await.unwrap();
        for _ in 0..5 {
            let sid = SessionId::new();
            store.create(meta_for(sid)).await.unwrap();
            created.insert(sid);
        }
    }

    // Fresh store over the same root, with index.jsonl already present
    // from the block above — this exercises the ordinary load path, not
    // rebuild. Delete it to force the "no index.jsonl present" scan path
    // this test is actually about.
    std::fs::remove_file(root.join("index.jsonl")).unwrap();

    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    let metas = store.list(SessionFilter::default()).await.unwrap();
    let got: HashSet<SessionId> = metas.iter().map(|m| m.id).collect();
    assert_eq!(got, created);
}

// ---------------------------------------------------------------------
// Rebuild equivalence over 50 sessions
// ---------------------------------------------------------------------

#[tokio::test]
async fn rebuild_equivalence_over_50_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let parent = SessionId::new();

    let pre_list: HashSet<SessionId>;
    let pre_children: HashSet<SessionId>;
    {
        let store = JsonlSessionStore::open(root.clone()).await.unwrap();
        store.create(meta_for(parent)).await.unwrap();
        for _ in 0..49 {
            let child = SessionId::new();
            store
                .fork(&parent, LogSeq(0), meta_for(child))
                .await
                .unwrap();
        }
        pre_list = store
            .list(SessionFilter::default())
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        pre_children = store.children(&parent).await.unwrap().into_iter().collect();
    }
    assert_eq!(pre_list.len(), 50);
    assert_eq!(pre_children.len(), 49);

    std::fs::remove_file(root.join("index.jsonl")).unwrap();

    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    let post_list: HashSet<SessionId> = store
        .list(SessionFilter::default())
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    let post_children: HashSet<SessionId> =
        store.children(&parent).await.unwrap().into_iter().collect();

    assert_eq!(
        pre_list, post_list,
        "list() must match as sets after rebuild"
    );
    assert_eq!(
        pre_children, post_children,
        "children() must match as sets after rebuild"
    );
}

// ---------------------------------------------------------------------
// `ephemeral` round-trips through a genuine restart (try_load, not rebuild)
// ---------------------------------------------------------------------

#[tokio::test]
async fn ephemeral_flag_survives_a_genuine_restart_via_the_load_path() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let sid = SessionId::new();
    let mut meta = meta_for(sid);
    meta.ephemeral = true;

    {
        let store = JsonlSessionStore::open(root.clone()).await.unwrap();
        store.create(meta).await.unwrap();
    }

    // `index.jsonl` is present on disk and consistent with the one session
    // file, so reopening here exercises `try_load` -- the genuine
    // load-from-index-file path -- not `rebuild_scan`. The WARN capture
    // below asserts exactly that, the same way the corruption tests below
    // assert the opposite.
    let (log, _guard) = install_capture();
    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    assert!(
        !log.contains("index rebuild"),
        "expected no rebuild (a clean index.jsonl must load via try_load), got: {:?}",
        log.entries.lock().unwrap()
    );

    let metas = store
        .list(SessionFilter {
            include_ephemeral: true,
            ..Default::default()
        })
        .await
        .unwrap();
    let got = metas
        .iter()
        .find(|m| m.id == sid)
        .expect("ephemeral session must still be listed with include_ephemeral: true");
    assert!(
        got.ephemeral,
        "ephemeral must survive a disk round-trip through index.jsonl"
    );
}

// ---------------------------------------------------------------------
// Corruption: each condition independently triggers a full rebuild with a
// WARN containing "index rebuild", and no error is returned.
// ---------------------------------------------------------------------

#[tokio::test]
async fn truncated_trailing_line_in_index_triggers_rebuild_with_warn() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let sid = SessionId::new();
    {
        let store = JsonlSessionStore::open(root.clone()).await.unwrap();
        store.create(meta_for(sid)).await.unwrap();
    }

    let idx_path = root.join("index.jsonl");
    let mut content = std::fs::read_to_string(&idx_path).unwrap();
    assert!(content.ends_with('\n'));
    // Simulate a crash mid-write: a syntactically incomplete trailing line,
    // no trailing newline.
    content.push_str(r#"{"session":"truncated-mid-write"#);
    std::fs::write(&idx_path, &content).unwrap();

    let (log, _guard) = install_capture();
    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    assert!(
        log.contains("index rebuild"),
        "expected an \"index rebuild\" WARN, got: {:?}",
        log.entries.lock().unwrap()
    );

    let metas = store.list(SessionFilter::default()).await.unwrap();
    assert_eq!(metas.len(), 1);
    assert_eq!(metas[0].id, sid);
}

#[tokio::test]
async fn dangling_index_reference_triggers_rebuild_with_warn() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let keep = SessionId::new();
    let gone = SessionId::new();
    {
        let store = JsonlSessionStore::open(root.clone()).await.unwrap();
        store.create(meta_for(keep)).await.unwrap();
        store.create(meta_for(gone)).await.unwrap();
    }
    // The index now names a session file that no longer exists on disk.
    std::fs::remove_file(root.join(format!("{gone}.jsonl"))).unwrap();

    let (log, _guard) = install_capture();
    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    assert!(
        log.contains("index rebuild"),
        "expected an \"index rebuild\" WARN, got: {:?}",
        log.entries.lock().unwrap()
    );

    let metas = store.list(SessionFilter::default()).await.unwrap();
    let ids: HashSet<SessionId> = metas.iter().map(|m| m.id).collect();
    assert_eq!(ids, [keep].into_iter().collect());
}

#[tokio::test]
async fn duplicate_index_entry_triggers_rebuild_with_warn() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let sid = SessionId::new();
    {
        let store = JsonlSessionStore::open(root.clone()).await.unwrap();
        store.create(meta_for(sid)).await.unwrap();
    }

    let idx_path = root.join("index.jsonl");
    let content = std::fs::read_to_string(&idx_path).unwrap();
    let doubled = format!("{content}{content}");
    std::fs::write(&idx_path, doubled).unwrap();

    let (log, _guard) = install_capture();
    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    assert!(
        log.contains("index rebuild"),
        "expected an \"index rebuild\" WARN, got: {:?}",
        log.entries.lock().unwrap()
    );

    let metas = store.list(SessionFilter::default()).await.unwrap();
    assert_eq!(
        metas.len(),
        1,
        "duplicate entry must collapse to one session"
    );
    assert_eq!(metas[0].id, sid);
}

// ---------------------------------------------------------------------
// children(): ascending created order, empty for a leaf
// ---------------------------------------------------------------------

#[tokio::test]
async fn children_are_ascending_by_created_regardless_of_call_order() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::open(dir.path().to_path_buf())
        .await
        .unwrap();
    let parent = SessionId::new();
    store.create(meta_for(parent)).await.unwrap();

    let a = SessionId::new();
    let b = SessionId::new();
    let c = SessionId::new();

    // Created out of chronological order: c (t+30), a (t+10), b (t+20).
    store
        .create(meta_full(c, ts_plus(30), Some(fork_origin(parent))))
        .await
        .unwrap();
    store
        .create(meta_full(a, ts_plus(10), Some(fork_origin(parent))))
        .await
        .unwrap();
    store
        .create(meta_full(b, ts_plus(20), Some(fork_origin(parent))))
        .await
        .unwrap();

    let kids = store.children(&parent).await.unwrap();
    assert_eq!(kids, vec![a, b, c]);

    assert!(
        store.children(&a).await.unwrap().is_empty(),
        "a session with no children must return an empty vec"
    );
}

// ---------------------------------------------------------------------
// list(): AND-composed filters, descending created, ties by ascending id
// ---------------------------------------------------------------------

#[tokio::test]
async fn list_filter_composes_parent_status_label_and_limit_with_and_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::open(dir.path().to_path_buf())
        .await
        .unwrap();

    let parent = SessionId::new();
    store.create(meta_for(parent)).await.unwrap();
    let other_parent = SessionId::new();
    store.create(meta_for(other_parent)).await.unwrap();

    let match_a = SessionId::new();
    let match_b = SessionId::new();
    let wrong_status = SessionId::new();
    let wrong_label = SessionId::new();
    let wrong_parent = SessionId::new();

    let mut m = meta_full(match_a, ts_plus(10), Some(fork_origin(parent)));
    m.labels = vec!["x".into()];
    store.create(m).await.unwrap();

    let mut m = meta_full(match_b, ts_plus(20), Some(fork_origin(parent)));
    m.labels = vec!["x".into(), "y".into()];
    store.create(m).await.unwrap();

    let mut m = meta_full(wrong_status, ts_plus(30), Some(fork_origin(parent)));
    m.labels = vec!["x".into()];
    m.status = SessionStatus::Completed;
    store.create(m).await.unwrap();

    let mut m = meta_full(wrong_label, ts_plus(40), Some(fork_origin(parent)));
    m.labels = vec!["z".into()];
    store.create(m).await.unwrap();

    let mut m = meta_full(wrong_parent, ts_plus(50), Some(fork_origin(other_parent)));
    m.labels = vec!["x".into()];
    store.create(m).await.unwrap();

    let filter = SessionFilter {
        agent_def: None,
        label: Some("x".into()),
        status: Some(SessionStatus::Active),
        parent: Some(parent),
        limit: None,
        include_ephemeral: false,
    };
    let metas = store.list(filter.clone()).await.unwrap();
    let ordered: Vec<SessionId> = metas.iter().map(|m| m.id).collect();
    // Descending created: match_b (t+20) before match_a (t+10); nothing
    // else satisfies parent AND status AND label all at once.
    assert_eq!(ordered, vec![match_b, match_a]);

    let limited = store
        .list(SessionFilter {
            limit: Some(1),
            ..filter
        })
        .await
        .unwrap();
    assert_eq!(limited.len(), 1);
    assert_eq!(
        limited[0].id, match_b,
        "limit must apply after filtering and ordering"
    );
}

#[tokio::test]
async fn list_orders_descending_created_with_ties_broken_by_ascending_id() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::open(dir.path().to_path_buf())
        .await
        .unwrap();

    let same_time = ts();
    let mut tied_ids = vec![SessionId::new(), SessionId::new(), SessionId::new()];
    for &id in &tied_ids {
        store.create(meta_full(id, same_time, None)).await.unwrap();
    }
    tied_ids.sort();

    let newest = SessionId::new();
    store
        .create(meta_full(newest, ts_plus(100), None))
        .await
        .unwrap();

    let metas = store.list(SessionFilter::default()).await.unwrap();
    assert_eq!(metas[0].id, newest, "the newest session must sort first");
    let tied_order: Vec<SessionId> = metas[1..4].iter().map(|m| m.id).collect();
    assert_eq!(
        tied_order, tied_ids,
        "sessions with identical `created` must tie-break by ascending id"
    );
}

// ---------------------------------------------------------------------
// create/fork update the in-memory index without a rebuild
// ---------------------------------------------------------------------

#[tokio::test]
async fn create_and_fork_update_the_in_memory_index_without_a_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = JsonlSessionStore::open(root.clone()).await.unwrap();

    let parent = SessionId::new();
    store.create(meta_for(parent)).await.unwrap();
    let child = SessionId::new();
    store
        .fork(&parent, LogSeq(0), meta_for(child))
        .await
        .unwrap();

    // Delete both session files: if `children`/`list` ever fell back to a
    // directory scan, they would now see nothing. Because `SessionIndex`
    // holds the headers purely in memory once recorded, deleting the
    // backing files must not change the result.
    std::fs::remove_file(root.join(format!("{parent}.jsonl"))).unwrap();
    std::fs::remove_file(root.join(format!("{child}.jsonl"))).unwrap();

    assert_eq!(store.children(&parent).await.unwrap(), vec![child]);
    let ids: HashSet<SessionId> = store
        .list(SessionFilter::default())
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert!(ids.contains(&parent) && ids.contains(&child));
}

// ---------------------------------------------------------------------
// index.jsonl is append-only
// ---------------------------------------------------------------------

#[tokio::test]
async fn index_jsonl_is_append_only_over_100_creations() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = JsonlSessionStore::open(root.clone()).await.unwrap();
    let idx_path = root.join("index.jsonl");

    let mut prev: Vec<u8> = Vec::new();
    for i in 0..100 {
        let sid = SessionId::new();
        store.create(meta_for(sid)).await.unwrap();
        let content = std::fs::read(&idx_path).unwrap();
        assert!(
            content.starts_with(&prev),
            "byte-prefix invariant violated after creation {i}"
        );
        prev = content;
    }

    let text = String::from_utf8(prev).unwrap();
    assert_eq!(text.lines().count(), 100);
}

// ---------------------------------------------------------------------
// Index I/O failure never fails create/fork
// ---------------------------------------------------------------------

#[tokio::test]
async fn index_io_failure_never_fails_create_and_emits_a_warn() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let store = JsonlSessionStore::open(root.clone()).await.unwrap();

    // Establish index.jsonl first.
    store.create(meta_for(SessionId::new())).await.unwrap();

    let idx_path = root.join("index.jsonl");
    let mut perms = std::fs::metadata(&idx_path).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&idx_path, perms).unwrap();

    let (log, _guard) = install_capture();
    let second = SessionId::new();
    let result = store.create(meta_for(second)).await;

    // Restore permissions immediately so tempdir cleanup never depends on
    // the assertions below.
    let mut perms = std::fs::metadata(&idx_path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&idx_path, perms).unwrap();

    assert!(
        result.is_ok(),
        "create must succeed even when index.jsonl can't be written: {result:?}"
    );
    assert!(
        log.contains("index append failed"),
        "expected a WARN about the failed index append, got: {:?}",
        log.entries.lock().unwrap()
    );

    // The session itself was durably created despite the index failure.
    let got = store.meta(&second).await.unwrap();
    assert_eq!(got.id, second);
}

// ---------------------------------------------------------------------
// Tree reconstruction purely from children() calls
// ---------------------------------------------------------------------

#[tokio::test]
async fn three_level_fork_tree_reconstructable_from_children_calls() {
    let dir = tempfile::tempdir().unwrap();
    let store = JsonlSessionStore::open(dir.path().to_path_buf())
        .await
        .unwrap();

    let root_id = SessionId::new();
    store.create(meta_for(root_id)).await.unwrap();

    let mut level1 = Vec::new();
    for _ in 0..2 {
        let c = SessionId::new();
        store.fork(&root_id, LogSeq(0), meta_for(c)).await.unwrap();
        level1.push(c);
    }

    let mut level2: HashMap<SessionId, Vec<SessionId>> = HashMap::new();
    for &p in &level1 {
        let mut kids = Vec::new();
        for _ in 0..2 {
            let c = SessionId::new();
            store.fork(&p, LogSeq(0), meta_for(c)).await.unwrap();
            kids.push(c);
        }
        level2.insert(p, kids);
    }

    let got_level1: HashSet<SessionId> = store
        .children(&root_id)
        .await
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(got_level1, level1.iter().copied().collect());

    for &p in &level1 {
        let got: HashSet<SessionId> = store.children(&p).await.unwrap().into_iter().collect();
        assert_eq!(got, level2[&p].iter().copied().collect());
    }

    for kids in level2.values() {
        for &leaf in kids {
            assert!(
                store.children(&leaf).await.unwrap().is_empty(),
                "a leaf session must have no children"
            );
        }
    }
}
