//! Curator stage seam tests (D1-8 / Unit 2).
//!
//! Proves the pre-assembly curator seam end to end at the unit level:
//!
//! 1. Zero-cost: no curator -> the stage is a pass-through (the closure that
//!    builds `CurateCtx` is NEVER called), and the path returns unchanged.
//!    The end-to-end byte-identity proof is `context_golden` 11/11
//!    unregenerated; this is the per-stage unit proof.
//! 2. Unchanged: a no-op curator returns the original path.
//! 3. Derived (omit): a curator calling `base.derive(&[PathOp::Omit{...}])`
//!    yields a path missing that node.
//! 4. Failed: a curator returning `Failed { reason }` -> original path used.
//! 5. Read surface (§11.5): a curator reads a FOREIGN session via
//!    `ctx.store.read` and `ctx.resolver.resolve_prefix` -- cross-session
//!    read is live.
//! 6. Composition: two curators; the second curates the first's derived path.
//! 7. GP-03 default: a minimal `Plugin` impl (only `manifest` + `tools`)
//!    returns an empty `curators()`.
//!
//! This file exercises the runtime STAGE (`apply_curator`/
//! `run_curator_stage`) with one curator at a time. `compose_curators`'s
//! `None`/`Some(one)`/`Some(chain)` branches and `ComposedCurator`'s chaining
//! are private to the `conway` crate and are covered by its in-crate
//! `compose_curators_tests` module (`crates/conway/src/builder.rs`) --
//! test 6 below hand-chains two curators through the stage to prove the
//! staging property, which is a different claim from proving the composer.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;

use conway_core::content::{ContentBlock, StopReason, Usage};
use conway_core::ids::{AgentId, LogSeq, ModelId, SeqRange, SessionId};
use conway_core::log::{LogRecord, SessionMeta};
use conway_core::path::{
    NodeProvenance, NodeStamp, PathNode, PathOp, RecordRef, ResolvedPath, Selector, ValidatedPath,
};
// `SessionStore` must be in scope for `FakeStore`'s `create`/`append`/`head`
// (they are trait methods, not inherent ones).
use conway_core::ports::{CurateCtx, CurateOutcome, Curator, Plugin, PluginManifest, SessionStore};
use conway_core::provenance::Provenance;
use conway_core::transcript::TranscriptResolver;
use conway_runtime::context::curator_stage::{apply_curator, run_curator_stage};
use conway_testkit::FakeStore;

// ---------------------------------------------------------------------
// Fixed identifiers (stable, matching the golden file's convention).
// ---------------------------------------------------------------------

fn agent() -> AgentId {
    "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap()
}

fn session() -> SessionId {
    "01ARZ3NDEKTSV4RRFFQ69G5FBV".parse().unwrap()
}

fn foreign_session() -> SessionId {
    "01ARZ3NDEKTSV4RRFFQ69G5FBW".parse().unwrap()
}

fn ts() -> chrono::DateTime<chrono::Utc> {
    "2026-07-20T00:00:00Z".parse().unwrap()
}

/// An `Assistant` record, built with the SAME field set `context_golden`'s
/// fixtures use (`model`/`route_reason`/`usage`/`stop` are inline fields on
/// the variant -- there is no `prov` field on `Assistant`).
fn assistant_record(seq: LogSeq, text: &str) -> LogRecord {
    LogRecord::Assistant {
        seq,
        ts: ts(),
        content: vec![ContentBlock::Text { text: text.into() }],
        model: "anthropic/claude-sonnet-4-6".parse().unwrap(),
        route_reason: serde_json::json!({"AliasPrimary": {"alias": "coder"}}),
        usage: Usage::default(),
        stop: StopReason::EndTurn,
    }
}

/// One `(PathNode, Arc<LogRecord>)` pair, the shape `ResolvedPath.nodes`
/// carries. `Selector::DefaultRule` + fixed `ts()` for determinism.
fn node(
    session: SessionId,
    seq: LogSeq,
    stamp: NodeStamp,
    record: LogRecord,
) -> (PathNode, Arc<LogRecord>) {
    (
        PathNode {
            record: RecordRef { session, seq },
            stamp,
            prov: NodeProvenance {
                selected_by: Selector::DefaultRule,
                at: ts(),
            },
        },
        Arc::new(record),
    )
}

