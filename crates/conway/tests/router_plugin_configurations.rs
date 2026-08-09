//! Board item 01KZFC43J1J06BM4CCWKCKHSNV: the paired ABSENT/INSTALLED
//! configuration proof the item's own acceptance criteria (4, 5, 6) name
//! explicitly, both driven through a real `ConwayBuilder::build` and a real
//! `SessionHandle::prompt`, not a hand-constructed `Router`/`RouteRequest`.
//!
//! - [`absent_configuration_resolves_via_the_core_resolver_and_completes`]:
//!   with NO router installed, a role resolves to a model and a one-shot
//!   prompt completes against a configured backend, using
//!   `conway_core::routing::MinimalRouter` (the core resolver `build()`
//!   compiles when neither `with_router` nor `with_router_factory` is
//!   called). Every chain entry -- selected or not -- still carries a
//!   `RoutingReason` in the `explain_routing` report.
//! - [`installed_configuration_ordered_fallback_and_breaker_skip_the_open_endpoint`]:
//!   with `conway-plugin-routing`'s `RoutingRouterFactory` installed via
//!   `with_router_factory` (the library-embedder shape of the SAME
//!   `[plugins].install` mechanism `conway-cli`'s `first_party_plugins::
//!   router_bundle` resolves for the TUI and one-shot -- GP-05/C-03, no
//!   capability trapped in one mode), a failing primary candidate is
//!   skipped mid-turn (ordered fallback) for three consecutive turns, its
//!   Transport breaker opens on the third failure
//!   (`HealthConfig::transport_failures_to_open`'s default, 3), and a
//!   FOURTH turn never dials the primary candidate at all -- the router's
//!   own `HealthSkip` filtering, observed as a call count, not inferred.
//! - P-15 break-the-guard for the installed half (recorded in this item's
//!   own completion report, not committed here): stubbing out
//!   `conway-cli`'s `first_party_plugins::router_bundle` to return `vec![]`
//!   makes `[plugins].install = ["conway.routing"]` a hard "unknown id"
//!   config error, discriminating the installed configuration from the
//!   absent one -- the absent-configuration test above is unaffected by
//!   that stub (it names no router at all), which is the property that
//!   makes it a genuine negative control rather than a coincidentally
//!   passing one.

mod support;

use std::sync::Arc;

use conway::config::schema::{
    AgentsConfig, BackendEntry, ConwayConfig, HealthSection, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
    TuiSection,
};
use conway::{Conway, ConwayBuilder, EntryOutcome, RoutingReason, SessionSpec};
use conway_core::agent::{PermissionDecision, ResultStatus};
use conway_core::capabilities::{
    CacheMode, Capabilities, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{ContentBlock, StopReason, Usage};
use conway_core::error::BackendError;
use conway_core::fakes::{FakeGate, FakeStore, ScriptedBackend, ScriptedTurn};
use conway_core::ids::{BackendId, RoleAlias};
use conway_core::ports::{Backend, GenerateResponse, SessionStore};
use conway_plugin_routing::RoutingRouterFactory;

/// Every capability generous: the runtime's own unconditional
/// `tool_calling >= NonStreamingOnly` floor (whenever any tool is
/// registered, which built-ins always are) must never be what disqualifies
/// a candidate in these fixtures -- only the router's health/fallback
/// filtering should.
fn caps() -> Capabilities {
    Capabilities {
        tool_calling: ToolCallSupport::Streaming { validated: true },
        cache: CacheMode::None,
        parallel_tool_calls: true,
        structured_output: StructuredOutput::Grammar,
        max_context_tokens: 100_000,
        reasoning: true,
        reliability_tier: ReliabilityTier::Verified,
    }
}

fn text_response(text: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Usage::default(),
    }
}

fn backend_entry() -> BackendEntry {
    BackendEntry {
        kind: "openai-compat".to_string(),
        base_url: "http://127.0.0.1:9".to_string(),
        dialect: Some("openai".to_string()),
        ..BackendEntry::default()
    }
}

