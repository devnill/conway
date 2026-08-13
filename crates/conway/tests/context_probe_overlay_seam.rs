//! Board item `01KYXNBKWK2DZ7JE3VRKC5FRJB`: the router's `CapabilityIndex`
//! must agree with `Backend::capabilities()` (and therefore with
//! `AttemptEngine`'s T-1 gate) for every `(backend, model)` pair
//! `models.json` lists, in every direction -- never an independently
//! recomputed value that can silently drift.
//!
//! ## History: two corrections, in order
//!
//! (Board item 01KZHF270T3W8GZ7NM6DSNQ4MM later relocated `builder.rs`'s
//! `probe_openai_compat_backends` -- named throughout this history section
//! below as the mechanism this fix landed in -- into `conway_plugin_backends
//! ::OpenAiCompatBackendFactory::probe_capabilities`. The property this
//! whole file proves is unaffected: `ctx.models` there is the identical
//! `models_overrides_for(id, metadata)` map named below, and the RESTRICT
//! eligibility filter moved WITH the caller, into `ConwayBuilder::build`'s
//! own step 5 -- see that step's module doc. The mechanism names below are
//! left as history, describing the fix at the time it landed.)
//!
//! 1. The item as originally filed claimed the router reads a static,
//!    config-derived index while `AttemptEngine` reads live
//!    `Backend::capabilities()`, and that the two could therefore diverge
//!    for ANY backend. That premise was wrong: `builder.rs` step 5 builds
//!    the index via `CapabilityIndex::from_backends`, which calls
//!    `Backend::capabilities()` directly -- the *same* accessor T-1 reads --
//!    so a hand-built index that disagrees with a fake backend's own
//!    `capabilities()` exercises a state the architecture forbids by
//!    construction and proves nothing about production.
//! 2. The real, reachable divergence this file was rewritten to model was
//!    `probe_on_startup`'s overlay: `probe_openai_compat_backends`
//!    (`builder.rs`) used to construct each backend's startup
//!    `CapabilityProbe` with an **empty** `BTreeMap::new()` overrides table
//!    instead of the same `models_overrides_for(id, metadata)` map the
//!    backend itself was built with -- so a `models.json`-pinned window
//!    reached `Backend::capabilities()` (highest precedence there) but was
//!    silently bypassed in the router's index the moment the probe observed
//!    a *different* live server window. **This has now been fixed**
//!    (`builder.rs`'s `probe_openai_compat_backends` is passed
//!    `models_overrides_for(id, metadata)`, not an empty map): for every
//!    `models.json`-listed model, `models.json` wins outright, in both
//!    directions -- restoring `probe.rs`'s own documented merge rule
//!    (config `ModelOverrides` > metadata > probed value > dialect
//!    defaults) and `docs/routing.md`'s published precedence. Tests
//!    `listed_model_explain_reports_the_configured_window_not_the_wider_probed_one`,
//!    `probe_narrower_than_the_operator_configured_window_still_admits_and_calls_the_backend`,
//!    and `explain_reported_window_matches_backend_capabilities_for_the_same_pair`
//!    below are the fix's regression proof, in both directions.
//!
//! With the probe-overlay divergence closed, the ONE divergence the
//! architecture still permits is a snapshot-vs-live one, not a
//! recomputation one: `CapabilityIndex::from_backends` is a one-time read
//! taken at `build()` time; `AttemptEngine`'s T-1 gate re-reads
//! `Backend::capabilities()` live, on every attempt. A `Backend` whose own
//! window shrinks *after* `build()` is admitted by the router on its stale
//! snapshot and rejected by T-1 on the live, smaller value.
//! `t1_backstops_a_backend_that_shrinks_its_own_window_after_build` below
//! is the discriminating witness for that -- see its own doc for why the
//! probe fixtures below cannot be repurposed for this (their choice to
//! avoid it is deliberate, not an oversight): `probe_overlay_admits_on_an_
//! inflated_window_t1_rejects_on_the_live_one`, this file's original
//! (misnamed) test, asserted on `RoutingError::ContextTooLarge`'s `Display`
//! text plus "no POST was ever sent" -- both true whether the router's
//! index was wrong (the bug) or whether it was already correct and T-1
//! merely re-confirmed the same number the router used (post-fix, for the
//! probe-overlay scenario, T-1 never even runs: the router's `resolve`
//! itself now rejects, since its index already agrees with `models.json`).
//! That assertion shape cannot discriminate "T-1 backstopped a wrong index"
//! from "the index was already correct" -- it was retired for that reason,
//! not renamed, since renaming it as if it still proved the T-1-backstop
//! property would leave a passing check that cannot fail (GP-14).
//!
//! ## The mock HTTP server is loopback-only, not a live network dependency
//!
//! `probe_on_startup`'s mechanism is, by definition, an HTTP round trip --
//! there is no way to drive it at all without *some* HTTP server on the
//! other end. `crates/conway-plugin-backends/tests/capability_probe.rs` already
//! tests this exact mechanism (`CapabilityProbe::discover`/`discover_result`)
//! against a `wiremock::MockServer` bound to an ephemeral `127.0.0.1` port --
//! never a real, external endpoint. This file reuses that established,
//! already-in-tree technique (C-04 forbids live network in tests, not a
//! local loopback double of one) to drive the identical mechanism end-to-end
//! through the real `ConwayBuilder::build`, rather than reaching
//! `CapabilityProbe` directly the way `capability_probe.rs` does.
//!
//! ## Why the probe-driving tests need `flavor = "multi_thread"`
//!
//! `ConwayBuilder::build` is a synchronous function; internally, whenever it
//! needs to run async lower-crate code (the startup probe's own HTTP call),
//! it does so via `builder.rs`'s private `block_on` helper, which spawns a
//! *brand new OS thread* with its own throwaway single-threaded `tokio`
//! runtime and blocks the calling thread until that finishes (this exists so
//! `build()` can be called from inside an already-running `tokio` task
//! without the `Handle::current().block_on` panic -- see that function's own
//! doc). If the *outer* test runs on a single-worker-thread runtime
//! (`#[tokio::test]`'s default `current_thread` flavor), calling `.build()`
//! synchronously from the test body blocks that one worker thread -- the
//! same thread `wiremock`'s own accept/response task needs to make progress
//! on -- and the probe's request would simply time out with nothing ever
//! answering it. `flavor = "multi_thread", worker_threads = 2` (the same
//! convention `crates/conway/tests/keep_alive.rs`, `resume.rs`, and several
//! `conway-cli` seam tests already use for background-task concurrency)
//! gives `wiremock`'s task a second worker thread to run on while the first
//! is blocked inside `build()`'s bridge. `t1_backstops_a_backend_that_
//! shrinks_its_own_window_after_build` performs no HTTP round trip at all
//! (its `Backend` is an in-process double), so it stays on the default
//! `current_thread` flavor.
//!
//! ## The non-property: T-1 is NOT a backstop against estimator error
//!
//! Historical note, ADJUSTED by board item 01KZFBZHTWDF11TH7G0H613ERE: this
//! paragraph originally read that both admission gates -- the router's
//! `satisfies` and `AttemptEngine`'s own pre-flight check -- started from the
//! IDENTICAL `est_tokens` value the real `ContextBuilder` produced, so
//! neither re-derived or cross-checked that number and an under-count
//! defeated both identically. That is no longer how `AttemptEngine` gates:
//! it now calls `Backend::admit`, which computes its OWN independent
//! estimate from the actually-built `GenerateRequest` (each dialect's real
//! wire body), never `ContextBuilder`'s `est_tokens`. The two gates are
//! deliberately NOT required to agree (decision 01KZF13BAR473X5SXN8HN95T6B;
//! `docs/routing.md`'s "Advisory vs. authoritative" section) -- this file's
//! fixtures below still avoid pinning any test to a *specific* numeric
//! estimate from either estimator (the property this note originally warned
//! against relying on), which is exactly what keeps them valid under this
//! decoupling: `t1_backstops_a_backend_that_shrinks_its_own_window_after_build`
//! and `context_admission_seam.rs`'s own fixture use windows pinned far
//! below (1) or far above (any real content's estimate under either
//! estimator) rather than tuned to a predicted number, precisely so that
//! which estimator produced the rejecting number is never load-bearing.
//!
//! ## The unlisted probe-observed-model overlay path (RESTRICT, DECIDED)
//!
//! A model the probe discovers that `models.json` never names at all used
//! to be silently admitted anyway -- `probe_openai_compat_backends`
//! (`builder.rs`) inserted every probe-observed `(model_id, caps)` pair
//! unconditionally, with no filter against `metadata.models`. Operator
//! direction settled this: the probe may only *confirm* a model the
//! operator already declared in `models.json`, never introduce a new one on
//! the strength of a server's own say-so. `probe_openai_compat_backends`
//! now drops any probed pair absent from `models.json` before it ever
//! reaches `index_builder` (see that function's own comment for the full
//! reasoning). `probe_observed_model_absent_from_models_json_is_not_admitted`
//! and its negative control,
//! `probe_control_listed_model_still_admits_when_probe_reports_a_different_model`,
//! below are this fix's regression proof.

mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, BackendEntry, ConwayConfig, HealthSection, HooksConfig, LimitsConfig,
    ModelsConfig, PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig,
    ToolsConfig, TuiSection,
};
use conway::{Conway, ConwayBuilder, SessionSpec};
use conway_core::agent::{PermissionDecision, ResultStatus};
use conway_core::capabilities::{
    CacheMode, Capabilities, ReliabilityTier, StructuredOutput, ToolCallSupport,
};
use conway_core::content::{ContentBlock, StopReason, Usage};
use conway_core::fakes::{FakeGate, FakeStore};
use conway_core::ids::{BackendId, ModelId, RoleAlias};
use conway_core::ports::{GenerateResponse, SessionStore};
use serde_json::json;
use support::MutableCapsBackend;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The `(backend, model)` pair the probe-driven fixtures below name.
/// `vllm_hermes` is one of the two built-in dialects whose probe step folds
/// a discovered server-reported window (`max_model_len`) into its own
/// capability computation -- see the module doc's precedence walk-through.
const MODEL: &str = "probed/tiny-model";

/// A window no real assembled context (system + tool-schema segments alone
/// run to hundreds of tokens) plus any positive headroom can ever fit --
/// deliberately not tuned to a predicted `est_tokens` value, mirroring
/// `context_admission_seam.rs`'s own fixture discipline.
const TINY_LIVE_WINDOW: u32 = 1;

