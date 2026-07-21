//! `OpenAiCompatBackend`: one `Backend` adapter, dialect-selected behavior,
//! covering every OpenAI-compatible chat-completions server (architecture
//! §"Module: conway-backends", WI-019).
//!
//! `wire.rs` owns the segment↔message and response↔`GenerateResponse`
//! mapping, `stream.rs` owns SSE streaming, `dialect.rs` owns the small
//! per-dialect wire differences. This item wires and tests only the
//! `OpenAi` and `Ollama` dialects; `VllmHermes`, `LmStudio`, and
//! `LlamaCppServer` already compile through the same code paths (every
//! `Dialect::defaults()`/`chat_path()`/etc. arm is total) and are exercised
//! by WI-020/WI-022.

mod dialect;
mod stream;
mod wire;

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use conway_core::capabilities::{Capabilities, ProbeReport};
use conway_core::error::BackendError;
use conway_core::ids::{BackendId, ModelId};
use conway_core::ports::{Backend, BoxStream, GenerateRequest, GenerateResponse, StreamChunk};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::capabilities::{build_capabilities, CapabilityInputs};
use crate::config::{ConfigError, Dialect, ModelOverrides, OpenAiCompatConfig, SecretString};
use crate::error::classify_malformed_body;
use crate::http::HttpClient;
use crate::model_metadata::ModelMetadataStore;

/// Applied when `OpenAiCompatConfig::timeout` is `None`. Shorter than
/// `DEFAULT_ANTHROPIC_TIMEOUT` (600s): OpenAI-compatible endpoints in this
/// adapter's scope are typically local/same-LAN servers, so two minutes is
/// ample even for a slow non-streamed generation on modest hardware.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// One adapter for every OpenAI-compatible chat-completions server;
/// `dialect` selects wire quirks (`dialect.rs`) and default capabilities
/// (WI-017's `dialect_defaults`).
pub struct OpenAiCompatBackend {
    id: BackendId,
    base: Url,
    dialect: Dialect,
    http: HttpClient,
    auth: Option<SecretString>,
    models: ModelMetadataStore,
    overrides: BTreeMap<String, ModelOverrides>,
}

impl OpenAiCompatBackend {
    /// Merges the bundled `ModelMetadataStore::defaults()` with
    /// `config.metadata_path` (a missing file at that path is not an
    /// error — see `ModelMetadataStore::load`), then stores `config.models`
    /// as the per-model override table `capabilities` consults.
    pub fn new(config: OpenAiCompatConfig) -> Result<Self, ConfigError> {
        let mut models = ModelMetadataStore::defaults();
        if let Some(path) = &config.metadata_path {
            models = models.merge(ModelMetadataStore::load(path)?);
        }
        let timeout = config.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let http =
            HttpClient::with_timeout(timeout).expect("reqwest client with rustls TLS must build");
        Ok(Self {
            id: config.id,
            base: config.base_url,
            dialect: config.dialect,
            http,
            auth: config.api_key,
            models,
            overrides: config.models,
        })
    }

    /// `{base_url}/chat/completions`. `base_url` is already a validated
    /// `Url` (parsed at config-deserialize time), and `chat_path()` is a
    /// fixed, valid-URL-safe suffix, so this cannot fail in practice.
    fn chat_url(&self) -> Url {
        let base = self.base.as_str().trim_end_matches('/');
        format!("{base}{}", self.dialect.chat_path())
            .parse()
            .expect("base_url + chat_path must form a valid URL")
    }

    fn request_builder(&self, url: Url, body: &serde_json::Value) -> reqwest::RequestBuilder {
        let mut builder = self.http.inner().post(url).json(body);
        if let Some(key) = &self.auth {
            builder = builder.bearer_auth(key.expose_secret());
        }
        builder
    }
}

#[async_trait]
impl Backend for OpenAiCompatBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn capabilities(&self, model: &ModelId) -> Capabilities {
        build_capabilities(CapabilityInputs {
            dialect_defaults: self.dialect.defaults(),
            metadata: self.models.get(model),
            overrides: self.overrides.get(model.as_str()),
        })
    }

    async fn generate(&self, req: GenerateRequest) -> Result<GenerateResponse, BackendError> {
        let caps = self.capabilities(&req.model);
        let body = wire::build_request_body(&req, self.dialect, caps.parallel_tool_calls, false);
        let url = self.chat_url();
        let cancel = CancellationToken::new();
        let make = || self.request_builder(url.clone(), &body);
        let response = self.http.send_with_retry(make, &cancel).await?;
        let text = response
            .text()
            .await
            .map_err(|err| BackendError::Transport {
                detail: err.to_string(),
            })?;
        let parsed: wire::ChatCompletionResponse =
            serde_json::from_str(&text).map_err(|_| classify_malformed_body(&text))?;
        wire::to_generate_response(parsed, self.dialect, &req.tools)
    }

    async fn stream(
        &self,
        req: GenerateRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError> {
        let caps = self.capabilities(&req.model);
        let body = wire::build_request_body(&req, self.dialect, caps.parallel_tool_calls, true);
        let url = self.chat_url();
        let cancel = CancellationToken::new();
        let make = || self.request_builder(url.clone(), &body);
        // Only the initial response is sent through `send_with_retry`
        // (module boundary rule): once the body starts streaming, `spawn`
        // owns it and nothing further is retried.
        let response = self.http.send_with_retry(make, &cancel).await?;
        Ok(stream::spawn(response, self.dialect, req.tools))
    }

    async fn probe(&self) -> Result<ProbeReport, BackendError> {
        // Capability probing (`GET /models`, latency measurement) is
        // WI-020's scope; this item only wires generate/stream.
        Err(BackendError::BadRequest {
            detail: "OpenAiCompatBackend::probe is not implemented until WI-020".into(),
        })
    }
}
