//! Agent definition loader: parses `.conway/agents/*.md` (markdown with a
//! YAML frontmatter block) into [`conway_core::config::AgentDef`] values.
//!
//! This module performs the one piece of on-disk discovery/parsing named in
//! the facade module spec's `Provides` list (`agents::load_agent_defs`); it
//! does not resolve skills, wire tool selectors into a live registry, or
//! touch the runtime — `AgentDef.skills` is populated verbatim from the
//! frontmatter's `skills` list (a list of skill *names*), and resolving
//! those names to `SkillDef` bodies is a later concern.
//!
//! **Reconciliation (disclosed):** the binding
//! implementation notes describe the `tools` mapping as "`Option<Vec<String>>
//! -> ToolSelector::Explicit`; absent -> `ToolSelector::Inherit`". The
//! already-committed `conway_core::agent::ToolSelector` has no `Explicit` or
//! `Inherit` variant — only `All`, `Only(Vec<String>)`, and
//! `Except(Vec<String>)` — matching the same prose/reality gap a prior item
//! (per) already hit and resolved the same way. Since
//! `AgentDef.tools` is `ToolSelector` (not `Option<ToolSelector>`), an
//! absent `tools` key maps to `ToolSelector::All` (select everything, the
//! closest available meaning of "inherit/no restriction") and a present
//! `tools` list — including an explicit empty list — maps to
//! `ToolSelector::Only(list)`, preserving the documented distinction that an
//! explicit empty list means *no* tools.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use conway_core::agent::ToolSelector;
use conway_core::config::AgentDef;
use conway_core::ids::{ModelRef, RoleAlias};
use serde::Deserialize;

use crate::error::FacadeError;
use crate::Result;

/// The frontmatter's wire shape. `#[serde(deny_unknown_fields)]` so a typo'd
/// key fails loudly rather than being silently ignored.
///
/// `model` is `Option<String>` rather than `Option<ModelRef>`: the
/// already-committed `ModelRef` derives a plain struct `Deserialize` (wire
/// shape `{backend, model}`), not a string-parsed one (mirrors the same
/// reconciliation `config/schema.rs` already made for `RoleEntry.chain`), so
/// the documented `model: <backend>/<model>` string is parsed explicitly via
/// `ModelRef::from_str` below rather than left to serde.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFrontmatter {
    name: Option<String>,
    description: Option<String>,
    role: Option<RoleAlias>,
    tools: Option<Vec<String>>,
    model: Option<String>,
    max_steps: Option<u32>,
    result_contract: Option<serde_json::Value>,
    skills: Option<Vec<String>>,
}

/// Reads every `*.md` file directly inside `dir` (non-recursive; files not
/// ending in `.md` and subdirectories are ignored silently) and parses each
/// into an [`AgentDef`], keyed by its `name`. A missing `dir` is not an
/// error — it yields an empty map. Entries are processed in file-name
/// sorted order so error reporting is deterministic across platforms.
///
/// A single-root convenience over [`load_agent_defs_from_roots`] — `dir`
/// becomes that function's one-element `dirs[0]` (the "operator's own,
/// strict" root), so this function's behavior is byte-for-byte what it was
/// before multi-root support existed (board item
/// `01M0X1EH2GW5DKY9XD1EZ78S3F`): every existing caller (`ConwayBuilder::
/// build`, `crate::intent`, `conway-cli`'s `--agent` resolution) keeps
/// compiling and behaving identically without touching a single call site.
pub fn load_agent_defs(dir: &Path) -> Result<HashMap<String, AgentDef>> {
    load_agent_defs_from_roots(&[dir.to_path_buf()])
}

