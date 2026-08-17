//! End-to-end acceptance for `conway.skills` (board item
//! `01M03GMNB3P048G72M158XPDG2`): re-runs `docs/plugins/cookbook.md`
//! example 4's two named acceptance verdicts as REAL tests through a real
//! `Conway` built from a real on-disk `.conway/skills/*/SKILL.md` tree --
//! not the hook in isolation (the crate's own `lib.rs` unit tests do that
//! half, mirroring the cookbook's own split).
//!
//! # What each test proves, end to end
//!
//! - `skill_segment_is_narrowed_to_a_one_line_index_entry_end_to_end`
//!   (cookbook verdict 1): a real `.conway/skills/example/SKILL.md`,
//!   named in a real `.conway/agents/skilled.md` def's `skills: [example]`
//!   frontmatter, loaded by the facade's own `ConwayBuilder::build`,
//!   assembled by `ContextBuilder` into a `Provenance::Skill { name:
//!   "example" }` segment, and NARROWED by this plugin's `ContextHook`
//!   (installed via `Plugin::context_hooks` -- the SAME `with_plugin`
//!   surface the tool uses, no separate `with_context_hook` call) to a
//!   one-line index entry containing `read_skill(name="example")`. The
//!   full body does NOT survive into the assembled request the backend
//!   receives. The "absent" half of the same test proves that WITHOUT the
//!   plugin installed, the same skill's full body reaches the backend
//!   unchanged -- so the narrowing is genuinely this plugin's doing, not a
//!   built-in.
//! - `read_skill_returns_the_full_body_on_invoke_end_to_end` (cookbook
//!   verdict 2): a `ScriptedBackend` turn proposes a `read_skill(name=
//!   "example")` call, the runtime dispatches it to this plugin's real
//!   `Tool::invoke`, and the persisted `ToolResultRecord` carries the
//!   exact `SKILL_BODY` text -- the full document the hook withheld from
//!   context, now fetched on demand.
//!
//! Mirrors `crates/conway-plugin-skeleton/tests/skeleton_end_to_end.rs`'s
//! `ConwayBuilder` + `ScriptedBackend`/`FakeGate`/`FakeRouter`/`FakeStore`
//! shape (the credential-free fakes family), and `crates/conway/tests/
//! skills_e2e.rs`'s on-disk `.conway/skills` fixture shape.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, LimitsConfig, ModelsConfig, PermissionsConfig,
    PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::plugin::Plugin;
use conway::{ConwayBuilder, SessionSpec, SkillDef};
use conway_core::agent::PermissionDecision;
use conway_core::content::{ContentBlock, StopReason, ToolCall, Usage};
use conway_core::ids::{BackendId, ModelId, ModelRef, RoleAlias, SeqRange, ToolName};
use conway_core::log::LogRecord;
use conway_core::ports::{GenerateResponse, SessionStore};
use conway_core::provenance::Provenance;
use conway_testkit::{FakeGate, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};

use conway_plugin_skills::{SkillsPlugin, PLUGIN_ID, TOOL_NAME};

/// The skill body the fixture writes, exactly as `normalize_body` leaves
/// it. Kept as a constant so the narrowing assertion ("the full body does
/// NOT survive") and the `read_skill` assertion ("the full body IS
/// returned") compare against one source of truth.
const SKILL_BODY: &str = "# Example Skill\n\nThis is the verbatim body of the example skill. \
                           It is long enough that the one-line index entry is unambiguously \
                           shorter, and it carries a distinctive token -- DISTINCTIVE-TOKEN-9ZQ \
                           -- the narrowing assertion can check is absent from the index.";

/// Writes a real `.conway`-shaped fixture tree into a fresh scratch
/// directory and returns it:
///
/// - `skills/example/SKILL.md` -- one skill named `example`, body
///   [`SKILL_BODY`], description "An example skill.".
/// - `agents/skilled.md` -- one agent def named `skilled` whose `skills:
///   [example]` frontmatter names the skill above.
///
/// Each test process gets its own directory (no `tempfile` dependency in
/// the production code path -- matches `crates/conway/tests/skills_e2e.rs`'s
/// own `scratch_dir` convention; `tempfile` is a dev-dep here only for the
/// scratch root itself).
fn write_fixtures() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "conway-plugin-skills-e2e-{}-{n}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch root");

    fs::create_dir_all(dir.join(".conway").join("skills").join("example"))
        .expect("create skill dir");
    fs::write(
        dir.join(".conway")
            .join("skills")
            .join("example")
            .join("SKILL.md"),
        format!("---\nname: example\ndescription: An example skill.\n---\n{SKILL_BODY}\n",),
    )
    .expect("write SKILL.md");

    fs::create_dir_all(dir.join(".conway").join("agents")).expect("create agents dir");
    fs::write(
        dir.join(".conway").join("agents").join("skilled.md"),
        "---\nname: skilled\nrole: coder\nskills: [example]\n\
         description: A def that names a skill.\n\
         ---\nYou are an agent that uses a skill.\n",
    )
    .expect("write skilled.md");

    dir
}

fn fake_router() -> Arc<dyn conway_core::ports::Router> {
    Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }))
}