/// Comfortably larger than any real assembled context; used as the probe's
/// discovered window in the "probe wider" fixtures, and, in the negative
/// control, as the `models.json` override too (so both sources agree).
const HUGE_PROBED_WINDOW: u32 = 50_000_000;

/// The probe's discovered window in the "probe narrower" fixture (3b):
/// small enough that an inflated prompt can exceed it, but comfortably
/// larger than any *ordinary* real assembled context, so only the
/// deliberately-inflated prompt below crosses it.
const NARROW_PROBED_WINDOW: u32 = 4_096;

/// `models.json`'s operator-configured window in the "probe narrower"
/// fixture: comfortably larger than [`NARROW_PROBED_WINDOW`] plus the
/// inflated prompt used to cross it, proving the router admits on THIS
/// number, not the probe's.
const LARGE_LIVE_WINDOW: u32 = 200_000;

/// Mounts the one HTTP behavior every probe-driven fixture needs:
/// `vllm_hermes`'s dialect-selected `GET {base}/models` discovery step,
/// reporting a single model whose `max_model_len` the probe folds into its
/// own capability computation (see module doc). No mock is mounted for
/// `POST {base}/chat/completions` by this helper -- callers that need the
/// backend actually reached mount that themselves.
async fn mount_probe_response(server: &MockServer, max_model_len: u32) {
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "tiny-model", "max_model_len": max_model_len}]
        })))
        .mount(server)
        .await;
}

/// The facade's `models.json`: the ONLY channel that (a) seeds
/// `CapabilityIndex::from_backends`'s `model_refs` (so the pair is indexed
/// at all -- see `context_admission_seam.rs`'s own note on this) and (b)
/// projects a `max_context_tokens` override into the backend's own
/// `ModelOverrides` table (`models_overrides_for`), which is what
/// `Backend::capabilities()` actually returns.
fn write_model_metadata(dir: &std::path::Path, max_context_tokens: u32) -> PathBuf {
    let path = dir.join("models.json");
    std::fs::write(
        &path,
        format!(
            r#"{{"models":{{"{MODEL}":{{"max_context_tokens":{max_context_tokens},"tool_calling":"non_streaming","reasoning":false,"reliability_tier":"community"}}}}}}"#
        ),
    )
    .expect("write models.json fixture");
    path
}

