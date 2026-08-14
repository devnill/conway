//! `Backend::probe` for `OpenAiCompatBackend`: a single best-effort
//! liveness/readiness check.
//!
//! Deliberately bypasses `HttpClient::send_with_retry` — a probe is one
//! observation, never retried (architecture §4.5: unlike `BreakerKind::
//! Transport`, which owns the bounded transport-retry policy, nothing here
//! feeds an independent breaker of its own — see `docs/routing.md`'s
//! "Health and failover" section). `GET {base}/models`, falling back for the
//! `"ollama"` built-in profile only (when `/models` reports `404`) to
//! `GET {base_origin}/api/tags` and then, if that is also unsupported, to
//! `GET {base_origin}/api/version` — each request capped at
//! [`PROBE_TIMEOUT`]. This Ollama-specific fallback chain is matched by
//! `profile.id`, not by a declarative field: probe endpoint selection is
//! out of the declarative-provider-profiles item's scope (a fundamentally
//! different concern from the four wire-behavior fields/path that item
//! covers), so a user-supplied or newly-added profile simply gets the
//! generic `/models`-only probe every other built-in profile already has.
//!
//! The three-tier Ollama fallback is ordered richest-to-plainest: `/models`
//! and `/api/tags` both carry a model list, `/api/version` carries none but
//! is the most universally-served Ollama liveness endpoint — real
//! Ollama Cloud deployments have been observed 404-ing on both `/models`
//! (no OpenAI-compat model listing) and `/api/tags` (a local-instance
//! management endpoint), so a plain version check is the last resort that
//! still proves the server answers HTTP requests at all. If every tier
//! 404s, [`OpenAiCompatBackend::run_probe`] still returns
//! `Err(BackendError::BadRequest{..})` (via [`classify`]) rather than
//! inventing a synthetic success — a caller of [`Backend::probe`] classifying
//! this result is expected to recognize that a `BadRequest`-classified probe
//! failure means "this liveness path isn't served here", not "the endpoint
//! is down". No production code currently consumes
//! `Backend::probe` at all — `conway_plugin_routing`'s periodic health
//! prober, formerly this classification's only consumer, was retired (board
//! item, which is done: that citation is
//! provenance for how the consumer went away, not an open thread).
//!
//! **What remains open is what to do about the port method now.** That is
//! — "retiring the prober left
//! `Backend::probe` with no production consumer, decide whether the port
//! method stays". Until it is decided, this method and `Backend::probe`
//! itself are unaffected and remain part of the `Backend` port's public
//! contract, exercised directly by this crate's own tests.

use std::time::{Duration, Instant};

use conway_core::capabilities::ProbeReport;
use conway_core::error::BackendError;
use conway_core::ids::ModelId;
use serde::Deserialize;
use url::Url;

use crate::error::classify;
use crate::probe::{join_base, join_origin};

use super::OpenAiCompatBackend;

/// Per-request timeout for the probe. Short and independent of the
/// adapter's configured request timeout — a probe is a liveness check, not
/// a generation request.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
}

#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    #[serde(default)]
    models: Vec<OllamaTagEntry>,
}

#[derive(Debug, Deserialize)]
struct OllamaTagEntry {
    name: String,
}

fn elapsed_ms(started: Instant) -> u32 {
    u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX)
}

fn parse_model_ids(body: &str) -> Vec<ModelId> {
    serde_json::from_str::<ModelsResponse>(body)
        .map(|resp| resp.data.into_iter().map(|m| ModelId::new(m.id)).collect())
        .unwrap_or_default()
}

impl OpenAiCompatBackend {
    fn probe_request(&self, url: Url) -> reqwest::RequestBuilder {
        let mut builder = self.http.inner().get(url).timeout(PROBE_TIMEOUT);
        if let Some(key) = &self.auth {
            builder = builder.bearer_auth(key.expose_secret());
        }
        builder
    }

    /// `GET {base}/models`, 2s timeout, no retries. A connection failure is
    /// `Err(BackendError::Transport{..})` after exactly this one request —
    /// the `"ollama"` profile's `/api/tags`/`/api/version` fallbacks below
    /// only ever fire on a classified `404` response, never on a transport
    /// failure.
    pub(crate) async fn run_probe(&self) -> Result<ProbeReport, BackendError> {
        let url = join_base(&self.base, "/models");
        let started = Instant::now();
        let response =
            self.probe_request(url)
                .send()
                .await
                .map_err(|err| BackendError::Transport {
                    detail: err.to_string(),
                })?;
        let status = response.status();

        if status.is_success() {
            let latency_ms = elapsed_ms(started);
            let body = response.text().await.unwrap_or_default();
            return Ok(ProbeReport {
                ok: true,
                latency_ms,
                models: parse_model_ids(&body),
                detail: None,
                at: chrono::Utc::now(),
            });
        }

        if self.profile.id == "ollama" && status.as_u16() == 404 {
            if let Some(report) = self.probe_ollama_tags().await {
                return Ok(report);
            }
            if let Some(report) = self.probe_ollama_version().await {
                return Ok(report);
            }
        }

        let body = response.text().await.unwrap_or_default();
        Err(classify(status.as_u16(), &body, None))
    }