fn base_config(cwd: PathBuf) -> ConwayConfig {
    let mut roles = BTreeMap::new();
    roles.insert(
        "coder".to_string(),
        RoleEntry {
            chain: vec![],
            headroom_tokens: None,
            ..Default::default()
        },
    );
    roles.insert(
        "default".to_string(),
        RoleEntry {
            chain: vec![],
            headroom_tokens: None,
            ..Default::default()
        },
    );
    ConwayConfig {
        default_role: RoleAlias::new("default"),
        cwd,
        session: SessionConfig::default(),
        limits: LimitsConfig::default(),
        permissions: PermissionsConfig::default(),
        backends: BTreeMap::new(),
        routing: RoutingSection::default(),
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig::default(),
        tools: ToolsConfig::default(),
        plugins: PluginsConfig::default(),
        hooks: conway::config::schema::HooksConfig::default(),
    }
}

fn text_response(text: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Usage::default(),
    }
}

fn tool_call_response(call_id: &str, tool: &str, arguments: serde_json::Value) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: call_id.to_string(),
            name: ToolName::new(tool),
            arguments,
        }],
        stop: StopReason::ToolUse,
        usage: Usage::default(),
    }
}

/// Builds a `Conway` from `base_config(scratch)` with every port faked, a
/// real on-disk `.conway/skills` + `.conway/agents` tree the facade's own
/// `ConwayBuilder::build` discovers, and -- when `install` is `true` --
/// `SkillsPlugin::from_dir(scratch/.conway/skills)` attached exactly the
/// way a library embedder attaches any plugin: `ConwayBuilder::with_plugin`.
/// The plugin's `context_hooks()` contribution is installed by the builder
/// itself (no separate `with_context_hook` call) -- the packaging surface
/// this item exists to prove.
fn build_conway(
    scratch: &std::path::Path,
    backend: Arc<ScriptedBackend>,
    install: bool,
) -> (conway::Conway, Arc<FakeStore>) {
    let store = Arc::new(FakeStore::new());
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    let mut builder = ConwayBuilder::from_parts(base_config(scratch.to_path_buf()))
        .with_backend(backend)
        .with_session_store(store.clone())
        .with_permission_gate(gate)
        .with_router(fake_router());
    if install {
        let plugin = SkillsPlugin::from_dir(&scratch.join(".conway").join("skills"))
            .expect("SkillsPlugin::from_dir loads the on-disk skill");
        builder = builder.with_plugin(Arc::new(plugin));
    }
    let conway = builder
        .build()
        .expect("build should succeed with every port injected");
    (conway, store)
}

fn session_spec_naming_example_skill() -> SessionSpec {
    // `keep_alive: false` (the default) is correct even for the two-turn
    // `read_skill` test: a `ToolUse`-stopped turn is not a "Completed" turn,
    // so the root task keeps going through the tool dispatch into the second
    // `EndTurn` turn and THEN terminates -- the same pattern
    // `conway-plugin-skeleton`'s own `skeleton_tool_is_callable_end_to_end_
    // through_a_real_turn` uses with `SessionSpec::default()`.
    SessionSpec {
        agent_def: Some("skilled".to_string()),
        ..Default::default()
    }
}

/// Runs one root turn (`"go"`) and returns after the first `EndTurn`/tool
/// batch settles, so the backend's captured request is available.
async fn one_turn(conway: &conway::Conway, spec: SessionSpec) {
    let handle = conway
        .new_session(spec)
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("go").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");
}

/// The text of the `Provenance::Skill { name }` segment in `req`, or
/// `None` if no skill segment was assembled.
fn skill_segment_text_of(req: &conway_core::ports::GenerateRequest, name: &str) -> Option<String> {
    req.segments.iter().find_map(|s| {
        if matches!(&s.provenance, Provenance::Skill { name: n } if n == name) {
            s.content.iter().find_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
        } else {
            None
        }
    })
}

