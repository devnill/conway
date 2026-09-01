//! `skills/<name>/SKILL.md` -- board item `01M1DG5TTF6NHW2RXJRZ8ZPE7K`
//! reverses `crate`'s own earlier "skills and agents are out of scope"
//! ruling (see [`crate`]'s own top doc, "Question 3", amended in place
//! rather than deleted).
//!
//! **The operator's own words, quoted rather than paraphrased**: "we should
//! register the skills as slash commands in the same manner that claude
//! code does; they are critical for using plugins." Real Claude Code names
//! a plugin skill `/<plugin-id>:<skill-name>` (a colon separator); conway's
//! own [`conway_core::event_name::EVENT_NAMESPACE_SEPARATOR`] is `.`, used
//! for every OTHER plugin-declared command surface
//! ([`crate::commands`]'s own bare-name + host-namespacing scheme) and
//! deeply load-bearing (it is what makes a plugin command's full name
//! STRUCTURALLY unable to collide with a built-in). **Decision, recorded
//! here rather than re-litigated per caller:** a translated skill is
//! registered through the IDENTICAL bare-name + host-namespacing scheme
//! `crate::commands` already established (this module reuses
//! [`crate::commands::ClaudeCommand`] itself, via
//! [`crate::commands::ClaudeCommand::new`]) -- so `/ideate.refine` is the
//! full name that actually resolves. `conway_cli::tui::commands::parse`
//! separately accepts a leading `:` as an input ALIAS for `.` (this item's
//! own sibling change there), so an operator who literally types
//! `/ideate:refine`, the way Claude Code itself would have them type it,
//! reaches the SAME registration -- without teaching
//! `EVENT_NAMESPACE_SEPARATOR` a second separator, and without touching
//! `crate::commands`' own already-shipped, already-tested behavior.
//!
//! **A skill's own IDENTITY is its directory, not a frontmatter `name`
//! key.** Every real `skills/<name>/SKILL.md` this module has been checked
//! against (`ideate` 3.2.2's own `init`/`refine`/`execute`/`review`/
//! `autopilot`) carries NO `name` key at all -- Claude Code's own
//! convention treats the containing directory as the skill's canonical
//! name. This module never reads a `name` frontmatter key for that reason:
//! `bare_name` is always the directory's own name, unconditionally (if a
//! `name` key is present anyway, it is simply an unrecognized key, named in
//! [`crate::UnsupportedItem`] like any other, never consulted).
//!
//! **Recognized frontmatter: `description` only** -- mirrors
//! `crate::commands`' own posture exactly (question 2: permissive, not
//! `deny_unknown_fields`). Every other key seen on a real `SKILL.md`
//! (`user-invocable`, `argument-hint`) is named, not silently dropped;
//! `allowed-tools`, were a skill ever to declare it, gets the SAME
//! PERMISSION-shaped reason `crate::commands::frontmatter_key_reason`
//! already gives it for a `commands/*.md` file, for the identical
//! reason -- an operator who authored a tool restriction and had it
//! silently ignored has a permission surprise, not a fidelity one.
//!
//! **Cross-references survive by construction, not by rewriting prose.**
//! Every real skill this module has been checked against tells the reader,
//! in its own body text, to read a SIBLING file "relative to the plugin
//! root" (`skills/shared/human-presentation.md`, verbatim, in `ideate`'s
//! own five top-level skills). Rewriting that prose would be fragile
//! (matching arbitrary English against a moving target) and is not
//! attempted. Instead, every translated skill's own submitted prompt is
//! PREFIXED with one line naming this plugin's own absolute root directory
//! (see `plugin_root_note`, this module's own private helper) -- so a
//! model reading the resulting prompt has the one fact it needs (the
//! absolute path "relative to the plugin root" is relative TO) to resolve
//! the reference itself with its own Read tool, exactly the way a real
//! Claude Code session's own runtime already tells the model where a
//! loaded skill's file lives.
//! `crate::hooks::PLUGIN_ROOT_TOKEN` (the literal `${CLAUDE_PLUGIN_ROOT}`
//! substitution `hooks.json`/`.mcp.json` commands use) is NOT reused here
//! on purpose: no real `SKILL.md` body this module has been checked against
//! contains that literal token (they use prose instead) -- see
//! `crate::agents`' own doc for the SIBLING file kind that DOES use the
//! literal token in its own body text, and substitutes it directly instead.