fn base_config(
    chain: Vec<String>,
    backend_ids: &[&str],
    metadata_path: std::path::PathBuf,
) -> ConwayConfig {
    let mut roles = std::collections::BTreeMap::new();
    roles.insert(
        "coder".to_string(),
        RoleEntry {
            chain,
            headroom_tokens: None,
            ..Default::default()
        },
    );
    let mut backends = std::collections::BTreeMap::new();
    for id in backend_ids {
        backends.insert(id.to_string(), backend_entry());
    }
    ConwayConfig {
        default_role: RoleAlias::new("coder"),
        cwd: std::path::PathBuf::from("."),
        session: SessionConfig::default(),
        limits: LimitsConfig::default(),
        permissions: PermissionsConfig::default(),
        backends,
        routing: RoutingSection {
            default_headroom_tokens: 4_096,
        },
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig {
            metadata_path,
            probe_on_startup: false,
        },
        tui: TuiSection::default(),
        tools: ToolsConfig::default(),
        plugins: PluginsConfig::default(),
    }
}

/// Acceptance item 4: absent configuration. No `.with_router`/
/// `.with_router_factory` call anywhere below -- `build()`'s own default,
/// `conway_core::routing::MinimalRouter`.
#[tokio::test]
async fn absent_configuration_resolves_via_the_core_resolver_and_completes() {
    // MinimalRouter never consults a `CapabilityIndex` at all -- unlike the
    // installed-configuration test below, this fixture needs no
    // `models.json`; a nonexistent path resolves to empty metadata
    // (`config::model_metadata::load`'s own "missing -> empty" contract).
    let dir = support::unique_temp_dir("router-plugin-configurations-absent");
    let config = base_config(
        vec!["primary/model-a".to_string(), "primary/model-b".to_string()],
        &["primary"],
        dir.join("models.json"),
    );
    let backend: Arc<dyn Backend> = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("ok"))])
            .with_id(BackendId::new("primary")),
    );
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway: Conway = ConwayBuilder::from_parts(config)
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .build()
        .expect("build should succeed: MinimalRouter needs nothing but [roles]");

    // A role resolves to a model and a one-shot prompt completes against a
    // configured backend (acceptance item 4's literal wording).
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hello").await.expect("prompt");
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), turn.result())
        .await
        .expect("result must not hang")
        .expect("result() itself must not error");
    assert_eq!(
        result.status,
        ResultStatus::Completed,
        "the core resolver must route the one configured chain and complete, got: {:?}",
        result.status
    );

    // Acceptance item 6: every candidate -- selected AND skipped -- still
    // carries a RoutingReason, in this (absent-plugin) configuration.
    let report = conway.explain_routing(&RoleAlias::new("coder"));
    assert_eq!(report.entries.len(), 2, "one entry per chain candidate");
    match &report.entries[0].outcome {
        EntryOutcome::Selected { reason } => {
            assert!(
                matches!(reason, RoutingReason::AliasPrimary { .. }),
                "got {reason:?}"
            );
        }
        other => panic!("expected Selected, got {other:?}"),
    }
    match &report.entries[1].outcome {
        EntryOutcome::Skipped { reason } => {
            assert!(
                matches!(reason, RoutingReason::Fallback { .. }),
                "got {reason:?}"
            );
        }
        other => panic!("expected Skipped, got {other:?}"),
    }
}

