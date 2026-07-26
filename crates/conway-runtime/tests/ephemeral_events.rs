//! Acceptance tests for the `ephemeral: bool` field on `Event::AgentSpawned`
//! and `Event::AgentFinished` (board item 01KYD2ERT35VYDSBW9PBSFGYB7, the
//! `conway_ask` epic's item b).
//!
//! Two scopes:
//! - `AgentTree::attach` stamps `Event::AgentSpawned::ephemeral` from
//!   `AgentNode::ephemeral` verbatim, and `AgentTree::ephemeral_of` reads it
//!   back for the `Event::AgentFinished` stamp. Exercised directly here with
//!   an `ephemeral: true` node (mirroring how `conway`'s facade `fork_child`
//!   path attaches an `/ask` child via `resume_root`, which is the one
//!   production site that sets `AgentNode::ephemeral = true`).
//! - A `conway_subagent` fork (`SubagentHost::start`, the `Runtime` impl in
//!   `subagent.rs`) is NEVER ephemeral -- `SessionMeta::ephemeral` is
//!   hardcoded `false` on that path -- so both its `AgentSpawned` and
//!   `AgentFinished` carry `ephemeral: false`. Exercised end-to-end here.
//!
//! The facade `SessionHandle::ask` path itself lives in the `conway` crate
//! (it cannot be exercised from `conway-runtime`, which `conway` depends on);
//! `crates/conway/tests/ask.rs` covers the `/ask`-specific assertions.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use conway_core::agent::{
    Budget, PermissionDecision, ResultStatus, SubagentMode, SubagentSpec,
};
use conway_core::content::{ContentBlock, StopReason, Usage};
use conway_core::event::Event;
use conway_core::fakes::{FakeGate, FakeHealth, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};
use conway_core::ids::{AgentId, BackendId, LogSeq, ModelId, ModelRef, RoleAlias, SessionId};
use conway_core::log::SessionMeta;
use conway_core::ports::{Backend, Router, SessionStore, SubagentHost};
use conway_core::provenance::Provenance;
use conway_routing::config::HeadroomPolicy;
use conway_runtime::events::EventBus;
use conway_runtime::runtime::{RootSpec, Runtime, RuntimeDeps};
use conway_runtime::tree::{AgentNode, AgentTree};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------
// Direct `AgentTree::attach` stamping
// ---------------------------------------------------------------------

#[tokio::test]
async fn attach_stamps_agent_spawned_ephemeral_from_node_and_ephemeral_of_reads_it_back() {
    let bus = EventBus::with_default_capacity();
    let tree = Arc::new(AgentTree::new(bus.clone()));
    let mut stream = bus.subscribe();

    let parent = AgentId::new();
    let session = SessionId::new();
    // Attach the parent first (required so the child's `parent` resolves).
    tree.attach(AgentNode {
        id: parent,
        parent: None,
        session,
        kind: None,
        agent_def: None,
        role: Some(RoleAlias::new("planner")),
        budget: Budget::default(),
        cancel: CancellationToken::new(),
        inherited_upto: None,
        ephemeral: false,
    })
    .expect("root attach");

    // An ephemeral fork child (the shape `conway`'s facade `fork_child` /
    // `resume_root` path produces for `/ask`): `ephemeral: true`,
    // `kind: Some(Fork)` -> `attach` emits `AgentSpawned { ephemeral: true }`.
    let ephemeral_child = AgentId::new();
    tree.attach(AgentNode {
        id: ephemeral_child,
        parent: Some(parent),
        session,
        kind: Some(SubagentMode::Fork),
        agent_def: None,
        role: Some(RoleAlias::new("planner")),
        budget: Budget::default(),
        cancel: CancellationToken::new(),
        inherited_upto: Some(LogSeq(0)),
        ephemeral: true,
    })
    .expect("ephemeral child attach");

    // A normal fork child: `ephemeral: false` -> `AgentSpawned { ephemeral: false }`.
    let normal_child = AgentId::new();
    tree.attach(AgentNode {
        id: normal_child,
        parent: Some(parent),
        session,
        kind: Some(SubagentMode::Fork),
        agent_def: None,
        role: Some(RoleAlias::new("planner")),
        budget: Budget::default(),
        cancel: CancellationToken::new(),
        inherited_upto: Some(LogSeq(0)),
        ephemeral: false,
    })
    .expect("normal child attach");

    // `ephemeral_of` reads the field back for the `AgentFinished` stamp.
    assert!(tree.ephemeral_of(ephemeral_child), "ephemeral child");
    assert!(!tree.ephemeral_of(normal_child), "normal child");
    assert!(!tree.ephemeral_of(AgentId::new()), "unknown agent defaults false");

    // Drain the two `AgentSpawned` events in emission order and assert the
    // `ephemeral` flag is stamped verbatim from each node.
    let mut seen_ephemeral = None;
    let mut seen_normal = false;
    for _ in 0..2 {
        let envelope = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("stream ended early")
            .expect("envelope");
        match envelope.event {
            Event::AgentSpawned { ephemeral, .. } if envelope.agent == ephemeral_child => {
                assert!(ephemeral, "ephemeral child's AgentSpawned carries ephemeral: true");
                seen_ephemeral = Some(ephemeral);
            }
            Event::AgentSpawned { ephemeral, .. } if envelope.agent == normal_child => {
                assert!(!ephemeral, "normal child's AgentSpawned carries ephemeral: false");
                seen_normal = true;
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert_eq!(seen_ephemeral, Some(true), "ephemeral child observed");
    assert!(seen_normal, "normal child observed");
}

// ---------------------------------------------------------------------
// End-to-end `conway_subagent` fork: both events `ephemeral: false`
// ---------------------------------------------------------------------

fn text_response(text: &str) -> conway_core::ports::GenerateResponse {
    conway_core::ports::GenerateResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        },
    }
}

fn build_runtime(turns: usize) -> Arc<Runtime> {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(
            (0..turns)
                .map(|_| ScriptedTurn::Respond(text_response("ok")))
                .collect(),
        )
        .with_id(BackendId::new("b")),
    );
    let model = ModelRef {
        backend: backend.id(),
        model: ModelId::new("m"),
    };
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend);
    Runtime::new(RuntimeDeps {
        store,
        router,
        health: Arc::new(FakeHealth::new()),
        backends,
        plugins: vec![],
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs: HashMap::new(),
        event_bus: EventBus::with_default_capacity(),
        headroom: Arc::new(HeadroomPolicy::default()),
    })
}