use std::path::Path;
use std::sync::Arc;

use conway_core::ports::Command;

use crate::commands::ClaudeCommand;
use crate::frontmatter::{normalize_body, split_frontmatter};
use crate::fsutil::read_bounded;
use crate::unsupported::UnsupportedItem;

/// One `skills/<name>/SKILL.md` directory, after translation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillTranslation {
    /// `skills/<name>`, relative to the plugin directory -- matches
    /// `crate::unsupported::UnsupportedItem::name`'s own existing
    /// convention for a `skills/<name>` finding.
    pub relative_path: String,
    /// This skill's bare name -- always the directory's own name (this
    /// module's own top doc: identity is the directory, never a
    /// frontmatter key).
    pub bare_name: String,
    pub description: Option<String>,
    pub outcome: SkillMapOutcome,
}

/// Whether a `skills/<name>/SKILL.md` became a real, invokable
/// [`conway_core::ports::Command`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillMapOutcome {
    /// `prompt` is the (frontmatter-stripped, normalized, plugin-root-noted
    /// -- see `plugin_root_note`) body [`SkillTranslation::command`]
    /// submits verbatim.
    Ready { prompt: String },
    /// This directory did not become a command -- `reason` says why (no
    /// `SKILL.md`, unreadable file, malformed/unterminated frontmatter, an
    /// empty body, or a directory name that could never be typed). Always
    /// ALSO named in [`crate::UnsupportedItem`] by `read_skills` -- never
    /// silently dropped.
    Refused { reason: String },
}

impl SkillTranslation {
    /// `Some` only for [`SkillMapOutcome::Ready`] -- mirrors
    /// `crate::commands::CommandTranslation::command`'s own shape exactly,
    /// reusing [`ClaudeCommand`] itself (via
    /// [`ClaudeCommand::new`]) rather than a second, near-identical
    /// `Command` impl.
    pub fn command(&self) -> Option<Arc<dyn Command>> {
        let SkillMapOutcome::Ready { prompt } = &self.outcome else {
            return None;
        };
        let summary = self
            .description
            .clone()
            .unwrap_or_else(|| format!("runs {}'s own skill prompt", self.relative_path));
        Some(Arc::new(ClaudeCommand::new(
            self.bare_name.clone(),
            summary,
            prompt.clone(),
        )))
    }
}

/// Reads `<dir>/skills/<name>/SKILL.md` (one level of subdirectories; a
/// subdirectory with no `SKILL.md` directly inside it is not a skill
/// candidate at all, mirroring `crate::unsupported::scan_skills`'s own
/// former rule), translating every candidate and appending a named
/// [`UnsupportedItem`] for every frontmatter key this module does not
/// honor and every directory that does not become a command. `vec![]` when
/// the `skills` subdirectory is absent.
///
/// Directories are processed in SORTED name order -- deterministic, the
/// same discipline `crate::commands::read_commands` already establishes.
pub(crate) fn read_skills(
    dir: &Path,
    unsupported: &mut Vec<UnsupportedItem>,
) -> Vec<SkillTranslation> {
    let skills_dir = dir.join("skills");
    let Ok(entries) = std::fs::read_dir(&skills_dir) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        if entry.path().join("SKILL.md").is_file() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();

    let mut translations = Vec::with_capacity(names.len());
    for name in names {
        translations.push(translate_one(dir, &skills_dir, &name, unsupported));
    }
    translations
}

