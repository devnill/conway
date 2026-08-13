//! `AnthropicBackend`: native Anthropic Messages API adapter with explicit
//! cache-breakpoint mapping (architecture §"Module: conway-backends",
//! WI-021).
//!
//! `wire.rs` owns the segment↔message and response↔`GenerateResponse`
//! mapping, `stream.rs` owns SSE streaming and Anthropic's `content_block_*`
//! tool-call delta translation, `cache.rs` owns the `CacheMode::
//! ExplicitBreakpoints` cache-hint → `cache_control` mapping. Auth is a
//! single `x-api-key` header alongside `anthropic-version`; no OAuth
//! handshake exists in this file set. The key's shape is never inspected,
//! so any Anthropic-compatible endpoint's credential works as-is.

mod cache;
mod stream;
mod wire;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use conway_core::capabilities::{CacheMode, Capabilities, ProbeReport};
use conway_core::content::ToolSpec;
use conway_core::error::BackendError;
use conway_core::ids::{BackendId, ModelId};
use conway_core::ports::{
    check_admission, Admission, Backend, BoxStream, GenerateRequest, GenerateResponse, StreamChunk,
};
use conway_core::segment::PromptSegment;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::capabilities::{anthropic_defaults, build_capabilities, CapabilityInputs};
use crate::config::{AnthropicConfig, ConfigError, ModelOverrides, SecretString};
use crate::error::{classify, classify_malformed_body};
use crate::http::HttpClient;
use crate::model_metadata::ModelMetadataStore;
use crate::tool_calls::{ToolCallAccumulator, ToolCallStyle};
use wire::BreakpointTarget;

/// Applied when `req.params.max_tokens` is unset — Anthropic's `max_tokens`
/// field is required by the API.
const DEFAULT_MAX_TOKENS: u32 = 8192;

/// `probe()`'s per-request timeout: a `/v1/models` liveness check is cheap
/// and must fail fast (Implementation Notes).
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Constructs a `ToolCallAccumulator` for the Anthropic adapter. Anthropic's
/// `content_block_start`/`content_block_delta` events are translated
/// (`stream.rs`) into the `{"index":..,"id":..,"function":{"name":..,
/// "arguments":..}}` shape [`ToolCallStyle::Structured`]'s parser expects,
/// so this reuses `push_delta`/`push_complete` unmodified rather than
/// touching `src/tool_calls/*` (owned by WI-018/WI-022). Previously spelled
/// as `ToolCallAccumulator::new(Dialect::OpenAi, tools)` — the declarative
/// provider profiles item decoupled `ToolCallAccumulator` from `Dialect`
/// entirely, which incidentally resolves this function's own prior doc
/// caveat about "not wanting to add a sixth `Dialect` variant" just to name
/// this parsing strategy.
pub(crate) fn new_tool_call_accumulator(tools: &[ToolSpec]) -> ToolCallAccumulator {
    ToolCallAccumulator::new(ToolCallStyle::Structured, tools)
}

/// Native Anthropic Messages API adapter.
pub struct AnthropicBackend {
    id: BackendId,
    base: Url,
    anthropic_version: String,
    api_key: SecretString,
    http: HttpClient,
    models: ModelMetadataStore,
    overrides: BTreeMap<String, ModelOverrides>,
}

impl AnthropicBackend {
    pub fn new(config: AnthropicConfig) -> Result<Self, ConfigError> {
        config.validate()?;
        let timeout = config.effective_timeout();
        let http =
            HttpClient::with_timeout(timeout).expect("reqwest client with rustls TLS must build");
        Ok(Self {
            id: config.id,
            base: config.base_url,
            anthropic_version: config.anthropic_version,
            api_key: config.api_key,
            http,
            models: ModelMetadataStore::defaults(),
            overrides: config.models,
        })
    }

    fn messages_url(&self) -> Url {
        let base = self.base.as_str().trim_end_matches('/');
        format!("{base}/v1/messages")
            .parse()
            .expect("base_url + /v1/messages must form a valid URL")
    }

