//! Skill definition loader: parses `.conway/skills/<name>/SKILL.md` (markdown
//! with a YAML frontmatter block) into [`conway_core::config::SkillDef`]
//! values.
//!
//! Mirrors [`crate::agents::load_agent_defs`]'s shape exactly (board item
//! `01M03GKZ3MGZK3ETP6R27E2M9Y`, "Skill definitions have a type and a
//! consumer but no producer"): same `deny_unknown_fields` frontmatter struct,
//! same deterministic sort-before-parse discipline, same loud-failure-on-
//! malformed-file rule, same "a missing directory is not an error" rule. The
//! one structural difference is the on-disk shape itself — an `AgentDef` is
//! one file per definition (`<name>.md` directly inside the agents dir); a
//! `SkillDef` is one **directory** per definition
//! (`<name>/SKILL.md`), per `docs/vision/CATALOGUE.md` entry 2's proposed
//! layout. A top-level entry that is not a directory, or a directory with no
//! `SKILL.md` inside it, is not a skill candidate at all and is ignored
//! silently — exactly like `load_agent_defs` ignoring a non-`.md` file. A
//! `SKILL.md` that DOES exist but fails to parse is never skipped: a silently
//! skipped broken file is worse than a refused one, since the operator wrote
//! it expecting it to do something.
//!
//! **Selection is by name only.** `conway_core::config::AgentDef::skills:
//! Vec<String>` (already committed) names the skills an agent def wants by
//! name; resolving those names against the registry this module produces
//! into `Provenance::Skill` context segments is `conway_runtime::runtime`'s
//! job (`Runtime::start_root`/`resume_root`), not this module's. This module
//! never decides which agent gets which skill — it only discovers what
//! exists on disk. There is no "load every discovered skill into every
//! agent" path anywhere: that would put unbounded text in every context
//! window, directly against the project's framing that context is the
//! scarce resource.
//!
//! **`SkillDef::always_include` is parsed but not yet consumed.** The
//! frontmatter key round-trips into the struct (default `false` when absent)
//! so a `SKILL.md` that sets it does not fail to load, but nothing in this
//! item resolves it into a selection rule — the only selection mechanism
//! this item wires end to end is `AgentDef.skills`' explicit name list. Its
//! own field-level doc on `SkillDef` gives no operational meaning for it
//! either. Flagged rather than guessed: whether `always_include: true` should
//! mean "inject into every agent regardless of its `skills` list" is a real,
//! undecided question, and this item's own acceptance criteria only require
//! named-skill resolution — inventing unconditional-injection semantics here
//! would be exactly the "guessing a shape nobody agreed" this item's brief
//! warns against.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use conway_core::config::SkillDef;
use serde::Deserialize;

use crate::error::FacadeError;
use crate::Result;

/// The frontmatter's wire shape. `#[serde(deny_unknown_fields)]` so a typo'd
/// key fails loudly rather than being silently ignored — mirrors
/// `agents.rs`'s `RawFrontmatter`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFrontmatter {
    name: Option<String>,
    description: Option<String>,
    #[serde(default)]
    always_include: bool,
}

/// Reads every `<name>/SKILL.md` directly inside `dir` (one level of
/// subdirectories; a subdirectory with no `SKILL.md` is ignored, exactly
/// like `load_agent_defs` ignoring a non-`.md` file) and parses each into a
/// [`SkillDef`], keyed by its `name`. A missing `dir` is not an error — it
/// yields an empty map. Subdirectories are processed in name-sorted order so
/// error reporting is deterministic across platforms.
///
/// A single-root convenience over [`load_skill_defs_from_roots`] — `dir`
/// becomes that function's one-element `dirs[0]` (the "operator's own,
/// strict" root), so this function's behavior is byte-for-byte what it was
/// before multi-root support existed (board item
/// `01M0X1EH2GW5DKY9XD1EZ78S3F`): every existing caller (`ConwayBuilder::
/// build`, `conway_plugin_skills::SkillsPlugin::from_dir`) keeps compiling
/// and behaving identically without touching a single call site.
pub fn load_skill_defs(dir: &Path) -> Result<HashMap<String, SkillDef>> {
    load_skill_defs_from_roots(&[dir.to_path_buf()])
}

