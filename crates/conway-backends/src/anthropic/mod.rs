//! `AnthropicBackend`: native Anthropic Messages API adapter with explicit
//! cache-breakpoint mapping (architecture §"Module: conway-backends",
//! WI-021).
//!
//! `wire.rs` owns the segment↔message and response↔`GenerateResponse`
//! mapping, `stream.rs` owns SSE streaming and Anthropic's `content_block_*`
//! tool-call delta translation, `cache.rs` owns the `CacheMode::
//! ExplicitBreakpoints` cache-hint → `cache_control` mapping. API key only —
//! no OAuth path exists anywhere in this file set: only `x-api-key`/
//! `anthropic-version` headers are ever constructed (GP-09/C-02;
//! `AnthropicConfig` already rejects `sk-ant-oat*` keys at config-parse
//! time, WI-016).

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
use conway_core::ports::{Backend, BoxStream, GenerateRequest, GenerateResponse, StreamChunk};
use conway_core::segment::PromptSegment;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::capabilities::{anthropic_defaults, build_capabilities, CapabilityInputs};
use crate::config::{AnthropicConfig, ConfigError, Dialect, ModelOverrides, SecretString};
use crate::error::{classify, classify_malformed_body};
use crate::http::HttpClient;
use crate::model_metadata::ModelMetadataStore;
use crate::tool_calls::ToolCallAccumulator;
use wire::BreakpointTarget;

/// Applied when `req.params.max_tokens` is unset — Anthropic's `max_tokens`
/// field is required by the API.
const DEFAULT_MAX_TOKENS: u32 = 8192;

/// `probe()`'s per-request timeout: a `/v1/models` liveness check is cheap
/// and must fail fast (Implementation Notes).
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Fixed backend identity: unlike `OpenAiCompatConfig`, `AnthropicConfig`
/// carries no `id` field — there is exactly one Anthropic Messages API to
/// speak, not a family of independently-identified endpoints.
fn backend_id() -> BackendId {
    BackendId::new("anthropic")
}

/// Constructs a `ToolCallAccumulator` for the Anthropic adapter. Anthropic's
/// `content_block_start`/`content_block_delta` events are translated
/// (`stream.rs`) into the `{"index":..,"id":..,"function":{"name":..,
/// "arguments":..}}` shape `Dialect::OpenAi`'s parser expects, so this
/// reuses `push_delta`/`push_complete` unmodified rather than adding a
/// sixth `Dialect` variant or touching `src/tool_calls/*` (owned by
/// WI-018/WI-022).
pub(crate) fn new_tool_call_accumulator(tools: &[ToolSpec]) -> ToolCallAccumulator {
    ToolCallAccumulator::new(Dialect::OpenAi, tools)
}

/// Native Anthropic Messages API adapter (feature `anthropic`).
pub struct AnthropicBackend {
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
    /// `Authorization` header is ever constructed (GP-09/C-02).
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
        backend_id()
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

    async fn probe(&self) -> Result<ProbeReport, BackendError> {
        // A `/v1/messages` call is not free, so probe uses `GET
        // /v1/models` instead: 2s timeout, no retries (a probe is a single
        // observation — the health layer's `BreakerKind::Probe` is
        // independent of `BreakerKind::Transport`, Implementation Notes).
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
}
