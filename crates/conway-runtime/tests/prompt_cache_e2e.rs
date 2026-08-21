//! End-to-end proof that Anthropic prompt caching is actually LIVE (the
//! item that fixed conway never emitting `cache_control`): a real turn,
//! driven entirely through `Runtime::start_root` and, separately, a forked
//! child (`SubagentHost::start`), must hand `Backend::generate` a request
//! whose segments carry `PromptSegment::cache_hint` when the resolved
//! model's CAPABILITY declares `CacheMode::ExplicitBreakpoints` — exactly
//! what `conway-plugin-backends`'s `AnthropicBackend` declares for every real
//! Claude model (`anthropic_defaults`, `crates/conway-plugin-backends/src/
//! capabilities.rs`).
//!
//! Why this file exists rather than relying on `conway-plugin-backends/tests/
//! anthropic_cache_mapping.rs`: that file (and any other unit test on
//! `apply_cache_hints` alone) hand-constructs segments that ALREADY carry a
//! `cache_hint` — it would have passed throughout the entire outage this
//! item fixes, because the outage was never in the hint -> `cache_control`
//! mapping. It was that nothing upstream of that mapping ever SET a
//! `cache_hint` in production: `ContextBuilder::build` only attaches one
//! when `ContextInput.cache_mode` is `ExplicitBreakpoints`/`SlotKv`, and
//! every real call site (`runtime.rs::start_root`/resume, `subagent.rs`'s
//! fork/spawn) hardcoded `CacheMode::None` — a pre-routing placeholder,
//! since `ContextBuilder::build` runs before a model is chosen. Nothing in
//! this file hand-constructs a `ContextInput` or calls `ContextBuilder`
//! directly; every assertion below reads a `GenerateRequest` exactly as
//! `AttemptEngine::execute`'s post-routing cache-hint pass
//! (`crate::attempt::attach_route_cache_hints`) built it and handed it to
//! `Backend::generate`, for a real `Runtime::start_root`/fork call.
//!
//! `ScriptedBackend` stands in for `AnthropicBackend` (this crate does not
//! depend on `conway-plugin-backends`): its `capabilities()` is set to the
//! SAME `CacheMode::ExplicitBreakpoints { max_breakpoints: 4, .. }` value
//! `anthropic_defaults()` declares, so the mechanism under test —
//! capability-keyed cache-hint attachment in the attempt layer — runs
//! identically to how it would against the real adapter; only the HTTP
//! transport and `cache_control` wire-mapping (covered by
//! `anthropic_cache_mapping.rs`) differ.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use conway_core::agent::{Budget, PermissionDecision};
use conway_core::capabilities::{
    CacheMode, CacheTtl, Capabilities, HeadroomPolicy, ReliabilityTier, StructuredOutput,
    ToolCallSupport,
};
use conway_core::content::{ContentBlock, StopReason, Usage};
use conway_core::event::Event;
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias};
use conway_core::ports::{Backend, GenerateRequest, Router, SessionStore, SubagentHost};
use conway_core::provenance::Provenance;
use conway_runtime::events::EventBus;
use conway_runtime::runtime::{RootSpec, Runtime, RuntimeDeps};
use conway_testkit::{FakeGate, FakeHealth, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};
use futures::StreamExt;

// ---------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------

/// The exact `CacheMode` `conway-plugin-backends`'s `anthropic_defaults()`
/// declares for a real Claude model.
fn anthropic_like_capabilities() -> Capabilities {
    Capabilities {
        tool_calling: ToolCallSupport::Streaming { validated: true },
        cache: CacheMode::ExplicitBreakpoints {
            max_breakpoints: 4,
            ttls: vec![CacheTtl::FiveMinutes, CacheTtl::OneHour],
        },
        max_context_tokens: 200_000,
        structured_output: StructuredOutput::JsonSchema,
        parallel_tool_calls: true,
        reliability_tier: ReliabilityTier::Verified,
        reasoning: false,
    }
}

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

