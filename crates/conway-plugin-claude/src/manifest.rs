//! `.claude-plugin/plugin.json` -- the one file this crate reads for
//! identity/description only, never for behavior (question 2 of the item's
//! spec: "no counterpart" -- there is no `conway::plugin::Plugin` this
//! file's own fields become).
//!
//! **Parsed permissively, by design, and NOT via a `#[serde(deny_unknown_
//! fields)]` struct.** Question 2 asked whether to relax conway's own
//! strict frontmatter parsers for foreign input, or pre-translate before
//! the parser ever sees it. This crate takes the second path for every
//! file it reads (this one included): it reads a `serde_json::Value` and
//! pulls the handful of fields it actually uses, so a `plugin.json` key
//! Claude Code adds tomorrow (`author`, `keywords`, whatever) is simply
//! never looked at -- never a hard parse failure. `crates/conway/src/
//! skills.rs`/`agents.rs`'s own `deny_unknown_fields` structs are UNTOUCHED
//! by this decision (the spec is explicit: "do not relax it for conway's
//! own `.conway/` files" -- that strictness catches an OPERATOR's own typo
//! in a file conway itself defines the shape of; a Claude Code plugin
//! author's file is not that).

use std::path::Path;

use crate::error::ClaudeCompatError;
use crate::fsutil::read_bounded;

/// The handful of `.claude-plugin/plugin.json` fields this crate reads --
/// see this module's own doc for why this is NOT a `Deserialize` struct
/// with `deny_unknown_fields`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaudePluginManifest {
    pub name: Option<String>,
    pub version: Option<String>,
    pub description: Option<String>,
}

/// Reads `<dir>/.claude-plugin/plugin.json` if it exists. `Ok(None)` when
/// the file is simply absent -- a directory conway is asked to read as a
/// Claude Code plugin without this file is not an error (question 2's
/// "permissive" posture extends to the manifest's own presence, not only
/// its fields): [`crate::discover`] falls back to the directory's own name
/// for identity in that case.
pub(crate) fn read_manifest(dir: &Path) -> Result<Option<ClaudePluginManifest>, ClaudeCompatError> {
    let path = dir.join(".claude-plugin").join("plugin.json");
    if !path.is_file() {
        return Ok(None);
    }
    let text = read_bounded(&path)?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|source| ClaudeCompatError::MalformedJson {
            path: path.clone(),
            source,
        })?;
    let field = |name: &str| value.get(name).and_then(|v| v.as_str()).map(str::to_string);
    Ok(Some(ClaudePluginManifest {
        name: field("name"),
        version: field("version"),
        description: field("description"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_three_fields_this_crate_uses() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".claude-plugin")).unwrap();
        std::fs::write(
            dir.path().join(".claude-plugin").join("plugin.json"),
            r#"{"name":"acme-tools","version":"1.2.3","description":"acme's toolkit","author":{"name":"acme"},"keywords":["a","b"]}"#,
        )
        .unwrap();

        let manifest = read_manifest(dir.path()).unwrap().unwrap();
        assert_eq!(manifest.name.as_deref(), Some("acme-tools"));
        assert_eq!(manifest.version.as_deref(), Some("1.2.3"));
        assert_eq!(manifest.description.as_deref(), Some("acme's toolkit"));
    }

    #[test]
    fn an_absent_manifest_is_none_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(read_manifest(dir.path()).unwrap().is_none());
    }

    #[test]
    fn unknown_fields_never_fail_the_parse() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".claude-plugin")).unwrap();
        std::fs::write(
            dir.path().join(".claude-plugin").join("plugin.json"),
            r#"{"name":"x","totally-unknown-field":{"nested":true}}"#,
        )
        .unwrap();
        let manifest = read_manifest(dir.path()).unwrap().unwrap();
        assert_eq!(manifest.name.as_deref(), Some("x"));
    }

    #[test]
    fn malformed_json_is_a_typed_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".claude-plugin")).unwrap();
        std::fs::write(
            dir.path().join(".claude-plugin").join("plugin.json"),
            "not json at all {{{",
        )
        .unwrap();
        let err = read_manifest(dir.path()).unwrap_err();
        assert!(
            matches!(err, ClaudeCompatError::MalformedJson { .. }),
            "{err:?}"
        );
    }
}