/// A two-node path: a user turn followed by an assistant turn. Both are
/// `Head`-stamped own records of `session()`.
fn two_node_path() -> ResolvedPath {
    ResolvedPath {
        nodes: vec![
            node(
                session(),
                LogSeq(0),
                NodeStamp::Head,
                LogRecord::UserTurn {
                    seq: LogSeq(0),
                    ts: ts(),
                    text: "Please review src/lib.rs".into(),
                    prov: Provenance::UserPrompt,
                },
            ),
            node(
                session(),
                LogSeq(1),
                NodeStamp::Head,
                assistant_record(LogSeq(1), "Looking now."),
            ),
        ],
    }
}

/// Build a `CurateCtx` over `store` + a fresh resolver, naming `session()`
/// and `agent()` as the turn's own identity.
fn curate_ctx(store: Arc<FakeStore>, resolver: Arc<TranscriptResolver>) -> CurateCtx {
    CurateCtx {
        agent_id: agent(),
        session_id: session(),
        turn: 3,
        model: Some(ModelId::new("claude-sonnet-4-6")),
        store: store as Arc<dyn conway_core::ports::SessionStore>,
        resolver,
    }
}

/// A store with the current session seeded with the two-node-path records,
/// so a resolver walk over `session()` is non-empty. `async` (rather than a
/// `block_on` helper) because every caller is already a `#[tokio::test]`,
/// and blocking inside a Tokio runtime would panic.
async fn seeded_store() -> Arc<FakeStore> {
    let store = Arc::new(FakeStore::new());
    seed_session(&store, session(), "Please review src/lib.rs").await;
    store
}

async fn seed_session(store: &FakeStore, sid: SessionId, first_turn: &str) {
    store
        .create(SessionMeta {
            id: sid,
            agent_id: agent(),
            origin: None,
            agent_def: None,
            role: None,
            created: Utc::now(),
            cwd: PathBuf::from("/tmp"),
            labels: vec![],
            ephemeral: false,
            ask_origin: None,
            root: None,
            plugin_config: conway_core::ports::PluginConfig::default(),
        })
        .await
        .unwrap();
    let seq = store.head(&sid).await.unwrap();
    store
        .append(
            &sid,
            LogRecord::UserTurn {
                seq,
                ts: ts(),
                text: first_turn.to_string(),
                prov: Provenance::UserPrompt,
            },
        )
        .await
        .unwrap();
    let seq = store.head(&sid).await.unwrap();
    store
        .append(&sid, assistant_record(seq, "Looking now."))
        .await
        .unwrap();
}

// ──────────────────────────────────────────────────────────────────────
// Test curators
// ──────────────────────────────────────────────────────────────────────

/// A no-op curator: always `Unchanged`.
struct NoopCurator;

#[async_trait]
impl Curator for NoopCurator {
    async fn curate(&self, _ctx: &CurateCtx, _base: &ValidatedPath) -> CurateOutcome {
        CurateOutcome::Unchanged
    }
}

/// A curator that omits the SECOND base node (the assistant turn). Proves the
/// `Derived` path: the result has one node, the user turn, and the omitted
/// assistant turn is gone.
struct OmitSecondNodeCurator {
    /// Filled in by `curate` so the test can assert it ran.
    ran: Mutex<bool>,
}

#[async_trait]
impl Curator for OmitSecondNodeCurator {
    async fn curate(&self, _ctx: &CurateCtx, base: &ValidatedPath) -> CurateOutcome {
        *self.ran.lock().unwrap() = true;
        // The second node's RecordRef.
        let target = base.nodes().nth(1).expect("base has >=2 nodes").0.record;
        match base.derive(&[PathOp::Omit { node: target }]) {
            Ok(derivation) => CurateOutcome::Derived(derivation),
            Err(err) => CurateOutcome::Failed {
                reason: format!("derive failed: {err}"),
            },
        }
    }
}

