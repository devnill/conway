//! End-to-end acceptance for `conway.stepguard` (harness gap review
//! 2026-09-01, finding 11): a real, fully-faked `Conway` (no network, no
//! live provider) with `StepGuardPlugin::new()` attached exactly the way a
//! library embedder would (`ConwayBuilder::with_plugin`), driving three
//! byte-identical tool calls through a REAL model turn (`ScriptedBackend`)
//! and asserting on what a real session actually recorded and actually sent
//! back to the model next -- not merely that `StepGuard::after_tool_call`
//! returns the right `ObserverAnswer` in isolation, which is all the
//! in-crate `src/lib.rs` unit tests ever proved.
//!
//! Three things this file checks that no existing test does:
//!
//!   1. The session's own log contains exactly one
//!      `LogRecord::SystemNote { reason, .. }` after the THIRD identical
//!      call, not zero and not more than one -- appended by
//!      `conway_runtime::agent_loop::AgentLoop::run_inner`'s observer pass
//!      (see that module's own doc), never written by this plugin directly.
//!   2. That note is not merely logged -- it reaches the NEXT generation's
//!      own assembled context, read back off `SessionHandle::context_report`
//!      (the identical data `/context` renders from) as a
//!      `Provenance::SystemNote { reason }` segment.
//!   3. A control run whose three calls carry DIFFERING arguments produces
//!      no note at all -- proving the digest genuinely keys on
//!      `(tool, arguments)`, not merely on `(tool, call count)`.
//!
//! A fourth test below (`two_identical_calls_do_not_reach_the_threshold`)
//! pins the boundary the module doc states in prose (`NOTICE_AT == 3`, not
//! 2): the SAME script shape as the headline test, with the count held one
//! short of the threshold, must produce zero notes. This is the closest a
//! committed test can come to the item's own request to "run (a) with the
//! threshold set so it should NOT fire and confirm the assertion fails,
//! then restore" -- see this crate's own worker completion report for why
//! that literal induced-failure run was not performed by the worker (the
//! writer/build-lane split forbids `cargo test` in a writer's own
//! worktree); this test is the durable, always-passing proof that the
//! headline assertion is not vacuous, checked here for a reviewer or the
//! build lane to read rather than merely asserted in prose.

use std::collections::BTreeMap;
use std::sync::Arc;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::plugin::{
    async_trait, ContentBlock, PathArgs, PermissionClass, Plugin, PluginManifest, RenderKind,
    SeqRange, Tool, ToolCall, ToolCategory, ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec,
    TruncationPolicy,
};
use conway::test_support::test_builder;
use conway::{
    backend::{BackendId, GenerateResponse, StopReason, Usage},
    Conway, Provenance, RoleAlias, SessionSpec, SessionStore,
};
use conway_plugin_stepguard::{StepGuardPlugin, NOTE_REASON};
use conway_testkit::{text_response, FakeStore, ScriptedBackend, ScriptedTurn};

fn base_config(cwd: std::path::PathBuf) -> ConwayConfig {
    let mut roles = BTreeMap::new();
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
        hooks: HooksConfig::default(),
    }
}

/// A real, fully-faked `Conway` with `StepGuardPlugin::new()` AND a tiny
/// always-succeeds `probe` tool installed (`ProbePlugin`/`ProbeTool` below)
/// -- `conway-plugin-path`'s own `tests/` precedent for "give the model
/// something real to call repeatedly" rather than reaching for the fs
/// builtins, which this plugin's own logic has no dependency on at all
/// (it observes ANY tool call, not a filesystem-specific one).
fn stepguard_conway(
    cwd: std::path::PathBuf,
    backend: Arc<ScriptedBackend>,
    store: Arc<FakeStore>,
) -> Conway {
    test_builder(base_config(cwd))
        .with_backend(backend)
        .with_session_store(store)
        .with_plugin(Arc::new(StepGuardPlugin::new()))
        .with_plugin(Arc::new(ProbePlugin))
        .build()
        .expect("build should succeed with every port injected")
}

fn tool_call_response(call_id: &str, tool: &str, args: serde_json::Value) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![ToolCall {
            call_id: call_id.to_string(),
            name: ToolName::new(tool),
            arguments: args,
        }],
        stop: StopReason::ToolUse,
        usage: Usage::default(),
    }
}

/// A trivial always-succeeds tool -- gives the model something real to call
/// repeatedly with byte-identical (or deliberately differing) arguments,
/// exactly `conway-plugin-path`'s own `ProbePlugin`/`ProbeTool` fixture.
struct ProbePlugin;

impl Plugin for ProbePlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "test.probe".to_string(),
            version: "0.0.0".to_string(),
            tools: vec![ToolName::new("probe")],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(ProbeTool)]
    }
}

struct ProbeTool;

#[async_trait]
impl Tool for ProbeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("probe"),
            description: "test-only probe tool".into(),
            schema: serde_json::from_value(serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
            }))
            .unwrap(),
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        }
    }
    async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: "probed".to_string(),
            }],
            is_error: false,
            truncation: TruncationPolicy::None,
            artifacts: vec![],
        })
    }
    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }
}

#[test]
fn manifest_id_matches_the_published_constant() {
    assert_eq!(StepGuardPlugin::new().manifest().id, conway_plugin_stepguard::PLUGIN_ID);
}

