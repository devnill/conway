//! Acceptance tests for `Conway::classify_agent_intent` (C1): the facade NL
//! intent classifier for `/fork`/`/spawn`, run as an ephemeral one-turn
//! spawn session under the declarative `intent` role. Covers the
//! well-formed parse, the P-10 validation policy (malformed JSON ->
//! passthrough, invalid recipe -> passthrough with the CALLER's default
//! recipe, hallucinated agent_def -> stripped, configured agent_def ->
//! kept), the unconfigured-role passthrough (no session, no backend call),
//! a failed intent turn propagating, and the no-leak invariant: the intent
//! session is purged on EVERY exit path.

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, BackendEntry, BackendKind, ConwayConfig, HealthSection, LimitsConfig,
    ModelsConfig, PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig, TuiSection,
};
use conway::{AgentIntent, Conway, ConwayBuilder, ConwayError, SessionHandle, SessionSpec};
use conway_core::agent::{PermissionDecision, SubagentMode};
use conway_core::content::ContentBlock;
use conway_core::error::BackendError;
use conway_core::fakes::{FakeGate, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias};
use conway_core::log::SessionFilter;
use conway_core::ports::{Backend, GenerateResponse, SessionStore};

const CLASSIFY_TIMEOUT: Duration = Duration::from_secs(5);

fn fake_router() -> Arc<dyn conway_core::ports::Router> {
    Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }))
}

fn text_response(text: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        tool_calls: vec![],
        stop: conway_core::content::StopReason::EndTurn,
        usage: conway_core::content::Usage::default(),
    }
}

/// `with_intent_role` toggles the `[roles.intent]` entry -- the switch the
/// unconfigured-role passthrough test flips.
fn base_config(with_intent_role: bool) -> ConwayConfig {
    let mut roles = BTreeMap::new();
    roles.insert(
        "default".to_string(),
        RoleEntry {
            chain: vec![],
            headroom_tokens: None,
            ..Default::default()
        },
    );
    // The `intent` role's chain references `fake/echo-model`; config
    // validation (`merge::validate` step 3) requires every chain entry's
    // backend id to exist in `[backends]`, and `build()` then CONSTRUCTS
    // every config backend before merging injected ones over them. So the
    // `fake` entry must be constructible on its own. An `Anthropic`-kind
    // entry keyed "fake" is rejected (its id is hardcoded "anthropic"); an
    // `openai-compat` entry with a valid base_url + dialect constructs
    // without I/O (`OpenAiCompatBackend::new` stores config only, and
    // `probe_on_startup` defaults to false so no network probe runs). The
    // injected `ScriptedBackend` (id "fake") then overwrites it in the
    // backend map, so the stub is never routed to. `api_key` and
    // `api_key_env` are both empty, satisfying the mutual-exclusion rule.
    let mut backends = BTreeMap::new();
    if with_intent_role {
        roles.insert(
            "intent".to_string(),
            RoleEntry {
                chain: vec!["fake/echo-model".to_string()],
                headroom_tokens: None,
                ..Default::default()
            },
        );
        backends.insert(
            "fake".to_string(),
            BackendEntry {
                kind: BackendKind::OpenaiCompat,
                api_key: String::new(),
                api_key_env: String::new(),
                base_url: "http://localhost:11434".to_string(),
                dialect: Some("ollama".to_string()),
                stream_tools: None,
            },
        );
    }
    ConwayConfig {
        default_role: RoleAlias::new("default"),
        cwd: std::path::PathBuf::from("."),
        session: SessionConfig::default(),
        limits: LimitsConfig::default(),
        permissions: PermissionsConfig::default(),
        backends,
        routing: RoutingSection::default(),
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig::default(),
        tui: TuiSection::default(),
        tools: ToolsConfig::default(),
        plugins: PluginsConfig::default(),
    }
}

fn build_conway(
    config: ConwayConfig,
    store: Arc<dyn SessionStore>,
    backend: Arc<dyn Backend>,
) -> Conway {
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    ConwayBuilder::from_parts(config)
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build()
        .expect("build should succeed with every port injected")
}

/// A live parent for the intent session to hang off: a root whose first
/// iteration is gated idle (`prompt: None`), so it is present in the
/// runtime's tree without spending a scripted turn.
async fn idle_parent(conway: &Conway) -> SessionHandle {
    conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed")
}

/// Every session the store knows about, ephemeral ones included.
async fn all_sessions(store: &Arc<dyn SessionStore>) -> Vec<conway::SessionMeta> {
    store
        .list(SessionFilter {
            include_ephemeral: true,
            ..SessionFilter::default()
        })
        .await
        .expect("list should succeed")
}

