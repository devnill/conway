//! Wiremock integration tests for `CapabilityProbe::discover`/
//! `discover_result` and `OpenAiCompatBackend::probe`.

use std::collections::BTreeMap;
use std::time::Duration;

use conway_core::error::BackendError;
use conway_core::ids::ModelId;
use conway_core::ports::Backend;
use conway_plugin_backends::config::{Dialect, ModelOverrides, OpenAiCompatConfig};
use conway_plugin_backends::model_metadata::ModelMetadataStore;
use conway_plugin_backends::openai_compat::OpenAiCompatBackend;
use conway_plugin_backends::probe::CapabilityProbe;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn probe(base_url: &str, dialect: Dialect) -> CapabilityProbe {
    CapabilityProbe::new(
        base_url.parse().unwrap(),
        dialect.profile(),
        None,
        Duration::from_secs(5),
        ModelMetadataStore::empty(),
        BTreeMap::new(),
    )
}

fn probe_with(
    base_url: &str,
    dialect: Dialect,
    metadata: ModelMetadataStore,
    overrides: BTreeMap<String, ModelOverrides>,
) -> CapabilityProbe {
    CapabilityProbe::new(
        base_url.parse().unwrap(),
        dialect.profile(),
        None,
        Duration::from_secs(5),
        metadata,
        overrides,
    )
}

fn backend_config(base_url: &str, dialect: Dialect) -> OpenAiCompatConfig {
    OpenAiCompatConfig {
        id: conway_core::ids::BackendId::new("test"),
        base_url: base_url.parse().unwrap(),
        api_key: None,
        profile: dialect.profile(),
        timeout: None,
        metadata_path: None,
        models: BTreeMap::new(),
    }
}

/// Returns an address nothing is listening on: bind an ephemeral port, then
/// drop the listener immediately so the port is free but unaccepting.
fn unreachable_base_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}

/// Writes `toml` to a fresh temp file and loads it as a `ModelMetadataStore`
/// — `ModelMetadataStore::parse` is private, so a real file is the only way
/// to build a non-empty store from outside the crate.
fn metadata_store_from_toml(toml: &str) -> ModelMetadataStore {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut path = std::env::temp_dir();
    path.push(format!(
        "conway-capability-probe-test-{}-{nanos}.toml",
        std::process::id()
    ));
    std::fs::write(&path, toml).unwrap();
    let store = ModelMetadataStore::load(&path).unwrap();
    std::fs::remove_file(&path).ok();
    store
}

#[tokio::test]
async fn discover_yields_exactly_the_models_endpoint_ids() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "qwen3-coder:30b"}, {"id": "llama3.1:8b"}]
        })))
        .mount(&server)
        .await;

    let probe = probe(&server.uri(), Dialect::OpenAi);
    let discovered = probe.discover().await.unwrap();

    let keys: Vec<ModelId> = discovered.keys().cloned().collect();
    assert_eq!(
        keys,
        vec![ModelId::new("llama3.1:8b"), ModelId::new("qwen3-coder:30b")]
    );
}

#[tokio::test]
async fn llama_cpp_props_n_ctx_populates_max_context_tokens_unless_metadata_wins() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "local-model"}, {"id": "metadata-pinned"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/props"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "default_generation_settings": {"n_ctx": 16384},
            "chat_template": "{{ messages }}"
        })))
        .mount(&server)
        .await;

    let metadata = metadata_store_from_toml(
        r#"
        [[model]]
        id = "metadata-pinned"
        max_context_tokens = 99999
        "#,
    );
    let probe = probe_with(
        &server.uri(),
        Dialect::LlamaCppServer,
        metadata,
        BTreeMap::new(),
    );
    let discovered = probe.discover().await.unwrap();

    assert_eq!(
        discovered[&ModelId::new("local-model")].max_context_tokens,
        16384
    );
    assert_eq!(
        discovered[&ModelId::new("metadata-pinned")].max_context_tokens,
        99999,
        "explicit metadata value must win over the probed value"
    );
}

#[tokio::test]
async fn ollama_fallback_yields_models_when_v1_models_404s() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [{"name": "qwen3-coder:30b"}]
        })))
        .mount(&server)
        .await;

    let probe = probe(&server.uri(), Dialect::Ollama);
    let discovered = probe.discover().await.unwrap();

    assert_eq!(
        discovered.keys().collect::<Vec<_>>(),
        vec![&ModelId::new("qwen3-coder:30b")]
    );
}