/// Acceptance 2: the THIRD identical `probe` call appends exactly one
/// `LogRecord::SystemNote` with `reason == NOTE_REASON`, and that note
/// reaches the NEXT generation's own assembled context as a
/// `Provenance::SystemNote { reason }` segment -- read off
/// `SessionHandle::context_report`, the identical data `/context` renders.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_third_identical_call_appends_one_note_that_reaches_the_next_turns_context() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(FakeStore::new());
    let same_args = serde_json::json!({ "path": "a.txt" });
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(tool_call_response("tc1", "probe", same_args.clone())),
            ScriptedTurn::Respond(tool_call_response("tc2", "probe", same_args.clone())),
            ScriptedTurn::Respond(tool_call_response("tc3", "probe", same_args.clone())),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = stepguard_conway(tmp.path().to_path_buf(), backend.clone(), store.clone());

    let session = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = session.prompt("read a.txt three times please").await.expect("prompt");
    turn.result().await.expect("turn completes naturally");

    // (a) exactly one SystemNote, reason == NOTE_REASON, appended after the
    // THIRD call's own ToolResultRecord -- not before, not after a fourth.
    let records = store
        .read(&session.id(), SeqRange::full())
        .await
        .expect("read back session log");
    let tc3_index = records
        .iter()
        .position(|r| matches!(r, conway::LogRecord::ToolResultRecord { result, .. } if result.call_id == "tc3"))
        .expect("the third call's tool result must be in the log");
    let note_indices: Vec<usize> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| matches!(r, conway::LogRecord::SystemNote { .. }))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        note_indices.len(),
        1,
        "exactly one SystemNote must be appended, got {note_indices:?} in {records:#?}"
    );
    let note_index = note_indices[0];
    assert!(
        note_index > tc3_index,
        "the note must be appended AFTER the third call's own result (index {tc3_index}), got \
         note at index {note_index}"
    );
    match &records[note_index] {
        conway::LogRecord::SystemNote { reason, prov, .. } => {
            assert_eq!(reason, NOTE_REASON);
            assert_eq!(prov, &Provenance::SystemNote { reason: NOTE_REASON.to_string() });
        }
        other => panic!("expected SystemNote at index {note_index}, got {other:?}"),
    }

    // (b) the model's NEXT request contained it -- the report for the
    // generation that produced "done", the one issued right after the
    // note was appended.
    let report = session
        .context_report(session.root())
        .await
        .expect("context_report");
    assert!(
        report.segments.iter().any(|entry| matches!(
            &entry.provenance,
            Provenance::SystemNote { reason } if reason == NOTE_REASON
        )),
        "the next turn's own context report must carry a SystemNote segment: {:?}",
        report.segments
    );
}

/// Acceptance (c): three calls with DIFFERING arguments never produce a
/// note -- the digest genuinely keys on `(tool, arguments)`, not merely on
/// call count.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn differing_arguments_never_produce_a_note() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(tool_call_response(
                "tc1",
                "probe",
                serde_json::json!({ "path": "a.txt" }),
            )),
            ScriptedTurn::Respond(tool_call_response(
                "tc2",
                "probe",
                serde_json::json!({ "path": "b.txt" }),
            )),
            ScriptedTurn::Respond(tool_call_response(
                "tc3",
                "probe",
                serde_json::json!({ "path": "c.txt" }),
            )),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = stepguard_conway(tmp.path().to_path_buf(), backend.clone(), store.clone());

    let session = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = session
        .prompt("read three different files please")
        .await
        .expect("prompt");
    turn.result().await.expect("turn completes naturally");

    let records = store
        .read(&session.id(), SeqRange::full())
        .await
        .expect("read back session log");
    assert!(
        !records.iter().any(|r| matches!(r, conway::LogRecord::SystemNote { .. })),
        "distinct arguments must never be conflated into a repeated-step note: {records:#?}"
    );

    let report = session
        .context_report(session.root())
        .await
        .expect("context_report");
    assert!(
        !report
            .segments
            .iter()
            .any(|entry| matches!(&entry.provenance, Provenance::SystemNote { .. })),
        "no SystemNote segment should ever reach a turn's context here: {:?}",
        report.segments
    );
}

/// The threshold boundary, pinned directly: TWO identical calls (one short
/// of `NOTICE_AT == 3`) must produce zero notes -- the same script shape as
/// the headline test above, with the repeat count held below the
/// threshold. See this file's own module doc for why this is the durable
/// stand-in for the item's own "run with the threshold set so it should
/// NOT fire" instruction.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_identical_calls_do_not_reach_the_threshold() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(FakeStore::new());
    let same_args = serde_json::json!({ "path": "a.txt" });
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(tool_call_response("tc1", "probe", same_args.clone())),
            ScriptedTurn::Respond(tool_call_response("tc2", "probe", same_args.clone())),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = stepguard_conway(tmp.path().to_path_buf(), backend.clone(), store.clone());

    let session = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session");
    let turn = session.prompt("read a.txt twice please").await.expect("prompt");
    turn.result().await.expect("turn completes naturally");

    let records = store
        .read(&session.id(), SeqRange::full())
        .await
        .expect("read back session log");
    assert!(
        !records.iter().any(|r| matches!(r, conway::LogRecord::SystemNote { .. })),
        "two identical calls is one short of NOTICE_AT == 3 and must not fire: {records:#?}"
    );
}
