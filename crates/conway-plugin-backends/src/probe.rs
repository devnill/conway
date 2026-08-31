//! `CapabilityProbe`: startup-time discovery that queries an OpenAI-compatible
//! endpoint's model list and server properties, merging the result with
//! `ModelMetadata` to produce `Capabilities` per model (architecture
//! §"Module: conway-backends").
//!
//! Every discovery step is best-effort — a 5s timeout, zero retries, and any
//! failure (transport error, non-2xx status, malformed body) is treated as
//! "this step found nothing", never propagated. Discovery is therefore never
//! a hard dependency: [`CapabilityProbe::discover`] always returns `Ok`, even
//! when every endpoint is unreachable, falling back to capabilities derived
//! from `ModelMetadata` and config `ModelOverrides` alone.
//!
//! Merge precedence for each discovered model, per the module's capability
//! rule: config `ModelOverrides` > `ModelMetadata` entry > probed server
//! value > `DialectDefaults`. Discovery may only narrow `max_context_tokens`
//! when no explicit value exists; it never raises `tool_calling` above the
//! metadata/dialect value and never sets `reliability_tier` to `Verified` —
//! both of those fields are untouched by the probed layer here, which only
//! ever adjusts `max_context_tokens` and (for the `"llama_cpp_server"`
//! built-in profile with a missing/empty `chat_template`) downgrades
//! `reliability_tier` to `Unknown`. The extra probing steps below (`/api/tags`,
//! `/api/show`, `/props`) are matched by `profile.id`, not by a
//! declarative field — endpoint selection for discovery is a different
//! concern from the wire-behavior/capability fields `Profile` declares (see
//! `openai_compat/probe_impl.rs`'s module doc), so a user-supplied or
//! newly-added profile gets the generic `/models`-only step every other
//! built-in profile without a named extra step already has.
//!
//! `discover` never fabricates a `Capabilities` entry for a model that was
//! neither observed from an endpoint nor listed in the configured `models`
//! overrides.
//!
//! # `/api/show` vs. `/api/ps` for Ollama's `max_context_tokens`
//!
//! Ollama exposes two different numbers under the name "context length" for
//! the same model, and they can genuinely differ: `POST /api/show`'s
//! `model_info["<arch>.context_length"]` is the model's own declared
//! ceiling (a stable property of the model, observed at 262,144 for a real
//! local `qwen3` model this item's investigation used); `GET /api/ps`'s
//! per-entry `context_length` is whatever window the CURRENTLY LOADED
//! runtime instance happens to be configured with right now (observed at
//! 32,768 for that same running instance — smaller, because Ollama's own
//! runtime default/last-requested `num_ctx` governs it, not the model's
//! real capacity).
//!
//! **This module probes `/api/show`, never `/api/ps`, and does so
//! deliberately.** `/api/ps` requires the model to already be resident
//! (nothing is loaded until a generation happens, so a discovery step that
//! ran before the first real request would usually see an empty list
//! anyway) and reports a load-time-contingent number that this exact item's
//! own defect is a case study in NOT trusting: the ORIGINAL 32,768-token
//! ceiling that refused a real session came from exactly this kind of
//! "smaller, session-contingent number substituting for the model's real
//! capacity" pattern (there it was the profile's own static floor rather
//! than a live `/api/ps` read, but the failure mode — a small, plausible-
//! looking number standing in for the model's real window — is the same
//! shape). `/api/show`'s architecture ceiling is a fact about the model,
//! independent of what happens to be loaded at probe time, which is what
//! `ModelMetadata`/`Capabilities::max_context_tokens` are FOR: describing
//! the model, not the moment.
//!
//! The accepted risk: if an operator's server is deliberately configured
//! with a smaller `num_ctx` than the model's architecture ceiling (a real,
//! memory-constrained local setup), this probe reports a window larger than
//! that server will currently honor, and a request sized against it can
//! still be refused by Ollama itself. That refusal is a real, LOUD,
//! provider-side error surfaced through the ordinary transport-error path —
//! not a silent conway-side pre-refusal for a request the server might
//! actually have served, which is what trusting a too-small number produces
//! instead. Given this item's own governing principle (declaration
//! honesty: a refusal must be honest about what actually gated it, and a
//! silent under-declaration is strictly worse than a loud one), erring
//! toward the model's real ceiling is the better default.

use std::collections::BTreeMap;
use std::time::Duration;