    async fn probe_ollama_tags(&self) -> Option<ProbeReport> {
        let url = join_origin(&self.base, "/api/tags");
        let started = Instant::now();
        let response = self.probe_request(url).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let latency_ms = elapsed_ms(started);
        let body = response.text().await.unwrap_or_default();
        let tags: OllamaTagsResponse = serde_json::from_str(&body).ok()?;
        Some(ProbeReport {
            ok: true,
            latency_ms,
            models: tags
                .models
                .into_iter()
                .map(|m| ModelId::new(m.name))
                .collect(),
            detail: None,
            at: chrono::Utc::now(),
        })
    }

    /// `GET {base_origin}/api/version`, the last-resort Ollama liveness
    /// fallback: carries no model list, but is the plainest
    /// endpoint every real Ollama server answers, for deployments (e.g.
    /// Ollama Cloud) that 404 on both `/models` and `/api/tags`. Body
    /// content is irrelevant — a successful status alone proves liveness.
    async fn probe_ollama_version(&self) -> Option<ProbeReport> {
        let url = join_origin(&self.base, "/api/version");
        let started = Instant::now();
        let response = self.probe_request(url).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let latency_ms = elapsed_ms(started);
        Some(ProbeReport {
            ok: true,
            latency_ms,
            models: vec![],
            detail: None,
            at: chrono::Utc::now(),
        })
    }
}

#[cfg(test)]
mod tests {
    use conway_core::ids::BackendId;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::config::{Dialect, OpenAiCompatConfig};

    fn backend_config(base_url: &str, dialect: Dialect) -> OpenAiCompatConfig {
        OpenAiCompatConfig {
            id: BackendId::new("test"),
            base_url: base_url.parse().unwrap(),
            api_key: None,
            profile: dialect.profile(),
            timeout: None,
            metadata_path: None,
            models: Default::default(),
        }
    }

    /// Criterion 1: when both `/models` and `/api/tags` 404 (the observed
    /// Ollama Cloud shape), the probe still lands on a real liveness path —
    /// `/api/version` — instead of giving up.
    #[tokio::test]
    async fn ollama_probe_falls_back_to_api_version_when_models_and_tags_both_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"version": "0.5.1"})))
            .expect(1)
            .mount(&server)
            .await;

        let backend =
            OpenAiCompatBackend::new(backend_config(&server.uri(), Dialect::Ollama)).unwrap();
        let report = backend.run_probe().await.unwrap();

        assert!(report.ok);
        assert!(report.models.is_empty());
    }

    /// `/api/version` is a last resort: when `/api/tags` already answers
    /// with a model list, `/api/version` must never be called.
    #[tokio::test]
    async fn ollama_probe_prefers_api_tags_over_api_version() {
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
        Mock::given(method("GET"))
            .and(path("/api/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"version": "0.5.1"})))
            .expect(0)
            .mount(&server)
            .await;

        let backend =
            OpenAiCompatBackend::new(backend_config(&server.uri(), Dialect::Ollama)).unwrap();
        let report = backend.run_probe().await.unwrap();

        assert!(report.ok);
        assert_eq!(report.models, vec![ModelId::new("qwen3-coder:30b")]);
    }

    /// Criterion 2 (classification half, exercised at the source): once
    /// every Ollama liveness path — `/models`, `/api/tags`, `/api/version`
    /// — is unsupported, `run_probe` reports the failure as
    /// `BackendError::BadRequest` (via `classify`'s 404 row), never as a
    /// transport-level failure. It is `conway_plugin_routing::prober`'s job to
    /// read that classification and withhold a health observation rather
    /// than trip the breaker.
    #[tokio::test]
    async fn ollama_probe_unsupported_everywhere_is_bad_request_not_transport() {
        let server = MockServer::start().await;
        for endpoint in ["/models", "/api/tags", "/api/version"] {
            Mock::given(method("GET"))
                .and(path(endpoint))
                .respond_with(ResponseTemplate::new(404))
                .mount(&server)
                .await;
        }

        let backend =
            OpenAiCompatBackend::new(backend_config(&server.uri(), Dialect::Ollama)).unwrap();
        let err = backend
            .run_probe()
            .await
            .expect_err("every liveness path 404s");

        assert!(matches!(err, BackendError::BadRequest { .. }), "{err:?}");
    }

    /// A non-Ollama dialect never consults the Ollama-only fallback chain.
    #[tokio::test]
    async fn non_ollama_dialect_never_falls_back_to_ollama_paths() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": []})))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"version": "0.5.1"})))
            .expect(0)
            .mount(&server)
            .await;

        let backend =
            OpenAiCompatBackend::new(backend_config(&server.uri(), Dialect::OpenAi)).unwrap();
        let err = backend.run_probe().await.expect_err("404 with no fallback");

        assert!(matches!(err, BackendError::BadRequest { .. }), "{err:?}");
    }
}
