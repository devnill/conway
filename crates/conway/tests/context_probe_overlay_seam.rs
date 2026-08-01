//! Board item `01KYXNBKWK2DZ7JE3VRKC5FRJB`: "give T-1 a reason to exist" --
//! test a divergence between the router's `CapabilityIndex` and
//! `AttemptEngine`'s live `Backend::capabilities()` that T-1's backstop gate
//! (`crates/conway-runtime/src/attempt.rs`, around line 210) actually
//! guards against in production.
//!
//! ## This is NOT the config-vs-live scenario -- that one is architecturally
//! precluded, on purpose
//!
//! The item as originally filed claimed the router reads a static,
//! config-derived index while `AttemptEngine` reads live
//! `Backend::capabilities()`, and that the two could therefore diverge. That
//! premise was **wrong** and was corrected before this item was claimed (see
//! the item's own corrected spec, and `abec801`, the sibling seam test's
//! commit, which found the same thing independently): `builder.rs`'s step 5
//! populates the index via `CapabilityIndex::from_backends`, which calls
//! `Backend::capabilities()` directly -- the *same* accessor `AttemptEngine`'s
//! T-1 gate reads -- and `conway_routing::capability::CapabilityIndex::
//! from_backends`'s own doc says this is deliberate: "this is what pins the
//! router's admission decisions to exactly what `Backend::capabilities()` --
//! and therefore `AttemptEngine`'s T-1 gate -- will actually see." Hand-
//! building a `CapabilityIndex` that disagrees with a fake backend's own
//! `capabilities()` would exercise a state the architecture forbids by
//! construction and would prove nothing about production; this file does not
//! do that.
//!
//! ## The divergence this file *does* model: the startup probe overlay
//! (path 1 of the item's three candidate paths)
//!
//! `builder.rs` step 5, when `config.models.probe_on_startup` is set, layers
//! a `CapabilityProbe`'s discovered capabilities on top of the
//! `from_backends` base via `CapabilityIndex::into_builder()` +
//! `CapabilityIndexBuilder::insert` (which unconditionally overwrites, not
//! `entry().or_insert_with`). That overlay is fed from a *different* set of
//! inputs than `Backend::capabilities()` is:
//!
//! - `Backend::capabilities()` (`OpenAiCompatBackend::capabilities`) composes
//!   `build_capabilities` from the backend's own `self.overrides` --
//!   populated from the facade's `models.json`, via `models_overrides_for`,
//!   *before* the backend was constructed -- which is the *highest*-
//!   precedence input (`overrides > metadata > dialect_defaults`).
//! - The startup probe's own capability computation
//!   (`CapabilityProbe::discover_result`, called from `builder.rs`'s
//!   `probe_openai_compat_backends`) is constructed with
//!   `ModelMetadataStore::defaults()` and an **empty** `BTreeMap::new()`
//!   overrides table (`builder.rs`, the `CapabilityProbe::new(..)` call) --
//!   it never sees the facade's `models.json` overrides at all. For the
//!   `"vllm_hermes"`/`"llama_cpp_server"` dialects, a probed
//!   `max_model_len`/`n_ctx` value is folded into the probe's own
//!   `dialect_defaults.max_context_tokens` *before* it composes
//!   `Capabilities` -- so with no metadata/override of its own to override
//!   it, the probed value becomes the effective `max_context_tokens` the
//!   overlay writes into the router's index, however large the live server
//!   claims its window is.
//!
//! So a `models.json` entry that pins a *small* `max_context_tokens` for a
//! `vllm_hermes` backend still reaches `Backend::capabilities()` (the
//! backend's own override table honors it) but is silently bypassed in the
//! router's index the moment `probe_on_startup` successfully observes a
//! larger window from the live server -- the router admits on the inflated
//! probed number, `AttemptEngine`'s T-1 gate rejects on the real, small,
//! operator-configured one. This is genuinely reachable in production by any
//! operator running a `vllm_hermes`/`llama_cpp_server` backend with
//! `probe_on_startup = true` and an explicit `models.json` window override --
//! nothing about it requires a pathological `Backend` impl.
//!
//! ## The mock HTTP server is loopback-only, not a live network dependency
//!
//! `probe_on_startup`'s mechanism is, by definition, an HTTP round trip --
//! there is no way to drive it at all without *some* HTTP server on the
//! other end. `crates/conway-backends/tests/capability_probe.rs` already
//! tests this exact mechanism (`CapabilityProbe::discover`/`discover_result`)
//! against a `wiremock::MockServer` bound to an ephemeral `127.0.0.1` port --
//! never a real, external endpoint. This file reuses that established,
//! already-in-tree technique (C-04 forbids live network in tests, not a
//! local loopback double of one) to drive the identical mechanism end-to-end
//! through the real `ConwayBuilder::build`, rather than reaching
//! `CapabilityProbe` directly the way `capability_probe.rs` does.
//!
//! ## Why the outer test needs `flavor = "multi_thread"`
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
//! is blocked inside `build()`'s bridge.
//!
//! ## The non-property: T-1 is NOT a backstop against estimator error
//!
//! Both admission gates -- the router's `satisfies`/`context_shortfall` and
//! `AttemptEngine`'s own `caps.max_context_tokens >= required` check -- start
//! from the identical `est_tokens` value the real `ContextBuilder` produced.
//! Neither gate re-derives or cross-checks that number against anything; an
//! under-count defeats both of them in exactly the same way, at exactly the
//! same threshold. This is a live misreading risk given open board item
//! `01KYTMJA0JHT5SAPYDGV251V17` (tool schemas counted once but sent twice, so
//! the estimate crossing this seam is systematically low): T-1 backstops a
//! disagreement between two *sources* of `Capabilities` for the same
//! `est_tokens`, not a wrong `est_tokens` itself. A future reader should not
//! conclude from this test (or from T-1's existence at all) that the
//! estimator's under-count is caught here -- it is not, and nothing in this
//! file exercises that failure mode.

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, BackendEntry, BackendKind, ConwayConfig, HealthSection, LimitsConfig,
    ModelsConfig, PermissionsConfig, RoleEntry, RoutingSection, SessionConfig, TuiSection,
};
use conway::{Conway, ConwayBuilder, SessionSpec};
use conway_core::agent::{PermissionDecision, ResultStatus};
use conway_core::fakes::FakeGate;
use conway_core::ids::RoleAlias;
use conway_core::ports::SessionStore;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The `(backend, model)` pair every fixture below names. `vllm_hermes` is
/// one of the two built-in dialects whose probe step folds a discovered
/// server-reported window (`max_model_len`) into its own capability
/// computation -- see the module doc's precedence walk-through.
const MODEL: &str = "probed/tiny-model";