/// A real, validated `ConwayConfig`: one role (`coder`, the `default_role`)
/// naming exactly `MODEL` on a real `openai-compat`/`vllm_hermes` backend
/// pointed at `base_url`, with `probe_on_startup` always true -- the overlay
/// this file exists to exercise never runs otherwise.
fn config_naming(base_url: String, metadata_path: PathBuf) -> ConwayConfig {
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
        "probed".to_string(),
        BackendEntry {
            kind: "openai-compat".to_string(),
            base_url,
            dialect: Some("vllm_hermes".to_string()),
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
            default_headroom_tokens: 8,
        },
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig {
            metadata_path,
            probe_on_startup: true,
        },
        tui: TuiSection::default(),
        tools: ToolsConfig::default(),
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

/// Wires a `Conway` exactly as a real embedder would: `ConwayBuilder`
/// constructs its own real `OpenAiCompatBackend` from `config` (no
/// `.with_backend` call anywhere in these fixtures -- the double this file
/// needs for the probe-driven tests is the *HTTP server* the backend and
/// the probe both talk to, not the `Backend` itself), with the first-party
/// `conway-plugin-routing` engine installed via `with_router_factory` (board
/// item 01KZFC43J1J06BM4CCWKCKHSNV: `conway` no longer compiles a capability-
/// /health-filtering `DeclarativeRouter` in by default -- see that item's
/// own doc for what changes without this call) so this file's assertions
/// about the router's probe-overlaid `CapabilityIndex` stay meaningful.
///
/// `with_backend_factory(OpenAiCompatBackendFactory)` (board item
/// 01KZHF270T3W8GZ7NM6DSNQ4MM: `conway` no longer compiles either dialect
/// in, so `config_naming`'s `kind = "openai-compat"` entry resolves to
/// nothing without a registered factory) -- the exact same factory
/// `conway-cli`'s own default-on backend arm links for real (`first_party_
/// plugins::backend_bundle`).
fn build_conway(config: ConwayConfig) -> Conway {
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    ConwayBuilder::from_parts(config)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router_factory(Arc::new(conway_plugin_routing::RoutingRouterFactory))
        .with_backend_factory(Arc::new(conway_plugin_backends::OpenAiCompatBackendFactory))
        .build()
        .expect(
            "build should succeed: a probe failure/degraded result is a warning, never a hard \
             error (CapabilityProbe's own doc)",
        )
}

/// **3a**: the primary fix proof. The probe observes a huge window from the
/// live server; `models.json` pins a 1-token window for the SAME listed
/// model. `Conway::explain_routing`'s `CapabilitySummary` -- the exact
/// number GP-07 promises an operator can inspect -- must report the
/// operator-configured window, never the wider probed one.
///
/// This is the real discriminator the earlier (retired) version of this
/// item's spec was missing: unlike an error-text assertion, the router's
/// own index is read directly, so a builder that silently reverts to
/// feeding the probe an empty overrides map immediately shows up here as
/// `50_000_000`, not `1` (see this item's completion report for the
/// captured break-the-guard output).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn listed_model_explain_reports_the_configured_window_not_the_wider_probed_one() {
    let server = MockServer::start().await;
    mount_probe_response(&server, HUGE_PROBED_WINDOW).await;

    let dir = support::unique_temp_dir("context-probe-overlay-explain-agree");
    let metadata_path = write_model_metadata(&dir, TINY_LIVE_WINDOW);
    let config = config_naming(server.uri(), metadata_path);
    let conway = build_conway(config);

    let report = conway.explain_routing(&RoleAlias::new("coder"));
    let entry = report
        .entries
        .iter()
        .find(|e| e.model_ref.to_string() == MODEL)
        .unwrap_or_else(|| panic!("no explain entry for {MODEL}: {:?}", report.entries));
    let caps = entry
        .capabilities
        .as_ref()
        .expect("a models.json-listed model must be indexed (capabilities: Some(..))");

    assert_eq!(
        caps.max_context_tokens, TINY_LIVE_WINDOW,
        "explain must report the operator-configured window ({TINY_LIVE_WINDOW}), not the \
         wider probed one ({HUGE_PROBED_WINDOW}); got {}",
        caps.max_context_tokens
    );
}

