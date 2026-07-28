//! Per-model metadata: the local-file-backed store of per-`(backend,
//! model)` facts (`reliability_tier`, `tool_calling`, `quantization`, …)
//! that [`crate::capabilities::build_capabilities`] layers under a config's
//! `ModelOverrides` and above a dialect's baseline defaults.
//!
//! Deliberately never a hard network dependency: `ModelMetadataStore::load`
//! reads exactly one local file (or none — a missing path is not an error).
//! If models.dev-derived data is ever added, it is a build-time-generated
//! file merged at the same position as [`ModelMetadataStore::defaults`],
//! never a runtime fetch.

use std::collections::BTreeMap;
use std::path::Path;

use conway_core::capabilities::{ReliabilityTier, StructuredOutput, ToolCallSupport};
use conway_core::ids::ModelId;
use serde::Deserialize;

use crate::config::ConfigError;

/// One model's declared metadata, as read from a `[[model]]` table in a
/// metadata file. Every field but `id` is optional: an absent field means
/// "no opinion", deferring to `ModelOverrides` or the dialect default in
/// [`crate::capabilities::build_capabilities`].
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct ModelMetadata {
    pub id: String,
    #[serde(default)]
    pub max_context_tokens: Option<u32>,
    #[serde(default)]
    pub tool_calling: Option<ToolCallSupportSpec>,
    #[serde(default)]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default)]
    pub structured_output: Option<StructuredOutputSpec>,
    #[serde(default)]
    pub reasoning: Option<bool>,
    #[serde(default)]
    pub reliability_tier: Option<ReliabilityTier>,
    /// e.g. `"Q4_K_M"`. Informational, and — only when `reliability_tier`
    /// itself is absent — a fallback tier heuristic; see
    /// [`quantization_tier_hint`].
    #[serde(default)]
    pub quantization: Option<String>,
}

/// Wire vocabulary for [`ModelMetadata::tool_calling`]: `"none"` |
/// `"non_streaming"` | `"streaming"` | `"streaming_validated"`. A distinct
/// type from [`ToolCallSupport`] because the wire format for `Streaming`
/// splits into two named variants rather than the `{validated: bool}` shape
/// `ToolCallSupport` uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallSupportSpec {
    None,
    NonStreaming,
    Streaming,
    StreamingValidated,
}

impl ToolCallSupportSpec {
    pub fn to_capability(self) -> ToolCallSupport {
        match self {
            ToolCallSupportSpec::None => ToolCallSupport::None,
            ToolCallSupportSpec::NonStreaming => ToolCallSupport::NonStreamingOnly,
            ToolCallSupportSpec::Streaming => ToolCallSupport::Streaming { validated: false },
            ToolCallSupportSpec::StreamingValidated => {
                ToolCallSupport::Streaming { validated: true }
            }
        }
    }
}

/// Wire vocabulary for [`ModelMetadata::structured_output`]: `"none"` |
/// `"json_schema"` | `"grammar"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredOutputSpec {
    None,
    JsonSchema,
    Grammar,
}

impl StructuredOutputSpec {
    pub fn to_capability(self) -> StructuredOutput {
        match self {
            StructuredOutputSpec::None => StructuredOutput::None,
            StructuredOutputSpec::JsonSchema => StructuredOutput::JsonSchema,
            StructuredOutputSpec::Grammar => StructuredOutput::Grammar,
        }
    }
}

/// Quantization-string → tier heuristic, applied by
/// [`crate::capabilities::build_capabilities`] only when
/// [`ModelMetadata::reliability_tier`] is absent. Never promotes to
/// `Verified` — `Verified` is only ever set explicitly (research-backends:
/// sub-4-bit quants produce malformed tool-call arguments, so the heuristic
/// only ever lowers confidence).
///
/// - `Q2*`, `Q3*`, `IQ*` → `Unknown`
/// - `Q4*`, `Q5*`, `Q6*`, `Q8*`, `F16*`, `BF16*` → `Community`
/// - anything else → `None` (no opinion; caller falls through to the
///   dialect default)
pub(crate) fn quantization_tier_hint(quantization: &str) -> Option<ReliabilityTier> {
    let upper = quantization.to_ascii_uppercase();
    if upper.starts_with("Q2") || upper.starts_with("Q3") || upper.starts_with("IQ") {
        Some(ReliabilityTier::Unknown)
    } else if upper.starts_with("Q4")
        || upper.starts_with("Q5")
        || upper.starts_with("Q6")
        || upper.starts_with("Q8")
        || upper.starts_with("F16")
        || upper.starts_with("BF16")
    {
        Some(ReliabilityTier::Community)
    } else {
        None
    }
}

