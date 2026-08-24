//! Acceptance tests for `Runtime::run_ephemeral_turn` -- the mode-agnostic
//! spawn/drain/purge primitive extracted from `conway`'s intent classifier
//! (board item `01KZVZ0ASR4CRFG822YWEAW30K`, Stage 2c). Deliberately NOT
//! `Runtime::ask`'s own test file (`ask.rs`): `ask` enforces Fork-only at
//! its trait boundary, so it cannot exercise the one property this
//! primitive exists to add -- an ephemeral child started in `SubagentMode::
//! Spawn`.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conway_core::agent::{Budget, PermissionDecision, ResultStatus, SubagentMode, SubagentSpec};
use conway_core::capabilities::{HeadroomPolicy, ProbeReport};
use conway_core::content::{ContentBlock, StopReason, Usage};
use conway_core::error::{BackendError, StoreError};
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias};
use conway_core::ports::{
    Backend, BoxStream, GenerateRequest, GenerateResponse, Router, SessionStore, StreamChunk,
};
use conway_runtime::events::EventBus;
use conway_runtime::runtime::{RootSpec, Runtime, RuntimeDeps};
use conway_testkit::{FakeGate, FakeHealth, FakeRouter};
use futures::stream;
use tokio::time::sleep;

/// One scripted turn: a response built from `content`, emitted after
/// `delay` -- mirrors `ask.rs`'s own `AskTurn` (duplicated rather than
/// shared: these are two separate test binaries with no common
/// dev-dependency crate for fixtures this small).
#[derive(Clone)]
struct Turn {
    content: Vec<ContentBlock>,
    delay: Duration,
}

impl Turn {
    fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::Text { text: text.into() }],
            delay: Duration::ZERO,
        }
    }

    fn response(&self) -> GenerateResponse {
        GenerateResponse {
            content: self.content.clone(),
            tool_calls: vec![],
            stop: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            },
        }
    }
}

struct ScriptBackend {
    id: BackendId,
    script: Mutex<VecDeque<Turn>>,
}

impl ScriptBackend {
    fn new(id: BackendId, script: Vec<Turn>) -> Self {
        Self {
            id,
            script: Mutex::new(script.into()),
        }
    }
}

#[async_trait]
impl Backend for ScriptBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn capabilities(&self, _model: &ModelId) -> conway_core::capabilities::Capabilities {
        conway_core::capabilities::Capabilities {
            tool_calling: conway_core::capabilities::ToolCallSupport::None,
            cache: conway_core::capabilities::CacheMode::None,
            parallel_tool_calls: false,
            structured_output: conway_core::capabilities::StructuredOutput::None,
            max_context_tokens: 128_000,
            reasoning: false,
            reliability_tier: conway_core::capabilities::ReliabilityTier::Unknown,
        }
    }

    async fn generate(&self, req: GenerateRequest) -> Result<GenerateResponse, BackendError> {
        let _ = req;
        let turn =
            self.script
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| BackendError::BadRequest {
                    detail: "script exhausted".into(),
                })?;
        sleep(turn.delay).await;
        Ok(turn.response())
    }

    async fn stream(
        &self,
        req: GenerateRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError> {
        let response = self.generate(req).await?;
        let mut chunks: Vec<Result<StreamChunk, BackendError>> = response
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(Ok(StreamChunk::TextDelta(text.clone()))),
                _ => None,
            })
            .collect();
        chunks.push(Ok(StreamChunk::Done(response)));
        Ok(Box::pin(stream::iter(chunks)))
    }

    async fn probe(&self) -> Result<ProbeReport, BackendError> {
        Ok(ProbeReport {
            ok: true,
            latency_ms: 1,
            models: vec![],
            detail: None,
            at: chrono::Utc::now(),
        })
    }
}

/// Builds a `Runtime` and returns it alongside its own `Arc<dyn
/// SessionStore>` -- kept separately so a test can inspect the store
/// directly (e.g. confirm a purge actually happened), which `Runtime`
/// itself does not expose.
fn build_runtime(backend: Arc<dyn Backend>) -> (Arc<Runtime>, Arc<dyn SessionStore>) {
    let model = ModelRef {
        backend: backend.id(),
        model: ModelId::new("m"),
    };
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend);
    let store: Arc<dyn SessionStore> = Arc::new(conway_testkit::FakeStore::new());

    let runtime = Runtime::new(RuntimeDeps {
        store: store.clone(),
        path_store: std::sync::Arc::new(conway_testkit::FakePathStore::new()),
        router,
        health: Arc::new(FakeHealth::new()),
        backends,
        plugins: vec![],
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs: HashMap::new(),
        instructions: Vec::new(),
        skills: Default::default(),
        event_bus: EventBus::with_default_capacity(),
        headroom: Arc::new(HeadroomPolicy::default()),

        session_discovery: Arc::new(conway_testkit::FakeSessionDiscoveryHost::new()),
    });
    (runtime, store)
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
        labels: Vec::new(),
    }
}