/// **3b**: the inverse-direction regression proof. `models.json` pins a
/// LARGE window; the probe observes a NARROWER one for the same listed
/// model. A turn whose assembled context sits between the two (over the
/// probe's narrow window, comfortably under the operator's configured one)
/// must be admitted, and the backend must actually be called -- proving the
/// router no longer rejects a candidate the operator declared adequate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_narrower_than_the_operator_configured_window_still_admits_and_calls_the_backend() {
    let server = MockServer::start().await;
    mount_probe_response(&server, NARROW_PROBED_WINDOW).await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": { "content": "ok" },
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let dir = support::unique_temp_dir("context-probe-overlay-narrower-admit");
    let metadata_path = write_model_metadata(&dir, LARGE_LIVE_WINDOW);
    let config = config_naming(server.uri(), metadata_path);
    let conway = build_conway(config);

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    // Deliberately inflated well past NARROW_PROBED_WINDOW (4_096 tokens)
    // but comfortably under LARGE_LIVE_WINDOW (200_000) -- ~10_000 tokens
    // at this estimator's ceil(chars/4) heuristic. If the router were still
    // admitting on the probe's narrower number, this exact prompt would be
    // rejected before the backend was ever reached.
    let inflated_prompt = "x ".repeat(20_000);
    let turn = handle.prompt(inflated_prompt).await.expect("prompt");
    let result = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("result must not hang")
        .expect("result should succeed");

    server.verify().await;
    assert_eq!(
        result.status,
        ResultStatus::Completed,
        "an admitted request must complete normally, got: {:?}",
        result.status
    );
}

/// **3c**: index == live. For the listed model, the explain-reported
/// `max_context_tokens` must equal what a `Backend::capabilities()` call
/// with the identical inputs (`models.json`'s override, the same dialect)
/// returns -- the property whose absence was the whole defect. A second,
/// independently-constructed `OpenAiCompatBackend` is used rather than
/// reaching into `Conway`'s internals (no accessor exists, deliberately --
/// `ConwayBuilder::build` owns backend construction end-to-end): both
/// instances are fed the exact same deterministic inputs
/// (`models_overrides_for`'s own equivalent, built by hand here since that
/// function is private to `conway::builder`), so equal inputs must yield
/// equal outputs -- any divergence is exactly the bug this item fixes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explain_reported_window_matches_backend_capabilities_for_the_same_pair() {
    use conway_core::ports::Backend as _;
    use conway_core::routing::ModelOverrides;
    use conway_plugin_backends::config::{Dialect, OpenAiCompatConfig};
    use conway_plugin_backends::openai_compat::OpenAiCompatBackend;

    let server = MockServer::start().await;
    mount_probe_response(&server, HUGE_PROBED_WINDOW).await;

    let dir = support::unique_temp_dir("context-probe-overlay-index-equals-live");
    let metadata_path = write_model_metadata(&dir, TINY_LIVE_WINDOW);
    let config = config_naming(server.uri(), metadata_path);
    let conway = build_conway(config);

    let report = conway.explain_routing(&RoleAlias::new("coder"));
    let entry = report
        .entries
        .iter()
        .find(|e| e.model_ref.to_string() == MODEL)
        .unwrap_or_else(|| panic!("no explain entry for {MODEL}: {:?}", report.entries));
    let via_index = entry
        .capabilities
        .as_ref()
        .expect("a models.json-listed model must be indexed");

    // Same override `models_overrides_for("probed", ..)` would produce for
    // this pair, built by hand (that helper is private to `conway::builder`).
    let mut overrides = BTreeMap::new();
    overrides.insert(
        "tiny-model".to_string(),
        ModelOverrides {
            stream_tools: None,
            max_context_tokens: Some(TINY_LIVE_WINDOW),
            reliability_tier: Some(ReliabilityTier::Community),
            parallel_tool_calls: None,
            min_headroom_tokens: None,
        },
    );
    let cfg = OpenAiCompatConfig {
        id: BackendId::new("probed"),
        base_url: url::Url::parse(&server.uri()).unwrap(),
        api_key: None,
        profile: Dialect::VllmHermes.profile(),
        timeout: None,
        metadata_path: None,
        models: overrides,
    };
    let independent_backend = OpenAiCompatBackend::new(cfg).expect("valid config must construct");
    let direct = independent_backend.capabilities(&ModelId::new("tiny-model"));

    assert_eq!(
        via_index.max_context_tokens, direct.max_context_tokens,
        "the router's index must agree exactly with Backend::capabilities() for the same \
         models.json-listed pair -- via_index={}, direct={}",
        via_index.max_context_tokens, direct.max_context_tokens
    );
}