/// Reads skill definitions from `dirs`, an ORDERED list of roots, and
/// merges them into one map keyed by name.
///
/// **Precedence, in one sentence:** the first root's own definitions always
/// win a name collision with any later root's — so `dirs[0]`, meant to be
/// the operator's own `.conway/skills`, always shadows a plugin's.
///
/// **Isolation.** `dirs[0]` keeps [`load_skill_defs`]'s original strict
/// contract unchanged: a malformed `SKILL.md` (bad frontmatter, a
/// name/directory-name mismatch, an empty body, or a name that collides
/// with another directory already loaded from `dirs[0]` itself) is a loud,
/// propagated error. Every root AFTER `dirs[0]` is treated as third-party
/// (a plugin's own directory, which the operator did not author and cannot
/// fix): a `SKILL.md` in one of those roots that fails to parse, or whose
/// name collides with another directory already loaded from the SAME root,
/// is skipped (`tracing::warn!`, never a propagated error) rather than
/// aborting the whole load — one broken plugin directory must never make
/// the operator's own skills, or a different, well-formed plugin's,
/// unloadable. An unreadable or missing non-primary root (including a
/// permission error, unlike `dirs[0]`'s `NotFound`-only carve-out) is
/// likewise not this operator's file to fix, so it also yields no entries
/// rather than an error.
///
/// Ties among two OTHER (non-`dirs[0]`) roots resolve the same way: first
/// in `dirs` wins, no error — the one total order this function has, not a
/// richer precedence system.
///
/// An empty `dirs` yields an empty map (no root is "the operator's own",
/// so there is nothing to be strict about).
pub fn load_skill_defs_from_roots(dirs: &[PathBuf]) -> Result<HashMap<String, SkillDef>> {
    let mut combined: HashMap<String, SkillDef> = HashMap::new();
    for (index, dir) in dirs.iter().enumerate() {
        let root_defs = if index == 0 {
            load_skill_defs_strict(dir)?
        } else {
            load_skill_defs_lenient(dir)
        };
        for (name, def) in root_defs {
            combined.entry(name).or_insert(def);
        }
    }
    Ok(combined)
}

/// The original single-root algorithm, unchanged: a missing `dir` is not an
/// error (empty map); any other read failure, or any malformed `SKILL.md`,
/// propagates loudly. Used for `dirs[0]` only — see
/// [`load_skill_defs_from_roots`]'s own doc for why.
fn load_skill_defs_strict(dir: &Path) -> Result<HashMap<String, SkillDef>> {
    let read_dir = match fs::read_dir(dir) {
        Ok(read_dir) => read_dir,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(err) => return Err(FacadeError::Io(err)),
    };

    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in read_dir {
        let entry = entry.map_err(FacadeError::Io)?;
        let path = entry.path();
        if path.is_dir() {
            candidates.push(path);
        }
    }
    candidates.sort();

    let mut defs: HashMap<String, SkillDef> = HashMap::with_capacity(candidates.len());
    for skill_dir in candidates {
        let skill_md = skill_dir.join("SKILL.md");
        if !skill_md.is_file() {
            // Not a skill directory (no `SKILL.md` inside it) -- not a
            // candidate in the first place, mirroring `load_agent_defs`'s
            // silent skip of a non-`.md` file.
            continue;
        }
        let stem = skill_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let def = load_one(&skill_md, stem)?;
        insert_unique(&mut defs, def, &skill_md)?;
    }
    Ok(defs)
}

