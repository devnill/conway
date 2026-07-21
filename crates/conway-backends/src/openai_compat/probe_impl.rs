//! `Backend::probe` for `OpenAiCompatBackend`: a single best-effort
//! liveness/readiness check (WI-020).
//!
//! Deliberately bypasses `HttpClient::send_with_retry` — a probe is one
//! observation, never retried (architecture §4.5: `BreakerKind::Probe` is
//! independent of `BreakerKind::Transport`, which owns the bounded
//! transport-retry policy). `GET {base}/models`, falling back to
//! `GET {base_origin}/api/tags` only for `Dialect::Ollama` when `/models`
//! reports `404`, each request capped at [`PROBE_TIMEOUT`].

use std::time::{Duration, Instant};

use conway_core::capabilities::ProbeReport;
use conway_core::error::BackendError;
use conway_core::ids::ModelId;
use serde::Deserialize;
use url::Url;

use crate::config::Dialect;
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
    /// the `Dialect::Ollama` `/api/tags` fallback below only ever fires on a
    /// classified `404` response, never on a transport failure.
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

        if matches!(self.dialect, Dialect::Ollama) && status.as_u16() == 404 {
            if let Some(report) = self.probe_ollama_tags().await {
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
}
