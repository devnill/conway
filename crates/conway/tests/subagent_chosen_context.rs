//! End-to-end acceptance for `SubagentSpec::context` (board item
//! `01M0R06MY4TV010EVFG4KBD2CF`): the "eighth axis" `ForkSpec`/`SpawnSpec`
//! previously had no way to narrow -- a parent starting a child with a
//! CHOSEN context rather than an inherited one.
//!
//! Every wire-content assertion here reads `ScriptedBackend::calls()` --
//! the actual assembled `GenerateRequest` a turn sent -- never an
//! intermediate value (`ForkSpec::context` round-tripping through `Into`,
//! or a returned id), mirroring `compose_context_path_end_to_end.rs`'s own
//! discipline for the sibling, mid-chain capability this is the
//! boundary-time counterpart of.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::backend::{BackendId, GenerateRequest};
use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::plugin::{ContentBlock, SeqRange};
use conway::test_support::build_conway;
use conway::{
    ForkSpec, LogRecord, LogSeq, RecordRef, RoleAlias, SessionSpec, SessionStore, SpawnSpec,
};
use conway_testkit::{text_response, FakeStore, ScriptedBackend, ScriptedTurn};

fn base_config() -> ConwayConfig {
    let mut roles = BTreeMap::new();
    roles.insert(
        "default".to_string(),
        RoleEntry {
            chain: vec![],
            headroom_tokens: None,
            ..Default::default()
        },
    );
    ConwayConfig {
        default_role: RoleAlias::new("default"),
        cwd: std::path::PathBuf::from("."),
        session: SessionConfig::default(),
        limits: LimitsConfig::default(),
        permissions: PermissionsConfig::default(),
        backends: BTreeMap::new(),
        routing: RoutingSection::default(),
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig::default(),
        tools: ToolsConfig::default(),
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

fn all_text(req: &GenerateRequest) -> String {
    let mut out = String::new();
    for segment in &req.segments {
        for block in &segment.content {
            if let ContentBlock::Text { text } = block {
                out.push_str(text);
                out.push('\n');
            }
        }
    }
    out
}

/// Acceptance 1: a `ForkSpec::context` REPLACES the ordinary fork default
/// (the forker's entire inherited transcript) with the chosen selection --
/// proven on the actual wire request the child's own first turn sends. The
/// directive still survives (it is always appended as the child's own head
/// content record, independent of `context` -- see `SubagentSpec::context`'s
/// own doc on how the two compose), but the parent's own prior turn text
/// must NOT appear: a fork with a chosen context inherits nothing beyond
/// that choice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_with_chosen_context_replaces_the_inherited_transcript() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());

    // Mint session B's id and give it real content to choose from -- a
    // separate `Conway` sharing the same store, mirroring
    // `compose_context_path_end_to_end.rs`'s own mint pattern.
    let mint_backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("b replies"))])
            .with_id(BackendId::new("fake")),
    );
    let mint_conway = build_conway(base_config(), mint_backend, store.clone());
    let session_b = mint_conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session b");
    let turn_b = session_b
        .prompt("unique-marker-from-session-b")
        .await
        .expect("prompt b");
    turn_b.result().await.expect("b's own turn must complete");
    let b_id = session_b.id();

    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("a's own first reply")),
            ScriptedTurn::Respond(text_response("child replied")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(base_config(), backend.clone(), store.clone());
    let session_a = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session a");

    let turn_a = session_a
        .prompt("a-turn-one-content-must-not-reach-the-child")
        .await
        .expect("prompt a");
    turn_a.result().await.expect("a's own turn must complete");

    let child = session_a
        .fork(
            session_a.root(),
            ForkSpec::new("child-directive-text").context(vec![RecordRef {
                session: b_id,
                seq: LogSeq(0),
            }]),
        )
        .await
        .expect("fork with a chosen context should succeed");
    session_a
        .await_agent(child)
        .await
        .expect("the forked child's own first turn must complete");

    let calls = backend.calls();
    assert_eq!(
        calls.len(),
        2,
        "a's own turn (1) + the forked child's own first turn (1): {calls:?}"
    );
    let child_request = &calls[1];
    let text = all_text(child_request);
    assert!(
        text.contains("unique-marker-from-session-b"),
        "the chosen foreign record must be in the child's first-turn context: {text}"
    );
    assert!(
        text.contains("child-directive-text"),
        "the fork's own directive must still be appended as the child's own head content, \
         independent of context: {text}"
    );
    assert!(
        !text.contains("a-turn-one-content-must-not-reach-the-child"),
        "a chosen context REPLACES the ordinary inherited-prefix default outright -- the \
         forker's own prior turn must not leak in: {text}"
    );

    // The covers_upto reasoning (finding `01M0P50E04EY3BHQJHZX74HSSC`),
    // pinned structurally too, not only via the runtime-level test.
    let child_session = session_a
        .tree()
        .nodes
        .into_iter()
        .find(|n| n.agent_id == child)
        .expect("forked child in the tree")
        .session;
    let records = store
        .read(&child_session, SeqRange::full())
        .await
        .expect("read the child's own log");
    let covers_upto = records
        .iter()
        .find_map(|r| match r {
            LogRecord::ContextPathSet { covers_upto, .. } => Some(*covers_upto),
            _ => None,
        })
        .expect("a chosen context must write a ContextPathSet head");
    assert_eq!(
        covers_upto,
        LogSeq::ZERO,
        "a brand-new child's own log is empty when the chosen-context head is written -- ZERO \
         here means \"read my (currently empty) own log from the start\", never the finding's \
         silent-reversal trap, which needs a PRIOR head with an exclusion to resurrect"
    );
}