/// Same directory scan as [`load_skill_defs_strict`], but for a non-primary
/// (third-party/plugin) root: any failure — the directory itself
/// unreadable, one `SKILL.md`'s frontmatter malformed, or a name collision
/// within this SAME root — is logged via `tracing::warn!` and skipped
/// rather than propagated, so one broken entry never costs the rest of this
/// root, or any other root, its own valid definitions. Never returns an
/// error; a root that cannot be read at all yields an empty map, exactly
/// like a missing `dirs[0]` does in the strict path.
fn load_skill_defs_lenient(dir: &Path) -> HashMap<String, SkillDef> {
    let read_dir = match fs::read_dir(dir) {
        Ok(read_dir) => read_dir,
        Err(_) => return HashMap::new(),
    };

    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in read_dir {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.is_dir() {
            candidates.push(path);
        }
    }
    candidates.sort();

    let mut defs: HashMap<String, SkillDef> = HashMap::with_capacity(candidates.len());
    for skill_dir in candidates {
        let skill_md = skill_dir.join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        let stem = skill_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        // Try the operator-native shape first (a well-formed plugin might
        // genuinely ship it); fall back to the third-party-tolerant shape
        // (board item `01M1DG5TTF6NHW2RXJRZ8ZPE7K`) only if that fails --
        // see `load_one_third_party`'s own doc.
        match load_one(&skill_md, stem).or_else(|_| load_one_third_party(&skill_md, stem, dir)) {
            Ok(def) => {
                if defs.contains_key(&def.name) {
                    tracing::warn!(
                        path = %skill_md.display(),
                        name = %def.name,
                        "duplicate skill name within a non-primary skills root; skipping"
                    );
                } else {
                    defs.insert(def.name.clone(), def);
                }
            }
            Err(err) => {
                tracing::warn!(
                    path = %skill_md.display(),
                    error = %err,
                    "malformed skill definition in a non-primary skills root; skipping"
                );
            }
        }
    }
    defs
}

/// The third-party-tolerant fallback [`load_skill_defs_lenient`] tries only
/// AFTER the operator-native shape ([`load_one`]) fails -- see that call
/// site's own comment. Board item `01M1DG5TTF6NHW2RXJRZ8ZPE7K` added this
/// so `ConwayBuilder::with_extra_skill_dir` (already documented on that
/// method as "the seam a Claude Code compat layer... calls to hand a
/// plugin's own directories to a real build") actually produces usable
/// skills when handed a REAL third-party `skills/` directory: a real
/// `SKILL.md` (`ideate` 3.2.2's own, checked directly) commonly carries NO
/// `name` key at all -- many third-party skill conventions, Claude Code's
/// own included, treat the containing directory as the skill's own
/// identity -- and carries frontmatter keys conway's own format never
/// defined (`user-invocable`, `argument-hint`). Both are tolerated here,
/// never rejected: identity is ALWAYS `stem` (a frontmatter `name` key, if
/// present, is simply one more ignored key -- caught by
/// `ThirdPartyRawFrontmatter`'s own `#[serde(flatten)]`), and every
/// unrecognized key is likewise ignored rather than a hard parse failure,
/// the same "not this operator's file to fix" posture
/// [`load_skill_defs_lenient`]'s own doc already states for a malformed
/// file in a non-primary root.
///
/// **Cross-references.** A real `SKILL.md` this fallback has been checked
/// against tells its own reader, in PROSE, to open a sibling file
/// "relative to the plugin root" (`skills/shared/human-presentation.md`,
/// verbatim) -- not a token this loader could substitute (contrast
/// `conway_plugin_claude::hooks`'s own `${CLAUDE_PLUGIN_ROOT}`
/// substitution for `hooks.json`/`.mcp.json` commands, a DIFFERENT file
/// kind that DOES use that literal token). The resulting body is prefixed
/// with one line (`plugin_root_note`) naming `skills_root`'s own PARENT
/// directory -- the plugin's own root, one level above the `skills/`
/// directory a caller hands `with_extra_skill_dir` -- so a model reading
/// the injected skill body has the one fact it needs to resolve such a
/// reference with its own Read tool.
fn load_one_third_party(path: &Path, stem: &str, skills_root: &Path) -> Result<SkillDef> {
    let content = fs::read_to_string(path).map_err(|err| FacadeError::SkillDef {
        path: path.to_path_buf(),
        message: format!("failed to read file: {err}"),
    })?;
    let (yaml_src, body) = split_frontmatter(&content, path)?;

    let raw: ThirdPartyRawFrontmatter =
        serde_yaml::from_str(yaml_src).map_err(|err| FacadeError::SkillDef {
            path: path.to_path_buf(),
            message: format!("invalid YAML frontmatter: {err}"),
        })?;

    let normalized = normalize_body(body);
    if normalized.is_empty() {
        return Err(FacadeError::SkillDef {
            path: path.to_path_buf(),
            message: "empty skill body".to_string(),
        });
    }
    let plugin_root = skills_root.parent().unwrap_or(skills_root);
    let body = format!("{}\n\n{normalized}", plugin_root_note(plugin_root));

    Ok(SkillDef {
        name: stem.to_string(),
        description: raw.description,
        body,
        always_include: raw.always_include,
    })
}