/// A spawn (NOT fork) ephemeral spec -- the shape `Runtime::ask` refuses
/// but `run_ephemeral_turn` must accept, mirroring `conway::intent`'s own
/// `SubagentSpec` for its classifier child.
fn spawn_spec(prompt: &str) -> SubagentSpec {
    SubagentSpec {
        mode: SubagentMode::Spawn,
        prompt: prompt.to_string(),
        agent_def: None,
        role: None,
        pin: None,
        tools: None,
        budget: Budget::default(),
        result_contract: None,
        keep_alive: false,
        ephemeral: true,
        ask_origin: None,
        cwd: None,
        root: None,
        tag: None,
        plugin_config: None,
        context: None,
    }
}

async fn start_and_finish_root(runtime: &Runtime, prompt: &str) -> conway_core::ids::AgentId {
    use conway_core::event::Event;
    use futures::StreamExt;

    let mut stream = runtime.subscribe();
    let root = runtime.start_root(root_spec(prompt)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let envelope = stream.next().await.expect("event stream ended early");
            if envelope.agent == root {
                if let Event::AgentFinished { .. } = envelope.event {
                    return;
                }
            }
        }
    })
    .await
    .expect("root never finished");
    root
}

/// The property `Runtime::ask` structurally cannot exercise: a
/// `SubagentMode::Spawn` ephemeral child runs to completion and its full
/// reply text is returned -- `run_ephemeral_turn` accepts what `ask`
/// rejects with `RuntimeError::AskRequiresFork`.
#[tokio::test]
async fn run_ephemeral_turn_accepts_spawn_mode_and_returns_full_text_and_completed_result() {
    let backend = Arc::new(ScriptBackend::new(
        BackendId::new("b"),
        vec![Turn::text("root ok"), Turn::text("classifier reply")],
    ));
    let (runtime, _store) = build_runtime(backend);
    let parent = start_and_finish_root(&runtime, "investigate").await;

    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        runtime.run_ephemeral_turn(parent, parent, spawn_spec("classify this")),
    )
    .await
    .expect("run_ephemeral_turn did not resolve")
    .expect("run_ephemeral_turn errored");

    assert_eq!(outcome.reply, "classifier reply");
    assert_eq!(outcome.result.status, ResultStatus::Completed);
}

/// The purge half of the contract: once a terminal is observed, the
/// child's session is removed from the store -- a caller (like
/// `conway::intent::classify`) that never sees the child's `SessionId` must
/// still be able to trust it is gone.
#[tokio::test]
async fn run_ephemeral_turn_purges_the_child_session_once_terminal_is_observed() {
    let backend = Arc::new(ScriptBackend::new(
        BackendId::new("b"),
        vec![Turn::text("root ok"), Turn::text("reply")],
    ));
    let (runtime, store) = build_runtime(backend);
    let parent = start_and_finish_root(&runtime, "investigate").await;

    let outcome = runtime
        .run_ephemeral_turn(parent, parent, spawn_spec("classify this"))
        .await
        .expect("run_ephemeral_turn errored");
    assert_eq!(outcome.result.status, ResultStatus::Completed);

    // The child attached to the tree (tree nodes are never detached), so its
    // session id is still readable from the live tree even after purge...
    let child_session = runtime
        .tree()
        .nodes
        .iter()
        .find(|n| n.parent == Some(parent))
        .map(|n| n.session)
        .expect("child node present in tree");
    assert_eq!(
        child_session, outcome.result.transcript_ref,
        "the tree's own child session must match the returned result's transcript_ref"
    );

    // ...but the store no longer has its session: the purge really ran.
    let err = store
        .meta(&child_session)
        .await
        .expect_err("the child's session must have been purged, not merely orphaned in the tree");
    assert!(
        matches!(err, StoreError::NotFound { .. }),
        "expected NotFound after purge, got {err:?}"
    );
}

/// A non-`Completed` terminal (here: a cancelled child) is still returned
/// as `Ok` -- this primitive makes no judgment about the result's status,
/// that is each caller's own domain concern (`conway::intent::classify`
/// turns a `Failed`/other status into `ConwayError::IntentClassification`
/// itself) -- and the child is still purged either way.
#[tokio::test]
async fn run_ephemeral_turn_returns_a_non_completed_terminal_and_still_purges() {
    let backend = Arc::new(ScriptBackend::new(
        BackendId::new("b"),
        vec![
            Turn::text("root ok"),
            Turn {
                content: vec![ContentBlock::Text {
                    text: "unreachable".into(),
                }],
                delay: Duration::from_secs(60),
            },
        ],
    ));
    let (runtime, store) = build_runtime(backend);
    let parent = start_and_finish_root(&runtime, "investigate").await;
    runtime
        .cancel(
            parent,
            "test: parent cancelled before the child starts".to_string(),
        )
        .expect("cancel root");

    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        runtime.run_ephemeral_turn(parent, parent, spawn_spec("classify this")),
    )
    .await
    .expect("run_ephemeral_turn hung on a cancelled child")
    .expect("run_ephemeral_turn errored");

    assert!(
        matches!(outcome.result.status, ResultStatus::Cancelled { .. }),
        "expected Cancelled, got {:?}",
        outcome.result.status
    );
    assert_eq!(outcome.reply, "");

    let child_session = outcome.result.transcript_ref;
    let err = store
        .meta(&child_session)
        .await
        .expect_err("a cancelled child's session must still be purged");
    assert!(matches!(err, StoreError::NotFound { .. }));
}