/// Acceptance item 5: installed configuration -- `RoutingRouterFactory`
/// (the SAME first-party plugin `conway-cli`'s `first_party_plugins::
/// router_bundle` links for the TUI and one-shot) via
/// `with_router_factory`, the library-embedder install path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn installed_configuration_ordered_fallback_and_breaker_skip_the_open_endpoint() {
    // Unlike the absent-configuration test above, `DeclarativeRouter`
    // filters on `RouterBuildContext::capability_index`, which
    // `ConwayBuilder::build` populates from `.conway/models.json` -- both
    // chain candidates need a real entry there or the router rejects them
    // as an unindexed pair before ever reaching fallback/breaker logic.
    let dir = support::unique_temp_dir("router-plugin-configurations-installed");
    let metadata_path = dir.join("models.json");
    std::fs::write(
        &metadata_path,
        r#"{"models":{
            "primary/model-a":{"max_context_tokens":100000,"tool_calling":"streaming","reasoning":true,"reliability_tier":"verified"},
            "secondary/model-b":{"max_context_tokens":100000,"tool_calling":"streaming","reasoning":true,"reliability_tier":"verified"}
        }}"#,
    )
    .expect("write models.json fixture");
    let config = base_config(
        vec![
            "primary/model-a".to_string(),
            "secondary/model-b".to_string(),
        ],
        &["primary", "secondary"],
        metadata_path,
    );
    let primary = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Fail(BackendError::Transport {
                detail: "primary unreachable".into(),
            });
            3
        ])
        .with_id(BackendId::new("primary"))
        .with_capabilities(caps()),
    );
    let secondary = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(text_response("ok-1")),
            ScriptedTurn::Respond(text_response("ok-2")),
            ScriptedTurn::Respond(text_response("ok-3")),
            ScriptedTurn::Respond(text_response("ok-4")),
        ])
        .with_id(BackendId::new("secondary"))
        .with_capabilities(caps()),
    );
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway: Conway = ConwayBuilder::from_parts(config)
        .with_backend(primary.clone() as Arc<dyn Backend>)
        .with_backend(secondary.clone() as Arc<dyn Backend>)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router_factory(Arc::new(RoutingRouterFactory))
        .build()
        .expect("build should succeed: a valid two-candidate chain");

    // Each turn below opens its OWN session (`SessionHandle::prompt` is a
    // one-shot-per-handle API -- a second `.prompt()` call on the same
    // handle does not resolve): breaker/health state lives on the shared
    // `Runtime` this `Conway` instance owns, not per-session, so a fresh
    // session per turn still exercises the SAME `HealthRegistry` across all
    // four turns, exactly as four separate real `conway -p` invocations
    // against the same long-lived `settings.json` would.

    // Turns 1-3: primary fails (ORDERED FALLBACK -- ordinary Transport
    // failure advances to the next candidate within the same turn), the
    // turn still completes via secondary, and the third failure trips the
    // Transport breaker (default transport_failures_to_open: 3).
    for _ in 0..3 {
        let handle = conway
            .new_session(SessionSpec::default())
            .await
            .expect("new_session should succeed");
        let turn = handle.prompt("hello").await.expect("prompt");
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), turn.result())
            .await
            .expect("result must not hang")
            .expect("result() itself must not error");
        assert_eq!(
            result.status,
            ResultStatus::Completed,
            "ordered fallback to secondary must complete the turn, got: {:?}",
            result.status
        );
    }
    assert_eq!(
        primary.calls().len(),
        3,
        "primary must have been dialed exactly once per turn for turns 1-3"
    );
    assert_eq!(
        secondary.calls().len(),
        3,
        "secondary served all 3 fallbacks"
    );

    // Turn 4: the BREAKER (not ordinary fallback) now skips primary before
    // it is ever dialed again -- the discriminating, observable proof that
    // health state, not just per-turn retry, is live under the installed
    // plugin.
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hello").await.expect("prompt");
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), turn.result())
        .await
        .expect("result must not hang")
        .expect("result() itself must not error");
    assert_eq!(
        result.status,
        ResultStatus::Completed,
        "got: {:?}",
        result.status
    );
    assert_eq!(
        primary.calls().len(),
        3,
        "the OPEN breaker must skip primary on turn 4 -- no new call beyond the first 3"
    );
    assert_eq!(
        secondary.calls().len(),
        4,
        "secondary served turn 4 directly"
    );

    // Acceptance item 6 (installed half): both candidates still carry a
    // RoutingReason -- primary now HealthSkip, secondary Selected.
    let report = conway.explain_routing(&RoleAlias::new("coder"));
    assert_eq!(report.entries.len(), 2);
    match &report.entries[0].outcome {
        EntryOutcome::Skipped { reason } => {
            assert!(
                matches!(reason, RoutingReason::HealthSkip { .. }),
                "primary's open breaker must show as HealthSkip, got {reason:?}"
            );
        }
        other => panic!("expected primary Skipped, got {other:?}"),
    }
    match &report.entries[1].outcome {
        EntryOutcome::Selected { reason } => {
            assert!(
                matches!(reason, RoutingReason::Fallback { .. }),
                "got {reason:?}"
            );
        }
        other => panic!("expected secondary Selected, got {other:?}"),
    }
}
