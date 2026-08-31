//! One end-to-end admission test:
//! a real `ContextBuilder`'s `est_tokens`, through a real
//! `DeclarativeRouter` compiled from real config, into a real
//! `AttemptEngine`'s own T-1 backstop — asserting the one thing
//! ("reject; never truncate or escalate") actually promises an operator:
//! **THE BACKEND WAS NEVER CALLED.**
//!
//! ## Why this lives in `crates/conway/tests/`, not `conway-routing` or
//! `conway-runtime`
//!
//! `ConwayBuilder::build` (this crate) is the one place in the whole tree
//! that wires all three pieces from real, validated config exactly as
//! production does: its step 5 builds `CapabilityIndex::from_backends` by
//! calling `Backend::capabilities()` on the already-constructed backend —
//! the SAME accessor `AttemptEngine`'s T-1 gate reads directly — its step 7
//! compiles a `DeclarativeRouter` from that index plus
//! `RoutingConfig`/`HeadroomPolicy`, and `Runtime::new` hands both to the
//! `AgentLoop` that `SessionHandle::prompt` drives. `conway-runtime`'s own
//! `agent_loop_e2e.rs` proves `AgentLoop`'s *reaction* to a value
//! (`CapturingRouter::erroring(RoutingError::ContextTooLarge { .. })`); it
//! cannot prove the router ever *produces* that value, because it never
//! constructs a real one. `conway-routing`'s own `router.rs` tests prove the
//! router produces the value from a hand-built `CapabilityIndex`/
//! `RouteRequest`; they never reach an `AttemptEngine` or a `Backend`. This
//! file is the only one that drives all three through the identical
//! construction path a real embedder's `ConwayBuilder::from_config(..).build()?`
//! takes — the gap the item names: "two work items, each complete and
//! tested, connection owned by neither."
//!
//! **Correction (board item `01M1AVZPTRSWVE33G4DTJY7Q1B`, dated so this
//! does not stand as a current claim): no test below calls
//! `ConwayBuilder::with_router_factory`, and `conway`'s own `[dependencies]`
//! no longer link `conway-plugin-routing` at all (see that crate's
//! dev-dependency comment in `crates/conway/Cargo.toml`) — so `build()`'s
//! step 7 takes its OWN `else` branch and compiles `conway_core::routing::
//! MinimalRouter`, not a `DeclarativeRouter`, for every test in this file.**
//! `MinimalRouter` does no capability or headroom filtering at all (see its
//! own doc) — every chain candidate reaches `AttemptEngine` unfiltered, and
//! the rejection this file proves is produced ENTIRELY by `Backend::admit`/
//! `check_admission`, the AUTHORITATIVE T-1 backstop (`ports/backend.rs`'s
//! own doc: "THE production admission path"), not by any router-level
//! advisory check. This does not weaken what the file proves — `check_
//! admission` is the SAME gate a `DeclarativeRouter`-routed candidate is
//! checked against too, and an operator who never installs `conway.routing`
//! (a real, documented, opt-in configuration; `docs/routing.md`'s own
//! opening section) hits exactly this path in production — but the claim
//! above about which `Router` runs was wrong, and this note exists so it
//! is not silently taken as accurate.
//!
//! ## Only the `Backend` is faked
//!
//! Every other port here is real production code: a real `ConwayConfig`,
//! a real `ConwayBuilder::build` (real `ContextBuilder`, real
//! `CapabilityIndex::from_backends`, real `DeclarativeRouter::new`, real
//! `AttemptEngine`), a real `AgentLoop` driven via `SessionHandle::prompt`.
//! No `.with_router(..)` call appears anywhere below — the router is always
//! the one `ConwayBuilder::build` compiles itself from `config`.
//! `ScriptedBackend` is the sole double, and only because a fake is the
//! only way to observe "never called" (`LIVENESS_TESTS.md`'s
//! observable-outcome rule: a call count is normally just an intermediate
//! signal — it is promoted to the actual proof here because
//! `ScriptedBackend::calls()` stays a faithful zero/non-zero count no
//! matter WHICH real layer, router or attempt engine, ends up being the one
//! that rejects — see the break-the-guard note recorded in this item's
//! completion report for why that layer-independence is exactly what makes
//! the assertion meaningful).
//!
//! ## The fixture: a headroom-only rejection, not a mixed one
//!
//! `DeclarativeRouter::resolve` (closed by an earlier gap item,
//! commit `f8b8cc4`) returns `ContextTooLarge` only when EVERY candidate in
//! the chain is rejected and each one *solely* on the headroom gate; a
//! mixed failure (headroom plus a missing capability) falls back to
//! `NoCandidate`, which carries none of structured fields. The single
//! candidate below (`fake/tiny-model`) is given every OTHER capability a
//! request could plausibly need (`Streaming { validated: true }` tool
//! calling, `Grammar` structured output, `parallel_tool_calls`,
//! `reasoning`, `Verified` reliability), so the only way it can fail is the
//! context window — exactly the fixture shape this item's own steering text
//! warns is required, or the router's `NoCandidate` fallback (not this
//! item's concern) is what this test would catch instead.
//!
//! ## Out of scope here (separate, filed)
//!
//! - The capability-source disagreement between the router's static index
//!   and `AttemptEngine`'s live `backend.capabilities()`: this fixture keeps
//!   both in EXACT
//!   agreement by construction — `CapabilityIndex::from_backends` calls the
//!   very same `ScriptedBackend::capabilities()` the attempt engine's own
//!   gate would call — not by luck.
//! - The mixed-failure gap: not
//!   constructed here (see above).
//! - The estimator's tool-schema under-count:
//!   this test never predicts or asserts a numeric `est_tokens` value — it
//!   proves the seam carries WHATEVER the real estimator produces, by
//!   pinning `max_context_tokens` far below (the rejection test) or far
//!   above (the negative control) any plausible real estimate, rather than
//!   by computing the estimate itself.

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, BackendEntry, ConwayConfig, HealthSection, HooksConfig, LimitsConfig,
    ModelsConfig, PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig,
    ToolsConfig,
};
use conway::test_support::test_builder_without_router;
use conway::SessionSpec;
use conway_core::agent::ResultStatus;
use conway_core::capabilities::{
    CacheMode, Capabilities, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::ids::{BackendId, RoleAlias};
use conway_core::ports::SessionStore;
use conway_testkit::{text_response, FakeStore, ScriptedBackend, ScriptedTurn};

/// The one model this fixture's role chain names. Its window
/// (`max_context_tokens`) is the only knob that differs between the
/// rejection test and its negative control.
const MODEL: &str = "fake/tiny-model";

/// Every capability EXCEPT the context window set generously — see the
/// module doc's "headroom-only, not mixed" note: this is what keeps the
/// router's rejection attributable to context size alone.
fn caps(max_context_tokens: u32) -> Capabilities {
    Capabilities {
        tool_calling: ToolCallSupport::Streaming { validated: true },
        cache: CacheMode::None,
        parallel_tool_calls: true,
        structured_output: StructuredOutput::Grammar,
        max_context_tokens,
        reasoning: true,
        reliability_tier: ReliabilityTier::Verified,
    }
}

/// Model metadata (`models.json`): the ONLY channel that seeds
/// `CapabilityIndex::from_backends`'s `model_refs` — `ConwayBuilder::build`
/// step 5 reads `metadata.models.keys()`, not the role chain directly. The
/// numbers inside are irrelevant: `ScriptedBackend::capabilities()` (set via
/// `.with_capabilities` below) is what the index actually stores; this file
/// only needs to name the pair so the index has an entry for it at all.
/// Without it, `capability_index.get(&model_ref)` would be `None` and the
/// router would report an unindexed-model `CapabilitySkip` (`window: None`)
/// — which can never classify as headroom-only, so `resolve` would return
/// `NoCandidate`, not `ContextTooLarge`.
fn write_model_metadata(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("models.json");
    std::fs::write(
        &path,
        format!(
            r#"{{"models":{{"{MODEL}":{{"max_context_tokens":999999,"tool_calling":"streaming","reasoning":false,"reliability_tier":"verified"}}}}}}"#
        ),
    )
    .expect("write models.json fixture");
    path
}

/// A real, validated `ConwayConfig`: one role (`coder`, the `default_role`)
/// whose chain names exactly `MODEL` on backend `fake`. `headroom` sets
/// `routing.default_headroom_tokens` and is held fixed across both tests —
/// only the model's window (`caps`, at the call site) differs.
fn config_naming(headroom: u32, metadata_path: PathBuf) -> ConwayConfig {
    let mut roles = BTreeMap::new();
    roles.insert(
        "coder".to_string(),
        RoleEntry {
            chain: vec![MODEL.to_string()],
            headroom_tokens: None,
            ..Default::default()
        },
    );
    let mut backends = BTreeMap::new();
    backends.insert(
        "fake".to_string(),
        BackendEntry {
            kind: "openai-compat".to_string(),
            base_url: "http://127.0.0.1:9".to_string(),
            dialect: Some("openai".to_string()),
            ..BackendEntry::default()
        },
    );
    ConwayConfig {
        default_role: RoleAlias::new("coder"),
        cwd: PathBuf::from("."),
        session: SessionConfig::default(),
        limits: LimitsConfig::default(),
        permissions: PermissionsConfig::default(),
        backends,
        routing: RoutingSection {
            default_headroom_tokens: headroom,
        },
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig {
            metadata_path,
            probe_on_startup: false,
        },
        tools: ToolsConfig::default(),
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

#[tokio::test]
async fn oversized_context_is_refused_before_the_backend_is_ever_called() {
    let dir = support::unique_temp_dir("context-admission-seam-reject");
    let metadata_path = write_model_metadata(&dir);
    let config = config_naming(8, metadata_path);

    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response(
            "should never run",
        ))])
        .with_id(BackendId::new("fake"))
        // A window of 1 token can never cover any real assembled
        // context (system/tool-schema segments alone run to hundreds
        // of tokens) plus an 8-token headroom -- deliberately not
        // tuned to a predicted `est_tokens` value (see module doc).
        .with_capabilities(caps(1)),
    );
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway = test_builder_without_router(config)
        .with_backend(backend.clone())
        .with_session_store(store)
        // `conway` no longer
        // compiles either dialect in, so this file's config-derived
        // `kind = "openai-compat"` entry (overwritten by the injected
        // `backend` above, but still resolved by `build()` before that
        // overwrite happens) needs a registered factory.
        .with_backend_factory(Arc::new(conway_plugin_backends::OpenAiCompatBackendFactory))
        .build()
        .expect(
            "build should succeed: real ContextBuilder/DeclarativeRouter/AttemptEngine wiring \
             from valid config",
        );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hello").await.expect("prompt");
    let result = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result must not hang")
        .expect("result() itself must not error -- the turn ends Failed, not the stream");

    // PRIMARY (/LIVENESS_TESTS.md): the observable outcome, not an
    // intermediate signal -- actual promise.
    assert!(
        backend.calls().is_empty(),
        "the backend must NEVER be called for an oversized context (: reject, never \
         truncate or escalate); calls: {:?}",
        backend.calls()
    );

    // SECONDARY: the typed error shape, so a regression to the plain
    // `NoCandidate` fallback (which carries none of structured
    // fields) is caught here too, not just "some failure occurred".
    match &result.status {
        ResultStatus::Failed { error } => {
            assert!(error.contains("context rejected"), "got: {error}");
            assert!(error.contains("accepts at most 1"), "got: {error}");
            assert!(error.contains(MODEL), "got: {error}");
            assert!(
                error.contains("no truncation or escalation is performed"),
                "got: {error}"
            );
        }
        other => panic!("expected ResultStatus::Failed, got {other:?}"),
    }
}