/// A window no real assembled context (system + tool-schema segments alone
/// run to hundreds of tokens) plus any positive headroom can ever fit --
/// deliberately not tuned to a predicted `est_tokens` value, mirroring
/// `context_admission_seam.rs`'s own fixture discipline.
const TINY_LIVE_WINDOW: u32 = 1;

/// Comfortably larger than any real assembled context; used both as the
/// probe's discovered window (in every test) and, in the negative control
/// only, as the `models.json` override too (so both sources agree).
const HUGE_PROBED_WINDOW: u32 = 50_000_000;

/// Mounts the one HTTP behavior this fixture needs: `vllm_hermes`'s
/// dialect-selected `GET {base}/models` discovery step, reporting a single
/// model whose `max_model_len` the probe folds into its own capability
/// computation (see module doc). No mock is mounted for
/// `POST {base}/chat/completions` -- the rejection test's whole point is
/// that endpoint is never hit, and the negative control mounts its own.
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
/// `Backend::capabilities()` actually returns. This is the value T-1's gate
/// sees; the probe's own separately-computed value (mounted above) is what
/// the router's index sees whenever `probe_on_startup` is set.
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
    let mut roles = std::collections::BTreeMap::new();
    roles.insert(
        "coder".to_string(),
        RoleEntry {
            chain: vec![MODEL.to_string()],
            headroom_tokens: None,
        },
    );
    let mut backends = std::collections::BTreeMap::new();
    backends.insert(
        "probed".to_string(),
        BackendEntry {
            kind: BackendKind::OpenaiCompat,
            api_key: String::new(),
            api_key_env: String::new(),
            base_url,
            dialect: Some("vllm_hermes".to_string()),
            stream_tools: None,
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
    }
}

