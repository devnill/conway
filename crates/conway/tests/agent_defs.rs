//! WI-099: agent definition loader — non-recursive `*.md` discovery,
//! well-formed parsing (all `AgentDef` fields), and fail-loud, path-naming
//! errors for every documented malformation.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use conway::agents::load_agent_defs;
use conway::{AgentDef, ConwayError};
use conway_core::agent::ToolSelector;
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias};

/// Each test gets its own scratch directory (no external `tempfile`
/// dependency, matching `tests/support/mod.rs`'s existing convention) so
/// fixtures with deliberately conflicting/broken content never interfere
/// with each other or with parallel test threads.
fn scratch_dir(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "conway-agent-defs-test-{label}-{}-{n}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agents")
}

/// Copies the named fixtures (by file name, e.g. `"reviewer.md"`) into a
/// fresh scratch directory and returns that directory's path.
fn dir_with_fixtures(label: &str, names: &[&str]) -> PathBuf {
    let dir = scratch_dir(label);
    for name in names {
        let content = fs::read_to_string(fixtures_dir().join(name))
            .unwrap_or_else(|err| panic!("read fixture {name}: {err}"));
        fs::write(dir.join(name), content).unwrap_or_else(|err| panic!("write {name}: {err}"));
    }
    dir
}

fn load_single(label: &str, name: &str) -> Result<HashMap<String, AgentDef>, ConwayError> {
    let dir = dir_with_fixtures(label, &[name]);
    load_agent_defs(&dir)
}

fn expect_agent_def_error(
    result: Result<HashMap<String, AgentDef>, ConwayError>,
    file_name: &str,
) -> String {
    match result {
        Ok(defs) => panic!("expected an error, got Ok({defs:?})"),
        Err(ConwayError::AgentDef { path, message }) => {
            assert_eq!(
                path.file_name().and_then(|n| n.to_str()),
                Some(file_name),
                "error path should name the offending file"
            );
            message
        }
        Err(other) => panic!("expected ConwayError::AgentDef, got {other:?}"),
    }
}

#[test]
fn missing_dir_returns_ok_empty_map() {
    let defs = load_agent_defs(Path::new("/does/not/exist/conway-agents")).unwrap();
    assert!(defs.is_empty());
}

#[test]
fn well_formed_fixture_parses_all_fields() {
    let defs = load_single("reviewer", "reviewer.md").unwrap();
    let def = defs.get("reviewer").expect("reviewer entry present");

    assert_eq!(def.name, "reviewer");
    assert_eq!(def.role, Some(RoleAlias::new("coder")));
    assert_eq!(
        def.tools,
        ToolSelector::Only(vec!["read".to_string(), "grep".to_string()])
    );
    assert_eq!(
        def.model,
        Some(ModelRef {
            backend: BackendId::new("anthropic"),
            model: ModelId::new("claude-sonnet-4-6"),
        })
    );
    assert_eq!(def.max_steps, Some(20));
    assert!(def.result_contract.is_some());
    assert_eq!(def.skills, vec!["review-checklist".to_string()]);
    assert_eq!(
        def.description,
        Some("Reviews diffs for correctness and style.".to_string())
    );

    let expected_prompt = "You are a careful, thorough code reviewer. Read the diff, check for\n\
correctness, security issues, and style violations. Report findings\n\
concisely.";
    assert_eq!(def.system_prompt, expected_prompt);
}

#[test]
fn minimal_fixture_parses_with_only_name() {
    let defs = load_single("minimal", "minimal.md").unwrap();
    let def = defs.get("minimal").expect("minimal entry present");

    assert_eq!(def.name, "minimal");
    assert_eq!(def.description, None);
    assert_eq!(def.role, None);
    assert_eq!(def.tools, ToolSelector::All);
    assert_eq!(def.model, None);
    assert_eq!(def.max_steps, None);
    assert_eq!(def.result_contract, None);
    assert!(def.skills.is_empty());
    assert_eq!(def.system_prompt, "Minimal system prompt.");
}

#[test]
fn no_frontmatter_errors() {
    let result = load_single("no-frontmatter", "no_frontmatter.md");
    let message = expect_agent_def_error(result, "no_frontmatter.md");
    assert!(message.contains("missing YAML frontmatter"), "{message}");
}

#[test]
fn unterminated_frontmatter_errors() {
    let result = load_single("unterminated", "unterminated.md");
    let message = expect_agent_def_error(result, "unterminated.md");
    assert!(message.contains("unterminated frontmatter"), "{message}");
}

#[test]
fn invalid_yaml_error_includes_underlying_text_and_line_number() {
    let result = load_single("bad-yaml", "bad_yaml.md");
    let message = expect_agent_def_error(result, "bad_yaml.md");
    assert!(message.contains("invalid YAML frontmatter"), "{message}");
    assert!(message.contains("line"), "{message}");
}

#[test]
fn missing_name_errors() {
    let result = load_single("missing-name", "missing_name.md");
    let message = expect_agent_def_error(result, "missing_name.md");
    assert!(
        message.contains("missing required field 'name'"),
        "{message}"
    );
}

#[test]
fn name_stem_mismatch_errors_naming_both_values() {
    let result = load_single("name-mismatch", "name_mismatch.md");
    let message = expect_agent_def_error(result, "name_mismatch.md");
    assert!(message.contains("someone_else"), "{message}");
    assert!(message.contains("name_mismatch"), "{message}");
}

#[test]
fn bad_result_contract_errors() {
    let result = load_single("bad-contract", "bad_contract.md");
    let message = expect_agent_def_error(result, "bad_contract.md");
    assert!(message.contains("invalid result_contract"), "{message}");
}

#[test]
fn empty_body_errors() {
    let result = load_single("empty-body", "empty_body.md");
    let message = expect_agent_def_error(result, "empty_body.md");
    assert!(message.contains("empty system prompt"), "{message}");
}

#[test]
fn unknown_frontmatter_key_names_the_key() {
    let dir = scratch_dir("unknown-key");
    fs::write(
        dir.join("bogus.md"),
        "---\nname: bogus\nnope: true\n---\nBody.\n",
    )
    .unwrap();
    let result = load_agent_defs(&dir);
    let message = expect_agent_def_error(result, "bogus.md");
    assert!(message.contains("nope"), "{message}");
}

#[test]
fn non_md_files_and_subdirectories_are_ignored() {
    let dir = scratch_dir("ignored");
    fs::write(dir.join("README.txt"), "not an agent def").unwrap();
    fs::create_dir_all(dir.join("nested")).unwrap();
    fs::write(
        dir.join("nested").join("hidden.md"),
        "---\nname: hidden\n---\nBody.\n",
    )
    .unwrap();

    let defs = load_agent_defs(&dir).unwrap();
    assert!(defs.is_empty());
}

#[test]
fn multiple_valid_files_load_and_key_by_name() {
    let dir = dir_with_fixtures("multi", &["reviewer.md", "minimal.md"]);
    let defs = load_agent_defs(&dir).unwrap();
    assert_eq!(defs.len(), 2);
    assert!(defs.contains_key("reviewer"));
    assert!(defs.contains_key("minimal"));
}

#[test]
fn explicit_empty_tools_list_means_no_tools() {
    let dir = scratch_dir("empty-tools");
    fs::write(
        dir.join("notools.md"),
        "---\nname: notools\ntools: []\n---\nBody.\n",
    )
    .unwrap();
    let defs = load_agent_defs(&dir).unwrap();
    let def = defs.get("notools").unwrap();
    assert_eq!(def.tools, ToolSelector::Only(Vec::new()));
}