#[tokio::test]
async fn ollama_fallback_is_not_attempted_for_other_dialects() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    // No `/api/tags` mock: if `OpenAi` ever consulted it, wiremock would
    // 404 by default (no matcher configured), which would still pass — so
    // instead assert the resulting map is empty, proving no fallback model
    // was picked up from anywhere.
    let probe = probe(&server.uri(), Dialect::OpenAi);
    let discovered = probe.discover().await.unwrap();
    assert!(discovered.is_empty());
}

/// Discovery/`/api/show` note: the model's own architecture ceiling
/// (`model_info["<arch>.context_length"]`), not `/api/ps`'s loaded-runtime
/// window — see `probe.rs`'s module doc for why this module deliberately
/// probes the former.
#[tokio::test]
async fn ollama_api_show_populates_max_context_tokens_from_the_architecture_ceiling() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "glm-5.2"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model_info": {
                "general.architecture": "glm4",
                "glm4.context_length": 1_000_000
            }
        })))
        .mount(&server)
        .await;

    let probe = probe(&server.uri(), Dialect::Ollama);
    let discovered = probe.discover().await.unwrap();

    assert_eq!(
        discovered[&ModelId::new("glm-5.2")].max_context_tokens,
        1_000_000,
        "the probed architecture ceiling must populate max_context_tokens, not the ollama \
         dialect's 32,768-token floor"
    );

    // Confirm the real request shape: a POST naming the model, not a GET.
    let received = server.received_requests().await.unwrap();
    let show_request = received
        .iter()
        .find(|r| r.url.path() == "/api/show")
        .expect("an /api/show request must have been made");
    assert_eq!(show_request.method.to_string(), "POST");
    let body: serde_json::Value = show_request.body_json().unwrap();
    assert_eq!(body["model"], "glm-5.2");
}

#[tokio::test]
async fn ollama_api_show_context_length_is_overridden_by_pinned_metadata() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "pinned-model"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model_info": {
                "general.architecture": "llama",
                "llama.context_length": 999_999
            }
        })))
        .mount(&server)
        .await;

    let metadata = metadata_store_from_toml(
        r#"
        [[model]]
        id = "pinned-model"
        max_context_tokens = 4096
        "#,
    );
    let probe = probe_with(&server.uri(), Dialect::Ollama, metadata, BTreeMap::new());
    let discovered = probe.discover().await.unwrap();

    assert_eq!(
        discovered[&ModelId::new("pinned-model")].max_context_tokens,
        4096,
        "explicit ModelMetadata must win over the probed /api/show value"
    );
}

/// `/api/show` is best-effort exactly like every other discovery step: a
/// 404 (a server old enough not to have the endpoint, or a genuinely
/// unknown model name) must not fail discovery or drop the model — it
/// falls through to the ollama dialect's own floor, same as if this probe
/// step didn't exist at all.
#[tokio::test]
async fn ollama_api_show_404_falls_back_to_the_dialect_floor_without_failing_discovery() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "unshowable-model"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let probe = probe(&server.uri(), Dialect::Ollama);
    let discovered = probe.discover().await.unwrap();

    assert_eq!(
        discovered[&ModelId::new("unshowable-model")].max_context_tokens,
        Dialect::Ollama.defaults().max_context_tokens,
        "a failed /api/show must fall back to the dialect floor, not fail discovery"
    );
}

#[tokio::test]
async fn total_discovery_failure_yields_ok_with_metadata_derived_capabilities_and_is_degraded() {
    let unreachable = unreachable_base_url();
    let metadata = metadata_store_from_toml(
        r#"
        [[model]]
        id = "pinned-model"
        max_context_tokens = 4096
        "#,
    );
    let mut overrides = BTreeMap::new();
    overrides.insert(
        "pinned-model".to_string(),
        ModelOverrides {
            stream_tools: None,
            max_context_tokens: None,
            reliability_tier: None,
            parallel_tool_calls: None,
            min_headroom_tokens: None,
        },
    );

    let probe = probe_with(&unreachable, Dialect::OpenAi, metadata, overrides);
    let result = probe.discover_result().await;

    assert!(
        result.degraded,
        "no endpoint was reachable: must be degraded"
    );
    assert_eq!(
        result.capabilities[&ModelId::new("pinned-model")].max_context_tokens,
        4096,
        "capabilities must still be derived from ModelMetadata for a configured model"
    );

    // `discover()` itself is still `Ok`, never `Err`.
    let via_discover = probe.discover().await;
    assert!(via_discover.is_ok());
}