/// Wires a `Conway` exactly as a real embedder would: `ConwayBuilder`
/// constructs its own real `OpenAiCompatBackend` from `config` (no
/// `.with_backend` call anywhere in this file -- unlike
/// `context_admission_seam.rs`, the double this file needs is the *HTTP
/// server* the backend and the probe both talk to, not the `Backend`
/// itself) and compiles its own `DeclarativeRouter` over the resulting,
/// probe-overlaid `CapabilityIndex`.
fn build_conway(config: ConwayConfig) -> Conway {
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let store: Arc<dyn SessionStore> = Arc::new(conway_core::fakes::FakeStore::new());
    ConwayBuilder::from_parts(config)
        .with_session_store(store)
        .with_permission_gate(gate)
        .build()
        .expect(
            "build should succeed: a probe failure/degraded result is a warning, never a hard \
             error (CapabilityProbe's own doc)",
        )
}

/// The rejection test: the probe observes a huge window from the live
/// server; `models.json` pins a 1-token window that the backend's own
/// `ModelOverrides` table actually honors. Router admits on the probed
/// number; `AttemptEngine`'s T-1 gate rejects on the live one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_overlay_admits_on_an_inflated_window_t1_rejects_on_the_live_one() {
    let server = MockServer::start().await;
    mount_probe_response(&server, HUGE_PROBED_WINDOW).await;
    // No mock for the completions endpoint at all: the whole point of this
    // test is that it must never be reached.

    let dir = support::unique_temp_dir("context-probe-overlay-reject");
    let metadata_path = write_model_metadata(&dir, TINY_LIVE_WINDOW);
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
        .expect("result() itself must not error -- the turn ends Failed, not the stream");

    // PRIMARY (GP-14/LIVENESS_TESTS.md observable-outcome rule): the backend
    // must never actually be called. `received_requests` records every HTTP
    // request the mock server saw across BOTH the probe's `/models` GET and
    // any completions POST; asserting none of them is a POST to
    // `/chat/completions` is what proves the backend was never invoked --
    // not an intermediate signal a broken gate could also produce, since a
    // POST there can only originate from `OpenAiCompatBackend::generate`.
    let requests = server.received_requests().await.unwrap_or_default();
    assert!(
        requests.iter().any(|r| r.url.path() == "/models"),
        "sanity: the probe itself must have run at least once, requests: {requests:?}"
    );
    assert!(
        !requests
            .iter()
            .any(|r| r.method.as_str() == "POST" && r.url.path() == "/chat/completions"),
        "the backend must NEVER be called for an oversized context (P-9: reject, never \
         truncate or escalate); requests: {requests:?}"
    );

    // SECONDARY: the rejection must name the LIVE window (1), never the
    // probed/indexed one (50_000_000) -- proving it was T-1's own
    // `Backend::capabilities()` read, not a stale copy of the router's
    // admission, that produced this error.
    match &result.status {
        ResultStatus::Failed { error } => {
            assert!(error.contains("context rejected"), "got: {error}");
            assert!(
                error.contains(&format!("accepts at most {TINY_LIVE_WINDOW}")),
                "must name the LIVE window ({TINY_LIVE_WINDOW}), got: {error}"
            );
            assert!(
                !error.contains(&HUGE_PROBED_WINDOW.to_string()),
                "must NOT name the probed/indexed window ({HUGE_PROBED_WINDOW}) -- that would \
                 mean T-1 read the router's index instead of the live backend, got: {error}"
            );
            assert!(error.contains(MODEL), "got: {error}");
            assert!(
                error.contains("no truncation or escalation is performed"),
                "got: {error}"
            );
        }
        other => panic!("expected ResultStatus::Failed, got {other:?}"),
    }
}

/// GP-14: "any check that cannot fail is not a check" -- the negative
/// control proving the assertion above can fail. Identical fixture, exactly
/// one field changed: `models.json`'s `max_context_tokens` for `MODEL`,
/// widened from `TINY_LIVE_WINDOW` to `HUGE_PROBED_WINDOW` (the same value
/// the probe already reports). With the two sources back in agreement, both
/// gates admit and the backend IS called.
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
