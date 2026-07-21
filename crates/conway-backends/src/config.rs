//! Configuration types for the `anthropic` and `openai-compat` adapters.
//!
//! Types only — file discovery, path resolution, and env-var expansion live
//! in the `conway` facade (architecture §"Module: conway-backends" scope).
//! `AnthropicConfig` and `OpenAiCompatConfig` are the concrete,
//! adapter-specific configuration each `Backend::new` constructor consumes;
//! they are distinct from `conway_core::routing::BackendConfig`, the
//! generic/declarative backend entry the facade loads from
//! `RoutingConfig`-adjacent files before dispatching to one of these two
//! concrete shapes.
//!
//! `ModelOverrides` is re-exported from `conway_core::routing` rather than
//! redefined here, so a single type is shared between the generic
//! declarative config (`conway_core::routing::BackendConfig::models`) and
//! these adapter-specific configs — avoiding two structurally-different
//! `ModelOverrides` types with the same name in the workspace.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use conway_core::ids::BackendId;
use serde::{Deserialize, Deserializer};
use url::Url;

pub use conway_core::routing::ModelOverrides;

fn default_anthropic_base() -> Url {
    Url::parse("https://api.anthropic.com").expect("default Anthropic base URL must be valid")
}

fn default_anthropic_version() -> String {
    "2023-06-01".to_string()
}

/// Default Anthropic request timeout, applied by callers when
/// `AnthropicConfig::timeout` is `None`.
pub const DEFAULT_ANTHROPIC_TIMEOUT: Duration = Duration::from_secs(600);

/// A secret value (an API key) whose `Debug` output never reveals the
/// underlying string. Deliberately has no `Serialize` impl: a config
/// round-trip must never re-emit the secret in plaintext (log lines, event
/// payloads, or persisted state).
#[derive(Clone)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The underlying secret value. Named to make call sites grep-able and
    /// deliberate — this is the only way to read the secret back out.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl<'de> Deserialize<'de> for SecretString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(SecretString)
    }
}

/// Errors produced while parsing/validating an adapter config.
///
/// `#[non_exhaustive]` because later work items (WI-017's
/// `ModelMetadataStore::load`) add variants (`Metadata { .. }`) to this same
/// enum without that being a breaking change for this work item's callers.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// C-02 / GP-09: Anthropic subscription OAuth tokens are contractually
    /// prohibited and technically blocked. Rejected at config-parse time,
    /// not at first request.
    #[error(
        "Anthropic subscription OAuth tokens (sk-ant-oat*) are not supported: subscription OAuth tokens are not supported by conway; use a standard API key (sk-ant-api*) from console.anthropic.com"
    )]
    SubscriptionTokenRejected,
    #[error("missing API key: api_key must not be empty or whitespace-only")]
    MissingApiKey,
    /// WI-017: `ModelMetadataStore::load` failed to parse a metadata file at
    /// `path` (syntactically invalid TOML, or a shape that does not match
    /// `ModelMetadata`). A missing file is never this variant — `load`
    /// treats "file does not exist" as `Ok(ModelMetadataStore::empty())`.
    #[error("failed to load model metadata from {path}: {detail}")]
    Metadata { path: String, detail: String },
}

/// Raw wire shape for [`AnthropicConfig`]. Exists solely so deserialization
/// can route through `TryFrom` and reject subscription-OAuth keys as part of
/// deserialization itself (C-02, GP-09), rather than only at first use.
#[derive(Debug, Clone, Deserialize)]
struct AnthropicConfigRaw {
    api_key: SecretString,
    #[serde(default = "default_anthropic_base")]
    base_url: Url,
    #[serde(default = "default_anthropic_version")]
    anthropic_version: String,
    #[serde(default, with = "humantime_serde::option")]
    timeout: Option<Duration>,
    #[serde(default)]
    models: BTreeMap<String, ModelOverrides>,
}

/// Configuration for `AnthropicBackend` (feature `anthropic`; adapter itself
/// is WI-021). Deserializing this type is how `sk-ant-oat*` rejection at
/// config-parse time (C-02, GP-09) is enforced: a value that fails
/// [`AnthropicConfig::validate`] fails deserialization, it is never
/// possible to construct an `AnthropicConfig` carrying a rejected key.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "AnthropicConfigRaw")]
pub struct AnthropicConfig {
    pub api_key: SecretString,
    pub base_url: Url,
    pub anthropic_version: String,
    pub timeout: Option<Duration>,
    pub models: BTreeMap<String, ModelOverrides>,
}

impl AnthropicConfig {
    /// Rejection rule: `api_key` trimmed; a value starting with
    /// `sk-ant-oat` is a subscription OAuth token and is rejected. An
    /// empty/whitespace-only key is rejected separately.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let trimmed = self.api_key.expose_secret().trim();
        if trimmed.is_empty() {
            return Err(ConfigError::MissingApiKey);
        }
        if trimmed.starts_with("sk-ant-oat") {
            return Err(ConfigError::SubscriptionTokenRejected);
        }
        Ok(())
    }

    /// `self.timeout`, or [`DEFAULT_ANTHROPIC_TIMEOUT`] when unset.
    pub fn effective_timeout(&self) -> Duration {
        self.timeout.unwrap_or(DEFAULT_ANTHROPIC_TIMEOUT)
    }
}

impl TryFrom<AnthropicConfigRaw> for AnthropicConfig {
    type Error = ConfigError;

    fn try_from(raw: AnthropicConfigRaw) -> Result<Self, Self::Error> {
        let config = AnthropicConfig {
            api_key: raw.api_key,
            base_url: raw.base_url,
            anthropic_version: raw.anthropic_version,
            timeout: raw.timeout,
            models: raw.models,
        };
        config.validate()?;
        Ok(config)
    }
}

/// Configuration for `OpenAiCompatBackend` (feature `openai-compat`; adapter
/// itself is WI-019/WI-022). One adapter, dialect-selected behavior.
#[derive(Debug, Clone, Deserialize)]
pub struct OpenAiCompatConfig {
    pub id: BackendId,
    pub base_url: Url,
    #[serde(default)]
    pub api_key: Option<SecretString>,
    pub dialect: Dialect,
    #[serde(default, with = "humantime_serde::option")]
    pub timeout: Option<Duration>,
    #[serde(default)]
    pub metadata_path: Option<PathBuf>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelOverrides>,
}

/// The dialect family an `OpenAiCompatBackend` instance speaks. Selects the
/// `ToolCallAccumulator` variant and known-quirk workarounds (WI-018,
/// WI-022) so one adapter covers five servers instead of five adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dialect {
    OpenAi,
    Ollama,
    VllmHermes,
    LmStudio,
    LlamaCppServer,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_string_debug_never_reveals_value() {
        let secret = SecretString::new("sk-ant-api03-super-secret");
        assert_eq!(format!("{secret:?}"), "***");
        assert_eq!(secret.expose_secret(), "sk-ant-api03-super-secret");
    }

    #[test]
    fn config_error_display_is_exact_for_subscription_token_rejection() {
        let err = ConfigError::SubscriptionTokenRejected;
        assert_eq!(
            err.to_string(),
            "Anthropic subscription OAuth tokens (sk-ant-oat*) are not supported: subscription OAuth tokens are not supported by conway; use a standard API key (sk-ant-api*) from console.anthropic.com"
        );
    }
}