use conway_core::capabilities::{Capabilities, ReliabilityTier};
use conway_core::error::BackendError;
use conway_core::ids::ModelId;
use serde::Deserialize;
use url::Url;

use crate::capabilities::{build_capabilities, CapabilityInputs};
use crate::config::{ModelOverrides, SecretString};
use crate::http::HttpClient;
use crate::model_metadata::ModelMetadataStore;
use crate::profile::Profile;

/// Per-step timeout for discovery requests. Deliberately short and
/// independent of the adapter's configured request timeout — discovery is a
/// best-effort startup probe, not a user-facing generation request.
///
/// `pub(crate)`, not private: `factory.rs`'s `OpenAiCompatBackendFactory::
/// probe_capabilities` constructs its own `CapabilityProbe` and needs this
/// same value for `CapabilityProbe::new`'s `timeout` parameter — before
/// a later item, that caller lived in `crates/
/// conway/src/builder.rs` (a different crate) and had to maintain its own
/// `PROBE_TIMEOUT` constant "mirroring" this one; now that the caller moved
/// into this same crate, the duplicate constant is gone and this is the one
/// value both `probe_capabilities` and every test below share.
pub(crate) const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Joins `suffix` onto `base`, trimming exactly one trailing slash from
/// `base` first — the same join rule `OpenAiCompatBackend::chat_url` uses.
/// Used for endpoints that live under the configured `base_url` (`/models`).
pub(crate) fn join_base(base: &Url, suffix: &str) -> Url {
    let trimmed = base.as_str().trim_end_matches('/');
    format!("{trimmed}{suffix}")
        .parse()
        .expect("base_url + suffix must form a valid URL")
}

/// Joins `suffix` onto `base`'s origin (scheme + host + port), discarding
/// any path component. Used for host-level endpoints (`/api/tags`,
/// `/props`) that are not versioned under `base_url`'s path — e.g. Ollama
/// serves `/v1/models` under the configured `.../v1` base but `/api/tags` at
/// the server root.
pub(crate) fn join_origin(base: &Url, suffix: &str) -> Url {
    let origin = base.origin().ascii_serialization();
    format!("{origin}{suffix}")
        .parse()
        .expect("origin + suffix must form a valid URL")
}

/// OpenAI-shaped `/models` response: `{"data":[{"id":...}]}`.
#[derive(Debug, Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
    /// VLLM-specific: the model's configured context length. Populated only
    /// when `profile.id == "vllm_hermes"` consults it.
    #[serde(default)]
    max_model_len: Option<u32>,
}

/// Ollama-shaped `/api/tags` response: `{"models":[{"name":...}]}`.
#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    #[serde(default)]
    models: Vec<OllamaTagEntry>,
}

#[derive(Debug, Deserialize)]
struct OllamaTagEntry {
    name: String,
}

/// Ollama-shaped `POST /api/show` response (the fields this module reads
/// out of it): `model_info` is a flat map of dotted keys
/// (`"general.architecture"`, `"<arch>.context_length"`, ...) rather than a
/// nested structure, so it is read as an untyped map and the
/// architecture-prefixed context-length key is looked up dynamically —
/// see [`context_length_from_show`].
#[derive(Debug, Deserialize)]
struct OllamaShowResponse {
    #[serde(default)]
    model_info: BTreeMap<String, serde_json::Value>,
}

/// `/api/show`'s `model_info` names its context-length key
/// `"<arch>.context_length"`, where `<arch>` is itself declared under
/// `"general.architecture"` in the same map (e.g. `"llama.context_length"`,
/// `"qwen3.context_length"`) — there is no fixed key, so this reads the
/// architecture first and builds the real key from it. `None` if either
/// piece is missing or not the expected JSON type, never a panic: this is
/// untrusted server response data.
fn context_length_from_show(model_info: &BTreeMap<String, serde_json::Value>) -> Option<u32> {
    let arch = model_info.get("general.architecture")?.as_str()?;
    let key = format!("{arch}.context_length");
    model_info.get(&key)?.as_u64().map(|n| n as u32)
}