fn root_spec(prompt: &str) -> RootSpec {
    RootSpec {
        session: None,
        agent_def: None,
        role: Some(RoleAlias::new("planner")),
        tools: None,
        budget: Budget::default(),
        cwd: PathBuf::from("/tmp"),
        prompt: Some(prompt.to_string()),
        keep_alive: false,
        model: None,
    }
}

fn fork_spec(prompt: &str) -> SubagentSpec {
    SubagentSpec::fork(prompt, Budget::default())
}

/// A `conway_subagent` fork (the `SubagentHost::start` path in `subagent.rs`)
/// is never ephemeral: `SessionMeta::ephemeral` is hardcoded `false` there, so
/// both `Event::AgentSpawned` and `Event::AgentFinished` must carry
/// `ephemeral: false`.
#[tokio::test]
async fn conway_subagent_fork_emits_ephemeral_false_on_spawn_and_finish() {
    let runtime = build_runtime(2);
    let mut stream = runtime.subscribe();
    let root = runtime.start_root(root_spec("investigate")).await.unwrap();

    // Drain the root's own `AgentFinished` so the bus is quiescent before the
    // fork -- the assertion below drains only the child's two lifecycle
    // events and must not be confused by the root's finish racing through.
    loop {
        let envelope = stream.next().await.expect("root stream open");
        if matches!(envelope.event, Event::AgentFinished { .. }) && envelope.agent == root {
            break;
        }
    }

    let child = SubagentHost::start(&*runtime, root, fork_spec("look closer"))
        .await
        .unwrap();

    let mut seen_spawn = false;
    // `seen_finish` is declared `let` (not `let mut`): the only assignment
    // is the one that immediately breaks the loop, so a `let mut` would trip
    // `unused_assignments`. A plain `let` plus a labeled break carries the
    // "we did observe AgentFinished" bit out of the loop cleanly.
    let seen_finish;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    'outer: loop {
        let envelope = tokio::time::timeout_at(deadline, stream.next())
            .await
            .expect("timed out waiting for child lifecycle events")
            .expect("stream open");
        if envelope.agent != child {
            continue;
        }
        match envelope.event {
            Event::AgentSpawned { ephemeral, .. } => {
                assert!(
                    !ephemeral,
                    "conway_subagent fork's AgentSpawned must carry ephemeral: false"
                );
                seen_spawn = true;
            }
            Event::AgentFinished { ephemeral, result, .. } => {
                assert!(
                    !ephemeral,
                    "conway_subagent fork's AgentFinished must carry ephemeral: false"
                );
                assert_eq!(result.status, ResultStatus::Completed);
                seen_finish = true;
                break 'outer;
            }
            _ => {}
        }
    }
    assert!(seen_spawn, "child AgentSpawned observed");
    assert!(
        seen_finish, "child AgentFinished observed",
    );
}

// ---------------------------------------------------------------------
// `Provenance` import silencer: `Provenance` is part of this crate's public
// log-record surface but is unused in the trimmed harness above. Keeping
// the import documents the dependency the production code path exercises.
// ---------------------------------------------------------------------
#[allow(dead_code)]
fn _provenance_anchor() -> Provenance {
    Provenance::UserPrompt
}

// `SessionMeta` is imported for the same reason -- the production fork path
// stamps `meta.ephemeral` into `AgentNode::ephemeral`; the direct-attach test
// above bypasses `SessionMeta` but the type remains part of this item's
// surface.
#[allow(dead_code)]
fn _session_meta_anchor(meta: &SessionMeta) -> bool {
    meta.ephemeral
}