/// Reads agent definitions from `dirs`, an ORDERED list of roots, and
/// merges them into one map keyed by name.
///
/// **Precedence, in one sentence:** the first root's own definitions always
/// win a name collision with any later root's — so `dirs[0]`, meant to be
/// the operator's own `.conway/agents`, always shadows a plugin's.
///
/// **Isolation.** `dirs[0]` keeps [`load_agent_defs`]'s original strict
/// contract unchanged: a malformed file (bad frontmatter, a name/stem
/// mismatch, an empty prompt, or a name that collides with another file
/// already loaded from `dirs[0]` itself) is a loud, propagated error. Every
/// root AFTER `dirs[0]` is treated as third-party (a plugin's own
/// directory, which the operator did not author and cannot fix): a file in
/// one of those roots that fails to parse, or whose name collides with
/// another file already loaded from the SAME root, is skipped
/// (`tracing::warn!`, never a propagated error) rather than aborting the
/// whole load — one broken plugin directory must never make the operator's
/// own agents, or a different, well-formed plugin's, unloadable. An
/// unreadable or missing non-primary root (including a permission error,
/// unlike `dirs[0]`'s `NotFound`-only carve-out) is likewise not this
/// operator's file to fix, so it also yields no entries rather than an
/// error.
///
/// Ties among two OTHER (non-`dirs[0]`) roots resolve the same way: first
/// in `dirs` wins, no error — the one total order this function has, not a
/// richer precedence system.
///
/// An empty `dirs` yields an empty map (no root is "the operator's own",
/// so there is nothing to be strict about).
pub fn load_agent_defs_from_roots(dirs: &[PathBuf]) -> Result<HashMap<String, AgentDef>> {
    let mut combined: HashMap<String, AgentDef> = HashMap::new();
    for (index, dir) in dirs.iter().enumerate() {
        let root_defs = if index == 0 {
            load_agent_defs_strict(dir)?
        } else {
            load_agent_defs_lenient(dir)
        };
        for (name, def) in root_defs {
            combined.entry(name).or_insert(def);
        }
    }
    Ok(combined)
}

/// The original single-root algorithm, unchanged: a missing `dir` is not an
/// error (empty map); any other read failure, or any malformed file,
/// propagates loudly. Used for `dirs[0]` only — see
/// [`load_agent_defs_from_roots`]'s own doc for why.
fn load_agent_defs_strict(dir: &Path) -> Result<HashMap<String, AgentDef>> {
    let read_dir = match fs::read_dir(dir) {
        Ok(read_dir) => read_dir,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(err) => return Err(FacadeError::Io(err)),
    };

    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in read_dir {
        let entry = entry.map_err(FacadeError::Io)?;
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            paths.push(path);
        }
    }
    paths.sort();

    let mut defs: HashMap<String, AgentDef> = HashMap::with_capacity(paths.len());
    for path in paths {
        let def = load_one(&path)?;
        insert_unique(&mut defs, def, &path)?;
    }
    Ok(defs)
}

/// Same directory scan as [`load_agent_defs_strict`], but for a
/// non-primary (third-party/plugin) root: any failure — the directory
/// itself unreadable, one file's frontmatter malformed, or a name collision
/// within this SAME root — is logged via `tracing::warn!` and skipped
/// rather than propagated, so one broken entry never costs the rest of this
/// root, or any other root, its own valid definitions. Never returns an
/// error; a root that cannot be read at all yields an empty map, exactly
/// like a missing `dirs[0]` does in the strict path.
fn load_agent_defs_lenient(dir: &Path) -> HashMap<String, AgentDef> {
    let read_dir = match fs::read_dir(dir) {
        Ok(read_dir) => read_dir,
        Err(_) => return HashMap::new(),
    };

    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in read_dir {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            paths.push(path);
        }
    }
    paths.sort();

    let mut defs: HashMap<String, AgentDef> = HashMap::with_capacity(paths.len());
    for path in paths {
        match load_one(&path) {
            Ok(def) => {
                if defs.contains_key(&def.name) {
                    tracing::warn!(
                        path = %path.display(),
                        name = %def.name,
                        "duplicate agent name within a non-primary agents root; skipping"
                    );
                } else {
                    defs.insert(def.name.clone(), def);
                }
            }
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "malformed agent definition in a non-primary agents root; skipping"
                );
            }
        }
    }
    defs
}

/// Inserts `def` into `defs`, failing if an agent with the same `name` is
/// already present. Given the name/file-stem-equality rule enforced by
/// [`load_one`], two *files* can only collide this way on a case-insensitive
/// filesystem (two distinct file names that both stem-match their own
/// declared name, whose names then compare equal) — hence "checked
/// defensively" in the criteria. See the unit test below for direct
/// coverage that does not depend on filesystem case sensitivity.
fn insert_unique(defs: &mut HashMap<String, AgentDef>, def: AgentDef, path: &Path) -> Result<()> {
    if defs.contains_key(&def.name) {
        return Err(FacadeError::AgentDef {
            path: path.to_path_buf(),
            message: format!("duplicate agent definition: `{}`", def.name),
        });
    }
    defs.insert(def.name.clone(), def);
    Ok(())
}

