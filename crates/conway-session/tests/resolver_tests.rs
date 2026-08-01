//! Integration tests for `TranscriptResolver` (WI-049 criteria): root
//! resolution, prefix + own concatenation across a fork chain, transitivity
//! against an independently-written reference implementation, sibling `Arc`
//! sharing, memoization, bounded-LRU eviction, the fork-snapshot invariant,
//! cycle/depth corrupt-ancestry detection, `Send`-ness, and a random
//! fork-tree property test.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use proptest::prelude::*;

use conway_core::error::StoreError;
use conway_core::ids::{AgentId, LogSeq, SessionId};
use conway_core::log::{LogRecord, SessionStatus};
use conway_core::ports::SessionStore;
use conway_core::provenance::Provenance;
use conway_session::{
    FsyncPolicy, JsonlSessionStore, SessionMeta, StoreConfig, TranscriptResolver,
};

fn ts() -> DateTime<Utc> {
    "2026-07-20T00:00:00Z".parse().unwrap()
}

fn meta_for(id: SessionId) -> SessionMeta {
    SessionMeta {
        id,
        agent_id: AgentId::new(),
        origin: None,
        agent_def: None,
        role: None,
        created: ts(),
        cwd: PathBuf::from("/tmp/project"),
        labels: vec![],
        status: SessionStatus::Active,
        ephemeral: false,
        ask_origin: None,
        root: None,
    }
}

fn user_turn(text: &str) -> LogRecord {
    LogRecord::UserTurn {
        seq: LogSeq(0), // overwritten by `append`; the store is the seq authority.
        ts: ts(),
        text: text.into(),
        prov: Provenance::UserPrompt,
    }
}

fn context_mask(target_seq: LogSeq, excluded: bool) -> LogRecord {
    LogRecord::ContextMask {
        seq: LogSeq(0), // overwritten by `append`; the store is the seq authority.
        ts: ts(),
        target_seq,
        excluded,
    }
}

/// Appends a `ContextMask` and returns it with the seq the store actually
/// assigned (mirrors `append_n`'s pattern) -- the mask record itself is
/// left in `resolve_prefix`'s output (same precedent as
/// `ContextReportRecord`, which already flows through unfiltered and is
/// dropped downstream by kind, not by the resolver), so tests need the
/// exact record to build their expected transcript.
async fn append_mask(
    store: &JsonlSessionStore,
    sid: &SessionId,
    target_seq: LogSeq,
    excluded: bool,
) -> LogRecord {
    let seq = store
        .append(sid, context_mask(target_seq, excluded))
        .await
        .unwrap();
    LogRecord::ContextMask {
        seq,
        ts: ts(),
        target_seq,
        excluded,
    }
}

/// `Never`-fsync store: these tests care about resolution logic, not
/// durability, so skip the fsync-policy machinery entirely.
async fn open_store(root: &std::path::Path) -> JsonlSessionStore {
    JsonlSessionStore::open_with(
        root.to_path_buf(),
        StoreConfig {
            fsync: FsyncPolicy::Never,
            ..StoreConfig::default()
        },
    )
    .await
    .unwrap()
}

/// Appends `n` distinguishable records (labeled `{label}-{i}`) and returns
/// them with the seq the store actually assigned, so tests can build exact
/// expected transcripts without duplicating the store's seq-assignment
/// logic.
async fn append_n(
    store: &JsonlSessionStore,
    sid: &SessionId,
    label: &str,
    n: usize,
) -> Vec<LogRecord> {
    let mut recs = Vec::with_capacity(n);
    for i in 0..n {
        let mut rec = user_turn(&format!("{label}-{i}"));
        let seq = store.append(sid, rec.clone()).await.unwrap();
        if let LogRecord::UserTurn { seq: s, .. } = &mut rec {
            *s = seq;
        }
        recs.push(rec);
    }
    recs
}

// ---------------------------------------------------------------------
// root resolution
// ---------------------------------------------------------------------

#[tokio::test]
async fn root_session_resolves_to_its_own_records_in_seq_order() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let resolver = TranscriptResolver::new(8);

    let sid = SessionId::new();
    store.create(meta_for(sid)).await.unwrap();
    let expected = append_n(&store, &sid, "root", 5).await;

    let resolved = resolver.resolve(&store, &sid).await.unwrap();
    assert_eq!(&*resolved, expected.as_slice());
}