/// Normalizes a raw model id for the fallback lookup pass in
/// [`ModelMetadataStore::get`]: lowercased, `:` and `/` collapsed to `-`,
/// a trailing `-latest` stripped.
fn normalize_model_id(id: &str) -> String {
    let lowered = id.to_lowercase().replace([':', '/'], "-");
    match lowered.strip_suffix("-latest") {
        Some(stripped) => stripped.to_string(),
        None => lowered,
    }
}

/// Wire shape of a metadata file: an array-of-tables, `[[model]]`.
#[derive(Debug, Deserialize)]
struct ModelMetadataFile {
    #[serde(default)]
    model: Vec<ModelMetadata>,
}

/// Bundled default metadata for widely-used models, embedded at compile
/// time (never fetched over the network at runtime). Loaded first, so a
/// `metadata_path` file or config-level `ModelOverrides` can always take
/// precedence over these values.
const DEFAULTS: &str = r#"
[[model]]
id = "claude-sonnet-4-6"
reliability_tier = "verified"
tool_calling = "streaming_validated"
parallel_tool_calls = true

[[model]]
id = "claude-haiku-4-5"
reliability_tier = "verified"
tool_calling = "streaming_validated"
parallel_tool_calls = true

[[model]]
id = "gpt-4.1"
reliability_tier = "verified"
tool_calling = "streaming_validated"
parallel_tool_calls = true

[[model]]
id = "gpt-5"
reliability_tier = "verified"
tool_calling = "streaming_validated"
parallel_tool_calls = true

[[model]]
id = "qwen3-coder-30b"
reliability_tier = "community"
tool_calling = "non_streaming"

[[model]]
id = "qwen3-coder-80b"
reliability_tier = "community"
tool_calling = "non_streaming"

[[model]]
id = "llama3.1-8b"
reliability_tier = "community"
tool_calling = "non_streaming"

[[model]]
id = "glm-5.2"
reliability_tier = "community"
tool_calling = "non_streaming"

# Kimi K3 coding-plan models, served over an Anthropic-compatible endpoint
# (see `docs/crates/conway-backends.md`). Two context variants ship as
# separate ids because the window is selected by the model id itself, not by
# a parameter. The `[1m]` suffix is literal — it is part of the id the
# provider expects, not TOML syntax, which is why the id is quoted.
[[model]]
id = "k3-256k"
max_context_tokens = 262144
reliability_tier = "community"
tool_calling = "streaming_validated"
reasoning = true

[[model]]
id = "k3[1m]"
max_context_tokens = 1048576
reliability_tier = "community"
tool_calling = "streaming_validated"
reasoning = true
"#;

/// A loaded set of [`ModelMetadata`] entries, keyed by the raw `id` from
/// the file that produced them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelMetadataStore {
    entries: BTreeMap<String, ModelMetadata>,
}

impl ModelMetadataStore {
    /// A store with no entries. What [`ModelMetadataStore::load`] returns
    /// for a nonexistent path — not an error.
    pub fn empty() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    /// The bundled compile-time defaults (see [`DEFAULTS`]).
    pub fn defaults() -> Self {
        Self::parse(DEFAULTS).expect("bundled DEFAULTS model metadata must parse")
    }

