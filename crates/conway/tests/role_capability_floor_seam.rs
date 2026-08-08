//! Per-role capability-floor seam test (Change 2): a real
//! `[roles.<alias>].min_reliability` floor, parsed from JSON exactly as
//! `.conway/settings.json` would be, driven through a real
//! `ConwayConfig::routing()` -> `DeclarativeRouter` -> `AttemptEngine` ->
//! `Backend`, asserting the OBSERVABLE OUTCOME (GP-14 / `LIVENESS_TESTS.md`):
//! a candidate that does not meet the configured floor is never called.
//!
//! ## Why `min_reliability`, not `tool_calling`
//!
//! `conway-runtime`'s `AgentLoop::route_and_attempt` (`crates/conway-runtime/
//! src/agent_loop.rs`) builds its own `RouteRequest.required` for every
//! real turn: headroom, plus (when the turn has any registered tools --
//! true by default, since `fs`/`subagent`/`report` ship registered) a
//! floor of `ToolCallSupport::NonStreamingOnly` on `tool_calling`
//! specifically. Probing the config floor with `tool_calling` would
//! therefore be confounded -- a rejection could come from that pre-existing
//! runtime behavior alone, with the new per-role config path contributing
//! nothing (this is not hypothetical: an earlier draft of this file used
//! `tool_calling` and its break-the-guard step failed to break, because
//! the runtime's own floor was silently doing the rejecting). None of the
//! other five fields (`structured_output`, `parallel_tool_calls`,
//! `reasoning`, `min_reliability`, `min_context`) get any such runtime-side
//! bump, so `min_reliability` isolates the config path cleanly.
//!
//! ## Why config parsing, not a hand-built `RequiredCaps`
//!
//! This file parses a JSON document (the same shape a real `settings.json`
//! would contain, including the `min_reliability` string wire vocabulary)
//! with `serde_json::from_str::<ConwayConfig>`, so the seam under test --
//! JSON wire format -> `RoleEntry` -> `RequiredCaps` -> admission -- is
//! exercised end to end, the same "config parsing, not by hand-constructing
//! `RequiredCaps`" discipline `context_admission_seam.rs` uses for headroom.
//!
//! ## What "admission" required beyond the schema mapping
//!
//! Investigation while writing this test found `DeclarativeRouter` never
//! read `RoleConfig::required` at all -- `CompiledRole` didn't carry the
//! field, and `check_candidate` consulted only the caller-supplied
//! `req.required`. Populating `RequiredCaps` from config
//! (`ConwayConfig::routing()`, this change's schema half) was therefore
//! necessary but not sufficient: without also wiring the router to merge
//! the role's configured floor into its admission check
//! (`crates/conway-plugin-routing/src/router.rs`'s `effective_required` /
//! `crate::capability::strictest`, added alongside this test), the schema
//! change alone would have been exactly the kind of unreached
//! configuration this whole item exists to prevent. Both are proven here:
//! this file end to end through a real `Conway` (with the
//! `conway-plugin-routing` first-party plugin installed -- board item
//! 01KZFC43J1J06BM4CCWKCKHSNV, see `build_conway`'s own doc), and
//! `crates/conway-plugin-routing/tests/router_resolution.rs`'s
//! `role_configured_capability_floor_rejects_a_candidate_that_does_not_meet_it`
//! / `..._admits_a_candidate_that_meets_it` directly against
//! `DeclarativeRouter`.
//!
//! ## Break-the-guard (recorded in the completion report)
//!
//! Reverting `ConwayConfig::routing()`'s mapping back to the hardcoded
//! `RequiredCaps::default()` makes
//! `undercapable_model_is_never_called_and_turn_fails` fail: the backend
//! IS called (the floor silently vanishes, exactly the pre-existing bug),
//! proving this test can fail and would have caught the defect this change
//! fixes.
//!
//! ## Fixture shape
//!
//! One role (`coder`), one backend (`fake`), one model
//! (`fake/tiny-model`) declared with generous context window and every
//! OTHER capability set high, so the sole way it can be rejected is the
//! configured `min_reliability` floor — mirroring
//! `context_admission_seam.rs`'s "only one dial moves" discipline.

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::ConwayConfig;
use conway::{Conway, ConwayBuilder, SessionSpec};
use conway_core::agent::{PermissionDecision, ResultStatus};
use conway_core::capabilities::{
    CacheMode, Capabilities, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{ContentBlock, StopReason, Usage};
use conway_core::fakes::{FakeGate, FakeStore, ScriptedBackend, ScriptedTurn};
use conway_core::ids::BackendId;
use conway_core::ports::{Backend, GenerateResponse, SessionStore};

/// The one model this fixture's role chain names.
const MODEL: &str = "fake/tiny-model";

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

/// Every capability generous EXCEPT `reliability_tier`, which the caller
/// sets explicitly — the one dial this fixture moves. `tool_calling` is set
/// to `Streaming { validated: true }` (the top rank) so the runtime's own
/// unconditional `NonStreamingOnly` floor (see the module doc) never
/// contributes to a rejection here.
fn caps(reliability_tier: ReliabilityTier) -> Capabilities {
    Capabilities {
        tool_calling: ToolCallSupport::Streaming { validated: true },
        cache: CacheMode::None,
        parallel_tool_calls: true,
        structured_output: StructuredOutput::Grammar,
        max_context_tokens: 10_000_000,
        reasoning: true,
        reliability_tier,
    }
}

fn write_model_metadata(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("models.json");
    std::fs::write(
        &path,
        format!(
            r#"{{"models":{{"{MODEL}":{{"max_context_tokens":10000000,"tool_calling":"streaming","reasoning":true,"reliability_tier":"verified"}}}}}}"#
        ),
    )
    .expect("write models.json fixture");
    path
}

/// A real `ConwayConfig`, parsed from JSON — the actual `settings.json`
/// wire shape, including the `min_reliability` capability-floor string.
/// `coder` (the `default_role`) requires at least `verified` reliability.
fn config_naming(metadata_path: PathBuf) -> ConwayConfig {
    let json = format!(
        r#"{{
          "default_role": "coder",
          "backends": {{
            "fake": {{
              "kind": "openai-compat",
              "dialect": "openai",
              "base_url": "http://127.0.0.1:9",
              "api_key_env": ""
            }}
          }},
          "roles": {{
            "coder": {{
              "chain": ["{MODEL}"],
              "min_reliability": "verified"
            }}
          }},
          "models": {{
            "metadata_path": {metadata_path:?}
          }}
        }}"#
    );
    serde_json::from_str(&json).expect("fixture JSON must parse as ConwayConfig")
}