///: "any check that cannot fail is not a check" — proof that the
/// assertion above can fail. Identical fixture, exactly one field changed:
/// the model's window, widened from 1 to comfortably fit any real assembled
/// context. The flip is asserted directly, not merely implied: the backend
/// IS now called, exactly once, and the turn completes normally. Committed
/// alongside the rejection test above, per this item's own instruction not
/// to leave this as something "run once by hand and described".
#[tokio::test]
async fn widening_the_model_window_admits_the_identical_request_and_calls_the_backend() {
    let dir = support::unique_temp_dir("context-admission-seam-admit");
    let metadata_path = write_model_metadata(&dir);
    let config = config_naming(8, metadata_path);

    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("ok"))])
            .with_id(BackendId::new("fake"))
            .with_capabilities(caps(10_000_000)),
    );
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway = test_builder_without_router(config)
        .with_backend(backend.clone())
        .with_session_store(store)
        // `conway` no longer
        // compiles either dialect in, so this file's config-derived
        // `kind = "openai-compat"` entry (overwritten by the injected
        // `backend` above, but still resolved by `build()` before that
        // overwrite happens) needs a registered factory.
        .with_backend_factory(Arc::new(conway_plugin_backends::OpenAiCompatBackendFactory))
        .build()
        .expect(
            "build should succeed: real ContextBuilder/DeclarativeRouter/AttemptEngine wiring \
             from valid config",
        );

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
        "widening the window must admit the identical request and reach the backend exactly \
         once, calls: {:?}",
        backend.calls()
    );
    assert_eq!(
        result.status,
        ResultStatus::Completed,
        "an admitted request must complete normally, got: {:?}",
        result.status
    );
}