fn load_one(path: &Path) -> Result<AgentDef> {
    let content = fs::read_to_string(path).map_err(|err| FacadeError::AgentDef {
        path: path.to_path_buf(),
        message: format!("failed to read file: {err}"),
    })?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    parse_agent_def(&content, stem, path)
}

fn parse_agent_def(content: &str, stem: &str, path: &Path) -> Result<AgentDef> {
    let (yaml_src, body) = split_frontmatter(content, path)?;

    let raw: RawFrontmatter =
        serde_yaml::from_str(yaml_src).map_err(|err| FacadeError::AgentDef {
            path: path.to_path_buf(),
            message: format!("invalid YAML frontmatter: {err}"),
        })?;

    let name = raw.name.ok_or_else(|| FacadeError::AgentDef {
        path: path.to_path_buf(),
        message: "missing required field 'name'".to_string(),
    })?;

    if name != stem {
        return Err(FacadeError::AgentDef {
            path: path.to_path_buf(),
            message: format!("agent name `{name}` does not match file stem `{stem}`"),
        });
    }

    let system_prompt = normalize_body(body);
    if system_prompt.is_empty() {
        return Err(FacadeError::AgentDef {
            path: path.to_path_buf(),
            message: "empty system prompt".to_string(),
        });
    }

    let model = raw
        .model
        .map(|s| {
            s.parse::<ModelRef>().map_err(|err| FacadeError::AgentDef {
                path: path.to_path_buf(),
                message: format!("invalid model reference `{s}`: {err}"),
            })
        })
        .transpose()?;

    let tools = match raw.tools {
        Some(names) => ToolSelector::Only(names),
        None => ToolSelector::All,
    };

    let result_contract = raw
        .result_contract
        .map(|value| compile_result_contract(value, path))
        .transpose()?;

    Ok(AgentDef {
        name,
        description: raw.description,
        system_prompt,
        role: raw.role,
        model,
        tools,
        skills: raw.skills.unwrap_or_default(),
        max_steps: raw.max_steps,
        result_contract,
    })
}

/// Validates `value` compiles as a JSON Schema document (draft 2020-12,
/// `jsonschema`'s default when no `$schema` keyword is present — same
/// compile-only pattern as `conway-plugin-backends`'
/// `tool_calls::validate::SchemaValidator::compile`) and, on success,
/// deserializes it into the `schemars::schema::RootSchema` shape
/// `AgentDef.result_contract` requires (permissive: unrecognized keywords
/// land in `SchemaObject::extensions` rather than failing).
fn compile_result_contract(
    value: serde_json::Value,
    path: &Path,
) -> Result<schemars::schema::RootSchema> {
    jsonschema::validator_for(&value).map_err(|err| FacadeError::AgentDef {
        path: path.to_path_buf(),
        message: format!("invalid result_contract: {err}"),
    })?;
    serde_json::from_value(value).map_err(|err| FacadeError::AgentDef {
        path: path.to_path_buf(),
        message: format!("invalid result_contract: {err}"),
    })
}