/// The one line prepended to every skill translated by
/// [`load_one_third_party`] -- see that function's own doc,
/// "Cross-references".
fn plugin_root_note(plugin_root: &Path) -> String {
    format!(
        "[conway: this skill's own plugin root directory is `{}`. Any reference in the text \
         below described as \"relative to the plugin root\" resolves against that absolute \
         path.]",
        plugin_root.display()
    )
}

/// The third-party (lenient-root-only) frontmatter shape -- see
/// [`load_one_third_party`]'s own doc for why this needs to differ from
/// [`RawFrontmatter`] (the operator's own, `deny_unknown_fields`-strict
/// shape, unchanged and still used for `dirs[0]` via [`load_one`]). No
/// `#[serde(flatten)]` catch-all here (unlike `conway_plugin_claude`'s own
/// permissive parsers, which capture unrecognized keys to NAME them in a
/// report): this crate has no such reporting concept, so an unrecognized
/// key is simply never looked at -- serde's own default (no
/// `deny_unknown_fields`) already ignores it without this struct needing
/// to name it.
#[derive(Deserialize)]
struct ThirdPartyRawFrontmatter {
    description: Option<String>,
    #[serde(default)]
    always_include: bool,
}

/// Inserts `def` into `defs`, failing if a skill with the same `name` is
/// already present. Given the name/directory-name-equality rule enforced by
/// [`load_one`], two distinct directories can only collide this way on a
/// case-insensitive filesystem — see `agents.rs`'s `insert_unique`, whose
/// same "checked defensively" rationale applies here unchanged.
fn insert_unique(defs: &mut HashMap<String, SkillDef>, def: SkillDef, path: &Path) -> Result<()> {
    if defs.contains_key(&def.name) {
        return Err(FacadeError::SkillDef {
            path: path.to_path_buf(),
            message: format!("duplicate skill definition: `{}`", def.name),
        });
    }
    defs.insert(def.name.clone(), def);
    Ok(())
}

fn load_one(path: &Path, stem: &str) -> Result<SkillDef> {
    let content = fs::read_to_string(path).map_err(|err| FacadeError::SkillDef {
        path: path.to_path_buf(),
        message: format!("failed to read file: {err}"),
    })?;
    parse_skill_def(&content, stem, path)
}

fn parse_skill_def(content: &str, stem: &str, path: &Path) -> Result<SkillDef> {
    let (yaml_src, body) = split_frontmatter(content, path)?;

    let raw: RawFrontmatter =
        serde_yaml::from_str(yaml_src).map_err(|err| FacadeError::SkillDef {
            path: path.to_path_buf(),
            message: format!("invalid YAML frontmatter: {err}"),
        })?;

    let name = raw.name.ok_or_else(|| FacadeError::SkillDef {
        path: path.to_path_buf(),
        message: "missing required field 'name'".to_string(),
    })?;

    if name != stem {
        return Err(FacadeError::SkillDef {
            path: path.to_path_buf(),
            message: format!("skill name `{name}` does not match directory name `{stem}`"),
        });
    }

    let body = normalize_body(body);
    if body.is_empty() {
        return Err(FacadeError::SkillDef {
            path: path.to_path_buf(),
            message: "empty skill body".to_string(),
        });
    }

    Ok(SkillDef {
        name,
        description: raw.description,
        body,
        always_include: raw.always_include,
    })
}