// ---------------------------------------------------------------------
// Escalation along an already-configured fallback chain (board item
// `01M1AVZPTRSWVE33G4DTJY7Q1B`).
//
// Per this file's own correction note above, no test here installs
// `conway.routing`, so it is `conway_core::routing::MinimalRouter` (no
// filtering at all) handing `AttemptEngine` the WHOLE chain, in order, and
// `AttemptEngine::execute` (`conway-runtime/src/attempt.rs`) that was found,
// on inspection, to already skip a candidate whose own `Backend::admit`
// refuses it and continue to the next one -- an operator who DOES install
// `conway.routing` gets the identical outcome one layer earlier, since
// `DeclarativeRouter::resolve` (`conway-plugin-routing/src/router.rs`) was
// separately confirmed, by reading, to return every surviving candidate too,
// not merely the first. Either way, no production code changes for T-1 in
// this item; this is a characterization test proving an EXISTING mechanism
// reaches the real seam, not a mock of it. `RoutingError::ContextTooLarge`
// (the message this whole file is otherwise about) is reserved for the case
// this test is NOT: every candidate rejected, and each one solely on
// headroom.
// ---------------------------------------------------------------------

const SMALL_MODEL: &str = "fake-small/tiny-model";
const BIG_MODEL: &str = "fake-big/roomy-model";