/// A curator that always fails with a fixed reason.
struct FailingCurator {
    reason: String,
}

#[async_trait]
impl Curator for FailingCurator {
    async fn curate(&self, _ctx: &CurateCtx, _base: &ValidatedPath) -> CurateOutcome {
        CurateOutcome::Failed {
            reason: self.reason.clone(),
        }
    }
}

/// A curator that reads a FOREIGN session via `ctx.store.read` and records
/// what it saw. Proves the §11.5 cross-session read surface is live.
struct ForeignReadCurator {
    /// The foreign session to read.
    foreign: SessionId,
    /// The records read from the foreign session (set by `curate`).
    seen: Mutex<Option<Vec<LogRecord>>>,
    /// Whether `ctx.resolver.resolve_prefix` succeeded for the foreign session.
    resolver_ok: Mutex<bool>,
}

#[async_trait]
impl Curator for ForeignReadCurator {
    async fn curate(&self, ctx: &CurateCtx, _base: &ValidatedPath) -> CurateOutcome {
        // Cross-session record read (§11.5): read the FOREIGN session's
        // records directly via the store.
        let records = ctx
            .store
            .read(&self.foreign, SeqRange::full())
            .await
            .expect("foreign session exists");
        *self.seen.lock().unwrap() = Some(records);

        // And via the resolver's effective-transcript walk -- proves the
        // resolver can reach across sessions too.
        let head = ctx.store.head(&self.foreign).await.unwrap();
        let resolved = ctx
            .resolver
            .resolve_prefix(ctx.store.as_ref(), &self.foreign, head)
            .await;
        *self.resolver_ok.lock().unwrap() = resolved.is_ok();

        // This curator does NOT produce a cross-tree Derived (that's Unit 3);
        // it only proves the read surface is live. Decline to act.
        CurateOutcome::Unchanged
    }
}

// ──────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn zero_cost_no_curator_passes_path_through_without_building_ctx() {
    // No curator -> the closure that builds CurateCtx is NEVER called.
    // A panic inside the closure proves it was not invoked, and the path
    // returns byte-identical to the input.
    let path = two_node_path();
    let sentinel = Arc::new(Mutex::new(false));
    let sentinel_clone = sentinel.clone();
    let (out, failed) = apply_curator(None, path.clone(), || {
        *sentinel_clone.lock().unwrap() = true;
        panic!("build_ctx must not be called when no curator is installed");
    })
    .await
    .unwrap();

    assert!(!*sentinel.lock().unwrap(), "ctx builder was called");
    assert_eq!(failed, None, "no curator ran, so nothing to record");
    assert_eq!(out.nodes.len(), path.nodes.len());
    // Byte-identity: same node records in the same order.
    for (a, b) in out.nodes.iter().zip(path.nodes.iter()) {
        assert_eq!(a.0.record, b.0.record);
        assert_eq!(a.0.stamp, b.0.stamp);
    }
}

#[tokio::test]
async fn unchanged_noop_curator_returns_original_path() {
    let store = seeded_store().await;
    let resolver = Arc::new(TranscriptResolver::new(64));
    let ctx = curate_ctx(store, resolver);
    let path = two_node_path();

    let noop: Arc<dyn Curator> = Arc::new(NoopCurator);
    let (out, failed) = run_curator_stage(&noop, &ctx, path.clone()).await;

    assert_eq!(failed, None, "an Unchanged curator records no failure");
    assert_eq!(out.nodes.len(), path.nodes.len());
    for (a, b) in out.nodes.iter().zip(path.nodes.iter()) {
        assert_eq!(
            a.0.record, b.0.record,
            "unchanged path must keep every node"
        );
    }
}