#[tokio::test]
async fn root_session_with_zero_records_resolves_to_empty() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let resolver = TranscriptResolver::new(8);

    let sid = SessionId::new();
    store.create(meta_for(sid)).await.unwrap();

    let resolved = resolver.resolve(&store, &sid).await.unwrap();
    assert!(resolved.is_empty());
}

// ---------------------------------------------------------------------
// context mask (WI-125): excluded from resolve_prefix output, never
// deleted from the raw log; persists across a store reopen; inherited by
// a fork up to the fork point.
// ---------------------------------------------------------------------

#[tokio::test]
async fn masked_record_is_omitted_from_resolved_transcript_but_present_in_raw_log() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let resolver = TranscriptResolver::new(8);

    let sid = SessionId::new();
    store.create(meta_for(sid)).await.unwrap();
    let records = append_n(&store, &sid, "r", 3).await;
    let target = records[1].seq().unwrap();
    let mask = append_mask(&store, &sid, target, true).await;

    let resolved = resolver.resolve(&store, &sid).await.unwrap();
    assert_eq!(
        &*resolved,
        &[records[0].clone(), records[2].clone(), mask.clone()],
        "the masked record must be omitted from the resolved transcript"
    );

    // The raw log is untouched: all 3 original records plus the mask
    // record itself, in append order, still readable and inspectable.
    let raw = store
        .read(&sid, conway_core::ids::SeqRange::full())
        .await
        .unwrap();
    assert_eq!(
        raw.len(),
        4,
        "masking must not delete anything from the log"
    );
    assert_eq!(&raw[0..3], records.as_slice());
    assert_eq!(raw[3], mask);
}

#[tokio::test]
async fn a_later_unmask_reverses_an_earlier_mask() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let resolver = TranscriptResolver::new(8);

    let sid = SessionId::new();
    store.create(meta_for(sid)).await.unwrap();
    let records = append_n(&store, &sid, "r", 2).await;
    let target = records[0].seq().unwrap();

    let mask = append_mask(&store, &sid, target, true).await;
    let masked = resolver.resolve(&store, &sid).await.unwrap();
    assert_eq!(masked.as_ref(), &[records[1].clone(), mask.clone()]);

    let unmask = append_mask(&store, &sid, target, false).await;
    let unmasked = resolver
        .resolve_prefix(&store, &sid, store.head(&sid).await.unwrap())
        .await
        .unwrap();
    assert_eq!(
        unmasked.as_ref(),
        &[records[0].clone(), records[1].clone(), mask, unmask,],
        "un-masking (a second ContextMask with excluded: false) must restore the record"
    );
}

#[tokio::test]
async fn mask_persists_across_a_store_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let sid = SessionId::new();
    let target;
    let records;
    let mask;
    {
        let store = open_store(&root).await;
        store.create(meta_for(sid)).await.unwrap();
        records = append_n(&store, &sid, "r", 3).await;
        target = records[1].seq().unwrap();
        mask = append_mask(&store, &sid, target, true).await;
    } // dropped: every in-memory handle discarded, next open is cold.

    let store = open_store(&root).await;
    let resolver = TranscriptResolver::new(8);
    let resolved = resolver.resolve(&store, &sid).await.unwrap();
    assert_eq!(
        resolved.as_ref(),
        &[records[0].clone(), records[2].clone(), mask],
        "the mask must survive a store reopen, not just live in memory"
    );
}