#[tokio::test]
async fn discover_never_returns_capabilities_for_an_unobserved_unconfigured_model() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "observed-model"}]
        })))
        .mount(&server)
        .await;

    let mut overrides = BTreeMap::new();
    overrides.insert(
        "pinned-but-unobserved".to_string(),
        ModelOverrides {
            stream_tools: None,
            max_context_tokens: None,
            reliability_tier: None,
            parallel_tool_calls: None,
            min_headroom_tokens: None,
        },
    );
    let probe = probe_with(
        &server.uri(),
        Dialect::OpenAi,
        ModelMetadataStore::empty(),
        overrides,
    );
    let result = probe.discover_result().await;

    assert!(
        result
            .capabilities
            .contains_key(&ModelId::new("observed-model")),
        "observed model must be present"
    );
    assert!(
        result
            .capabilities
            .contains_key(&ModelId::new("pinned-but-unobserved")),
        "configured-override model must be present even though undiscovered"
    );
    assert!(
        !result
            .capabilities
            .contains_key(&ModelId::new("never-mentioned-anywhere")),
        "a model neither observed nor configured must never appear"
    );
    assert!(
        !result.degraded,
        "at least one endpoint succeeded with at least one model"
    );
}

#[tokio::test]
async fn backend_probe_returns_ok_with_latency_and_healthy_on_200() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "m1"}]
        })))
        .mount(&server)
        .await;

    let backend = OpenAiCompatBackend::new(backend_config(&server.uri(), Dialect::OpenAi)).unwrap();
    let report = backend.probe().await.unwrap();

    assert!(report.ok);
    assert_eq!(report.models, vec![ModelId::new("m1")]);
}

#[tokio::test]
async fn backend_probe_against_connection_refused_is_transport_after_exactly_one_request() {
    let unreachable = unreachable_base_url();
    let backend = OpenAiCompatBackend::new(backend_config(&unreachable, Dialect::OpenAi)).unwrap();

    let err = backend.probe().await.expect_err("nothing is listening");
    assert!(matches!(err, BackendError::Transport { .. }), "{err:?}");
}

#[tokio::test]
async fn backend_probe_against_401_is_auth() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"error": {"message": "bad key"}})),
        )
        .mount(&server)
        .await;

    let backend = OpenAiCompatBackend::new(backend_config(&server.uri(), Dialect::OpenAi)).unwrap();
    let err = backend.probe().await.expect_err("401 must be Auth");
    assert!(matches!(err, BackendError::Auth { .. }), "{err:?}");
}

/// The startup-probe-overrides-discard defect ("the startup
/// capability probe discards the operator's models.json overrides"): a
/// `vllm_hermes` server reports a huge `max_model_len`, but the caller's own
/// `overrides` map (mirroring `models_overrides_for`'s projection of
/// `models.json` in `conway::builder`, copied verbatim onto
/// `BackendBuildContext::models`) pins `max_context_tokens` to a tiny
/// explicit value. Per the module doc's merge precedence (config
/// `ModelOverrides` > `ModelMetadata` entry > probed server value >
/// `DialectDefaults`), the override must win outright — proving that a
/// `CapabilityProbe` constructed with a non-empty `overrides` map (as
/// `OpenAiCompatBackendFactory::probe_capabilities`
/// now passes) composes the operator-pinned
/// value, not the server-reported one.
#[tokio::test]
async fn vllm_hermes_max_model_len_is_overridden_by_a_pinned_override() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "hermes-model", "max_model_len": 50_000_000}]
        })))
        .mount(&server)
        .await;

    let mut overrides = BTreeMap::new();
    overrides.insert(
        "hermes-model".to_string(),
        ModelOverrides {
            stream_tools: None,
            max_context_tokens: Some(1),
            reliability_tier: None,
            parallel_tool_calls: None,
            min_headroom_tokens: None,
        },
    );
    let probe = probe_with(
        &server.uri(),
        Dialect::VllmHermes,
        ModelMetadataStore::empty(),
        overrides,
    );
    let discovered = probe.discover().await.unwrap();

    assert_eq!(
        discovered[&ModelId::new("hermes-model")].max_context_tokens,
        1,
        "an explicit override must win over the server-reported max_model_len, however large"
    );
}

