//! Acceptance tests for the `ephemeral: bool` field on `Event::AgentSpawned`
//! and `Event::AgentFinished` (the
//! `conway_ask` epic's item b).
//!
//! Two scopes:
//! - `AgentTree::attach` stamps `Event::AgentSpawned::ephemeral` from
//!   `AgentNode::ephemeral` verbatim, and `AgentTree::ephemeral_of` reads it
//!   back for the `Event::AgentFinished` stamp. Exercised directly here with
//!   an `ephemeral: true` node -- the exact shape `SubagentHost::start`
//!   builds for an `ephemeral: true` fork spec, which is how both the
//!   `conway_ask` tool and (post-B2) the facade's `SessionHandle::ask`
//!   attach their `/ask` children.
//! - A `conway_fork` (`SubagentHost::start`, the `Runtime` impl in
//!   `subagent.rs`) is NEVER ephemeral -- `SessionMeta::ephemeral` is
//!   hardcoded `false` on that path -- so both its `AgentSpawned` and
//!   `AgentFinished` carry `ephemeral: false`. Exercised end-to-end here.
//!
//! The facade `SessionHandle::ask` path itself lives in the `conway` crate
//! (it cannot be exercised from `conway-runtime`, which `conway` depends on);
//! `crates/conway/tests/ask.rs` covers the `/ask`-specific assertions.
//!
//! Also covers `AgentTree::is_prunable_on_finish` (`EventBus.
//! seqs` still leaks for spawned and forked agents) at the bottom of this
//! file -- it reuses this file's same direct-`attach` harness, since the
//! decision it tests is a pure function of tree state.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use conway_core::agent::{Budget, PermissionDecision, ResultStatus, SubagentMode, SubagentSpec};
use conway_core::capabilities::HeadroomPolicy;
use conway_core::content::{ContentBlock, StopReason, Usage};
use conway_core::event::Event;
use conway_core::ids::{AgentId, BackendId, LogSeq, ModelId, ModelRef, RoleAlias, SessionId};
use conway_core::log::SessionMeta;
use conway_core::ports::{Backend, Router, SessionStore, SubagentHost};
use conway_core::provenance::Provenance;
use conway_runtime::events::EventBus;
use conway_runtime::runtime::{RootSpec, Runtime, RuntimeDeps};
use conway_runtime::tree::{AgentNode, AgentTree};
use conway_testkit::{FakeGate, FakeHealth, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};
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

    // An ephemeral fork child (the shape `SubagentHost::start` produces for
    // an `ephemeral: true` fork spec -- the `conway_ask` tool's, and
    // post-B2 the facade `/ask`'s, child): `ephemeral: true`,
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
    assert!(
        !tree.ephemeral_of(AgentId::new()),
        "unknown agent defaults false"
    );

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
                assert!(
                    ephemeral,
                    "ephemeral child's AgentSpawned carries ephemeral: true"
                );
                seen_ephemeral = Some(ephemeral);
            }
            Event::AgentSpawned { ephemeral, .. } if envelope.agent == normal_child => {
                assert!(
                    !ephemeral,
                    "normal child's AgentSpawned carries ephemeral: false"
                );
                seen_normal = true;
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert_eq!(seen_ephemeral, Some(true), "ephemeral child observed");
    assert!(seen_normal, "normal child observed");
}

