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
    #[error("missing API key: api_key must not be empty or whitespace-only")]
    MissingApiKey,
    /// WI-017: `ModelMetadataStore::load` failed to parse a metadata file at
    /// `path` (syntactically invalid TOML, or a shape that does not match
    /// `ModelMetadata`). A missing file is never this variant — `load`
    /// treats "file does not exist" as `Ok(ModelMetadataStore::empty())`.
    #[error("failed to load model metadata from {path}: {detail}")]
    Metadata { path: String, detail: String },
}

/// Raw wire shape for [`AnthropicConfig`]. Exists so deserialization routes
/// through `TryFrom`, which runs [`AnthropicConfig::validate`] as part of
/// deserialization itself rather than deferring it to first use.
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
/// is WI-021). Deserializing this type runs [`AnthropicConfig::validate`]: a
/// value that fails validation fails deserialization, so it is never
/// possible to construct an `AnthropicConfig` carrying an empty key.
///
/// conway does not inspect the *shape* of an API key. Any non-empty key is
/// passed through to the configured `base_url` as-is, which is what lets an
/// Anthropic-compatible third-party endpoint (a coding-plan subscription, a
/// self-hosted shim) work without conway adjudicating whether the
/// credential looks legitimate. An unusable key surfaces as the provider's
/// own auth error, which is more accurate than any guess conway could make
/// from a prefix.
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
    /// Rejects only an empty/whitespace-only `api_key`. Key *shape* is never
    /// inspected: a missing credential is a configuration mistake conway can
    /// name precisely, while an unrecognized key format is the provider's
    /// call to make, not conway's.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.api_key.expose_secret().trim().is_empty() {
            return Err(ConfigError::MissingApiKey);
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

    /// conway does not police key shape. A subscription-style token is a
    /// valid key as far as config validation is concerned -- whether it
    /// works is the provider's answer to give, not conway's. This is what
    /// lets a Kimi coding-plan key (or any Anthropic-compatible endpoint's
    /// credential) configure without special-casing.
    #[test]
    fn subscription_style_keys_are_accepted_without_inspection() {
        for key in [
            "sk-ant-oat01-some-subscription-token",
            "sk-ant-api03-a-standard-key",
            "kimi-coding-plan-key",
        ] {
            let config = AnthropicConfig {
                api_key: SecretString::new(key),
                base_url: default_anthropic_base(),
                anthropic_version: default_anthropic_version(),
                timeout: None,
                models: BTreeMap::new(),
            };
            assert!(
                config.validate().is_ok(),
                "key shape must not be inspected: {key}"
            );
        }
    }

    #[test]
    fn empty_or_whitespace_api_key_is_still_rejected() {
        for key in ["", "   ", "\t\n"] {
            let config = AnthropicConfig {
                api_key: SecretString::new(key),
                base_url: default_anthropic_base(),
                anthropic_version: default_anthropic_version(),
                timeout: None,
                models: BTreeMap::new(),
            };
            assert!(
                matches!(config.validate(), Err(ConfigError::MissingApiKey)),
                "a missing key is a config mistake conway should name: {key:?}"
            );
        }
    }
}