#[tokio::test]
async fn derived_omit_drops_the_target_node_and_keeps_the_rest() {
    let store = seeded_store().await;
    let resolver = Arc::new(TranscriptResolver::new(64));
    let ctx = curate_ctx(store, resolver);
    let path = two_node_path();
    let omitted_ref = path.nodes[1].0.record;
    let kept_ref = path.nodes[0].0.record;

    let curator = Arc::new(OmitSecondNodeCurator {
        ran: Mutex::new(false),
    });
    let curator_dyn: Arc<dyn Curator> = curator.clone();
    let (out, failed) = run_curator_stage(&curator_dyn, &ctx, path).await;

    assert!(*curator.ran.lock().unwrap(), "curator ran");
    assert_eq!(failed, None, "a successful derive records no failure");
    assert_eq!(out.nodes.len(), 1, "omitted node is gone");
    assert_eq!(out.nodes[0].0.record, kept_ref, "the first node is kept");
    assert_ne!(
        out.nodes[0].0.record, omitted_ref,
        "the omitted node is absent"
    );
}

#[tokio::test]
async fn failed_curator_returns_original_path() {
    let store = seeded_store().await;
    let resolver = Arc::new(TranscriptResolver::new(64));
    let ctx = curate_ctx(store, resolver);
    let path = two_node_path();
    let original_len = path.nodes.len();

    let curator = Arc::new(FailingCurator {
        reason: "synthetic failure".to_string(),
    });
    let curator: Arc<dyn Curator> = curator;
    let (out, failed) = run_curator_stage(&curator, &ctx, path.clone()).await;

    // §11.6: the reason is returned so it reaches the durable ContextReport,
    // not just a log line.
    assert_eq!(failed.as_deref(), Some("synthetic failure"));
    // §11.6: fail-open -- the uncurated path is used.
    assert_eq!(
        out.nodes.len(),
        original_len,
        "failed curator uses the original path"
    );
    for (a, b) in out.nodes.iter().zip(path.nodes.iter()) {
        assert_eq!(a.0.record, b.0.record);
    }
}

#[tokio::test]
async fn read_surface_can_read_a_foreign_session_via_store_and_resolver() {
    let store = Arc::new(FakeStore::new());
    // Seed BOTH the current session and a foreign session.
    seed_session(&store, session(), "current turn").await;
    seed_session(&store, foreign_session(), "a foreign lesson").await;

    let resolver = Arc::new(TranscriptResolver::new(64));
    let ctx = curate_ctx(store.clone(), resolver);
    let path = two_node_path();

    let curator = Arc::new(ForeignReadCurator {
        foreign: foreign_session(),
        seen: Mutex::new(None),
        resolver_ok: Mutex::new(false),
    });
    let curator_dyn: Arc<dyn Curator> = curator.clone();
    let _out = run_curator_stage(&curator_dyn, &ctx, path).await;

    let seen = curator
        .seen
        .lock()
        .unwrap()
        .clone()
        .expect("curator read foreign records");
    // The foreign session has two records (UserTurn + Assistant).
    assert_eq!(seen.len(), 2, "foreign session read returned its records");
    let first = &seen[0];
    let text = match first {
        LogRecord::UserTurn { text, .. } => text.clone(),
        _ => String::new(),
    };
    assert_eq!(
        text, "a foreign lesson",
        "the foreign session's own records are reachable"
    );

    assert!(
        *curator.resolver_ok.lock().unwrap(),
        "resolver.resolve_prefix reached the foreign session"
    );
}