/// The spawn side of acceptance 1: `SpawnSpec::context` primes an
/// otherwise clean-slate child with hand-picked material -- proven the same
/// way, on the wire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_with_chosen_context_primes_an_otherwise_clean_slate_child() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());

    let mint_backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("b replies"))])
            .with_id(BackendId::new("fake")),
    );
    let mint_conway = build_conway(base_config(), mint_backend, store.clone());
    let session_b = mint_conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session b");
    let turn_b = session_b
        .prompt("unique-marker-from-session-b-spawn")
        .await
        .expect("prompt b");
    turn_b.result().await.expect("b's own turn must complete");
    let b_id = session_b.id();

    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("a's own first reply")),
            ScriptedTurn::Respond(text_response("spawned child replied")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(base_config(), backend.clone(), store.clone());
    let session_a = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session a");

    let turn_a = session_a
        .prompt("a-own-content-must-not-reach-the-spawned-child")
        .await
        .expect("prompt a");
    turn_a.result().await.expect("a's own turn must complete");

    let child = session_a
        .spawn(
            session_a.root(),
            SpawnSpec::new("spawn-prompt-text").context(vec![RecordRef {
                session: b_id,
                seq: LogSeq(0),
            }]),
        )
        .await
        .expect("spawn with a chosen context should succeed");
    session_a
        .await_agent(child)
        .await
        .expect("the spawned child's own first turn must complete");

    let calls = backend.calls();
    let child_request = calls.last().expect("at least one call recorded");
    let text = all_text(child_request);
    assert!(
        text.contains("unique-marker-from-session-b-spawn"),
        "the chosen foreign record must prime the spawned child: {text}"
    );
    assert!(
        text.contains("spawn-prompt-text"),
        "the spawn's own prompt must still be appended: {text}"
    );
    assert!(
        !text.contains("a-own-content-must-not-reach-the-spawned-child"),
        "spawn is clean-slate regardless of context -- the spawner's own transcript must never \
         appear: {text}"
    );
}