/// The `tree()` snapshot keeps ephemeral children ( provenance) but must
/// project each node's `ephemeral` flag so a consumer (the TUI `/tree`
/// renderer) can tell an ephemeral `/ask` child apart from a persistent
/// subagent (MIN-3).
#[tokio::test]
async fn snapshot_projects_ephemeral_flag_per_node() {
    let bus = EventBus::with_default_capacity();
    let tree = AgentTree::new(bus);

    let parent = AgentId::new();
    let session = SessionId::new();
    tree.attach(AgentNode {
        id: parent,
        parent: None,
        session,
        kind: None,
        agent_def: None,
        role: None,
        budget: Budget::default(),
        cancel: CancellationToken::new(),
        inherited_upto: None,
        ephemeral: false,
    })
    .expect("root attach");

    let ephemeral_child = AgentId::new();
    tree.attach(AgentNode {
        id: ephemeral_child,
        parent: Some(parent),
        session,
        kind: Some(SubagentMode::Fork),
        agent_def: None,
        role: None,
        budget: Budget::default(),
        cancel: CancellationToken::new(),
        inherited_upto: Some(LogSeq(0)),
        ephemeral: true,
    })
    .expect("ephemeral child attach");

    let persistent_child = AgentId::new();
    tree.attach(AgentNode {
        id: persistent_child,
        parent: Some(parent),
        session,
        kind: Some(SubagentMode::Spawn),
        agent_def: None,
        role: None,
        budget: Budget::default(),
        cancel: CancellationToken::new(),
        inherited_upto: None,
        ephemeral: false,
    })
    .expect("persistent child attach");

    let snapshot = tree.snapshot();
    // Both children stay IN the snapshot (: adding a marker, never
    // filtering).
    assert_eq!(
        snapshot.nodes.len(),
        3,
        "snapshot keeps every attached node"
    );
    let flag_of = |id: AgentId| {
        snapshot
            .nodes
            .iter()
            .find(|n| n.agent_id == id)
            .unwrap_or_else(|| panic!("{id} must be in the snapshot"))
            .ephemeral
    };
    assert!(flag_of(ephemeral_child), "ephemeral child projects true");
    assert!(
        !flag_of(persistent_child),
        "persistent child projects false"
    );
    assert!(!flag_of(parent), "root projects false");
}

// ---------------------------------------------------------------------
// End-to-end `conway_fork`: both events `ephemeral: false`
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
        root: None,
        prompt: Some(prompt.to_string()),
        keep_alive: false,
        model: None,
        system_prompt_override: None,
        result_contract: None,
    }
}

fn fork_spec(prompt: &str) -> SubagentSpec {
    SubagentSpec::fork(prompt, Budget::default())
}

/// A `conway_fork` (the `SubagentHost::start` path in `subagent.rs`)
/// is never ephemeral: `SessionMeta::ephemeral` is hardcoded `false` there, so
/// both `Event::AgentSpawned` and `Event::AgentFinished` must carry
/// `ephemeral: false`.
#[tokio::test]
async fn conway_fork_emits_ephemeral_false_on_spawn_and_finish() {
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

    let child = SubagentHost::start(&*runtime, root, root, fork_spec("look closer"))
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
                    "conway_fork's AgentSpawned must carry ephemeral: false"
                );
                seen_spawn = true;
            }
            Event::AgentFinished {
                ephemeral, result, ..
            } => {
                assert!(
                    !ephemeral,
                    "conway_fork's AgentFinished must carry ephemeral: false"
                );
                assert_eq!(result.status, ResultStatus::Completed);
                seen_finish = true;
                break 'outer;
            }
            _ => {}
        }
    }
    assert!(seen_spawn, "child AgentSpawned observed");
    assert!(seen_finish, "child AgentFinished observed",);
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
// ---------------------------------------------------------------------
// B3: `AgentTree::set_ephemeral` + `Runtime::promote_agent`
// ---------------------------------------------------------------------

