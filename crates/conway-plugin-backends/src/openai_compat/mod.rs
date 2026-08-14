//! `OpenAiCompatBackend`: one `Backend` adapter, dialect-selected behavior,
//! covering every OpenAI-compatible chat-completions server (architecture
//! §"Module: conway-backends").
//!
//! `wire.rs` owns the segment↔message and response↔`GenerateResponse`
//! mapping, `stream.rs` owns SSE streaming, [`crate::profile::Profile`]
//! (declarative provider profiles item) owns the small per-provider wire
//! differences that used to live in a private `dialect.rs`. Every built-in
//! profile — `openai`, `ollama`, `vllm_hermes`, `lm_studio`,
//! `llama_cpp_server`, `kimi` — and any user-supplied profile compile
//! through the same code paths (every field this module reads is total).

mod probe_impl;
mod stream;
mod wire;

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use conway_core::capabilities::{Capabilities, ProbeReport};
use conway_core::error::BackendError;
use conway_core::ids::{BackendId, ModelId};
use conway_core::ports::{
    check_admission, Admission, Backend, BoxStream, GenerateRequest, GenerateResponse, StreamChunk,
};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::capabilities::{build_capabilities, CapabilityInputs};
use crate::config::{ConfigError, ModelOverrides, OpenAiCompatConfig, SecretString};
use crate::error::classify_malformed_body;
use crate::http::HttpClient;
use crate::model_metadata::ModelMetadataStore;
use crate::profile::Profile;

/// Applied when `OpenAiCompatConfig::timeout` is `None`. Shorter than
/// `DEFAULT_ANTHROPIC_TIMEOUT` (600s): OpenAI-compatible endpoints in this
/// adapter's scope are typically local/same-LAN servers, so two minutes is
/// ample even for a slow non-streamed generation on modest hardware.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// One adapter for every OpenAI-compatible chat-completions server;
/// `profile` selects wire quirks and default capabilities (declarative
/// provider profiles item; `crate::profile::Profile`).
pub struct OpenAiCompatBackend {
    id: BackendId,
    base: Url,
    profile: Profile,
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
        // `profile.chat_path` is untrusted (a user-supplied profile
        // file can set it to anything), so this validates it composes to a
        // real URL *now* — a typed, named error at construction — rather
        // than deferring to a panic the first time a request is sent.
        let base = config.base_url.as_str().trim_end_matches('/');
        if format!("{base}{}", config.profile.chat_path)
            .parse::<Url>()
            .is_err()
        {
            return Err(ConfigError::Profile {
                path: format!("profile '{}'", config.profile.id),
                detail: format!(
                    "chat_path '{}' does not form a valid URL when joined with base_url '{base}'",
                    config.profile.chat_path
                ),
            });
        }
        let timeout = config.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let http =
            HttpClient::with_timeout(timeout).expect("reqwest client with rustls TLS must build");
        Ok(Self {
            id: config.id,
            base: config.base_url,
            profile: config.profile,
            http,
            auth: config.api_key,
            models,
            overrides: config.models,
        })
    }

    /// `{base_url}{profile.chat_path}`. Never fails: `new` already validated
    /// this exact join parses as a URL.
    fn chat_url(&self) -> Url {
        let base = self.base.as_str().trim_end_matches('/');
        format!("{base}{}", self.profile.chat_path)
            .parse()
            .expect("validated in OpenAiCompatBackend::new")
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
            dialect_defaults: self.profile.dialect_defaults(),
            metadata: self.models.get(model),
            overrides: self.overrides.get(model.as_str()),
        })
    }

    async fn generate(&self, req: GenerateRequest) -> Result<GenerateResponse, BackendError> {
        let caps = self.capabilities(&req.model);
        let body = wire::build_request_body(&req, &self.profile, caps.parallel_tool_calls, false);
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
        wire::to_generate_response(parsed, &self.profile, &req.tools)
    }

    async fn stream(
        &self,
        req: GenerateRequest,
    ) -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError> {
        let caps = self.capabilities(&req.model);
        let body = wire::build_request_body(&req, &self.profile, caps.parallel_tool_calls, true);
        let url = self.chat_url();
        let cancel = CancellationToken::new();
        let make = || self.request_builder(url.clone(), &body);
        // Only the initial response is sent through `send_with_retry`
        // (module boundary rule): once the body starts streaming, `spawn`
        // owns it and nothing further is retried.
        let response = self.http.send_with_retry(make, &cancel).await?;
        Ok(stream::spawn(response, self.profile.clone(), req.tools))
    }

    async fn probe(&self) -> Result<ProbeReport, BackendError> {
        self.run_probe().await
    }

    /// This dialect's own counting:
    /// builds the exact chat-completions wire body `generate`/`stream`
    /// would send — this profile's own message envelope, tool-schema
    /// shape, and any per-provider quirks `wire::build_request_body`
    /// applies — distinct from `AnthropicBackend`'s native Messages body,
    /// estimates its size locally with
    /// `crate::admission::estimate_wire_tokens` (no network I/O; no
    /// OpenAI-compatible server this crate targets even exposes a
    /// count-tokens endpoint), then calls the ONE shared arithmetic helper,
    /// [`check_admission`], for the fits/shortfall comparison rather than
    /// restating it -- one implementation of the headroom arithmetic, never a
    /// second copy.
    fn admit(
        &self,
        req: &GenerateRequest,
        headroom_tokens: u32,
    ) -> Result<Admission, BackendError> {
        let caps = self.capabilities(&req.model);
        let body = wire::build_request_body(req, &self.profile, caps.parallel_tool_calls, false);
        let est_tokens = crate::admission::estimate_wire_tokens(&body);
        check_admission(
            req.model.clone(),
            est_tokens,
            headroom_tokens,
            caps.max_context_tokens,
        )
    }
}
