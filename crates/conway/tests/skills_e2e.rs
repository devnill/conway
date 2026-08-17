//! End-to-end acceptance for the skill-definition producer (board item
//! `01M03GKZ3MGZK3ETP6R27E2M9Y`): a real on-disk `.conway/skills/example/
//! SKILL.md` discovered by [`conway::skills::load_skill_defs`], named in a
//! real on-disk agent def's `skills: [example]` frontmatter, loaded by
//! [`conway::agents::load_agent_defs`], threaded into
//! [`RuntimeDeps::skills`], resolved by `Runtime::start_root`'s
//! `resolve_skills` against the `AgentDef.skills` name list, and assembled
//! by `ContextBuilder` into a `Provenance::Skill { name: "example" }`
//! context segment -- the whole producer -> threading -> resolution ->
//! assembly chain the item exists to wire, asserted through the real
//! `Runtime` (never a hand-built `AgentSpec`), the same shape
//! `tests/agent_defs.rs::result_contract_via_def` uses for the
//! `result_contract` field.
//!
//! The loader's own unit tests (`crates/conway/src/skills.rs`) already prove
//! file -> `SkillDef.body` verbatim; `context_golden.rs` already proves a
//! `SkillFragment` -> a `Provenance::Skill` `PromptSegment`. This file proves
//! the gap between them: that the name an `AgentDef` lists actually reaches
//! the registry, resolves, and is assembled end to end, and that the body
//! arrives unchanged.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use conway::agents::load_agent_defs;
use conway::skills::load_skill_defs;
use conway_core::agent::{AgentDefRef, Budget, PermissionDecision};
use conway_core::capabilities::{CacheMode, HeadroomPolicy};
use conway_core::content::{ContentBlock, StopReason, Usage};
use conway_core::event::Event;
use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef};
use conway_core::ids::{LogSeq, SessionId};
use conway_core::log::LogRecord;
use conway_core::ports::{Backend, GenerateResponse, HealthRegistry, Plugin, Router, SessionStore};
use conway_core::provenance::Provenance;
use conway_core::segment::CacheTtl;
use conway_runtime::context::path::path_from_legacy;
use conway_runtime::context::{ContextBuilder, ContextInput, SkillFragment};
use conway_runtime::events::EventBus;
use conway_runtime::runtime::{RootSpec, Runtime, RuntimeDeps};
use conway_testkit::{FakeGate, FakeHealth, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};
use futures::StreamExt;

/// The skill body the fixture writes, exactly as `normalize_body` leaves it
/// (one leading newline stripped, trailing whitespace trimmed): the
/// `SKILL.md` body after its closing `---` is
/// `\n# Example Skill\n\n...unchanged.\n`, which normalizes to this. Kept as a
/// constant so both the loader verbatim assertion and the assembled-context
/// verbatim assertion compare against one source of truth.
const SKILL_BODY: &str = "# Example Skill\n\nThis is the verbatim body of the example skill. \
                           It must appear in the\nassembled context unchanged.";

/// Writes a real `.conway`-shaped fixture tree into a fresh scratch
/// directory and returns it:
///
/// - `skills/example/SKILL.md` -- one skill named `example`, body
///   [`SKILL_BODY`].
/// - `agents/skilled.md` -- one agent def named `skilled` whose `skills:
///   [example]` frontmatter names the skill above.
///
/// No `tempfile` dependency (matches `tests/agent_defs.rs`'s own
/// `scratch_dir` convention); each test process gets its own directory.
fn write_fixtures() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("conway-skills-e2e-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch root");

    fs::create_dir_all(dir.join("skills").join("example")).expect("create skill dir");
    fs::write(
        dir.join("skills").join("example").join("SKILL.md"),
        format!("---\nname: example\ndescription: An example skill.\n---\n{SKILL_BODY}\n",),
    )
    .expect("write SKILL.md");

    fs::create_dir_all(dir.join("agents")).expect("create agents dir");
    fs::write(
        dir.join("agents").join("skilled.md"),
        "---\nname: skilled\nrole: coder\nskills: [example]\n\
         description: A def that names a skill.\n\
         ---\nYou are an agent that uses a skill.\n",
    )
    .expect("write skilled.md");

    dir
}

fn text_response(text: &str) -> GenerateResponse {
    GenerateResponse {
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        tool_calls: vec![],
        stop: StopReason::EndTurn,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        },
    }
}

/// Builds a real `Runtime` with `skills` and `agent_defs` both produced by
/// the real loaders from [`write_fixtures`], and a one-turn scripted backend
/// so a root agent started with a prompt completes immediately. Returns the
/// runtime and its backing store. Mirrors `tests/agent_defs.rs::
/// result_contract_via_def::build_runtime_with_def`'s shape, minus the
/// second backend (one role is enough here -- `FakeRouter::single` routes
/// any role to the single backend).
fn build_runtime(
    skill_defs: HashMap<String, conway_core::config::SkillDef>,
    agent_defs: HashMap<String, conway_core::config::AgentDef>,
) -> (Arc<Runtime>, Arc<dyn SessionStore>) {
    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("ok"))])
            .with_id(BackendId::new("b")),
    );
    let router: Arc<dyn Router> = Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("b"),
        model: ModelId::new("m"),
    }));
    let mut backends: HashMap<BackendId, Arc<dyn Backend>> = HashMap::new();
    backends.insert(backend.id(), backend);

    let runtime = Runtime::new(RuntimeDeps {
        store: store.clone(),
        router,
        health: Arc::new(FakeHealth::new()) as Arc<dyn HealthRegistry>,
        backends,
        plugins: Vec::<Arc<dyn Plugin>>::new(),
        gate: Arc::new(FakeGate::new(PermissionDecision::AllowOnce)),
        agent_defs,
        skills: skill_defs,
        event_bus: EventBus::new(1024),
        headroom: Arc::new(HeadroomPolicy::default()),
    });
    (runtime, store)
}