/// The tree setter behind the facade's promote: flips the flag in place
/// (`ephemeral_of` — the `Event::AgentFinished` stamp source — reads the
/// new value back immediately), and an unknown agent is a typed
/// `AgentNotFound`, never a silent no-op.
#[tokio::test]
async fn set_ephemeral_flips_the_flag_and_errors_on_unknown_agent() {
    let bus = EventBus::with_default_capacity();
    let tree = AgentTree::new(bus);

    let parent = AgentId::new();
    let session = SessionId::new();
    tree.attach(AgentNode {
        id: parent,
        parent: None,
        session,
        kind: None,
        agent_def: None,
        role: None,
        budget: Budget::default(),
        cancel: CancellationToken::new(),
        inherited_upto: None,
        ephemeral: false,
    })
    .expect("root attach");

    let child = AgentId::new();
    let child_session = SessionId::new();
    tree.attach(AgentNode {
        id: child,
        parent: Some(parent),
        session: child_session,
        kind: Some(SubagentMode::Fork),
        agent_def: None,
        role: None,
        budget: Budget::default(),
        cancel: CancellationToken::new(),
        inherited_upto: Some(LogSeq(0)),
        ephemeral: true,
    })
    .expect("ephemeral child attach");

    assert!(tree.ephemeral_of(child), "precondition: child is ephemeral");
    let returned = tree
        .set_ephemeral(child, false)
        .expect("set_ephemeral on an attached agent");
    assert_eq!(
        returned, child_session,
        "set_ephemeral must return the agent's own session for the caller's emit"
    );
    assert!(
        !tree.ephemeral_of(child),
        "ephemeral_of must read the flipped flag back"
    );
    let snapshot = tree.snapshot();
    assert!(
        !snapshot
            .nodes
            .iter()
            .find(|n| n.agent_id == child)
            .expect("child in snapshot")
            .ephemeral,
        "the snapshot must project the flipped flag"
    );

    let err = tree
        .set_ephemeral(AgentId::new(), false)
        .expect_err("unknown agent must error");
    assert!(
        matches!(err, conway_core::error::RuntimeError::AgentNotFound { .. }),
        "unknown agent must be AgentNotFound, got: {err:?}"
    );
}

/// `Runtime::promote_agent` end-to-end: an ephemeral fork child (the exact
/// shape `SubagentHost::start` builds for an `ephemeral: true` spec — the
/// facade `/ask`'s child) is flipped in the live tree and exactly one
/// `Event::AgentPromoted` is emitted under the CHILD's own session/agent,
/// which the method also returns.
#[tokio::test]
async fn promote_agent_flips_tree_and_emits_agent_promoted_under_the_child() {
    let runtime = build_runtime(2);
    let mut stream = runtime.subscribe();
    let root = runtime.start_root(root_spec("investigate")).await.unwrap();

    // Quiesce the bus (see the fork test above for why this drain exists).
    loop {
        let envelope = stream.next().await.expect("root stream open");
        if matches!(envelope.event, Event::AgentFinished { .. }) && envelope.agent == root {
            break;
        }
    }

    let child = SubagentHost::start(
        &*runtime,
        root,
        root,
        SubagentSpec {
            ephemeral: true,
            ..fork_spec("an ephemeral aside")
        },
    )
    .await
    .unwrap();
    let child_session = runtime
        .tree()
        .nodes
        .iter()
        .find(|n| n.agent_id == child)
        .expect("child in tree")
        .session;

    let returned = runtime
        .promote_agent(child)
        .expect("promote_agent on an attached child");
    assert_eq!(
        returned, child_session,
        "promote_agent must return the promoted agent's own session"
    );
    assert!(
        !runtime
            .tree()
            .nodes
            .iter()
            .find(|n| n.agent_id == child)
            .expect("child in tree")
            .ephemeral,
        "the live tree must show the flipped flag"
    );

    let envelope = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let envelope = stream.next().await.expect("stream open");
            if matches!(envelope.event, Event::AgentPromoted { .. }) {
                break envelope;
            }
        }
    })
    .await
    .expect("timed out waiting for AgentPromoted");
    assert_eq!(envelope.agent, child);
    assert_eq!(
        envelope.session, child_session,
        "AgentPromoted must be stamped under the child's own session"
    );

    // Drain the child's finish so the test leaves no live task; the finish
    // must now carry `ephemeral: false` (stamped from the flipped node).
    let finish_ephemeral = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let envelope = stream.next().await.expect("stream open");
            if let Event::AgentFinished { ephemeral, .. } = envelope.event {
                if envelope.agent == child {
                    break ephemeral;
                }
            }
        }
    })
    .await
    .expect("timed out waiting for the promoted child's AgentFinished");
    assert!(
        !finish_ephemeral,
        "the promoted child's AgentFinished must carry ephemeral: false"
    );

    let err = runtime
        .promote_agent(AgentId::new())
        .expect_err("unknown agent must error");
    assert!(
        matches!(err, conway_core::error::RuntimeError::AgentNotFound { .. }),
        "unknown agent must be AgentNotFound, got: {err:?}"
    );
}