#[tokio::test]
async fn fork_inherits_parents_mask_state_up_to_the_fork_point() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let resolver = TranscriptResolver::new(8);

    let parent = SessionId::new();
    store.create(meta_for(parent)).await.unwrap();
    let parent_records = append_n(&store, &parent, "p", 3).await;
    let target = parent_records[1].seq().unwrap();
    let mask = append_mask(&store, &parent, target, true).await;

    // Mask a *second* record only after the fork point, so the fork
    // snapshot must NOT pick it up (matches the module's existing
    // local-bound/snapshot semantics for ordinary appends).
    let at_seq = store.head(&parent).await.unwrap();
    let child = SessionId::new();
    store.fork(&parent, at_seq, meta_for(child)).await.unwrap();
    append_mask(&store, &parent, parent_records[2].seq().unwrap(), true).await;

    let child_records = append_n(&store, &child, "c", 2).await;

    let resolved = resolver.resolve(&store, &child).await.unwrap();
    let mut expected = vec![parent_records[0].clone(), parent_records[2].clone(), mask];
    expected.extend(child_records.iter().cloned());
    assert_eq!(
        resolved.as_ref(),
        expected.as_slice(),
        "the child inherits the parent's mask state as of the fork point (record 1 masked), \
         but not a mask the parent appends afterward (record 2 stays visible)"
    );
}

// ---------------------------------------------------------------------
// prefix + own concatenation across a 3-level chain, distinct records at
// every level (grandparent inherited-only, parent inherited+own, child
// own)
// ---------------------------------------------------------------------

#[tokio::test]
async fn three_level_chain_matches_parent_prefix_plus_own_element_wise() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let resolver = TranscriptResolver::new(8);

    let grandparent = SessionId::new();
    store.create(meta_for(grandparent)).await.unwrap();
    let gp_records = append_n(&store, &grandparent, "gp", 5).await;

    let parent = SessionId::new();
    let ga = LogSeq(3);
    store
        .fork(&grandparent, ga, meta_for(parent))
        .await
        .unwrap();
    let parent_records = append_n(&store, &parent, "p", 4).await;

    // Local units (F-049-1): at_seq indexes the PARENT's OWN records, and
    // the parent's inherited prefix always flows through in full — a fork
    // at the parent's local head (4) captures the parent's entire
    // effective transcript (3 inherited + 4 own), per GP-02.
    let child = SessionId::new();
    let pa = LogSeq(4);
    store.fork(&parent, pa, meta_for(child)).await.unwrap();
    let child_records = append_n(&store, &child, "c", 3).await;

    let mut expected: Vec<LogRecord> = gp_records[0..3].to_vec();
    expected.extend(parent_records.iter().cloned());
    expected.extend(child_records.iter().cloned());
    assert_eq!(expected.len(), 10);

    let resolved = resolver.resolve(&store, &child).await.unwrap();
    assert_eq!(&*resolved, expected.as_slice());

    // The inherited prefix, element-wise, equals the parent's resolved
    // prefix at the fork's own (local) boundary: 3 inherited + 4 own = 7.
    let parent_prefix = resolver.peek_prefix(&parent, pa).unwrap();
    assert_eq!(parent_prefix.len(), 7);
    assert_eq!(&resolved[0..7], &*parent_prefix);
}

// ---------------------------------------------------------------------
// transitivity: depth-5 chain vs. an independently-written reference
// implementation
// ---------------------------------------------------------------------

/// Independently-written (from the WI-049 spec's own description, not the
/// production code) reference model: given every session's own records and
/// origin, compute the effective transcript recursively.
struct RefSession {
    origin: Option<(SessionId, u64)>,
    own: Vec<LogRecord>,
}

fn reference_effective(
    sessions: &HashMap<SessionId, RefSession>,
    sid: SessionId,
) -> Vec<LogRecord> {
    let s = sessions.get(&sid).expect("session in model");
    match s.origin {
        None => s.own.clone(),
        Some((parent, at_seq)) => {
            let mut result = reference_prefix(sessions, parent, at_seq);
            result.extend(s.own.iter().cloned());
            result
        }
    }
}

fn reference_prefix(
    sessions: &HashMap<SessionId, RefSession>,
    sid: SessionId,
    upto: u64,
) -> Vec<LogRecord> {
    // LOCAL units (F-049-1): the whole inherited prefix flows through,
    // then this session's OWN records up to `upto`.
    let s = sessions.get(&sid).expect("session in model");
    let mut result = match s.origin {
        None => Vec::new(),
        Some((parent, at_seq)) => reference_prefix(sessions, parent, at_seq),
    };
    let n = (upto as usize).min(s.own.len());
    result.extend(s.own[..n].iter().cloned());
    result
}