/// Identical algorithm to `agents.rs::split_frontmatter` -- see that
/// function's own doc for the parsing rules (BOM/leading-blank-line
/// stripping, `---` delimiters). Duplicated rather than shared because
/// `agents.rs` keeps it private to its own module; both copies are unit-
/// tested independently below, same as that module's own discipline.
fn split_frontmatter<'a>(content: &'a str, path: &Path) -> Result<(&'a str, &'a str)> {
    let content = content.strip_prefix('\u{FEFF}').unwrap_or(content);

    let mut offset = 0usize;
    for line in content.split_inclusive('\n') {
        if line.trim().is_empty() {
            offset += line.len();
        } else {
            break;
        }
    }
    let after_blank = &content[offset..];

    let after_open = after_blank
        .strip_prefix("---\r\n")
        .or_else(|| after_blank.strip_prefix("---\n"))
        .ok_or_else(|| FacadeError::SkillDef {
            path: path.to_path_buf(),
            message: "missing YAML frontmatter: file must begin with a `---` delimiter line"
                .to_string(),
        })?;

    let mut pos = 0usize;
    let close_start = loop {
        if pos >= after_open.len() {
            return Err(FacadeError::SkillDef {
                path: path.to_path_buf(),
                message: "unterminated frontmatter: no closing `---` delimiter found".to_string(),
            });
        }
        let rest = &after_open[pos..];
        let line_len = rest.find('\n').map(|i| i + 1).unwrap_or(rest.len());
        let line = &rest[..line_len];
        let line_no_nl = line.trim_end_matches(['\n', '\r']);
        if line_no_nl.trim_end() == "---" {
            break pos;
        }
        pos += line_len;
    };

    let yaml_src = &after_open[..close_start];
    let body = &after_open[close_start + 3..];
    Ok((yaml_src, body))
}