/// Llama.cpp-shaped `/props` response.
#[derive(Debug, Deserialize)]
struct LlamaCppProps {
    #[serde(default)]
    default_generation_settings: Option<LlamaCppGenerationSettings>,
    #[serde(default)]
    chat_template: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LlamaCppGenerationSettings {
    #[serde(default)]
    n_ctx: Option<u32>,
}

/// Per-model probed hints, layered between a model's `ModelMetadata` and its
/// dialect's `DialectDefaults` — see the module-level merge-precedence note.
#[derive(Debug, Default, Clone, Copy)]
struct ProbedHints {
    max_context_tokens: Option<u32>,
    reliability_downgrade: bool,
}

/// The outcome of one [`CapabilityProbe::discover_result`] call: the
/// composed capabilities plus whether discovery observed nothing over the
/// network (in which case `capabilities` reflects `ModelMetadata` and
/// configured `models` overrides alone).
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveryResult {
    pub capabilities: BTreeMap<ModelId, Capabilities>,
    pub degraded: bool,
}

/// Startup-time capability discovery for one `OpenAiCompatBackend`
/// configuration. Construction never performs I/O; every network call is
/// made lazily, inside [`CapabilityProbe::discover`]/[`CapabilityProbe::discover_result`].
pub struct CapabilityProbe {
    http: HttpClient,
    base: Url,
    profile: Profile,
    auth: Option<SecretString>,
    metadata: ModelMetadataStore,
    overrides: BTreeMap<String, ModelOverrides>,
}

impl CapabilityProbe {
    /// `timeout` sizes the underlying `HttpClient`'s base client timeout;
    /// every individual discovery request additionally caps itself at
    /// `DISCOVERY_TIMEOUT` regardless of this value.
    pub fn new(
        base: Url,
        profile: Profile,
        auth: Option<SecretString>,
        timeout: Duration,
        metadata: ModelMetadataStore,
        overrides: BTreeMap<String, ModelOverrides>,
    ) -> Self {
        let http =
            HttpClient::with_timeout(timeout).expect("reqwest client with rustls TLS must build");
        Self {
            http,
            base,
            profile,
            auth,
            metadata,
            overrides,
        }
    }

    fn request(&self, url: Url) -> reqwest::RequestBuilder {
        let mut builder = self.http.inner().get(url).timeout(DISCOVERY_TIMEOUT);
        if let Some(key) = &self.auth {
            builder = builder.bearer_auth(key.expose_secret());
        }
        builder
    }

    /// [`Self::request`]'s `POST` counterpart, for `/api/show` — the only
    /// discovery step that isn't a plain `GET`.
    fn post_request(&self, url: Url, body: &serde_json::Value) -> reqwest::RequestBuilder {
        let mut builder = self
            .http
            .inner()
            .post(url)
            .timeout(DISCOVERY_TIMEOUT)
            .json(body);
        if let Some(key) = &self.auth {
            builder = builder.bearer_auth(key.expose_secret());
        }
        builder
    }

    /// Step 1 (all dialects): `GET {base}/models`.
    async fn fetch_models(&self) -> Option<Vec<ModelEntry>> {
        let response = self
            .request(join_base(&self.base, "/models"))
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let body: ModelsResponse = response.json().await.ok()?;
        if body.data.is_empty() {
            None
        } else {
            Some(body.data)
        }
    }

    /// Step 2 (`"ollama"` profile fallback): `GET {base_origin}/api/tags`.
    async fn fetch_ollama_tags(&self) -> Option<Vec<String>> {
        let response = self
            .request(join_origin(&self.base, "/api/tags"))
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let body: OllamaTagsResponse = response.json().await.ok()?;
        if body.models.is_empty() {
            None
        } else {
            Some(body.models.into_iter().map(|m| m.name).collect())
        }
    }

    /// Step 2b (`"ollama"` profile, run after step 1/2 for every model name
    /// observed so far): `POST {base_origin}/api/show`, body `{"model":
    /// name}`, reading `model_info`'s architecture-prefixed
    /// `"<arch>.context_length"` — the model's own declared ceiling, not
    /// the currently-loaded runtime window (`GET /api/ps`'s
    /// `context_length`, which this module deliberately does NOT probe;
    /// see the module doc's "`/api/show` vs. `/api/ps`" note for why).
    /// Best-effort per model: a failure for one name never affects any
    /// other, and never affects `degraded`.
    async fn fetch_ollama_show_context_length(&self, model_name: &str) -> Option<u32> {
        let response = self
            .post_request(
                join_origin(&self.base, "/api/show"),
                &serde_json::json!({ "model": model_name }),
            )
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let body: OllamaShowResponse = response.json().await.ok()?;
        context_length_from_show(&body.model_info)
    }

    /// Step 3 (`"llama_cpp_server"` profile): `GET {base_origin}/props`.
    async fn fetch_llama_cpp_props(&self) -> Option<LlamaCppProps> {
        let response = self
            .request(join_origin(&self.base, "/props"))
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.json().await.ok()
    }