fn translate_one(
    plugin_root: &Path,
    skills_dir: &Path,
    bare_name: &str,
    unsupported: &mut Vec<UnsupportedItem>,
) -> SkillTranslation {
    let relative_path = format!("skills/{bare_name}");

    macro_rules! refuse {
        ($reason:expr) => {{
            let reason: String = $reason;
            unsupported.push(UnsupportedItem::skill(relative_path.clone(), reason.clone()));
            return SkillTranslation {
                relative_path,
                bare_name: bare_name.to_string(),
                description: None,
                outcome: SkillMapOutcome::Refused { reason },
            };
        }};
    }

    if bare_name.is_empty() || bare_name.chars().any(char::is_whitespace) {
        refuse!(format!(
            "this skill's directory-derived name {bare_name:?} is empty or contains \
             whitespace -- `CommandRegistry::build` would reject it outright and fail the \
             whole plugin's registration, so it is refused here instead, degrading only this \
             one skill"
        ));
    }

    let skill_md = skills_dir.join(bare_name).join("SKILL.md");
    let content = match read_bounded(&skill_md) {
        Ok(content) => content,
        Err(err) => refuse!(format!("could not read this skill's SKILL.md: {err}")),
    };

    let (frontmatter_src, body) = match split_frontmatter(&content) {
        Ok(parts) => parts,
        Err(reason) => refuse!(reason.to_string()),
    };

    let (description, other_keys) = match frontmatter_src {
        None => (None, Vec::new()),
        Some(src) => match parse_frontmatter(src) {
            Ok(parsed) => parsed,
            Err(err) => refuse!(format!("invalid YAML frontmatter: {err}")),
        },
    };

    for key in &other_keys {
        unsupported.push(UnsupportedItem::skill_frontmatter_key(
            &relative_path,
            key,
            frontmatter_key_reason(key),
        ));
    }

    let normalized = normalize_body(body);
    if normalized.is_empty() {
        refuse!("this skill's SKILL.md body is empty -- nothing to submit".to_string());
    }
    let prompt = format!("{}\n\n{normalized}", plugin_root_note(plugin_root));

    SkillTranslation {
        relative_path,
        bare_name: bare_name.to_string(),
        description,
        outcome: SkillMapOutcome::Ready { prompt },
    }
}

/// The one line prepended to every translated skill's own submitted
/// prompt -- see this module's own top doc, "Cross-references survive by
/// construction". Deliberately plain, unambiguous prose (not a token a
/// downstream step must further substitute): the model reading the
/// resulting prompt has the plugin's own absolute root directory available
/// to it directly.
fn plugin_root_note(plugin_root: &Path) -> String {
    format!(
        "[conway: this skill's own plugin root directory is `{}`. Any reference in the \
         text below described as \"relative to the plugin root\" resolves against that \
         absolute path.]",
        plugin_root.display()
    )
}

/// The reason named for a frontmatter key this module does not honor --
/// identical shape to `crate::commands::frontmatter_key_reason`
/// (duplicated rather than shared: the two functions read from a different
/// closed set of recognized keys, `description` aside, and keeping each
/// self-contained avoids a shared function whose behavior secretly depends
/// on which caller invoked it).
fn frontmatter_key_reason(key: &str) -> String {
    if key == "allowed-tools" {
        "an operator who wrote this expecting Claude Code's own tool restriction gets none here \
         -- conway's translated skill imposes no tool restriction of any kind on the turn it \
         submits, which is a PERMISSION surprise, not merely a fidelity gap"
            .to_string()
    } else {
        format!(
            "conway's skills/*/SKILL.md translation does not read Claude Code's \"{key}\" \
             frontmatter key -- named here rather than silently dropped"
        )
    }
}