/// A `Runtime` wired to one `ScriptedBackend` whose declared capability is
/// `anthropic_like_capabilities()` — an "Anthropic-capability model" for
/// every purpose the cache-hint post-pass cares about, without this crate
/// depending on `conway-plugin-backends`.
fn build_runtime(turns: usize) -> (Arc<Runtime>, Arc<ScriptedBackend>) {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(
            (0..turns)
                .map(|_| ScriptedTurn::Respond(text_response("ok")))
                .collect(),
        )
        .with_id(BackendId::new("anthropic-like"))
        .with_capabilities(anthropic_like_capabilities()),
    );
    let model = ModelRef {
        backend: backend.id(),
        model: ModelId::new("claude-sonnet-4-6"),
    };
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend.clone());

    let runtime = Runtime::new(RuntimeDeps {
        store,
        path_store: std::sync::Arc::new(conway_testkit::FakePathStore::new()),
        router,
        health: Arc::new(FakeHealth::new()),
        backends,
        plugins: vec![],
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs: HashMap::new(),
        skills: Default::default(),
        event_bus: EventBus::with_default_capacity(),
        headroom: Arc::new(HeadroomPolicy::default()),
    });
    (runtime, backend)
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

async fn wait_for_agent_finished(
    stream: &mut conway_runtime::events::EventStream,
    agent: AgentId,
) -> conway_core::agent::AgentResult {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let envelope = stream.next().await.expect("event stream ended early");
            if envelope.agent == agent {
                if let Event::AgentFinished { result, .. } = envelope.event {
                    return result;
                }
            }
        }
    })
    .await
    .expect("agent never finished")
}

async fn start_and_finish_root(runtime: &Runtime, prompt: &str) -> AgentId {
    let mut stream = runtime.subscribe();
    let root = runtime.start_root(root_spec(prompt)).await.unwrap();
    wait_for_agent_finished(&mut stream, root).await;
    root
}

/// The (unique) breakpointed segment indices in `req.segments`, in order.
fn breakpointed_indices(req: &GenerateRequest) -> Vec<usize> {
    req.segments
        .iter()
        .enumerate()
        .filter(|(_, s)| s.cache_hint.as_ref().is_some_and(|h| h.breakpoint))
        .map(|(i, _)| i)
        .collect()
}

// ---------------------------------------------------------------------
// Root: A (ToolSchemas) breakpointed, no B (no inherited prefix to have one)
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_root_turn_against_an_anthropic_capability_model_emits_a_cache_breakpoint() {
    let (runtime, backend) = build_runtime(1);
    start_and_finish_root(&runtime, "investigate the bug").await;

    let calls = backend.calls();
    assert_eq!(
        calls.len(),
        1,
        "the root's one turn must reach the backend exactly once"
    );
    let req = &calls[0];

    let marked = breakpointed_indices(req);
    assert!(
        !marked.is_empty(),
        "a live turn against a CacheMode::ExplicitBreakpoints model must emit at least one \
         cache_hint breakpoint -- got none. `req.segments`: {:#?}",
        req.segments
    );

    // A root's first turn has no InheritedPrefix, so only breakpoint A (the
    // ToolSchemas segment) is available -- and it must be exactly the one
    // marked.
    let a = req
        .segments
        .iter()
        .rposition(|s| matches!(s.provenance, Provenance::ToolRegistry { .. }))
        .expect("ToolSchemas segment is unconditional");
    assert_eq!(
        marked,
        vec![a],
        "a root turn must breakpoint exactly A, no B"
    );

    let hint = req.segments[a]
        .cache_hint
        .as_ref()
        .expect("index came from the breakpointed-indices filter");
    assert_eq!(
        hint.ttl,
        CacheTtl::FiveMinutes,
        "AgentSpec::cache_ttl's default"
    );
}

// ---------------------------------------------------------------------
// Fork: child's turn breakpoints BOTH A (own ToolSchemas) and B (inherited)
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_forked_childs_turn_breakpoints_both_a_and_b() {
    let (runtime, backend) = build_runtime(2);
    let root = start_and_finish_root(&runtime, "investigate").await;

    let mut stream = runtime.subscribe();
    let child = SubagentHost::start(
        &*runtime,
        root,
        root,
        conway_core::agent::SubagentSpec::fork("look closer", Budget::default()),
    )
    .await
    .unwrap();
    wait_for_agent_finished(&mut stream, child).await;

    let calls = backend.calls();
    assert_eq!(
        calls.len(),
        2,
        "the root's turn, then the fork child's turn, each reach the backend once"
    );
    let child_req = &calls[1];

    let a = child_req
        .segments
        .iter()
        .rposition(|s| matches!(s.provenance, Provenance::ToolRegistry { .. }))
        .expect("ToolSchemas segment is unconditional");
    let b = child_req
        .segments
        .iter()
        .rposition(|s| matches!(s.provenance, Provenance::Inherited { .. }))
        .expect("a fork child always inherits at least the root's own prior turn");

    let marked = breakpointed_indices(child_req);
    assert_eq!(
        marked,
        {
            let mut expected = vec![b, a];
            expected.sort_unstable();
            expected
        },
        "a fork child's turn must breakpoint both A (own ToolSchemas) and B \
         (the inherited prefix) -- the case where caching compounds \
         most: every sibling forked at the same point shares B's prefix"
    );
}