/// Splits `content` into `(yaml_frontmatter, body)`. Parsing algorithm (per
/// the implementation notes): a UTF-8 BOM and leading blank lines are
/// stripped first; the content must then begin with a `---` line; the YAML
/// slice runs up to the next line that is exactly `---` after trimming
/// trailing whitespace; the body is everything after that closing
/// delimiter's own `---` marker (its line terminator is the "single leading
/// `\n`/`\r\n`" [`normalize_body`] strips).
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
        .ok_or_else(|| FacadeError::AgentDef {
            path: path.to_path_buf(),
            message: "missing YAML frontmatter: file must begin with a `---` delimiter line"
                .to_string(),
        })?;

    let mut pos = 0usize;
    let close_start = loop {
        if pos >= after_open.len() {
            return Err(FacadeError::AgentDef {
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

/// Strips a single leading `\n` (or `\r\n`) then `trim_end()`s. Internal
/// whitespace (indentation) is left untouched — the body is a system
/// prompt, where indentation is meaningful.
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
    use conway_core::ids::{BackendId, ModelId};

    fn agent_def(name: &str) -> AgentDef {
        AgentDef {
            name: name.to_string(),
            description: None,
            system_prompt: "prompt".to_string(),
            role: None,
            model: None,
            tools: ToolSelector::All,
            skills: Vec::new(),
            max_steps: None,
            result_contract: None,
        }
    }

    #[test]
    fn load_agent_defs_missing_dir_is_ok_empty() {
        let defs = load_agent_defs(Path::new("/nonexistent/does/not/exist")).unwrap();
        assert!(defs.is_empty());
    }

    #[test]
    fn insert_unique_rejects_duplicate_names() {
        let mut defs = HashMap::new();
        insert_unique(&mut defs, agent_def("reviewer"), Path::new("a.md")).unwrap();
        let err = insert_unique(&mut defs, agent_def("reviewer"), Path::new("b.md")).unwrap_err();
        match err {
            FacadeError::AgentDef { message, .. } => {
                assert!(message.contains("duplicate agent definition"), "{message}");
            }
            other => panic!("expected AgentDef error, got {other:?}"),
        }
    }

    #[test]
    fn split_frontmatter_strips_bom_and_leading_blank_lines() {
        let content = "\u{FEFF}\n\n---\nname: x\n---\nbody\n";
        let (yaml, body) = split_frontmatter(content, Path::new("x.md")).unwrap();
        assert_eq!(yaml, "name: x\n");
        assert_eq!(normalize_body(body), "body");
    }

    #[test]
    fn split_frontmatter_missing_delimiter_errors() {
        let err = split_frontmatter("no frontmatter here\n", Path::new("x.md")).unwrap_err();
        match err {
            FacadeError::AgentDef { message, .. } => {
                assert!(message.contains("missing YAML frontmatter"), "{message}");
            }
            other => panic!("expected AgentDef error, got {other:?}"),
        }
    }

    #[test]
    fn split_frontmatter_unterminated_errors() {
        let err = split_frontmatter("---\nname: x\n", Path::new("x.md")).unwrap_err();
        match err {
            FacadeError::AgentDef { message, .. } => {
                assert!(message.contains("unterminated frontmatter"), "{message}");
            }
            other => panic!("expected AgentDef error, got {other:?}"),
        }
    }

    #[test]
    fn normalize_body_trims_single_leading_newline_and_trailing_whitespace() {
        assert_eq!(normalize_body("\nbody text\n\n"), "body text");
        assert_eq!(normalize_body("\r\nbody text\r\n"), "body text");
        // Internal blank lines / indentation are preserved.
        assert_eq!(
            normalize_body("\nline one\n\n  indented\n"),
            "line one\n\n  indented"
        );
    }

    #[test]
    fn tools_absent_maps_to_all_present_maps_to_only() {
        let with_tools = parse_agent_def(
            "---\nname: t\ntools: [read, grep]\n---\nbody\n",
            "t",
            Path::new("t.md"),
        )
        .unwrap();
        assert_eq!(
            with_tools.tools,
            ToolSelector::Only(vec!["read".into(), "grep".into()])
        );

        let without_tools =
            parse_agent_def("---\nname: t\n---\nbody\n", "t", Path::new("t.md")).unwrap();
        assert_eq!(without_tools.tools, ToolSelector::All);

        let empty_tools = parse_agent_def(
            "---\nname: t\ntools: []\n---\nbody\n",
            "t",
            Path::new("t.md"),
        )
        .unwrap();
        assert_eq!(empty_tools.tools, ToolSelector::Only(Vec::new()));
    }

    #[test]
    fn model_field_parses_backend_slash_model() {
        let def = parse_agent_def(
            "---\nname: t\nmodel: anthropic/claude-sonnet-4-6\n---\nbody\n",
            "t",
            Path::new("t.md"),
        )
        .unwrap();
        assert_eq!(
            def.model,
            Some(ModelRef {
                backend: BackendId::new("anthropic"),
                model: ModelId::new("claude-sonnet-4-6"),
            })
        );
    }

    // -- multi-root (board item `01M0X1EH2GW5DKY9XD1EZ78S3F`) --------------

    fn write_agent(dir: &Path, name: &str, content: &str) {
        fs::write(dir.join(format!("{name}.md")), content).unwrap();
    }

    #[test]
    fn load_agent_defs_from_roots_single_root_matches_load_agent_defs() {
        let tmp = tempfile::tempdir().unwrap();
        write_agent(tmp.path(), "reviewer", "---\nname: reviewer\n---\nbody\n");

        let via_single = load_agent_defs(tmp.path()).unwrap();
        let via_roots = load_agent_defs_from_roots(&[tmp.path().to_path_buf()]).unwrap();
        assert_eq!(via_single, via_roots);
    }

    #[test]
    fn load_agent_defs_from_roots_empty_dirs_is_ok_empty() {
        let defs = load_agent_defs_from_roots(&[]).unwrap();
        assert!(defs.is_empty());
    }

    #[test]
    fn load_agent_defs_from_roots_a_second_root_actually_contributes_entries() {
        let primary = tempfile::tempdir().unwrap();
        let plugin = tempfile::tempdir().unwrap();
        write_agent(
            primary.path(),
            "reviewer",
            "---\nname: reviewer\n---\nbody\n",
        );
        write_agent(plugin.path(), "worker", "---\nname: worker\n---\nbody\n");

        let defs = load_agent_defs_from_roots(&[
            primary.path().to_path_buf(),
            plugin.path().to_path_buf(),
        ])
        .unwrap();
        assert_eq!(defs.len(), 2);
        assert!(defs.contains_key("reviewer"));
        assert!(defs.contains_key("worker"));
    }

    #[test]
    fn load_agent_defs_from_roots_primary_root_shadows_a_later_root_on_collision() {
        let primary = tempfile::tempdir().unwrap();
        let plugin = tempfile::tempdir().unwrap();
        write_agent(
            primary.path(),
            "reviewer",
            "---\nname: reviewer\ndescription: operator's own\n---\nbody\n",
        );
        write_agent(
            plugin.path(),
            "reviewer",
            "---\nname: reviewer\ndescription: a plugin's own\n---\nbody\n",
        );

        let defs = load_agent_defs_from_roots(&[
            primary.path().to_path_buf(),
            plugin.path().to_path_buf(),
        ])
        .unwrap();
        assert_eq!(defs.len(), 1);
        assert_eq!(
            defs["reviewer"].description.as_deref(),
            Some("operator's own")
        );
    }

    #[test]
    fn load_agent_defs_from_roots_malformed_file_in_a_later_root_is_skipped_not_fatal() {
        let primary = tempfile::tempdir().unwrap();
        let plugin = tempfile::tempdir().unwrap();
        write_agent(
            primary.path(),
            "reviewer",
            "---\nname: reviewer\n---\nbody\n",
        );
        // Malformed: missing frontmatter delimiter entirely.
        write_agent(plugin.path(), "broken", "not a valid agent file at all\n");
        // A well-formed sibling in the SAME plugin root must still load.
        write_agent(plugin.path(), "worker", "---\nname: worker\n---\nbody\n");

        let defs = load_agent_defs_from_roots(&[
            primary.path().to_path_buf(),
            plugin.path().to_path_buf(),
        ])
        .unwrap();
        assert_eq!(defs.len(), 2);
        assert!(defs.contains_key("reviewer"));
        assert!(defs.contains_key("worker"));
        assert!(!defs.contains_key("broken"));
    }

    #[test]
    fn load_agent_defs_from_roots_malformed_file_in_the_primary_root_still_errors() {
        let primary = tempfile::tempdir().unwrap();
        write_agent(primary.path(), "broken", "not a valid agent file at all\n");

        let err = load_agent_defs_from_roots(&[primary.path().to_path_buf()]).unwrap_err();
        match err {
            FacadeError::AgentDef { message, .. } => {
                assert!(message.contains("missing YAML frontmatter"), "{message}");
            }
            other => panic!("expected AgentDef error, got {other:?}"),
        }
    }
}