// ---------------------------------------------------------------------
// Cookbook example 4 verdict 1, re-run END TO END through a real Conway:
// the assembled request the backend receives shows the one-line INDEX
// form for the named skill, NOT the full body -- but ONLY when this
// plugin is installed. The "absent" half (no plugin) proves the full
// body reaches the backend unchanged, so the narrowing is genuinely this
// plugin's doing.
// ---------------------------------------------------------------------
#[tokio::test]
async fn skill_segment_is_narrowed_to_a_one_line_index_entry_end_to_end() {
    let scratch = write_fixtures();

    // WITH the plugin installed: the skill segment must be narrowed.
    let backend_with = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("ok"))])
            .with_id(BackendId::new("fake")),
    );
    let (conway_with, _store) = build_conway(&scratch, backend_with.clone(), true);
    one_turn(&conway_with, session_spec_naming_example_skill()).await;

    let reqs_with = backend_with.calls();
    assert!(
        !reqs_with.is_empty(),
        "the backend must have received a request"
    );
    let text_with = skill_segment_text_of(&reqs_with[0], "example").unwrap_or_else(|| {
        panic!(
            "the assembled request must contain a Provenance::Skill {{ name: \"example\" }} \
             segment; provenances seen: {:?}",
            reqs_with[0]
                .segments
                .iter()
                .map(|s| s.provenance.clone())
                .collect::<Vec<_>>()
        );
    });
    assert!(
        text_with.contains("read_skill(name=\"example\")"),
        "WITH the plugin, the skill segment must be the one-line index pointing at read_skill: \
         {text_with}"
    );
    assert!(
        !text_with.contains("DISTINCTIVE-TOKEN-9ZQ"),
        "WITH the plugin, the full body must NOT survive into the assembled request: {text_with}"
    );
    assert!(
        text_with.len() < SKILL_BODY.len(),
        "WITH the plugin, the index entry must be shorter than the full body"
    );

    // WITHOUT the plugin: the same skill's full body reaches the backend
    // unchanged -- the narrowing is the plugin's, not a built-in.
    let backend_without = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("ok"))])
            .with_id(BackendId::new("fake")),
    );
    let (conway_without, _store) = build_conway(&scratch, backend_without.clone(), false);
    one_turn(&conway_without, session_spec_naming_example_skill()).await;

    let reqs_without = backend_without.calls();
    assert!(!reqs_without.is_empty());
    let text_without = skill_segment_text_of(&reqs_without[0], "example").expect(
        "WITHOUT the plugin, the skill segment must still be present (ContextBuilder always \
         assembles one for a named skill) -- only its CONTENT differs",
    );
    assert_eq!(
        text_without, SKILL_BODY,
        "WITHOUT the plugin, the skill segment must carry the FULL body unchanged -- the \
         narrowing is genuinely this plugin's doing, not a built-in"
    );

    let _ = fs::remove_dir_all(&scratch);
}

// ---------------------------------------------------------------------
// Cookbook example 4 verdict 2, re-run END TO END through a real Conway:
// a `read_skill(name="example")` tool call dispatches to this plugin's
// real `Tool::invoke`, and the persisted `ToolResultRecord` carries the
// exact full body the hook withheld from context.
// ---------------------------------------------------------------------
#[tokio::test]
async fn read_skill_returns_the_full_body_on_invoke_end_to_end() {
    let scratch = write_fixtures();
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(tool_call_response(
                "call-1",
                TOOL_NAME,
                serde_json::json!({ "name": "example" }),
            )),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let (conway, store) = build_conway(&scratch, backend, true);
    let store: Arc<dyn SessionStore> = store;

    let handle = conway
        .new_session(session_spec_naming_example_skill())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("fetch the skill").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");

    let records = store
        .read(&handle.id(), SeqRange::full())
        .await
        .expect("read should succeed");
    let tool_result = records
        .iter()
        .find_map(|r| match r {
            LogRecord::ToolResultRecord { result, .. } if result.tool.as_str() == TOOL_NAME => {
                Some(result)
            }
            _ => None,
        })
        .expect("the session must have actually invoked read_skill");

    assert!(
        !tool_result.is_error,
        "read_skill(example) must succeed, not error: {tool_result:?}"
    );
    let text: String = tool_result
        .blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        text, SKILL_BODY,
        "read_skill must return the FULL body verbatim, proving the runtime dispatched to this \
         plugin's own Tool::invoke and not merely announced its name"
    );

    let _ = fs::remove_dir_all(&scratch);
}

/// An unknown skill name returns a model-visible error, never a hard
/// `Err`/crash -- cookbook example 4's own failure path, end to end.
#[tokio::test]
async fn read_skill_for_an_unknown_name_returns_a_model_visible_error_not_a_crash() {
    let scratch = write_fixtures();
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(tool_call_response(
                "call-1",
                TOOL_NAME,
                serde_json::json!({ "name": "does-not-exist" }),
            )),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let (conway, store) = build_conway(&scratch, backend, true);
    let store: Arc<dyn SessionStore> = store;

    let handle = conway
        .new_session(session_spec_naming_example_skill())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("fetch a bogus skill").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");

    let records = store
        .read(&handle.id(), SeqRange::full())
        .await
        .expect("read should succeed");
    let tool_result = records
        .iter()
        .find_map(|r| match r {
            LogRecord::ToolResultRecord { result, .. } if result.tool.as_str() == TOOL_NAME => {
                Some(result)
            }
            _ => None,
        })
        .expect("read_skill was called");
    assert!(
        tool_result.is_error,
        "an unknown name must be a model-visible error, not a silent success: {tool_result:?}"
    );
    let text: String = tool_result
        .blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("no such skill"),
        "the error text must name the failure mode for the model: {text}"
    );

    let _ = fs::remove_dir_all(&scratch);
}

/// Compiles-only check that the published install id matches the manifest
/// id a `[plugins].install` entry resolves against -- the same wiring-only
/// check `conway-cli`'s own `first_party_plugins::tests` makes for the
/// other first-party plugins.
#[test]
fn manifest_id_matches_the_published_constant() {
    let skills: std::collections::HashMap<String, SkillDef> = std::collections::HashMap::new();
    let plugin = SkillsPlugin::new(Arc::new(skills));
    assert_eq!(plugin.manifest().id, PLUGIN_ID);
}