// ---------------------------------------------------------------------
//: the attempt-layer post-pass may only ever change PRICE, never
// the request the model actually sees.
// ---------------------------------------------------------------------

/// `(role, content, provenance, tokens_est)` for every segment -- everything
/// EXCEPT `id` (agent-derived, so it differs across the two independent
/// `Runtime`s this test spins up even for identical content) and
/// `cache_hint` (what this test is proving does not matter).
fn content_identity(
    segments: &[conway_core::segment::PromptSegment],
) -> Vec<(
    conway_core::content::Role,
    Vec<ContentBlock>,
    Provenance,
    Option<u32>,
)> {
    segments
        .iter()
        .map(|s| {
            (
                s.role,
                s.content.clone(),
                s.provenance.clone(),
                s.tokens_est,
            )
        })
        .collect()
}

/// Two otherwise-identical roots against two otherwise-identical models,
/// differing ONLY in the resolved capability's `CacheMode` (one
/// `ExplicitBreakpoints`, one `None`) -- the exact axis
/// `attach_route_cache_hints` (`attempt.rs`) branches on.: stripping
/// `cache_hint` must make the two requests identical in everything that
/// reaches the model. This is the widened surface this item adds (a NEW
/// mutation site, cloning and editing segments post-routing, per attempted
/// route) -- `context_golden.rs`'s `cache_neutrality_holds_for_every_
/// golden_case` already covers `ContextBuilder::build`'s own hint
/// attachment; this test covers the new one, end to end.
#[tokio::test]
async fn gp06_stripping_cache_hint_makes_a_cached_and_uncached_route_identical() {
    let (cached_runtime, cached_backend) = build_runtime(1);

    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let uncached_backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("ok"))])
            .with_id(BackendId::new("no-cache"))
            .with_capabilities(Capabilities {
                cache: CacheMode::None,
                ..anthropic_like_capabilities()
            }),
    );
    let model = ModelRef {
        backend: uncached_backend.id(),
        model: ModelId::new("claude-sonnet-4-6"),
    };
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(model));
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(
        uncached_backend.id(),
        uncached_backend.clone() as Arc<dyn Backend>,
    );
    let uncached_runtime = Runtime::new(RuntimeDeps {
        store,
        path_store: std::sync::Arc::new(conway_testkit::FakePathStore::new()),
        router,
        health: Arc::new(FakeHealth::new()),
        backends,
        plugins: vec![],
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs: HashMap::new(),
        skills: Default::default(),
        event_bus: EventBus::with_default_capacity(),
        headroom: Arc::new(HeadroomPolicy::default()),
    });

    start_and_finish_root(&cached_runtime, "investigate the bug").await;
    start_and_finish_root(&uncached_runtime, "investigate the bug").await;

    let cached_req = cached_backend.calls().remove(0);
    let uncached_req = uncached_backend.calls().remove(0);

    // Sanity: the two requests actually exercised different branches --
    // otherwise this test would trivially pass by testing nothing.
    assert!(!breakpointed_indices(&cached_req).is_empty());
    assert!(breakpointed_indices(&uncached_req).is_empty());

    assert_eq!(cached_req.model, uncached_req.model);
    assert_eq!(cached_req.tools, uncached_req.tools);
    assert_eq!(cached_req.params, uncached_req.params);
    assert_eq!(
        content_identity(&cached_req.segments),
        content_identity(&uncached_req.segments),
        ": a cache hint must never be correctness-bearing -- with \
         cache_hint (and the agent-derived segment id) excluded, a cached \
         and an uncached route's requests must be identical"
    );
}