/// The happy path: a well-formed JSON reply parses into the right
/// `AgentIntent` -- the model's recipe and rewritten prompt win over the
/// caller's default -- and the intent session is GONE afterwards.
#[tokio::test]
async fn classify_parses_a_well_formed_reply_and_purges_the_intent_session() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response(
            r#"{"recipe": "spawn", "agent_def": null, "prompt": "check the diff"}"#,
        ))])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(base_config(true), store.clone(), backend.clone());
    let parent = idle_parent(&conway).await;

    let intent = tokio::time::timeout(
        CLASSIFY_TIMEOUT,
        conway.classify_agent_intent(
            parent.root(),
            SubagentMode::Fork,
            "spawn a fresh agent to check the diff",
        ),
    )
    .await
    .expect("classify must not hang")
    .expect("classify should succeed");

    assert_eq!(
        intent,
        AgentIntent {
            recipe: SubagentMode::Spawn,
            agent_def: None,
            prompt: "check the diff".to_string(),
        },
        "the model's recipe and rewritten prompt must win over the caller's default"
    );

    // The intent session is gone: only the parent's session remains.
    let sessions = all_sessions(&store).await;
    assert_eq!(
        sessions.len(),
        1,
        "the intent session must be purged after a successful classify, got: {sessions:?}"
    );
    assert_eq!(sessions[0].id, parent.id());

    // The classifier ran tool-less (P-10 surface minimization: it must
    // answer from the prompt alone).
    let calls = backend.calls();
    assert_eq!(calls.len(), 1, "exactly one classification turn must run");
    assert!(
        calls[0].tools.is_empty(),
        "the intent session must be given zero tools"
    );
}

/// A reply wrapped in a single ```json code fence (a real cheap-model
/// habit) still parses -- the one formatting concession parse makes.
#[tokio::test]
async fn classify_strips_a_single_json_code_fence() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response(
            "```json\n{\"recipe\": \"fork\", \"agent_def\": null, \"prompt\": \"continue this\"}\n```",
        ))])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(base_config(true), store.clone(), backend);
    let parent = idle_parent(&conway).await;

    let intent = tokio::time::timeout(
        CLASSIFY_TIMEOUT,
        conway.classify_agent_intent(parent.root(), SubagentMode::Spawn, "keep going with this"),
    )
    .await
    .expect("classify must not hang")
    .expect("classify should succeed");

    assert_eq!(
        intent,
        AgentIntent {
            recipe: SubagentMode::Fork,
            agent_def: None,
            prompt: "continue this".to_string(),
        }
    );
    assert_eq!(
        all_sessions(&store).await.len(),
        1,
        "the intent session must be purged"
    );
}

/// P-10: a malformed reply degrades to the verbatim passthrough (the
/// caller's default recipe, the RAW text, no def) -- a confused cheap
/// model must never break `/fork`/`/spawn` -- and the intent session is
/// still purged on the parse-failure path.
#[tokio::test]
async fn classify_malformed_json_passes_through_verbatim_and_still_purges() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response(
            "I think you probably want a fork, but I am not going to say so in JSON.",
        ))])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(base_config(true), store.clone(), backend);
    let parent = idle_parent(&conway).await;

    let raw = "fork this conversation to summarize it";
    let intent = tokio::time::timeout(
        CLASSIFY_TIMEOUT,
        conway.classify_agent_intent(parent.root(), SubagentMode::Fork, raw),
    )
    .await
    .expect("classify must not hang")
    .expect("a malformed reply must NOT fail the classify");

    assert_eq!(
        intent,
        AgentIntent {
            recipe: SubagentMode::Fork,
            agent_def: None,
            prompt: raw.to_string(),
        },
        "malformed JSON must degrade to the verbatim passthrough"
    );
    assert_eq!(
        all_sessions(&store).await.len(),
        1,
        "the intent session must be purged even on the parse-failure path"
    );
}

/// The unconfigured-role fallback: with no `[roles.intent]` entry,
/// classify returns the verbatim passthrough WITHOUT creating a session or
/// calling the backend at all (the pre-flight `UnknownRole` catch).
#[tokio::test]
async fn classify_without_an_intent_role_passes_through_without_a_session_or_backend_call() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    // An EMPTY script: any backend call would fail with "scripted backend
    // exhausted" -- the `calls()` assertion below pins that none happened.
    let backend = Arc::new(ScriptedBackend::new(vec![]).with_id(BackendId::new("fake")));
    let conway = build_conway(base_config(false), store.clone(), backend.clone());
    let parent = idle_parent(&conway).await;

    let raw = "spawn a reviewer for this diff";
    let intent = conway
        .classify_agent_intent(parent.root(), SubagentMode::Spawn, raw)
        .await
        .expect("the unconfigured-role fallback must succeed");

    assert_eq!(
        intent,
        AgentIntent {
            recipe: SubagentMode::Spawn,
            agent_def: None,
            prompt: raw.to_string(),
        },
        "no [roles.intent] -> verbatim passthrough with the caller's default recipe"
    );
    assert!(
        backend.calls().is_empty(),
        "the fallback must not run a classification turn at all"
    );
    let sessions = all_sessions(&store).await;
    assert_eq!(
        sessions.len(),
        1,
        "the fallback must not create an intent session, got: {sessions:?}"
    );
}