    fn models_url(&self) -> Url {
        let base = self.base.as_str().trim_end_matches('/');
        format!("{base}/v1/models")
            .parse()
            .expect("base_url + /v1/models must form a valid URL")
    }

    /// `x-api-key` + `anthropic-version` headers. No OAuth-style
    /// `Authorization` header is ever constructed.
    fn request_builder(&self, url: Url, body: &Value) -> reqwest::RequestBuilder {
        self.http
            .inner()
            .post(url)
            .header("x-api-key", self.api_key.expose_secret())
            .header("anthropic-version", self.anthropic_version.as_str())
            .json(body)
    }

    /// Applies `cache.rs`'s cache-hint mapping when, and only when, the
    /// resolved capabilities declare `CacheMode::ExplicitBreakpoints`
    /// (always true for this adapter's dialect defaults, but resolved via
    /// `capabilities()` rather than hardcoded so `overrides`/`metadata`
    /// remain the single source of truth).
    fn apply_cache(
        &self,
        body: &mut Value,
        segments: &[PromptSegment],
        placements: &[BreakpointTarget],
        caps: &Capabilities,
    ) {
        if let CacheMode::ExplicitBreakpoints {
            max_breakpoints, ..
        } = &caps.cache
        {
            cache::apply_cache_hints(body, segments, placements, *max_breakpoints);
        }
    }
}

#[async_trait]
impl Backend for AnthropicBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn capabilities(&self, model: &ModelId) -> Capabilities {
        build_capabilities(CapabilityInputs {
            dialect_defaults: anthropic_defaults(),
            metadata: self.models.get(model),
            overrides: self.overrides.get(model.as_str()),
        })
    }

    async fn generate(&self, req: GenerateRequest) -> Result<GenerateResponse, BackendError> {
        let caps = self.capabilities(&req.model);
        let (mut body, placements) = wire::build_request_body(&req, DEFAULT_MAX_TOKENS, false);
        self.apply_cache(&mut body, &req.segments, &placements, &caps);

        let url = self.messages_url();
        let cancel = CancellationToken::new();
        let make = || self.request_builder(url.clone(), &body);
        let response = self.http.send_with_retry(make, &cancel).await?;
        let text = response
            .text()
            .await
            .map_err(|err| BackendError::Transport {
                detail: err.to_string(),
            })?;
        let parsed: wire::MessagesResponse =
            serde_json::from_str(&text).map_err(|_| classify_malformed_body(&text))?;
        wire::to_generate_response(parsed, &req.tools)
    }

    async fn stream(
        &self,
        req: GenerateRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError> {
        let caps = self.capabilities(&req.model);
        let (mut body, placements) = wire::build_request_body(&req, DEFAULT_MAX_TOKENS, true);
        self.apply_cache(&mut body, &req.segments, &placements, &caps);

        let url = self.messages_url();
        let cancel = CancellationToken::new();
        let make = || self.request_builder(url.clone(), &body);
        // Only the initial response is sent through `send_with_retry`
        // (module boundary rule); everything after this runs off the
        // spawned driver task in `stream.rs` — nothing further is retried.
        let response = self.http.send_with_retry(make, &cancel).await?;
        Ok(stream::spawn(response, req.tools))
    }

    /// Anthropic's own dialect counting (board item 01KZDC4DKVC4JC3W4KN1WMC43N):
    /// builds the exact `/v1/messages` wire body `generate`/`stream` would
    /// send (Anthropic's message envelope, its system-prompt handling, its
    /// tool-schema shape — all distinct from `OpenAiCompatBackend`'s),
    /// estimates its size locally with `crate::admission::estimate_wire_tokens`
    /// (no network I/O — not even Anthropic's own `/v1/messages/count_tokens`),
    /// then calls the ONE shared arithmetic helper,
    /// [`check_admission`], for the fits/shortfall comparison rather than
    /// restating it -- one implementation of the headroom arithmetic, never a
    /// second copy.
    fn admit(
        &self,
        req: &GenerateRequest,
        headroom_tokens: u32,
    ) -> Result<Admission, BackendError> {
        let caps = self.capabilities(&req.model);
        let (body, _placements) = wire::build_request_body(req, DEFAULT_MAX_TOKENS, false);
        let est_tokens = crate::admission::estimate_wire_tokens(&body);
        check_admission(
            req.model.clone(),
            est_tokens,
            headroom_tokens,
            caps.max_context_tokens,
        )
    }

    async fn probe(&self) -> Result<ProbeReport, BackendError> {
        // A `/v1/messages` call is not free, so probe uses `GET
        // /v1/models` instead: 2s timeout, no retries (a probe is a single
        // observation, never retried like the transport-retry policy that
        // owns `BreakerKind::Transport`, Implementation Notes).
        let url = self.models_url();
        let started = Instant::now();
        let response = self
            .http
            .inner()
            .get(url)
            .timeout(PROBE_TIMEOUT)
            .header("x-api-key", self.api_key.expose_secret())
            .header("anthropic-version", self.anthropic_version.as_str())
            .send()
            .await
            .map_err(|err| BackendError::Transport {
                detail: err.to_string(),
            })?;

        let status = response.status();
        let latency_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(classify(status.as_u16(), &body, None));
        }

        Ok(ProbeReport {
            ok: true,
            latency_ms,
            models: parse_probe_models(&body),
            detail: None,
            at: Utc::now(),
        })
    }
}