/// Acceptance 3, demonstrated rather than asserted: a boundary-time chosen
/// context does NOT invalidate the PARENT's own cached prefix. The
/// observable evidence is the parent's own SUBSEQUENT wire request: its
/// leading segments -- role, content, and provenance, the bytes an
/// implicit-prefix-caching provider actually keys on -- are byte-for-byte
/// identical to what the parent's FIRST request already sent, unaffected by
/// the fork that happened in between. Forking a child (with or without a
/// chosen context) touches only the CHILD's own, brand-new session; it
/// never appends to, or rewrites, the parent's own log.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn boundary_time_chosen_context_does_not_invalidate_the_parents_cached_prefix() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());

    let mint_backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("b replies"))])
            .with_id(BackendId::new("fake")),
    );
    let mint_conway = build_conway(base_config(), mint_backend, store.clone());
    let session_b = mint_conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session b");
    let turn_b = session_b
        .prompt("session-b-content-for-the-fork")
        .await
        .expect("prompt b");
    turn_b.result().await.expect("b's own turn must complete");
    let b_id = session_b.id();

    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("a's first reply")),
            ScriptedTurn::Respond(text_response("child replied")),
            ScriptedTurn::Respond(text_response("a's second reply")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(base_config(), backend.clone(), store.clone());
    // `keep_alive: true`: this session is prompted TWICE below, and a
    // non-keep-alive session's agent task terminates after its first
    // prompt-to-completion turn (see `fork_from_keep_alive_child_persists_
    // for_a_second_turn`'s own doc in `fork_from_keep_alive_plugin_config.rs`
    // for the identical requirement).
    let session_a = conway
        .new_session(SessionSpec {
            keep_alive: true,
            ..SessionSpec::default()
        })
        .await
        .expect("new_session a");

    // Parent's FIRST turn -- the request whose prefix must survive.
    // `keep_alive: true` means this session's root NEVER reaches a terminal
    // `AgentResult` while alive, so `TurnHandle::result()` would hang here --
    // `.text()` (which resolves on `TurnFinished`, not `AgentFinished`) is
    // the correct wait, mirroring `keep_alive.rs`'s own documented idiom.
    let turn1 = session_a
        .prompt("a's first prompt")
        .await
        .expect("prompt a turn 1");
    tokio::time::timeout(Duration::from_secs(5), turn1.text())
        .await
        .expect("a's first turn's text() must not hang")
        .expect("a's first turn's text() should succeed");

    // A chosen-context fork happens IN BETWEEN the parent's two turns.
    let child = session_a
        .fork(
            session_a.root(),
            ForkSpec::new("child directive").context(vec![RecordRef {
                session: b_id,
                seq: LogSeq(0),
            }]),
        )
        .await
        .expect("fork with a chosen context should succeed");
    session_a
        .await_agent(child)
        .await
        .expect("the forked child's own first turn must complete");

    // Parent's SECOND turn -- if the fork touched the parent's own log or
    // rendering in any way, this request's leading segments would differ
    // from turn 1's.
    let turn2 = session_a
        .prompt("a's second prompt")
        .await
        .expect("prompt a turn 2");
    tokio::time::timeout(Duration::from_secs(5), turn2.text())
        .await
        .expect("a's second turn's text() must not hang")
        .expect("a's second turn's text() should succeed");

    let calls = backend.calls();
    assert_eq!(
        calls.len(),
        3,
        "a's turn 1 (1) + the forked child's own turn (1) + a's turn 2 (1): {calls:?}"
    );
    let r1 = &calls[0];
    let r2 = &calls[2];

    // Compare (role, content, provenance) -- everything a real backend
    // adapter's wire payload is actually built from -- for r1's leading
    // segments against the SAME leading span of r2. `segment.id` is
    // excluded deliberately even though it happens to be deterministic
    // here too (`derive_segment_id` hashes agent_id/ordinal/provenance/
    // content): what a caching provider keys on is the rendered bytes, not
    // this engine's own internal segment identifier.
    let sig = |req: &GenerateRequest| -> Vec<_> {
        req.segments
            .iter()
            .map(|s| (s.role, s.content.clone(), s.provenance.clone()))
            .collect::<Vec<_>>()
    };
    let sig1 = sig(r1);
    let sig2 = sig(r2);
    assert!(
        sig2.len() >= sig1.len(),
        "the parent's second request must be at least as long as its first: {} vs {}",
        sig2.len(),
        sig1.len()
    );
    assert_eq!(
        &sig2[..sig1.len()],
        &sig1[..],
        "the parent's own SECOND request must begin with EXACTLY the same segments (role, \
         content, provenance) as its FIRST -- a chosen-context fork in between must not have \
         touched, reordered, or invalidated any of the parent's own already-cached prefix"
    );
}