/// GP-14 negative control for the fix, unchanged from before this item:
/// identical fixture, exactly one field changed -- `models.json`'s
/// `max_context_tokens` for `MODEL`, widened from `TINY_LIVE_WINDOW` to
/// `HUGE_PROBED_WINDOW` (the same value the probe already reports). With
/// the two sources back in agreement, both gates admit and the backend IS
/// called.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn widening_the_live_override_to_match_the_probe_admits_and_calls_the_backend() {
    let server = MockServer::start().await;
    mount_probe_response(&server, HUGE_PROBED_WINDOW).await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": { "content": "ok" },
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let dir = support::unique_temp_dir("context-probe-overlay-admit");
    let metadata_path = write_model_metadata(&dir, HUGE_PROBED_WINDOW);
    let config = config_naming(server.uri(), metadata_path);
    let conway = build_conway(config);

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hello").await.expect("prompt");
    let result = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("result must not hang")
        .expect("result should succeed");

    server.verify().await;
    assert_eq!(
        result.status,
        ResultStatus::Completed,
        "an admitted request must complete normally, got: {:?}",
        result.status
    );
}

/// The bare model name (no `backend/` prefix) the mock `/models` endpoint
/// reports for the unlisted-overlay fixture below -- deliberately never
/// written into any `models.json` fixture in this file.
const UNDECLARED_MODEL_NAME: &str = "undeclared-model";

/// The full `backend/model` ref for [`UNDECLARED_MODEL_NAME`] on the
/// `"probed"` backend [`config_naming`] always configures.
const UNDECLARED_MODEL: &str = "probed/undeclared-model";

/// **3e**: the RESTRICT-policy proof (board item covering the DECIDED
/// policy: "the startup capability probe may only confirm models the
/// operator declared, never introduce new ones"). The live server reports a
/// model `models.json` never names at all -- `models.json` still lists
/// `MODEL` (so this isn't just "an empty models.json"), but the role's
/// chain is pointed at the UNDECLARED pair instead. Per `probe.rs`'s own
/// documented merge precedence (config `ModelOverrides` > `ModelMetadata`
/// entry > probed server value > `DialectDefaults`) and its "discovery may
/// only narrow" invariant, a server mentioning a model is never sufficient
/// on its own to make it routable: this pair must be rejected exactly as if
/// `probe_on_startup` had never run at all, with the SAME
/// "unknown (backend, model) pair" reason `router.rs`'s `check_candidate`
/// gives any pair `models.json` never declared (`docs/getting-started.md`'s
/// published error text). Asserts on the observable end-to-end routing
/// outcome of an actual turn -- not on the capability index's internal
/// contents -- so a builder that reverted to unconditionally inserting
/// every probed pair shows up here as the turn succeeding instead of
/// failing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_observed_model_absent_from_models_json_is_not_admitted() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": UNDECLARED_MODEL_NAME, "max_model_len": HUGE_PROBED_WINDOW}]
        })))
        .mount(&server)
        .await;
    // No mock for POST /chat/completions: an admitted-in-error request
    // would hang waiting for a response that never comes, turning a
    // regression into an unmistakable test timeout rather than a silent
    // pass.

    let dir = support::unique_temp_dir("context-probe-overlay-unlisted");
    // models.json still declares MODEL (a real, listed pair) -- proving
    // this isn't just "no models.json at all" -- but the role chain below
    // names a DIFFERENT pair the probe alone observed.
    let metadata_path = write_model_metadata(&dir, HUGE_PROBED_WINDOW);
    let mut config = config_naming(server.uri(), metadata_path);
    config.roles.get_mut("coder").expect("coder role").chain = vec![UNDECLARED_MODEL.to_string()];
    let conway = build_conway(config);

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hello").await.expect("prompt");
    let result = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("result must not hang")
        .expect("result() itself must not error -- the turn ends Failed, not the stream");

    match &result.status {
        ResultStatus::Failed { error } => {
            assert!(
                error.contains("unknown (backend, model) pair"),
                "a probe-observed model absent from models.json must be rejected with the same \
                 reason any undeclared pair gets, got: {error}"
            );
            assert!(error.contains(UNDECLARED_MODEL), "got: {error}");
        }
        other => panic!(
            "expected ResultStatus::Failed (the undeclared model must never be admitted), got \
             {other:?}"
        ),
    }
}