// ---------------------------------------------------------------------
// `EventBus.seqs` reclamation (still leaks for spawned and
// forked agents) -- `AgentTree::is_prunable_on_finish`
// ---------------------------------------------------------------------

/// `is_prunable_on_finish` is `true` only for a spawn/fork child that was
/// NEVER ephemeral at attach time (an ordinary `conway_fork`/`conway_spawn`)
/// -- `false` for a root, for an ephemeral child that was never promoted
/// (that case is `EventBus::emit`'s own ephemeral-based reclamation, not
/// this method's concern), and -- the promotion subtlety this item's own
/// record flags as the most likely thing to break -- for a child that WAS
/// ephemeral at attach and was later promoted: `ephemeral_of` reads `false`
/// after the promote, but the frozen attach-time value keeps this method
/// returning `false` too, so a promoted child stays excluded from
/// reclamation exactly like the existing
/// `promoted_then_finished_session_is_not_ephemeral_at_finish_and_survives`
/// (`conway-runtime`'s `events.rs`) expects.
#[tokio::test]
async fn is_prunable_on_finish_covers_root_plain_child_ephemeral_child_and_promoted_child() {
    let bus = EventBus::with_default_capacity();
    let tree = AgentTree::new(bus);

    let root = AgentId::new();
    let session = SessionId::new();
    tree.attach(AgentNode {
        id: root,
        parent: None,
        session,
        kind: None,
        agent_def: None,
        role: None,
        budget: Budget::default(),
        cancel: CancellationToken::new(),
        inherited_upto: None,
        ephemeral: false,
    })
    .expect("root attach");
    assert!(
        !tree.is_prunable_on_finish(root),
        "a root must never be prunable -- one counter per process is not a leak"
    );

    let plain_child = AgentId::new();
    tree.attach(AgentNode {
        id: plain_child,
        parent: Some(root),
        session: SessionId::new(),
        kind: Some(SubagentMode::Fork),
        agent_def: None,
        role: None,
        budget: Budget::default(),
        cancel: CancellationToken::new(),
        inherited_upto: Some(LogSeq(0)),
        ephemeral: false,
    })
    .expect("plain child attach");
    assert!(
        tree.is_prunable_on_finish(plain_child),
        "an ordinary, never-ephemeral spawn/fork child must be prunable -- the item's own acceptance case"
    );

    let ephemeral_child = AgentId::new();
    tree.attach(AgentNode {
        id: ephemeral_child,
        parent: Some(root),
        session: SessionId::new(),
        kind: Some(SubagentMode::Spawn),
        agent_def: None,
        role: None,
        budget: Budget::default(),
        cancel: CancellationToken::new(),
        inherited_upto: None,
        ephemeral: true,
    })
    .expect("ephemeral child attach");
    assert!(
        !tree.is_prunable_on_finish(ephemeral_child),
        "a currently-ephemeral child is `emit`'s own reclamation concern, not this method's"
    );

    // Promote it: the live `ephemeral` flag flips to `false`, but the
    // frozen attach-time value must keep this method returning `false`.
    tree.set_ephemeral(ephemeral_child, false)
        .expect("promote the ephemeral child");
    assert!(
        !tree.ephemeral_of(ephemeral_child),
        "precondition: promoted child now reads non-ephemeral"
    );
    assert!(
        !tree.is_prunable_on_finish(ephemeral_child),
        "a promoted child must stay excluded from reclamation, even though it is a \
         fork/spawn child and no longer reads as ephemeral"
    );

    assert!(
        !tree.is_prunable_on_finish(AgentId::new()),
        "an unknown agent defaults to not prunable, matching ephemeral_of's own default"
    );
}