/// P-10, reject-vs-strip (decided: STRIP): a reply naming a def that is
/// NOT configured must not reach the caller -- the field degrades to
/// `None` -- while the validated recipe and prompt survive.
#[tokio::test]
async fn classify_strips_a_hallucinated_agent_def_but_keeps_recipe_and_prompt() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response(
            r#"{"recipe": "fork", "agent_def": "nonexistent-def", "prompt": "review this"}"#,
        ))])
        .with_id(BackendId::new("fake")),
    );
    // `AgentsConfig::default().dir` (".conway/agents") does not exist under
    // the test process's cwd, so the configured def set is empty and EVERY
    // name is a hallucination.
    let conway = build_conway(base_config(true), store.clone(), backend);
    let parent = idle_parent(&conway).await;

    let intent = tokio::time::timeout(
        CLASSIFY_TIMEOUT,
        conway.classify_agent_intent(parent.root(), SubagentMode::Spawn, "have someone review this"),
    )
    .await
    .expect("classify must not hang")
    .expect("classify should succeed");

    assert_eq!(
        intent,
        AgentIntent {
            recipe: SubagentMode::Fork,
            agent_def: None,
            prompt: "review this".to_string(),
        },
        "a hallucinated def name must be stripped, never passed through"
    );
    assert_eq!(all_sessions(&store).await.len(), 1, "no session may leak");
}

/// The strip policy's other half: a def name that IS configured reaches
/// the caller intact -- the validation is not a blanket strip.
#[tokio::test]
async fn classify_keeps_a_configured_agent_def() {
    let agents_dir = support::unique_temp_dir("intent-defs");
    std::fs::write(
        agents_dir.join("reviewer.md"),
        "---\nname: reviewer\ndescription: Code review specialist\n---\nYou review code.\n",
    )
    .expect("write reviewer def");

    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response(
            r#"{"recipe": "spawn", "agent_def": "reviewer", "prompt": "review the diff"}"#,
        ))])
        .with_id(BackendId::new("fake")),
    );
    let mut config = base_config(true);
    config.agents.dir = agents_dir;
    let conway = build_conway(config, store.clone(), backend);
    let parent = idle_parent(&conway).await;

    let intent = tokio::time::timeout(
        CLASSIFY_TIMEOUT,
        conway.classify_agent_intent(parent.root(), SubagentMode::Fork, "spawn a reviewer"),
    )
    .await
    .expect("classify must not hang")
    .expect("classify should succeed");

    assert_eq!(
        intent,
        AgentIntent {
            recipe: SubagentMode::Spawn,
            agent_def: Some("reviewer".to_string()),
            prompt: "review the diff".to_string(),
        },
        "a configured def name must reach the caller intact"
    );
    assert_eq!(all_sessions(&store).await.len(), 1, "no session may leak");
}

/// P-10: an invalid `recipe` value degrades the WHOLE reply to the
/// verbatim passthrough carrying the CALLER's default recipe (here
/// `Spawn`, proving the default is caller-supplied, not hardcoded) and the
/// raw text.
#[tokio::test]
async fn classify_an_invalid_recipe_value_passes_through_with_the_callers_default_recipe() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response(
            r#"{"recipe": "explode", "agent_def": null, "prompt": "rewritten prompt that must be discarded"}"#,
        ))])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(base_config(true), store.clone(), backend);
    let parent = idle_parent(&conway).await;

    let raw = "do something with this";
    let intent = tokio::time::timeout(
        CLASSIFY_TIMEOUT,
        conway.classify_agent_intent(parent.root(), SubagentMode::Spawn, raw),
    )
    .await
    .expect("classify must not hang")
    .expect("an invalid recipe must NOT fail the classify");

    assert_eq!(
        intent,
        AgentIntent {
            recipe: SubagentMode::Spawn,
            agent_def: None,
            prompt: raw.to_string(),
        },
        "an invalid recipe must degrade the whole reply to the passthrough"
    );
    assert_eq!(all_sessions(&store).await.len(), 1, "no session may leak");
}

/// "Other errors propagate": the intent turn failing (here a backend
/// error, folded into a `Failed` terminal by the agent loop) surfaces as
/// `ConwayError::IntentClassification` -- NOT a passthrough -- and the
/// intent session is STILL purged.
#[tokio::test]
async fn classify_propagates_a_failed_intent_turn_and_still_purges() {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Fail(BackendError::Transport {
            detail: "connection reset".to_string(),
        })])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(base_config(true), store.clone(), backend);
    let parent = idle_parent(&conway).await;

    let err = tokio::time::timeout(
        CLASSIFY_TIMEOUT,
        conway.classify_agent_intent(parent.root(), SubagentMode::Fork, "classify this"),
    )
    .await
    .expect("classify must not hang")
    .expect_err("a failed intent turn must propagate, not pass through");

    assert!(
        matches!(err, ConwayError::IntentClassification { .. }),
        "expected IntentClassification, got: {err:?}"
    );
    assert_eq!(
        all_sessions(&store).await.len(),
        1,
        "the intent session must be purged even when the turn fails"
    );
}
