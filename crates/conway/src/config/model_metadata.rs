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

use crate::error::{FacadeError, Result};

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
        Ok(text) => serde_json::from_str(&text).map_err(|e| FacadeError::Config {
            path: Some(path.to_path_buf()),
            message: format!("invalid model metadata JSON at {}: {e}", path.display()),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ModelMetadata::empty()),
        Err(e) => Err(FacadeError::Config {
            path: Some(path.to_path_buf()),
            message: format!("failed to read model metadata at {}: {e}", path.display()),
        }),
    }
}

/// Merges one model's discovered-or-operator-typed context window into the
/// `ModelMetadata` document at `path` -- the PERSIST half of the setup-time
/// "discover, or ask if discovery fails" ruling (the DISCOVER half is
/// `conway_plugin_backends::probe::discover_context_window`; the ASK half is
/// `conway-cli`'s `first_run.rs`/`tui::app::provider_manage`). Creates the
/// file, and its parent directories, if it does not exist yet -- via the
/// same tmp-then-rename `super::writer::write_atomically` every other
/// operator-config file this crate writes uses (`config::writer`'s own
/// module doc: "the single implementation... per P-14").
///
/// **A whole-document parse/mutate/reserialize, unlike `config::writer`'s
/// byte-preserving splicer for `settings.json` -- deliberately, not an
/// oversight.** `settings.json` is hand-editable, many-sectioned, and
/// carries an operator `"//"`-comment convention (`config::writer`'s own
/// module doc explains why a full reserialize is unsafe there: it would
/// alphabetize top-level keys and re-render formatting a write never
/// touched). `ModelMetadata` is the shape that SAME doc names as the
/// opposite case, using `permissions.json`'s own precedent
/// (`crate::permissions::rewrite_permission_file_removing`): "a narrow,
/// single-purpose file, so there is no unrelated key to lose." `ModelMetadata`
/// has exactly one field (`models`), and today's `ModelMetadataEntry` has
/// exactly the four fields this function reads and re-writes -- there is
/// nothing here for a reserialize to drop yet, and no comment convention
/// this file supports at all. Neither struct actually carries `#[serde(
/// deny_unknown_fields)]`, so this safety holds only as long as every field
/// either struct gains stays one this function (and its siblings) knows
/// about; an unrecognized field on an operator's hand-edited file would be
/// silently dropped on the next rewrite rather than failing loud.
///
/// Every OTHER model already recorded in the file survives untouched -- this
/// reads the whole map, replaces `key`'s own entry, and writes the whole map
/// back. The three companion fields an entry also carries
/// (`tool_calling`/`reasoning`/`reliability_tier`) are preserved from any
/// EXISTING entry for `key`, so re-running setup against an already-recorded
/// model never quietly resets a reliability tier or capability hint an
/// operator (or an earlier probe) already set; a brand-new entry gets the
/// same conservative placeholders `first_run.rs`'s own copy-pasteable
/// snippet already used before this function existed
/// (`"yes"`/`false`/`"community"`) -- never a claim of verification this
/// function has no basis for.
///
/// `Ok(true)` if a write happened, `Ok(false)` if `key` already named this
/// EXACT `max_context_tokens` (idempotent -- re-running setup against an
/// already-recorded model is a no-op, mirroring `config::writer`'s own
/// "goal state already holds" discipline for every one of ITS writers).
pub fn set_context_window(path: &Path, key: &str, window: u32) -> Result<bool> {
    let mut meta = load(path)?;
    let existing = meta.models.get(key);
    if existing.is_some_and(|e| e.max_context_tokens == window) {
        return Ok(false);
    }
    let entry = ModelMetadataEntry {
        max_context_tokens: window,
        tool_calling: existing
            .map(|e| e.tool_calling.clone())
            .unwrap_or_else(|| "yes".to_string()),
        reasoning: existing.map(|e| e.reasoning).unwrap_or(false),
        reliability_tier: existing
            .map(|e| e.reliability_tier.clone())
            .unwrap_or_else(|| "community".to_string()),
    };
    meta.models.insert(key.to_string(), entry);
    let text = serde_json::to_string_pretty(&meta).map_err(|e| FacadeError::Config {
        path: Some(path.to_path_buf()),
        message: format!(
            "failed to serialize model metadata for {}: {e}",
            path.display()
        ),
    })?;
    super::writer::write_atomically(path, &format!("{text}\n")).map_err(FacadeError::Io)?;
    Ok(true)
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
    Err(FacadeError::UnsupportedFeature {
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

    fn fresh_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "conway-model-metadata-set-context-window-{label}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn set_context_window_creates_a_missing_file_with_conservative_placeholders() {
        let path = fresh_path("create");
        let _ = std::fs::remove_file(&path);

        let changed = set_context_window(&path, "ollama/glm-5.2", 1_048_576).unwrap();
        assert!(changed);

        let meta = load(&path).unwrap();
        let entry = &meta.models["ollama/glm-5.2"];
        assert_eq!(entry.max_context_tokens, 1_048_576);
        assert_eq!(entry.tool_calling, "yes");
        assert!(!entry.reasoning);
        assert_eq!(entry.reliability_tier, "community");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn set_context_window_preserves_every_other_model_and_the_entrys_own_other_fields() {
        let path = fresh_path("merge");
        std::fs::write(
            &path,
            r#"{"models":{
                "ollama/glm-5.2":{"max_context_tokens":32768,"tool_calling":"streaming","reasoning":true,"reliability_tier":"verified"},
                "anthropic/claude-haiku-4-5":{"max_context_tokens":200000,"tool_calling":"yes","reasoning":false,"reliability_tier":"verified"}
            }}"#,
        )
        .unwrap();

        let changed = set_context_window(&path, "ollama/glm-5.2", 1_048_576).unwrap();
        assert!(changed);

        let meta = load(&path).unwrap();
        // The re-discovered entry's window moved; its own other fields (set
        // by something else, e.g. an operator's hand-edit) survived.
        let updated = &meta.models["ollama/glm-5.2"];
        assert_eq!(updated.max_context_tokens, 1_048_576);
        assert_eq!(updated.tool_calling, "streaming");
        assert!(updated.reasoning);
        assert_eq!(updated.reliability_tier, "verified");
        // The unrelated model is untouched.
        assert_eq!(
            meta.models["anthropic/claude-haiku-4-5"].max_context_tokens,
            200_000
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn set_context_window_is_a_no_op_when_the_exact_value_is_already_recorded() {
        let path = fresh_path("noop");
        set_context_window(&path, "ollama/glm-5.2", 1_048_576).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let changed = set_context_window(&path, "ollama/glm-5.2", 1_048_576).unwrap();
        assert!(!changed);

        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            before, after,
            "a no-op write must not touch the file at all"
        );

        let _ = std::fs::remove_file(&path);
    }
}