/// GP-14 negative control for 3e: identical fixture, except the role's
/// chain names `MODEL` -- the pair `models.json` actually declares -- so
/// the SAME probe response (which still reports `UNDECLARED_MODEL_NAME` and
/// nothing else) has nothing to confirm for `MODEL` beyond metadata/dialect
/// defaults. Proves the assertion above can fail: if the filter in
/// `probe_openai_compat_backends` were broken in the other direction (e.g.
/// dropping every pair, listed or not), this would fail too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_control_listed_model_still_admits_when_probe_reports_a_different_model() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": UNDECLARED_MODEL_NAME, "max_model_len": HUGE_PROBED_WINDOW}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": { "content": "ok" },
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let dir = support::unique_temp_dir("context-probe-overlay-unlisted-control");
    let metadata_path = write_model_metadata(&dir, HUGE_PROBED_WINDOW);
    let config = config_naming(server.uri(), metadata_path);
    let conway = build_conway(config);

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hello").await.expect("prompt");
    let result = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("result must not hang")
        .expect("result should succeed");

    server.verify().await;
    assert_eq!(
        result.status,
        ResultStatus::Completed,
        "MODEL is declared in models.json regardless of what the probe reports; an admitted \
         request must complete normally, got: {:?}",
        result.status
    );
}

// ---------------------------------------------------------------------
// 3d: the one divergence the architecture still permits (snapshot vs
// live), preserved on a path that stays reachable after the probe-overlay
// fix above.
// ---------------------------------------------------------------------

/// A different `(backend, model)` pair than the probe fixtures above: this
/// test performs no HTTP round trip at all (its `Backend` is an in-process
/// double), so it needs no `wiremock` server and stays on the default
/// `current_thread` test flavor.
const T1_BACKSTOP_MODEL: &str = "mutable/tiny-model";

/// Every capability EXCEPT the context window set generously, mirroring
/// `context_admission_seam.rs`'s own `caps` helper -- keeps a rejection
/// attributable to context size alone.
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

/// `models.json` here ONLY seeds `CapabilityIndex::from_backends`'s
/// `model_refs` (so the pair is indexed at all) -- an injected backend
/// (`.with_backend`) is never subject to `models_overrides_for`, which only
/// applies to backends `ConwayBuilder::build` constructs itself from
/// `config.backends`. The number below is therefore irrelevant to what
/// either gate actually admits on.
fn t1_backstop_metadata(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("models.json");
    std::fs::write(
        &path,
        format!(
            r#"{{"models":{{"{T1_BACKSTOP_MODEL}":{{"max_context_tokens":1,"tool_calling":"non_streaming","reasoning":false,"reliability_tier":"community"}}}}}}"#
        ),
    )
    .expect("write models.json fixture");
    path
}

/// A real, validated `ConwayConfig` naming `T1_BACKSTOP_MODEL` on backend
/// `mutable`. `probe_on_startup` is false: this test's divergence has
/// nothing to do with the startup probe, and the config-derived backend
/// entry below (pointed at an unreachable dummy address) is always
/// overwritten by the injected `MutableCapsBackend` of the same id before
/// any request is ever made -- see `context_admission_seam.rs`'s identical
/// pattern.
fn t1_backstop_config(metadata_path: PathBuf) -> ConwayConfig {
    let mut roles = BTreeMap::new();
    roles.insert(
        "coder".to_string(),
        RoleEntry {
            chain: vec![T1_BACKSTOP_MODEL.to_string()],
            headroom_tokens: None,
            ..Default::default()
        },
    );
    let mut backends = BTreeMap::new();
    backends.insert(
        "mutable".to_string(),
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
            default_headroom_tokens: 8,
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
        hooks: HooksConfig::default(),
    }
}