#[tokio::test]
async fn depth_5_chain_matches_independent_reference_implementation() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let resolver = TranscriptResolver::new(16);

    let mut model: HashMap<SessionId, RefSession> = HashMap::new();
    let mut ids: Vec<SessionId> = Vec::new();

    let root = SessionId::new();
    store.create(meta_for(root)).await.unwrap();
    let root_own = append_n(&store, &root, "n0", 3).await;
    model.insert(
        root,
        RefSession {
            origin: None,
            own: root_own,
        },
    );
    ids.push(root);

    // Levels 1..=4, each forking from the immediately preceding level at a
    // different offset into the parent's own head, with a different own
    // record count — 5 distinct levels total, differing `at_seq` values.
    let specs: [(usize, u64); 4] = [(0, 2), (1, 3), (0, 1), (3, 4)];
    for (level, (parent_idx, own_count)) in specs.into_iter().enumerate() {
        let parent = ids[parent_idx];
        let parent_head = store.head(&parent).await.unwrap();
        let at_seq = LogSeq(parent_head.0.min((level as u64) + 1));
        let child = SessionId::new();
        store.fork(&parent, at_seq, meta_for(child)).await.unwrap();
        let own = append_n(
            &store,
            &child,
            &format!("n{}", level + 1),
            own_count as usize,
        )
        .await;
        model.insert(
            child,
            RefSession {
                origin: Some((parent, at_seq.0)),
                own,
            },
        );
        ids.push(child);
    }

    assert_eq!(ids.len(), 5, "depth-5 chain (root + 4 forks)");

    for &sid in &ids {
        let expected = reference_effective(&model, sid);
        let resolved = resolver.resolve(&store, &sid).await.unwrap();
        assert_eq!(
            &*resolved,
            expected.as_slice(),
            "mismatch for session {sid}"
        );
    }
}

// ---------------------------------------------------------------------
// sibling sharing: two forks from the same parent at the same at_seq share
// one parent-prefix Arc allocation
// ---------------------------------------------------------------------

#[tokio::test]
async fn siblings_at_the_same_fork_point_share_one_parent_prefix_arc() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let resolver = TranscriptResolver::new(8);

    let parent = SessionId::new();
    store.create(meta_for(parent)).await.unwrap();
    append_n(&store, &parent, "p", 5).await;
    let at_seq = LogSeq(3);

    let child_a = SessionId::new();
    store
        .fork(&parent, at_seq, meta_for(child_a))
        .await
        .unwrap();
    append_n(&store, &child_a, "a", 2).await;

    let child_b = SessionId::new();
    store
        .fork(&parent, at_seq, meta_for(child_b))
        .await
        .unwrap();
    append_n(&store, &child_b, "b", 2).await;

    resolver.resolve(&store, &child_a).await.unwrap();
    let prefix_after_a = resolver.peek_prefix(&parent, at_seq).unwrap();

    resolver.resolve(&store, &child_b).await.unwrap();
    let prefix_after_b = resolver.peek_prefix(&parent, at_seq).unwrap();

    assert!(
        Arc::ptr_eq(&prefix_after_a, &prefix_after_b),
        "resolving a second sibling must reuse the memoized parent-prefix Arc, not reallocate it"
    );
}

// ---------------------------------------------------------------------
// memoization: a second resolve of the same (sid, at_seq) reads no more
// file bytes than the first
// ---------------------------------------------------------------------

#[tokio::test]
async fn second_resolve_of_the_same_session_performs_zero_additional_reads() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let sid = SessionId::new();
    {
        let store = open_store(&root).await;
        store.create(meta_for(sid)).await.unwrap();
        append_n(&store, &sid, "x", 5).await;
    } // dropped: every in-memory handle discarded, next open is cold.

    let store = open_store(&root).await;
    let resolver = TranscriptResolver::new(8);

    let first = resolver.resolve(&store, &sid).await.unwrap();
    let after_first = store.lines_scanned();
    assert!(
        after_first > 0,
        "the first resolve of a cold session must scan its file"
    );

    let second = resolver.resolve(&store, &sid).await.unwrap();
    assert_eq!(
        store.lines_scanned(),
        after_first,
        "the second resolve of the same (sid, at_seq) must perform zero additional file reads"
    );
    assert_eq!(first.as_ref(), second.as_ref());
}