/// Starts a root agent from `spec` and waits for its terminal
/// `Event::AgentFinished` (a one-shot `keep_alive: false` root emits exactly
/// one, after its single turn) -- mirrors `context_report_persistence.rs`'s
/// `start_and_finish_root`.
async fn start_and_finish(runtime: &Runtime, spec: RootSpec) -> AgentId {
    let mut stream = runtime.subscribe();
    let root = runtime.start_root(spec).await.expect("start_root");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let envelope = stream.next().await.expect("event stream ended early");
            if envelope.agent == root {
                if let Event::AgentFinished { .. } = envelope.event {
                    return;
                }
            }
        }
    })
    .await
    .expect("root agent never finished within 5s");
    root
}

// ---------------------------------------------------------------------
// End-to-end through the real Runtime: a skill named in a real agent def's
// frontmatter is discovered, threaded, resolved, and assembled into a
// `Provenance::Skill` context segment. This is the gap the item exists to
// close -- the loader unit test proves the file parses, and
// `context_golden` proves a fragment assembles, but nothing before this
// proved the NAME in a def reached the registry and resolved end to end.
// ---------------------------------------------------------------------
#[tokio::test]
async fn skill_named_in_def_appears_as_provenance_skill_segment_end_to_end() {
    let dir = write_fixtures();
    let skill_defs = load_skill_defs(&dir.join("skills")).expect("skill defs load");
    let agent_defs = load_agent_defs(&dir.join("agents")).expect("agent defs load");

    assert_eq!(skill_defs.len(), 1, "the example skill must be discovered");
    assert!(
        agent_defs.contains_key("skilled"),
        "the skilled def must load; got keys {:?}",
        agent_defs.keys().collect::<Vec<_>>()
    );

    let (runtime, _store) = build_runtime(skill_defs, agent_defs);

    let root = start_and_finish(
        &runtime,
        RootSpec {
            session: None,
            agent_def: Some(AgentDefRef("skilled".to_string())),
            role: None,
            tools: None,
            budget: Budget::default(),
            cwd: PathBuf::from("/tmp"),
            root: None,
            prompt: Some("go".to_string()),
            keep_alive: false,
            model: None,
            system_prompt_override: None,
            result_contract: None,
        },
    )
    .await;

    let report = runtime
        .context_report(root)
        .expect("a context report exists for the finished turn");
    let found = report.segments.iter().any(|s| {
        matches!(
            &s.provenance,
            Provenance::Skill { name } if name == "example"
        )
    });
    assert!(
        found,
        "the assembled context must contain a Provenance::Skill {{ name: \"example\" }} segment; \
         provenances seen: {:?}",
        report
            .segments
            .iter()
            .map(|s| s.provenance.clone())
            .collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------
// Body verbatim: the real loader's `SkillDef.body`, resolved into a
// `SkillFragment` the way `resolve_skills` does (`name` + `body.clone()`),
// reaches the assembled `PromptSegment`'s text content unchanged. The
// Runtime-path `ContextReport` deliberately carries provenance only (no
// body), so the body-verbatim half of the verification anchor is asserted
// here at the `ContextBuilder` level, fed by the SAME real loader output.
// ---------------------------------------------------------------------
#[test]
fn skill_body_is_carried_verbatim_into_assembled_segment() {
    let dir = write_fixtures();
    let skill_defs = load_skill_defs(&dir.join("skills")).expect("skill defs load");
    let def = skill_defs
        .get("example")
        .expect("the example skill was discovered");
    assert_eq!(
        def.body, SKILL_BODY,
        "the loader must carry the file body verbatim into SkillDef.body"
    );

    // `resolve_skills` (`runtime/root.rs`) turns a `SkillDef` into a
    // `SkillFragment { name, text: skill.body.clone() }` -- replicate that
    // exact transform and feed it to the real `ContextBuilder`.
    let fragment = SkillFragment {
        name: def.name.clone(),
        text: def.body.clone(),
    };
    let input = ContextInput {
        agent_id: AgentId::new(),
        turn: 0,
        model: ModelId::new("m"),
        cache_mode: CacheMode::None,
        system_prompt: None,
        skills: vec![fragment],
        tools: vec![],
        path: path_from_legacy(
            None,
            &[LogRecord::UserTurn {
                seq: LogSeq(0),
                ts: chrono::Utc::now(),
                text: "go".to_string(),
                prov: Provenance::UserPrompt,
            }],
            SessionId::new(),
        )
        .unwrap(),
        cache_ttl: CacheTtl::FiveMinutes,
    };
    let (segments, _report) = ContextBuilder::new()
        .build(&input)
        .expect("context builder assembles");
    let skill_seg = segments
        .iter()
        .find(|s| {
            matches!(
                &s.provenance,
                Provenance::Skill { name } if name == "example"
            )
        })
        .expect("a Provenance::Skill { name: \"example\" } segment was assembled");
    let body = match &skill_seg.content[..] {
        [ContentBlock::Text { text }] => text.clone(),
        other => panic!("skill segment content should be a single Text block, got {other:?}"),
    };
    assert_eq!(
        body, SKILL_BODY,
        "the skill body must reach the assembled context verbatim"
    );
}