    /// Runs the full discovery sequence and composes a [`Capabilities`] for
    /// every observed or configured-override model id, never for any other
    /// model. `degraded` is `true` exactly when zero model ids were
    /// observed over the network — configured `models` overrides absent
    /// from discovery are always included regardless of `degraded`.
    pub async fn discover_result(&self) -> DiscoveryResult {
        let mut observed: BTreeMap<String, ProbedHints> = BTreeMap::new();

        if let Some(entries) = self.fetch_models().await {
            for entry in entries {
                let hints = observed.entry(entry.id).or_default();
                if self.profile.id == "vllm_hermes" {
                    if let Some(max_model_len) = entry.max_model_len {
                        hints.max_context_tokens = Some(max_model_len);
                    }
                }
            }
        }

        if self.profile.id == "ollama" && observed.is_empty() {
            if let Some(names) = self.fetch_ollama_tags().await {
                for name in names {
                    observed.entry(name).or_default();
                }
            }
        }

        // Step 2b: for every model this run has observed so far (from
        // `/models` or the `/api/tags` fallback above), ask `/api/show` for
        // its declared context window -- see
        // `fetch_ollama_show_context_length`'s doc for exactly which field
        // and why. One request per model, all best-effort: a model this
        // step can't reach keeps whatever `hints` it already had (`None`,
        // falling through to the dialect floor -- never worse than before
        // this step existed).
        if self.profile.id == "ollama" {
            let names: Vec<String> = observed.keys().cloned().collect();
            for name in names {
                if let Some(context_length) = self.fetch_ollama_show_context_length(&name).await {
                    observed.entry(name).or_default().max_context_tokens = Some(context_length);
                }
            }
        }

        if self.profile.id == "llama_cpp_server" {
            if let Some(props) = self.fetch_llama_cpp_props().await {
                let n_ctx = props.default_generation_settings.and_then(|s| s.n_ctx);
                let template_missing = props
                    .chat_template
                    .as_deref()
                    .unwrap_or("")
                    .trim()
                    .is_empty();
                for hints in observed.values_mut() {
                    if let Some(n_ctx) = n_ctx {
                        hints.max_context_tokens = Some(n_ctx);
                    }
                    if template_missing {
                        hints.reliability_downgrade = true;
                    }
                }
            }
        }

        let degraded = observed.is_empty();
        if degraded {
            tracing::warn!(
                profile = %self.profile.id,
                "capability discovery observed no models; falling back to metadata-derived capabilities"
            );
        }

        for key in self.overrides.keys() {
            observed.entry(key.clone()).or_default();
        }

        let mut capabilities = BTreeMap::new();
        for (id, hints) in &observed {
            let model_id = ModelId::new(id.clone());
            let mut dialect_defaults = self.profile.dialect_defaults();
            if let Some(max_context_tokens) = hints.max_context_tokens {
                dialect_defaults.max_context_tokens = max_context_tokens;
            }
            if hints.reliability_downgrade {
                dialect_defaults.reliability_tier = ReliabilityTier::Unknown;
            }
            let caps = build_capabilities(CapabilityInputs {
                dialect_defaults,
                metadata: self.metadata.get(&model_id),
                overrides: self.overrides.get(id),
            });
            capabilities.insert(model_id, caps);
        }

        DiscoveryResult {
            capabilities,
            degraded,
        }
    }

    /// [`CapabilityProbe::discover_result`], discarding the `degraded` flag.
    /// Always `Ok` — discovery is never a hard dependency (module
    /// boundary rule); see the module-level doc.
    pub async fn discover(&self) -> Result<BTreeMap<ModelId, Capabilities>, BackendError> {
        Ok(self.discover_result().await.capabilities)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_base_trims_exactly_one_trailing_slash() {
        let base: Url = "http://localhost:11434/v1/".parse().unwrap();
        assert_eq!(
            join_base(&base, "/models").as_str(),
            "http://localhost:11434/v1/models"
        );
        let base: Url = "http://localhost:11434/v1".parse().unwrap();
        assert_eq!(
            join_base(&base, "/models").as_str(),
            "http://localhost:11434/v1/models"
        );
    }

    #[test]
    fn join_origin_discards_the_path() {
        let base: Url = "http://localhost:11434/v1".parse().unwrap();
        assert_eq!(
            join_origin(&base, "/api/tags").as_str(),
            "http://localhost:11434/api/tags"
        );
    }
}