    /// Reads and parses `path` as a `[[model]]` array-of-tables TOML file.
    ///
    /// A nonexistent path is `Ok(Self::empty())`, not an error — a
    /// `metadata_path` is always optional. Any other I/O failure, or a
    /// syntactically/structurally invalid file, is
    /// `Err(ConfigError::Metadata { .. })`.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::empty());
            }
            Err(err) => {
                return Err(ConfigError::Metadata {
                    path: path.display().to_string(),
                    detail: err.to_string(),
                });
            }
        };
        Self::parse(&content).map_err(|detail| ConfigError::Metadata {
            path: path.display().to_string(),
            detail,
        })
    }

    fn parse(content: &str) -> Result<Self, String> {
        let file: ModelMetadataFile = toml::from_str(content).map_err(|err| err.to_string())?;
        let mut entries = BTreeMap::new();
        for entry in file.model {
            entries.insert(entry.id.clone(), entry);
        }
        Ok(Self { entries })
    }

    /// Merges `other` into `self`; `other` wins on key collision. Used to
    /// layer `DEFAULTS` under a `metadata_path` file's entries.
    pub fn merge(self, other: Self) -> Self {
        let mut entries = self.entries;
        entries.extend(other.entries);
        Self { entries }
    }

    /// Looks up `model`, trying (in order): the exact id as given, then the
    /// [normalized](normalize_model_id) id, then `None`.
    pub fn get(&self, model: &ModelId) -> Option<&ModelMetadata> {
        let raw = model.as_str();
        if let Some(entry) = self.entries.get(raw) {
            return Some(entry);
        }
        let normalized = normalize_model_id(raw);
        self.entries.get(&normalized)
    }

    /// Number of entries in the store.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_model_id_lowercases_collapses_separators_and_strips_latest() {
        assert_eq!(normalize_model_id("Qwen3-Coder:30b"), "qwen3-coder-30b");
        assert_eq!(normalize_model_id("foo/bar:baz"), "foo-bar-baz");
        assert_eq!(normalize_model_id("llama3.1-8b-latest"), "llama3.1-8b");
        assert_eq!(normalize_model_id("already-normal"), "already-normal");
    }

    #[test]
    fn defaults_parse_and_cover_documented_minimum_models() {
        let store = ModelMetadataStore::defaults();
        for id in [
            "claude-sonnet-4-6",
            "claude-haiku-4-5",
            "gpt-4.1",
            "gpt-5",
            "qwen3-coder-30b",
            "qwen3-coder-80b",
            "llama3.1-8b",
            "glm-5.2",
            "k3-256k",
            "k3[1m]",
        ] {
            assert!(
                store.get(&ModelId::new(id)).is_some(),
                "DEFAULTS missing entry for {id}"
            );
        }
    }

    /// The 1M-context Kimi variant's id contains literal `[`/`]`. TOML
    /// would read those as table syntax if the id were ever unquoted, and a
    /// mangled id silently costs the model its metadata (falling back to a
    /// wrong context window). Pin the exact string and its window.
    #[test]
    fn kimi_1m_model_id_survives_toml_parsing_with_its_brackets_intact() {
        let store = ModelMetadataStore::defaults();
        let entry = store
            .get(&ModelId::new("k3[1m]"))
            .expect("k3[1m] must parse with brackets intact, not as a TOML table");
        assert_eq!(entry.id, "k3[1m]");
        assert_eq!(entry.max_context_tokens, Some(1_048_576));
    }

    /// The two Kimi variants differ only by context window, which is the
    /// whole reason they are separate ids -- if these ever collapse to the
    /// same number, choosing between them becomes meaningless.
    #[test]
    fn kimi_context_variants_declare_distinct_windows() {
        let store = ModelMetadataStore::defaults();
        let k256 = store.get(&ModelId::new("k3-256k")).expect("k3-256k");
        let k1m = store.get(&ModelId::new("k3[1m]")).expect("k3[1m]");
        assert_eq!(k256.max_context_tokens, Some(262_144));
        assert_eq!(k1m.max_context_tokens, Some(1_048_576));
        assert!(
            k1m.max_context_tokens > k256.max_context_tokens,
            "the 1M variant must declare the larger window"
        );
    }

    #[test]
    fn quantization_hint_never_promotes_to_verified() {
        assert_eq!(
            quantization_tier_hint("Q3_K_S"),
            Some(ReliabilityTier::Unknown)
        );
        assert_eq!(
            quantization_tier_hint("IQ2_XS"),
            Some(ReliabilityTier::Unknown)
        );
        assert_eq!(
            quantization_tier_hint("Q4_K_M"),
            Some(ReliabilityTier::Community)
        );
        assert_eq!(
            quantization_tier_hint("Q8_0"),
            Some(ReliabilityTier::Community)
        );
        assert_eq!(
            quantization_tier_hint("F16"),
            Some(ReliabilityTier::Community)
        );
        assert_eq!(quantization_tier_hint("unknown-format"), None);
    }

    #[test]
    fn merge_prefers_other_on_key_collision() {
        let base = ModelMetadataStore::parse(
            r#"
            [[model]]
            id = "m1"
            max_context_tokens = 1000
            "#,
        )
        .unwrap();
        let overlay = ModelMetadataStore::parse(
            r#"
            [[model]]
            id = "m1"
            max_context_tokens = 9999
            "#,
        )
        .unwrap();
        let merged = base.merge(overlay);
        assert_eq!(
            merged.get(&ModelId::new("m1")).unwrap().max_context_tokens,
            Some(9999)
        );
    }
}