/// An earlier review found: M1: a llama.cpp /props response with an empty
/// chat_template downgrades reliability_tier to Unknown for probed models,
/// while an explicit override tier still wins the precedence chain.
#[tokio::test]
async fn llama_cpp_missing_chat_template_downgrades_reliability_to_unknown() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "bare-model"}, {"id": "pinned-model"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/props"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "default_generation_settings": {"n_ctx": 8192},
            "chat_template": ""
        })))
        .mount(&server)
        .await;

    let mut overrides = BTreeMap::new();
    overrides.insert(
        "pinned-model".to_string(),
        ModelOverrides {
            stream_tools: None,
            max_context_tokens: None,
            reliability_tier: Some(conway_core::capabilities::ReliabilityTier::Verified),
            parallel_tool_calls: None,
            min_headroom_tokens: None,
        },
    );
    let caps = probe_with(
        &server.uri(),
        Dialect::LlamaCppServer,
        ModelMetadataStore::empty(),
        overrides,
    )
    .discover()
    .await
    .unwrap();

    assert_eq!(
        caps[&ModelId::new("bare-model")].reliability_tier,
        conway_core::capabilities::ReliabilityTier::Unknown,
        "empty chat_template must downgrade unpinned models"
    );
    assert_eq!(
        caps[&ModelId::new("pinned-model")].reliability_tier,
        conway_core::capabilities::ReliabilityTier::Verified,
        "explicit override tier must win over the server-detected downgrade"
    );
}

// ---- `discover_context_window`: the setup-time discover-or-ask shared ----
// ---- primitive (board item: context-window declaration honesty, num_ctx) ----

/// The happy path: `"ollama"` profile, a model `/api/show` knows about --
/// returns `Some`, the exact figure the provider reports.
#[tokio::test]
async fn discover_context_window_returns_the_probed_architecture_ceiling_for_ollama() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model_info": {
                "general.architecture": "glm5.2",
                "glm5.2.context_length": 1_048_576
            }
        })))
        .mount(&server)
        .await;

    let base = server.uri().parse().unwrap();
    let window = conway_plugin_backends::probe::discover_context_window(
        &base,
        &Dialect::Ollama.profile(),
        None,
        "glm-5.2:cloud",
    )
    .await;
    assert_eq!(window, Some(1_048_576));
}

/// A dialect with no known discovery endpoint (every built-in profile
/// except `"ollama"`) returns `None` immediately -- no network call at
/// all, so the caller's discover-or-ask flow falls straight through to
/// asking without waiting on a doomed request.
#[tokio::test]
async fn discover_context_window_returns_none_for_a_dialect_with_no_known_endpoint() {
    let server = MockServer::start().await;
    // No mock registered at all: if this function made a request, wiremock
    // would panic on an unmatched request once `.verify()`-style strictness
    // applies, or the request would simply 404 -- either way `None` must
    // come back either without a call or from a failed one, never `Some`.
    let base = server.uri().parse().unwrap();
    for dialect in [
        Dialect::OpenAi,
        Dialect::VllmHermes,
        Dialect::LmStudio,
        Dialect::LlamaCppServer,
    ] {
        let window =
            conway_plugin_backends::probe::discover_context_window(&base, &dialect.profile(), None, "any-model")
                .await;
        assert_eq!(window, None, "{dialect:?} has no known discovery endpoint");
    }
}

/// A model `/api/show` doesn't recognize (e.g. retired -- confirmed live
/// 2026-08-30 against `kimi-k2.5`) is `None`, never a stale or invented
/// number: the caller's own job on `None` is to ask.
#[tokio::test]
async fn discover_context_window_returns_none_when_api_show_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"error": "model not found"})),
        )
        .mount(&server)
        .await;

    let base = server.uri().parse().unwrap();
    let window = conway_plugin_backends::probe::discover_context_window(
        &base,
        &Dialect::Ollama.profile(),
        None,
        "retired-model",
    )
    .await;
    assert_eq!(window, None);
}