/// Both models declared with a generous metadata `max_context_tokens`
/// (irrelevant to the test, same reasoning as `write_model_metadata`'s
/// own doc) -- the REAL window each backend enforces comes from
/// `ScriptedBackend::with_capabilities` at the call site, since
/// `ScriptedBackend::capabilities` answers the same `Capabilities` for
/// every model on one instance (`conway-testkit`'s own doc): two DIFFERENT
/// windows on this fixture require two DIFFERENT backend ids, not two
/// models on one.
fn write_two_candidate_model_metadata(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("models.json");
    std::fs::write(
        &path,
        format!(
            r#"{{"models":{{"{SMALL_MODEL}":{{"max_context_tokens":999999,"tool_calling":"streaming","reasoning":false,"reliability_tier":"verified"}},"{BIG_MODEL}":{{"max_context_tokens":999999,"tool_calling":"streaming","reasoning":false,"reliability_tier":"verified"}}}}}}"#
        ),
    )
    .expect("write models.json fixture");
    path
}

/// One role, `coder`, whose chain names `SMALL_MODEL` THEN `BIG_MODEL` --
/// chain order matters here: the test asserts the router tries the small
/// one first (never calls it) and falls through to the big one, not the
/// reverse.
fn two_candidate_config(headroom: u32, metadata_path: PathBuf) -> ConwayConfig {
    let mut roles = BTreeMap::new();
    roles.insert(
        "coder".to_string(),
        RoleEntry {
            chain: vec![SMALL_MODEL.to_string(), BIG_MODEL.to_string()],
            headroom_tokens: None,
            ..Default::default()
        },
    );
    let mut backends = BTreeMap::new();
    backends.insert(
        "fake-small".to_string(),
        BackendEntry {
            kind: "openai-compat".to_string(),
            base_url: "http://127.0.0.1:9".to_string(),
            dialect: Some("openai".to_string()),
            ..BackendEntry::default()
        },
    );
    backends.insert(
        "fake-big".to_string(),
        BackendEntry {
            kind: "openai-compat".to_string(),
            base_url: "http://127.0.0.1:9".to_string(),
            dialect: Some("openai".to_string()),
            ..BackendEntry::default()
        },
    );
    ConwayConfig {
        default_role: RoleAlias::new("coder"),
        cwd: PathBuf::from("."),
        session: SessionConfig::default(),
        limits: LimitsConfig::default(),
        permissions: PermissionsConfig::default(),
        backends,
        routing: RoutingSection {
            default_headroom_tokens: headroom,
        },
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig {
            metadata_path,
            probe_on_startup: false,
        },
        tools: ToolsConfig::default(),
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

/// PRIMARY: a session whose ONLY chain candidate would have died with
/// `ContextRejected` (proven by this file's own `oversized_context_is_
/// refused_before_the_backend_is_ever_called`, above) instead completes
/// when a second, larger-window candidate is configured -- acceptance
/// criterion 1 ("a session that would previously die with `ContextRejected`
/// continues"), proven at the real seam, via the strategy this item ranks
/// first: escalate along an already-declared chain. `small.calls()` staying
/// empty is the same "never called" observable this file's rejection test
/// already uses, now paired with `big.calls().len() == 1` to show the
/// fallback WAS reached, not merely that nothing broke.
#[tokio::test]
async fn a_too_small_primary_is_skipped_and_the_session_continues_via_a_larger_fallback() {
    let dir = support::unique_temp_dir("context-admission-seam-escalate");
    let metadata_path = write_two_candidate_model_metadata(&dir);
    let config = two_candidate_config(8, metadata_path);

    let small = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("should never run"))])
            .with_id(BackendId::new("fake-small"))
            .with_capabilities(caps(1)),
    );
    let big = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("ok"))])
            .with_id(BackendId::new("fake-big"))
            .with_capabilities(caps(10_000_000)),
    );
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway = test_builder_without_router(config)
        .with_backend(small.clone())
        .with_backend(big.clone())
        .with_session_store(store)
        .with_backend_factory(Arc::new(conway_plugin_backends::OpenAiCompatBackendFactory))
        .build()
        .expect(
            "build should succeed: real ContextBuilder/DeclarativeRouter/AttemptEngine wiring \
             from valid config",
        );

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hello").await.expect("prompt");
    let result = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result must not hang")
        .expect("result should succeed");

    assert!(
        small.calls().is_empty(),
        "the too-small candidate must never be called (never admits, so never dials out): {:?}",
        small.calls()
    );
    assert_eq!(
        big.calls().len(),
        1,
        "the router must fall through to the larger-window candidate exactly once: {:?}",
        big.calls()
    );
    assert_eq!(
        result.status,
        ResultStatus::Completed,
        "a session whose primary candidate is too small must still complete via its fallback \
         chain rather than dying with ContextRejected, got: {:?}",
        result.status
    );
}