/// Board item 01KZFC43J1J06BM4CCWKCKHSNV: `conway` no longer compiles a
/// capability-filtering `DeclarativeRouter` in by default -- the role's
/// configured `min_reliability` floor this file exists to prove is enforced
/// by that engine specifically, so it must be installed via
/// `with_router_factory` for these assertions to mean anything (absent it,
/// `conway_core::routing::MinimalRouter` performs no capability filtering at
/// all, and the backend would be called regardless of `reliability_tier`).
fn build_conway(config: ConwayConfig, backend: Arc<dyn Backend>, store: Arc<dyn SessionStore>) -> Conway {
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    ConwayBuilder::from_parts(config)
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router_factory(Arc::new(conway_plugin_routing::RoutingRouterFactory))
        .build()
        .expect(
            "build should succeed: real ContextBuilder/DeclarativeRouter/AttemptEngine wiring \
             from valid config",
        )
}

#[tokio::test]
async fn undercapable_model_is_never_called_and_turn_fails() {
    let dir = support::unique_temp_dir("role-capability-floor-seam-reject");
    let metadata_path = write_model_metadata(&dir);
    let config = config_naming(metadata_path);

    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("should never run"))])
            .with_id(BackendId::new("fake"))
            // Below the configured floor (`verified`): `Community` ranks
            // lower than `Verified`.
            .with_capabilities(caps(ReliabilityTier::Community)),
    );
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway = build_conway(config, backend.clone(), store);

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hello").await.expect("prompt");
    let result = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result must not hang")
        .expect("result() itself must not error -- the turn ends Failed, not the stream");

    // PRIMARY (GP-14/LIVENESS_TESTS.md): the observable outcome, not an
    // intermediate signal.
    assert!(
        backend.calls().is_empty(),
        "the backend must NEVER be called when the only chain candidate fails the role's \
         configured min_reliability floor; calls: {:?}",
        backend.calls()
    );

    // SECONDARY: the typed error names the missing capability, so a
    // regression that silently drops the floor (the pre-existing bug this
    // change fixes) is caught here even if something else still stops the
    // backend from being called.
    match &result.status {
        ResultStatus::Failed { error } => {
            assert!(error.contains("reliability_tier"), "got: {error}");
            assert!(error.contains("requires Verified"), "got: {error}");
        }
        other => panic!("expected ResultStatus::Failed, got {other:?}"),
    }
}

/// GP-14: "any check that cannot fail is not a check" — proof the assertion
/// above can fail. Identical fixture, exactly one field changed: the
/// model's `reliability_tier`, raised to meet the configured floor. The
/// backend is now called exactly once and the turn completes normally.
#[tokio::test]
async fn meeting_the_floor_admits_the_identical_request_and_calls_the_backend() {
    let dir = support::unique_temp_dir("role-capability-floor-seam-admit");
    let metadata_path = write_model_metadata(&dir);
    let config = config_naming(metadata_path);

    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("ok"))])
            .with_id(BackendId::new("fake"))
            .with_capabilities(caps(ReliabilityTier::Verified)),
    );
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway = build_conway(config, backend.clone(), store);

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hello").await.expect("prompt");
    let result = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result must not hang")
        .expect("result should succeed");

    assert_eq!(
        backend.calls().len(),
        1,
        "meeting the configured floor must admit the identical request and reach the backend \
         exactly once, calls: {:?}",
        backend.calls()
    );
    assert_eq!(
        result.status,
        ResultStatus::Completed,
        "an admitted request must complete normally, got: {:?}",
        result.status
    );
}