/// Parses `{"data":[{"id":"claude-...","type":"model",...}, ...]}` — the
/// same OpenAI-shaped `/v1/models` list Anthropic exposes — into a list of
/// `ModelId`s. Any parse failure yields an empty list rather than an error:
/// `probe` reports liveness, not model-discovery correctness.
fn parse_probe_models(body: &str) -> Vec<ModelId> {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    value
        .get("data")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("id").and_then(Value::as_str))
                .map(ModelId::new)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_probe_models_reads_data_array_ids() {
        let body = r#"{"data":[{"id":"claude-sonnet-4-6","type":"model"},{"id":"claude-haiku-4-5","type":"model"}]}"#;
        let models = parse_probe_models(body);
        assert_eq!(
            models,
            vec![
                ModelId::new("claude-sonnet-4-6"),
                ModelId::new("claude-haiku-4-5")
            ]
        );
    }

    #[test]
    fn parse_probe_models_is_empty_on_malformed_body() {
        assert!(parse_probe_models("not json").is_empty());
        assert!(parse_probe_models(r#"{"nope": true}"#).is_empty());
    }

    fn backend_with_base(base: &str) -> AnthropicBackend {
        AnthropicBackend::new(AnthropicConfig {
            api_key: SecretString::new("test-key"),
            id: BackendId::new("anthropic"),
            base_url: base.parse().expect("test base_url must parse"),
            anthropic_version: "2023-06-01".to_string(),
            timeout: None,
            models: BTreeMap::new(),
        })
        .expect("backend construction")
    }

    /// A third-party Anthropic-compatible endpoint can live under a path
    /// prefix rather than at the host root -- Kimi's coding plan is served
    /// from `https://api.kimi.com/coding/`. The prefix must be preserved
    /// and exactly one `/` must join it to `v1/messages`, whether or not
    /// the configured base carries a trailing slash.
    ///
    /// Pinned because a plausible "cleanup" of `messages_url` (using
    /// `Url::join`, say) would silently drop the `/coding` prefix and point
    /// every request at the wrong path.
    #[test]
    fn messages_url_preserves_a_path_prefix_with_or_without_a_trailing_slash() {
        for base in [
            "https://api.kimi.com/coding/",
            "https://api.kimi.com/coding",
        ] {
            assert_eq!(
                backend_with_base(base).messages_url().as_str(),
                "https://api.kimi.com/coding/v1/messages",
                "base {base} must compose to the /coding-prefixed messages path"
            );
        }
    }

    #[test]
    fn messages_url_at_the_host_root_is_unchanged() {
        assert_eq!(
            backend_with_base("https://api.anthropic.com")
                .messages_url()
                .as_str(),
            "https://api.anthropic.com/v1/messages"
        );
    }
}