// ---------------------------------------------------------------------
// bounded LRU: capacity 2, third distinct key evicts the first
// ---------------------------------------------------------------------

#[tokio::test]
async fn capacity_2_lru_evicts_the_first_key_and_recomputes_on_re_resolve() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let resolver = TranscriptResolver::new(2);

    let mut ids = Vec::new();
    let mut expected_lens = Vec::new();
    for i in 0..3 {
        let sid = SessionId::new();
        store.create(meta_for(sid)).await.unwrap();
        let recs = append_n(&store, &sid, &format!("s{i}"), 3).await;
        ids.push(sid);
        expected_lens.push(recs.len());
    }

    resolver.resolve(&store, &ids[0]).await.unwrap();
    let key0 = store.head(&ids[0]).await.unwrap();
    resolver.resolve(&store, &ids[1]).await.unwrap();
    resolver.resolve(&store, &ids[2]).await.unwrap();

    assert!(
        resolver.peek_prefix(&ids[0], key0).is_none(),
        "capacity-2 LRU must evict the first key once a third distinct key is inserted"
    );

    let re_resolved = resolver.resolve(&store, &ids[0]).await.unwrap();
    assert_eq!(re_resolved.len(), expected_lens[0]);
    assert!(
        resolver.peek_prefix(&ids[0], key0).is_some(),
        "re-resolving after eviction must recompute and repopulate the cache"
    );
}

// ---------------------------------------------------------------------
// snapshot: parent appends after fork never change resolve(child)
// ---------------------------------------------------------------------

#[tokio::test]
async fn parent_appends_after_fork_do_not_change_child_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let resolver = TranscriptResolver::new(8);

    let parent = SessionId::new();
    store.create(meta_for(parent)).await.unwrap();
    append_n(&store, &parent, "p", 5).await;

    let child = SessionId::new();
    let at_seq = LogSeq(3);
    store.fork(&parent, at_seq, meta_for(child)).await.unwrap();
    append_n(&store, &child, "c", 2).await;

    let before = resolver.resolve(&store, &child).await.unwrap();

    // Exercise the parent's own full-transcript cache entry too, both
    // before and after further parent appends.
    resolver.resolve(&store, &parent).await.unwrap();
    append_n(&store, &parent, "y", 5).await;
    resolver.resolve(&store, &parent).await.unwrap();

    let after = resolver.resolve(&store, &child).await.unwrap();
    assert_eq!(before.as_ref(), after.as_ref());
}

// ---------------------------------------------------------------------
// corrupt ancestry: cycle and depth-limit
// ---------------------------------------------------------------------

#[tokio::test]
async fn mutually_referencing_origins_return_corrupt_ancestry_instead_of_looping() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let resolver = TranscriptResolver::new(8);

    let a = SessionId::new();
    let b = SessionId::new();

    let mut meta_a = meta_for(a);
    meta_a.origin = Some(conway_core::log::ForkOrigin {
        parent: b,
        at_seq: LogSeq(0),
        mode: conway_core::log::SubagentMode::Fork,
    });
    let mut meta_b = meta_for(b);
    meta_b.origin = Some(conway_core::log::ForkOrigin {
        parent: a,
        at_seq: LogSeq(0),
        mode: conway_core::log::SubagentMode::Fork,
    });

    // `create` performs no origin validation — it just writes the header —
    // so this hand-crafted cycle can be constructed directly, bypassing
    // `fork`'s (unrelated) existence/range checks.
    store.create(meta_a).await.unwrap();
    store.create(meta_b).await.unwrap();

    let err = resolver.resolve(&store, &a).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Corrupt { .. }),
        "cyclic ancestry must be reported as StoreError::Corrupt, got {err:?}"
    );
}