/// Identical to `agents.rs::normalize_body` -- strips a single leading `\n`
/// (or `\r\n`) then `trim_end()`s. Internal whitespace (indentation) is left
/// untouched.
fn normalize_body(raw: &str) -> String {
    let stripped = raw
        .strip_prefix("\r\n")
        .or_else(|| raw.strip_prefix('\n'))
        .unwrap_or(raw);
    stripped.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(dir: &Path, name: &str, content: &str) {
        let skill_dir = dir.join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), content).unwrap();
    }

    #[test]
    fn load_skill_defs_missing_dir_is_ok_empty() {
        let defs = load_skill_defs(Path::new("/nonexistent/does/not/exist")).unwrap();
        assert!(defs.is_empty());
    }

    #[test]
    fn load_skill_defs_discovers_and_parses_a_real_skill() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(
            tmp.path(),
            "example",
            "---\nname: example\ndescription: An example skill.\n---\n# Example\n\nBody text.\n",
        );
        let defs = load_skill_defs(tmp.path()).unwrap();
        assert_eq!(defs.len(), 1);
        let skill = defs.get("example").unwrap();
        assert_eq!(skill.name, "example");
        assert_eq!(skill.description.as_deref(), Some("An example skill."));
        assert_eq!(skill.body, "# Example\n\nBody text.");
        assert!(!skill.always_include);
    }

    #[test]
    fn load_skill_defs_parses_always_include() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(
            tmp.path(),
            "example",
            "---\nname: example\nalways_include: true\n---\nBody.\n",
        );
        let defs = load_skill_defs(tmp.path()).unwrap();
        assert!(defs.get("example").unwrap().always_include);
    }

    #[test]
    fn load_skill_defs_ignores_a_directory_without_skill_md() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("not-a-skill")).unwrap();
        let defs = load_skill_defs(tmp.path()).unwrap();
        assert!(defs.is_empty());
    }

    #[test]
    fn load_skill_defs_ignores_a_non_directory_entry() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("README.md"), "not a skill").unwrap();
        let defs = load_skill_defs(tmp.path()).unwrap();
        assert!(defs.is_empty());
    }

    #[test]
    fn load_skill_defs_errors_loudly_on_malformed_frontmatter() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), "broken", "no frontmatter here\n");
        let err = load_skill_defs(tmp.path()).unwrap_err();
        match err {
            FacadeError::SkillDef { message, .. } => {
                assert!(message.contains("missing YAML frontmatter"), "{message}");
            }
            other => panic!("expected SkillDef error, got {other:?}"),
        }
    }

    #[test]
    fn load_skill_defs_errors_loudly_on_name_directory_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), "example", "---\nname: other\n---\nBody.\n");
        let err = load_skill_defs(tmp.path()).unwrap_err();
        match err {
            FacadeError::SkillDef { message, .. } => {
                assert!(
                    message.contains("does not match directory name"),
                    "{message}"
                );
            }
            other => panic!("expected SkillDef error, got {other:?}"),
        }
    }

    #[test]
    fn load_skill_defs_errors_loudly_on_unknown_frontmatter_key() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(
            tmp.path(),
            "example",
            "---\nname: example\ntypo_key: oops\n---\nBody.\n",
        );
        let err = load_skill_defs(tmp.path()).unwrap_err();
        match err {
            FacadeError::SkillDef { message, .. } => {
                assert!(message.contains("invalid YAML frontmatter"), "{message}");
            }
            other => panic!("expected SkillDef error, got {other:?}"),
        }
    }

    #[test]
    fn load_skill_defs_errors_loudly_on_empty_body() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), "example", "---\nname: example\n---\n");
        let err = load_skill_defs(tmp.path()).unwrap_err();
        match err {
            FacadeError::SkillDef { message, .. } => {
                assert!(message.contains("empty skill body"), "{message}");
            }
            other => panic!("expected SkillDef error, got {other:?}"),
        }
    }

    #[test]
    fn load_skill_defs_errors_loudly_on_duplicate_name() {
        // Two directories can only collide this way on a case-insensitive
        // filesystem in practice (directory names double as the `name` key,
        // and `load_one` already enforces name == directory name) -- covered
        // directly here rather than depending on filesystem case
        // sensitivity, mirroring `agents.rs`'s own `insert_unique` test.
        let mut defs = HashMap::new();
        let skill = SkillDef {
            name: "example".into(),
            description: None,
            body: "body".into(),
            always_include: false,
        };
        insert_unique(&mut defs, skill.clone(), Path::new("a/SKILL.md")).unwrap();
        let err = insert_unique(&mut defs, skill, Path::new("b/SKILL.md")).unwrap_err();
        match err {
            FacadeError::SkillDef { message, .. } => {
                assert!(message.contains("duplicate skill definition"), "{message}");
            }
            other => panic!("expected SkillDef error, got {other:?}"),
        }
    }

    #[test]
    fn split_frontmatter_strips_bom_and_leading_blank_lines() {
        let content = "\u{FEFF}\n\n---\nname: x\n---\nbody\n";
        let (yaml, body) = split_frontmatter(content, Path::new("x.md")).unwrap();
        assert_eq!(yaml, "name: x\n");
        assert_eq!(normalize_body(body), "body");
    }

    // -- multi-root (board item `01M0X1EH2GW5DKY9XD1EZ78S3F`) --------------

    #[test]
    fn load_skill_defs_from_roots_single_root_matches_load_skill_defs() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), "example", "---\nname: example\n---\nBody.\n");

        let via_single = load_skill_defs(tmp.path()).unwrap();
        let via_roots = load_skill_defs_from_roots(&[tmp.path().to_path_buf()]).unwrap();
        assert_eq!(via_single, via_roots);
    }

    #[test]
    fn load_skill_defs_from_roots_empty_dirs_is_ok_empty() {
        let defs = load_skill_defs_from_roots(&[]).unwrap();
        assert!(defs.is_empty());
    }

    #[test]
    fn load_skill_defs_from_roots_a_second_root_actually_contributes_entries() {
        let primary = tempfile::tempdir().unwrap();
        let plugin = tempfile::tempdir().unwrap();
        write_skill(primary.path(), "review", "---\nname: review\n---\nBody.\n");
        write_skill(plugin.path(), "ideate", "---\nname: ideate\n---\nBody.\n");

        let defs = load_skill_defs_from_roots(&[
            primary.path().to_path_buf(),
            plugin.path().to_path_buf(),
        ])
        .unwrap();
        assert_eq!(defs.len(), 2);
        assert!(defs.contains_key("review"));
        assert!(defs.contains_key("ideate"));
    }

    #[test]
    fn load_skill_defs_from_roots_primary_root_shadows_a_later_root_on_collision() {
        let primary = tempfile::tempdir().unwrap();
        let plugin = tempfile::tempdir().unwrap();
        write_skill(
            primary.path(),
            "review",
            "---\nname: review\ndescription: operator's own\n---\nBody.\n",
        );
        write_skill(
            plugin.path(),
            "review",
            "---\nname: review\ndescription: a plugin's own\n---\nBody.\n",
        );

        let defs = load_skill_defs_from_roots(&[
            primary.path().to_path_buf(),
            plugin.path().to_path_buf(),
        ])
        .unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(
            defs["review"].description.as_deref(),
            Some("operator's own")
        );
    }

    #[test]
    fn load_skill_defs_from_roots_malformed_file_in_a_later_root_is_skipped_not_fatal() {
        let primary = tempfile::tempdir().unwrap();
        let plugin = tempfile::tempdir().unwrap();
        write_skill(primary.path(), "review", "---\nname: review\n---\nBody.\n");
        // Malformed: missing frontmatter delimiter entirely.
        write_skill(plugin.path(), "broken", "not a valid skill file at all\n");
        // A well-formed sibling in the SAME plugin root must still load.
        write_skill(plugin.path(), "ideate", "---\nname: ideate\n---\nBody.\n");

        let defs = load_skill_defs_from_roots(&[
            primary.path().to_path_buf(),
            plugin.path().to_path_buf(),
        ])
        .unwrap();
        assert_eq!(defs.len(), 2);
        assert!(defs.contains_key("review"));
        assert!(defs.contains_key("ideate"));
        assert!(!defs.contains_key("broken"));
    }

    #[test]
    fn load_skill_defs_from_roots_malformed_file_in_the_primary_root_still_errors() {
        let primary = tempfile::tempdir().unwrap();
        write_skill(primary.path(), "broken", "not a valid skill file at all\n");

        let err = load_skill_defs_from_roots(&[primary.path().to_path_buf()]).unwrap_err();
        match err {
            FacadeError::SkillDef { message, .. } => {
                assert!(message.contains("missing YAML frontmatter"), "{message}");
            }
            other => panic!("expected SkillDef error, got {other:?}"),
        }
    }

    // -- third-party fallback (board item `01M1DG5TTF6NHW2RXJRZ8ZPE7K`) ------

    /// The headline shape a REAL third-party `SKILL.md` (`ideate` 3.2.2's
    /// own `refine`, `execute`, ...) has: no `name` key at all, and
    /// frontmatter keys conway never defined. Must translate via a
    /// non-primary root even though it would fail `load_skill_defs`'s own
    /// strict path outright.
    #[test]
    fn a_third_party_skill_with_no_name_key_and_unknown_keys_still_loads_from_a_non_primary_root()
    {
        let primary = tempfile::tempdir().unwrap();
        let plugin = tempfile::tempdir().unwrap();
        write_skill(
            plugin.path(),
            "refine",
            "---\ndescription: Decompose an idea into work.\nuser-invocable: true\n\
             argument-hint: \"[x]\"\n---\n\nDo the refine thing.\n",
        );

        let defs = load_skill_defs_from_roots(&[
            primary.path().to_path_buf(),
            plugin.path().to_path_buf(),
        ])
        .unwrap();
        let def = defs.get("refine").expect("must load via the fallback");
        assert_eq!(
            def.description.as_deref(),
            Some("Decompose an idea into work.")
        );
        assert!(def.body.contains("Do the refine thing."));
    }

    /// The load-bearing cross-reference case: a real skill's own body names
    /// a sibling "relative to the plugin root" -- the loaded `SkillDef`'s
    /// own body must carry the plugin's own absolute root so that
    /// reference, joined against it, resolves to a real file.
    #[test]
    fn a_third_party_skills_cross_reference_survives_with_a_resolvable_plugin_root() {
        let primary = tempfile::tempdir().unwrap();
        let plugin = tempfile::tempdir().unwrap();
        fs::create_dir_all(plugin.path().join("skills").join("shared")).unwrap();
        fs::write(
            plugin
                .path()
                .join("skills")
                .join("shared")
                .join("human-presentation.md"),
            "Be concise.\n",
        )
        .unwrap();
        write_skill(
            plugin.path(),
            "refine",
            "---\ndescription: refine\n---\n\nSee `skills/shared/human-presentation.md` \
             (relative to the plugin root). Read it.\n",
        );

        let defs = load_skill_defs_from_roots(&[
            primary.path().to_path_buf(),
            plugin.path().to_path_buf(),
        ])
        .unwrap();
        let body = &defs["refine"].body;
        assert!(body.contains("skills/shared/human-presentation.md"));
        assert!(
            body.contains(&plugin.path().display().to_string()),
            "the plugin's own absolute root must be named so the reference resolves: {body}"
        );
        let resolved = plugin.path().join("skills/shared/human-presentation.md");
        assert!(resolved.is_file(), "the referenced sibling must actually exist on disk");
    }

    /// A genuinely broken file (no frontmatter delimiter at all) fails
    /// BOTH the operator-native AND the third-party fallback shape --
    /// still skipped, not fatal, per `load_skill_defs_lenient`'s own
    /// contract.
    #[test]
    fn a_file_broken_under_both_shapes_is_still_skipped_not_fatal() {
        let primary = tempfile::tempdir().unwrap();
        let plugin = tempfile::tempdir().unwrap();
        write_skill(plugin.path(), "broken", "not a valid skill file at all\n");

        let defs = load_skill_defs_from_roots(&[
            primary.path().to_path_buf(),
            plugin.path().to_path_buf(),
        ])
        .unwrap();
        assert!(!defs.contains_key("broken"));
    }

    /// A well-formed OPERATOR-shaped skill (name matches stem, no unknown
    /// keys) in a non-primary root still loads through `load_one` directly
    /// -- the fallback is a SECOND attempt, not a replacement.
    #[test]
    fn an_operator_shaped_skill_in_a_non_primary_root_never_needs_the_fallback() {
        let primary = tempfile::tempdir().unwrap();
        let plugin = tempfile::tempdir().unwrap();
        write_skill(plugin.path(), "ideate", "---\nname: ideate\n---\nBody.\n");

        let defs = load_skill_defs_from_roots(&[
            primary.path().to_path_buf(),
            plugin.path().to_path_buf(),
        ])
        .unwrap();
        assert!(defs.contains_key("ideate"));
        assert!(
            !defs["ideate"].body.contains("plugin root directory"),
            "a well-formed operator-shaped skill must not get the fallback's own prepended \
             note: {}",
            defs["ideate"].body
        );
    }
}