/// **3d** -- the discriminating witness `probe_overlay_admits_on_an_
/// inflated_window_t1_rejects_on_the_live_one` (this file's original,
/// now-retired test) was mistakenly believed to be, for the probe-overlay
/// scenario the module doc explains that test can no longer distinguish.
/// This is the one divergence the architecture still permits, precisely
/// because it is structural, not a recomputation bug:
/// `CapabilityIndex::from_backends` (`builder.rs` step 5) is a single read,
/// taken once at `build()` time; `AttemptEngine`'s T-1 gate
/// (`conway-runtime/src/attempt.rs`) calls `Backend::capabilities()` fresh
/// on every attempt. A `Backend` double holding `Arc<Mutex<Capabilities>>`
/// (here, `support::MutableCapsBackend`) lets a test mutate the LIVE value
/// strictly after `build()` has already taken its snapshot -- something no
/// fixed-caps double (`FakeBackend`, `ScriptedBackend`) can express.
#[tokio::test]
async fn t1_backstops_a_backend_that_shrinks_its_own_window_after_build() {
    let dir = support::unique_temp_dir("context-probe-overlay-t1-backstop");
    let metadata_path = t1_backstop_metadata(&dir);
    let config = t1_backstop_config(metadata_path);

    let backend = Arc::new(MutableCapsBackend::new(
        BackendId::new("mutable"),
        // Large at build() time -- this is the value the router's index
        // snapshots.
        caps(HUGE_PROBED_WINDOW),
        text_response("should never run"),
    ));
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway = ConwayBuilder::from_parts(config)
        .with_backend(backend.clone())
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_backend_factory(Arc::new(conway_plugin_backends::OpenAiCompatBackendFactory))
        .build()
        .expect(
            "build should succeed: real ContextBuilder/DeclarativeRouter/AttemptEngine \
                 wiring from valid config",
        );

    // AFTER build(): the router's CapabilityIndex has already snapshotted
    // the large window above and cannot see this. Shrink the LIVE backend's
    // window to something no real assembled context can ever fit.
    backend.set_capabilities(caps(TINY_LIVE_WINDOW));

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hello").await.expect("prompt");
    let result = tokio::time::timeout(Duration::from_secs(5), turn.result())
        .await
        .expect("result must not hang")
        .expect("result() itself must not error -- the turn ends Failed, not the stream");

    // PRIMARY: the router admitted on its stale snapshot (routing itself
    // did not reject this request), but T-1 caught it live -- the backend's
    // `generate` must never actually run.
    match &result.status {
        ResultStatus::Failed { error } => {
            assert!(error.contains("context rejected"), "got: {error}");
            assert!(
                error.contains(&format!("accepts at most {TINY_LIVE_WINDOW}")),
                "must name the LIVE (shrunk) window ({TINY_LIVE_WINDOW}), got: {error}"
            );
            assert!(
                !error.contains(&HUGE_PROBED_WINDOW.to_string()),
                "must NOT name the router's stale build()-time snapshot \
                 ({HUGE_PROBED_WINDOW}) -- that would mean T-1 read the router's index \
                 instead of the live backend, got: {error}"
            );
            assert!(error.contains(T1_BACKSTOP_MODEL), "got: {error}");
        }
        other => panic!("expected ResultStatus::Failed, got {other:?}"),
    }
}

/// GP-14 negative control for 3d: identical fixture, except the backend's
/// window is never shrunk after `build()`. Both the router's snapshot and
/// the live value stay large, so the request is admitted and the backend IS
/// called -- proof the assertion above can fail.
#[tokio::test]
async fn t1_backstop_control_admits_when_the_live_window_never_shrinks() {
    let dir = support::unique_temp_dir("context-probe-overlay-t1-backstop-control");
    let metadata_path = t1_backstop_metadata(&dir);
    let config = t1_backstop_config(metadata_path);

    let backend = Arc::new(MutableCapsBackend::new(
        BackendId::new("mutable"),
        caps(HUGE_PROBED_WINDOW),
        text_response("ok"),
    ));
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway = ConwayBuilder::from_parts(config)
        .with_backend(backend.clone())
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_backend_factory(Arc::new(conway_plugin_backends::OpenAiCompatBackendFactory))
        .build()
        .expect("build should succeed");

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
        result.status,
        ResultStatus::Completed,
        "an admitted request must complete normally, got: {:?}",
        result.status
    );
}