#[tokio::test]
async fn ancestry_deeper_than_256_returns_corrupt_ancestry() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let resolver = TranscriptResolver::new(8);

    let mut prev = SessionId::new();
    store.create(meta_for(prev)).await.unwrap();
    for _ in 0..300 {
        let next = SessionId::new();
        store.fork(&prev, LogSeq(0), meta_for(next)).await.unwrap();
        prev = next;
    }

    let err = resolver.resolve(&store, &prev).await.unwrap_err();
    assert!(
        matches!(err, StoreError::Corrupt { .. }),
        "a 300-deep ancestry chain must be reported as StoreError::Corrupt (max depth 256), got {err:?}"
    );
}

// ---------------------------------------------------------------------
// resolve is async and Send: proven by compiling under tokio::spawn, which
// requires a 'static + Send future
// ---------------------------------------------------------------------

#[tokio::test]
async fn resolve_future_is_send() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(open_store(dir.path()).await);
    let resolver = Arc::new(TranscriptResolver::new(4));

    let sid = SessionId::new();
    store.create(meta_for(sid)).await.unwrap();
    append_n(&store, &sid, "x", 1).await;

    let store2 = Arc::clone(&store);
    let resolver2 = Arc::clone(&resolver);
    let handle = tokio::spawn(async move { resolver2.resolve(&*store2, &sid).await.unwrap() });
    let result = handle.await.unwrap();
    assert_eq!(result.len(), 1);
}

// ---------------------------------------------------------------------
// property test (≥128 cases): random fork trees vs. the independent
// reference implementation
// ---------------------------------------------------------------------

fn fork_tree_strategy() -> impl Strategy<Value = (usize, u8, Vec<(u32, u32, u8)>)> {
    (1usize..8, 0u8..5).prop_flat_map(|(n, root_own)| {
        proptest::collection::vec((any::<u32>(), any::<u32>(), 0u8..5u8), n.saturating_sub(1))
            .prop_map(move |specs| (n, root_own, specs))
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn resolve_matches_reference_for_random_fork_trees(case in fork_tree_strategy()) {
        let (_n, root_own, specs) = case;
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let dir = tempfile::tempdir().unwrap();
            let store = open_store(dir.path()).await;
            let resolver = TranscriptResolver::new(64);

            let mut model: HashMap<SessionId, RefSession> = HashMap::new();
            let mut ids: Vec<SessionId> = Vec::new();

            let root = SessionId::new();
            store.create(meta_for(root)).await.unwrap();
            let root_records = append_n(&store, &root, "n0", root_own as usize).await;
            model.insert(root, RefSession { origin: None, own: root_records });
            ids.push(root);

            for (i, (parent_pick, at_seq_raw, own_count)) in specs.into_iter().enumerate() {
                let node_idx = i + 1;
                let parent_pos = (parent_pick as usize) % ids.len();
                let parent = ids[parent_pos];
                let parent_head = store.head(&parent).await.unwrap();
                let at_seq = LogSeq(if parent_head.0 == 0 { 0 } else { at_seq_raw as u64 % (parent_head.0 + 1) });

                let child = SessionId::new();
                store.fork(&parent, at_seq, meta_for(child)).await.unwrap();
                let own = append_n(&store, &child, &format!("n{node_idx}"), own_count as usize).await;
                model.insert(child, RefSession { origin: Some((parent, at_seq.0)), own });
                ids.push(child);

                // Direct criterion check for this fork, independent of the
                // reference-model comparison below. Resolving the child
                // necessarily computes and memoizes (parent, at_seq) along
                // the way, so it's available via `peek_prefix` afterward.
                let resolved_child = resolver.resolve(&store, &child).await.unwrap();
                let parent_prefix = resolver.peek_prefix(&parent, at_seq).unwrap();
                // Local units (F-049-1): the inherited length is the
                // parent's full effective prefix at its local at_seq.
                let inherited_len = reference_prefix(&model, parent, at_seq.0).len();
                prop_assert_eq!(parent_prefix.len(), inherited_len);
                prop_assert_eq!(
                    resolved_child.len(),
                    inherited_len + model[&child].own.len()
                );
                prop_assert_eq!(&resolved_child[..inherited_len], &*parent_prefix);
            }

            for &sid in &ids {
                let expected = reference_effective(&model, sid);
                let resolved = resolver.resolve(&store, &sid).await.unwrap();
                prop_assert_eq!(resolved.as_ref(), expected.as_slice());
            }
            Ok(())
        })?;
    }
}
