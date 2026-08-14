//! Local, network-free model metadata: context windows and capability hints
//! used to compute headroom warnings and (later, by later work's
//! `CapabilityIndex`) routing capability floors.
//!
//! `load` reads a local JSON file only. The feature-gated, stubbed refresh
//! entry point below is never called from `config::load`; `mod.rs`'s tests
//! enforce this and the absence of any network-client identifier
//! structurally.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{ConwayError, Result};

/// Local model capability metadata, keyed by `"backend/model"` (the
/// `ModelRef::to_string()` format) — matching the `chain` entries in
/// `[roles.<alias>]` exactly, so `config::merge`'s headroom-warning lookup
/// is a direct key match. ASSUMPTION: the binding spec does not fix a key
/// convention for this file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelMetadata {
    #[serde(default)]
    pub models: HashMap<String, ModelMetadataEntry>,
}

impl ModelMetadata {
    pub fn empty() -> Self {
        Self::default()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelMetadataEntry {
    pub max_context_tokens: u32,
    pub tool_calling: String,
    pub reasoning: bool,
    pub reliability_tier: String,
}

/// Reads `path` as JSON. A missing file is `Ok(ModelMetadata::empty())`,
/// not an error — missing metadata is expected (a fresh install, a backend
/// with no declared capabilities yet).
pub fn load(path: &Path) -> Result<ModelMetadata> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| ConwayError::Config {
            path: Some(path.to_path_buf()),
            message: format!("invalid model metadata JSON at {}: {e}", path.display()),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ModelMetadata::empty()),
        Err(e) => Err(ConwayError::Config {
            path: Some(path.to_path_buf()),
            message: format!("failed to read model metadata at {}: {e}", path.display()),
        }),
    }
}

/// Explicit, caller-triggered metadata refresh. Not implemented in this
/// work item (scope is config schema/discovery/merge — an HTTP
/// fetch belongs to a later item that owns the actual client). This stub
/// exists so the feature-gate, signature, and "never called from `load`"
/// criteria are satisfiable now without pulling a network-client
/// dependency into this directory, which a structural test in `mod.rs`
/// forbids naming even under a disabled cfg.
#[cfg(feature = "metadata-refresh")]
pub async fn refresh(_url: &str, _dest: &Path) -> Result<()> {
    Err(ConwayError::UnsupportedFeature {
        feature: "metadata-refresh",
        message: "model_metadata::refresh has no client implementation yet".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_missing_file_is_ok_empty() {
        let path = std::env::temp_dir().join("conway-model-metadata-missing-does-not-exist.json");
        let _ = std::fs::remove_file(&path);
        let meta = load(&path).unwrap();
        assert!(meta.models.is_empty());
    }

    #[test]
    fn load_parses_present_file() {
        let path = std::env::temp_dir().join(format!(
            "conway-model-metadata-present-{}.json",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"{"models":{"anthropic/claude-haiku-4-5":{"max_context_tokens":32768,"tool_calling":"streaming","reasoning":false,"reliability_tier":"verified"}}}"#,
        )
        .unwrap();
        let meta = load(&path).unwrap();
        assert_eq!(
            meta.models["anthropic/claude-haiku-4-5"].max_context_tokens,
            32768
        );
        let _ = std::fs::remove_file(&path);
    }
}