#[tokio::test]
async fn composition_second_curator_curates_firsts_derived_path() {
    // Two curators: the first omits node[1] (the assistant turn); the second
    // omits whatever is NOW node[1] in the first curator's derived path.
    // After the first curator, the path has one node (node[0]); the second
    // curator's base has only one node, so its `nth(1)` would fail -- to make
    // the composition observable, the second curator instead omits node[0]
    // of its base and records how many nodes it saw.
    let store = seeded_store().await;
    let resolver = Arc::new(TranscriptResolver::new(64));
    let ctx = curate_ctx(store, resolver);
    let path = two_node_path();

    // First curator: omit the assistant turn (node[1]).
    let first = Arc::new(OmitSecondNodeCurator {
        ran: Mutex::new(false),
    });

    // Second curator: records its base length and omits its FIRST node.
    struct OmitFirstAndRecord {
        base_len_seen: Mutex<usize>,
    }
    #[async_trait]
    impl Curator for OmitFirstAndRecord {
        async fn curate(&self, _ctx: &CurateCtx, base: &ValidatedPath) -> CurateOutcome {
            let len = base.nodes().count();
            *self.base_len_seen.lock().unwrap() = len;
            let target = base.nodes().next().expect("non-empty base").0.record;
            match base.derive(&[PathOp::Omit { node: target }]) {
                Ok(derivation) => CurateOutcome::Derived(derivation),
                Err(err) => CurateOutcome::Failed {
                    reason: format!("derive failed: {err}"),
                },
            }
        }
    }

    let second = Arc::new(OmitFirstAndRecord {
        base_len_seen: Mutex::new(0),
    });

    // Compose manually (the ComposedCurator lives in the conway crate; here
    // we chain by hand to prove the staging composition property: the
    // second curator's base is the first's derived path).
    let first_dyn: Arc<dyn Curator> = first.clone();
    let second_dyn: Arc<dyn Curator> = second.clone();
    let (after_first, _) = run_curator_stage(&first_dyn, &ctx, path.clone()).await;
    assert!(*first.ran.lock().unwrap());
    assert_eq!(after_first.nodes.len(), 1, "first curator dropped node[1]");

    let (after_second, _) = run_curator_stage(&second_dyn, &ctx, after_first).await;
    assert_eq!(
        *second.base_len_seen.lock().unwrap(),
        1,
        "second curator saw the first's derived (1-node) path as its base"
    );
    assert_eq!(
        after_second.nodes.len(),
        0,
        "second curator dropped the remaining node"
    );
}

// ──────────────────────────────────────────────────────────────────────
// GP-03 default: a minimal Plugin impl returns empty curators()
// ──────────────────────────────────────────────────────────────────────

struct MinimalPlugin;

#[async_trait]
impl Plugin for MinimalPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "conway.test.minimal".into(),
            version: "0.0.0".into(),
            // No tools, no host capabilities required.
            tools: vec![],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn conway_core::ports::Tool>> {
        vec![]
    }
}

#[test]
fn gp03_default_plugin_returns_empty_curators() {
    // A plugin that implements ONLY `manifest` + `tools` (the two required
    // methods) keeps compiling and returns an empty `curators()` -- the
    // zero-cost default GP-03 establishes for every provided method.
    let plugin = MinimalPlugin;
    assert!(plugin.curators().is_empty(), "default curators() is empty");
    assert!(
        plugin.context_hooks().is_empty(),
        "default context_hooks() is empty too"
    );
}

/// A curator that panics, the way a real one plausibly would: an `.expect()`
/// against a foreign record list that turned out shorter than assumed.
struct PanickingCurator;

#[async_trait]
impl Curator for PanickingCurator {
    async fn curate(&self, _ctx: &CurateCtx, base: &ValidatedPath) -> CurateOutcome {
        let _ = base.nodes().nth(99).expect("curator assumed a longer path");
        CurateOutcome::Unchanged
    }
}

#[tokio::test]
async fn a_panicking_curator_is_contained_and_recorded_not_propagated() {
    // §11.6: "a curator that errors, PANICS, or returns `Failed` is contained
    // and recorded, and the turn proceeds on the uncurated path." Without
    // containment the unwind would reach the `tokio::spawn` boundary and the
    // supervisor would synthesize a panic result for the WHOLE agent -- a far
    // larger blast radius than the uncurated turn §11.6 promises.
    let store = seeded_store().await;
    let resolver = Arc::new(TranscriptResolver::new(64));
    let ctx = curate_ctx(store, resolver);
    let path = two_node_path();
    let original_len = path.nodes.len();

    let curator: Arc<dyn Curator> = Arc::new(PanickingCurator);
    let (out, failed) = run_curator_stage(&curator, &ctx, path).await;

    assert_eq!(
        out.nodes.len(),
        original_len,
        "the turn proceeds on the uncurated path"
    );
    assert_eq!(
        failed.as_deref(),
        Some("curator panicked"),
        "the panic is RECORDED, not silently swallowed"
    );
}