/// The frontmatter's wire shape, read PERMISSIVELY -- identical posture to
/// `crate::commands::RawFrontmatter`: `description` is the only key this
/// module gives meaning to, `#[serde(flatten)]` into a sorted `BTreeMap`
/// catches every other key (including a `name` key, deliberately never
/// consulted -- this module's own top doc).
#[derive(Debug, Default, serde::Deserialize)]
struct RawFrontmatter {
    description: Option<String>,
    #[serde(flatten)]
    other: std::collections::BTreeMap<String, serde_yaml::Value>,
}

fn parse_frontmatter(yaml_src: &str) -> Result<(Option<String>, Vec<String>), String> {
    if yaml_src.trim().is_empty() {
        return Ok((None, Vec::new()));
    }
    let raw: RawFrontmatter = serde_yaml::from_str(yaml_src).map_err(|err| err.to_string())?;
    Ok((raw.description, raw.other.into_keys().collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(plugin_root: &Path, name: &str, contents: &str) {
        let dir = plugin_root.join("skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), contents).unwrap();
    }

    #[tokio::test]
    async fn a_well_formed_skill_becomes_a_real_command() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(
            dir.path(),
            "refine",
            "---\ndescription: Decompose an idea into work.\n---\n\nDo the refine thing.\n",
        );
        let mut unsupported = Vec::new();
        let translations = read_skills(dir.path(), &mut unsupported);
        assert_eq!(translations.len(), 1);
        assert!(unsupported.is_empty(), "{unsupported:?}");

        let translation = &translations[0];
        assert_eq!(translation.relative_path, "skills/refine");
        assert_eq!(translation.bare_name, "refine");
        assert_eq!(
            translation.description.as_deref(),
            Some("Decompose an idea into work.")
        );

        let command = translation.command().expect("Ready must produce a Command");
        let spec = command.spec();
        assert_eq!(spec.name, "refine");
        assert!(!spec.name.contains('.'), "the bare name must never be pre-namespaced");

        let ctx = conway_core::ports::CommandCtx {
            focused_agent: conway_core::ids::AgentId::new(),
            root_agent: conway_core::ids::AgentId::new(),
            session_id: conway_core::ids::SessionId::new(),
            args: "ignored".to_string(),
        };
        let outcome = command.invoke(ctx).await;
        match outcome {
            conway_core::ports::CommandOutcome::SubmitPrompt { text } => {
                assert!(
                    text.contains(&dir.path().display().to_string()),
                    "the plugin's own absolute root must be named in the submitted prompt: {text}"
                );
                assert!(text.contains("Do the refine thing."), "{text}");
            }
            other => panic!("expected SubmitPrompt, got {other:?}"),
        }
    }

    /// The load-bearing cross-reference case: a skill's own body names a
    /// sibling file "relative to the plugin root" -- the resulting prompt
    /// must carry enough information (the plugin's own absolute root) that
    /// the literal reference, joined against it, resolves to a real file.
    #[test]
    fn a_sibling_reference_resolves_against_the_prepended_plugin_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("skills").join("shared")).unwrap();
        std::fs::write(
            dir.path()
                .join("skills")
                .join("shared")
                .join("human-presentation.md"),
            "Be concise.\n",
        )
        .unwrap();
        write_skill(
            dir.path(),
            "refine",
            "---\ndescription: refine\n---\n\nSee `skills/shared/human-presentation.md` \
             (relative to the plugin root). Read it.\n",
        );

        let mut unsupported = Vec::new();
        let translations = read_skills(dir.path(), &mut unsupported);
        let SkillMapOutcome::Ready { prompt } = &translations[0].outcome else {
            panic!("expected Ready: {:?}", translations[0].outcome);
        };

        // The reference text survives verbatim.
        assert!(prompt.contains("skills/shared/human-presentation.md"));
        // And the plugin's own absolute root is present, so joining the
        // two actually resolves to a real, existing file.
        let root_line_has_path = prompt.contains(&dir.path().display().to_string());
        assert!(root_line_has_path, "{prompt}");
        let resolved = dir.path().join("skills/shared/human-presentation.md");
        assert!(resolved.is_file(), "the referenced sibling must actually exist on disk");
    }

    #[test]
    fn a_skill_with_no_frontmatter_still_translates() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(dir.path(), "greet", "Say hello.\n");
        let mut unsupported = Vec::new();
        let translations = read_skills(dir.path(), &mut unsupported);
        assert_eq!(translations.len(), 1);
        assert!(unsupported.is_empty());
        assert_eq!(translations[0].description, None);
    }

    /// A real `ideate` shape: `user-invocable`/`argument-hint` are named,
    /// not silently dropped, and the skill STILL translates.
    #[test]
    fn unrecognized_frontmatter_keys_are_named_but_do_not_block_translation() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(
            dir.path(),
            "refine",
            "---\ndescription: Decompose an idea.\nuser-invocable: true\nargument-hint: \"[x]\"\n---\n\nBody.\n",
        );
        let mut unsupported = Vec::new();
        let translations = read_skills(dir.path(), &mut unsupported);
        assert!(matches!(
            translations[0].outcome,
            SkillMapOutcome::Ready { .. }
        ));
        let names: Vec<_> = unsupported.iter().map(|u| u.name.as_str()).collect();
        assert!(names.contains(&"skills/refine#user-invocable"), "{names:?}");
        assert!(names.contains(&"skills/refine#argument-hint"), "{names:?}");
    }

    #[test]
    fn allowed_tools_gets_the_permission_shaped_reason() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(
            dir.path(),
            "refine",
            "---\ndescription: x\nallowed-tools: Read, Edit\n---\n\nBody.\n",
        );
        let mut unsupported = Vec::new();
        read_skills(dir.path(), &mut unsupported);
        let item = unsupported
            .iter()
            .find(|u| u.name == "skills/refine#allowed-tools")
            .expect("allowed-tools must be named");
        assert!(item.reason.contains("PERMISSION"), "{item:?}");
    }

    #[test]
    fn a_directory_without_skill_md_is_not_a_candidate() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("skills").join("not-a-skill")).unwrap();
        let mut unsupported = Vec::new();
        let translations = read_skills(dir.path(), &mut unsupported);
        assert!(translations.is_empty());
        assert!(unsupported.is_empty());
    }

    #[test]
    fn an_empty_body_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(dir.path(), "blank", "---\ndescription: x\n---\n");
        let mut unsupported = Vec::new();
        let translations = read_skills(dir.path(), &mut unsupported);
        assert!(matches!(
            translations[0].outcome,
            SkillMapOutcome::Refused { .. }
        ));
        assert_eq!(unsupported.len(), 1);
        assert_eq!(unsupported[0].name, "skills/blank");
    }

    #[test]
    fn unterminated_frontmatter_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(dir.path(), "broken", "---\ndescription: x\nno closing delimiter\n");
        let mut unsupported = Vec::new();
        let translations = read_skills(dir.path(), &mut unsupported);
        assert!(matches!(
            translations[0].outcome,
            SkillMapOutcome::Refused { .. }
        ));
        assert!(unsupported[0].reason.contains("unterminated"), "{unsupported:?}");
    }

    #[test]
    fn an_absent_skills_directory_reports_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut unsupported = Vec::new();
        let translations = read_skills(dir.path(), &mut unsupported);
        assert!(translations.is_empty());
        assert!(unsupported.is_empty());
    }

    #[test]
    fn multiple_skills_translate_independently_in_sorted_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_skill(dir.path(), "zeta", "Zeta body.\n");
        write_skill(dir.path(), "alpha", "Alpha body.\n");
        let mut unsupported = Vec::new();
        let translations = read_skills(dir.path(), &mut unsupported);
        assert_eq!(
            translations.iter().map(|t| t.bare_name.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "zeta"]
        );
    }
}
