//! WI-113 `conway_ask` one-shot (`-p`) smoke test (epic item f): proves the
//! `conway_ask` -> `conway_subagent` spawn composition is reachable through
//! the real compiled `conway` binary driven by a single `-p` prompt against a
//! scripted OpenAI-compatible mock backend, and that the persisted
//! transcripts carry the load-bearing properties (curation reasoning stays in
//! the ephemeral child, full text in the tool result, spawn prompt is the
//! curated brief verbatim).
//!
//! Mirrors `oneshot.rs`'s own harness (`common::{run_conway, write_fixture}` +
//! `common::mock_backend::{Chunk, MockBackend, Script}`) and
//! `continuity.rs`'s `open_conway` pattern for listing sessions against the
//! on-disk `JsonlSessionStore` the subprocess wrote to. The orchestrator's,
//! ephemeral ask child's, and spawn child's own records are read straight
//! from `<fixture>/.conway/sessions/<session_id>.jsonl` (the store's own
//! layout, `conway-session/src/store.rs::session_path`) -- parsed as
//! `LogRecord` lines -- because `Conway` exposes no public `SessionStore::read`
//! and `SessionHandle::transcript` returns the ancestry-prefixed (effective)
//! view, not a session's own records.

mod common;

use std::sync::Arc;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{run_conway, write_fixture, Fixture};
use conway::config::CliOverrides;
use conway::gates::AllowListGate;
use conway::{Conway, ConwayBuilder, PermissionGate, SessionFilter, SessionId};
use conway_core::content::ContentBlock;
use conway_core::log::{LogRecord, SubagentMode};

/// The sentinel embedded at the head of the curated brief so the
/// "curation reasoning stays in the ephemeral child" assertion can grep for it
/// without pinning the brief's exact wording, and so the spawn-child's-first-
/// `UserTurn` assertion can match verbatim. Shared with the mock script below.
const BRIEF_SENTINEL: &str = "CURATED_BRIEF_SENTINEL_51203";

/// A self-contained brief longer than 2000 chars (clears `DEFAULT_SUMMARY_
/// LIMIT`, proving the tool result is `AskOutcome.text`, not
/// `AgentResult::summary`). Built by cycling a fixed set of clauses until the
/// length clears 2000, then trimming -- deterministic across runs.
fn long_brief() -> String {
    let mut s = String::from(BRIEF_SENTINEL);
    s.push_str(" Self-contained brief for a fresh coder spawn that implements X: ");
    let clauses = [
        "The system is split into a runtime, a facade, and a CLI.",
        "The runtime owns the agent loop, event bus, and subagent host.",
        "The facade exposes SessionHandle with prompt/ask/fork/spawn.",
        "conway_ask forks an ephemeral child to curate context out-of-band.",
        "conway_subagent spawns a fresh agent with the curated prompt.",
        "The child's curation reasoning lives in its own session, not the parent's.",
        "AskOutcome.text is the full concatenated TextDelta stream, untruncated.",
        "The EphemeralSessionRef artifact carries the child's transcript_ref.",
        "store.list with include_ephemeral surfaces the fork; the default hides it.",
        "P-1: ask composes SubagentHost::ask, it is not a third primitive.",
        "P-2: provenance -- the artifact names the child session.",
        "GP-01: full text, not a truncated summary.",
        "GP-02: fork-only, no mode parameter on conway_ask.",
        "The agent-id-checked drain ignores sibling TextDeltas.",
        "subscribe-before-launch guarantees no TextDelta is missed.",
    ];
    let mut i = 0;
    while s.len() <= 2000 {
        s.push_str(clauses[i % clauses.len()]);
        s.push(' ');
        i += 1;
    }
    s
}

/// Opens a fresh, read-only `Conway` against `fixture`'s on-disk session
/// store (mirrors `continuity.rs::open_conway` -- same `CliOverrides.cwd`
/// and empty-`AllowListGate` rationales apply: the fixture's session.root
/// resolves relative to the fixture dir, and `build` needs a non-`prompt`
/// gate).
async fn open_conway(fixture: &Fixture) -> Conway {
    let gate: Arc<dyn PermissionGate> = Arc::new(AllowListGate::new(Vec::new(), Vec::new()));
    ConwayBuilder::from_config(&fixture.config_path)
        .expect("load fixture config")
        .with_cli_overrides(CliOverrides {
            cwd: Some(fixture.dir.path().to_path_buf()),
            ..Default::default()
        })
        .with_permission_gate(gate)
        .build()
        .expect("build conway against the fixture's own store")
}

/// Reads `<fixture>/.conway/sessions/<sid>.jsonl` and parses each non-blank
/// line as a `LogRecord`. The `JsonlSessionStore`'s `append` writes each
/// record to the OS file immediately (only `sync_data` is gated by the fsync
/// interval), so the records are visible to this test process the moment the
/// subprocess exits -- no fsync flush race (see `conway-session/src/store.rs`).
fn read_session_records(fixture: &Fixture, sid: SessionId) -> Vec<LogRecord> {
    let path = fixture
        .dir
        .path()
        .join(".conway/sessions")
        .join(format!("{sid}.jsonl"));
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read session jsonl at {}: {e}", path.display()));
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<LogRecord>(line)
                .unwrap_or_else(|e| panic!("parse LogRecord: {e}; line: {line}"))
        })
        .collect()
}

/// The orchestrator's `-p` prompt -- drives the whole composition.
const PROMPT: &str =
    "use conway_ask to draft a context for a fresh coder spawn that implements X, then spawn it";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conway_p_drives_conway_ask_then_conway_subagent_spawn() {
    // `Chunk::Text` requires `&'static str`, so leak the brief's `String`
    // for the test's lifetime. The brief is also used (as a `&str`) in the
    // `conway_subagent` args and in the verbatim-equality assertions below.
    let brief: &'static str = Box::leak(long_brief().into_boxed_str());
    assert!(
        brief.len() > 2000,
        "test fixture: the brief must exceed 2000 chars; got {}",
        brief.len()
    );
    let ask_prompt =
        "Summarize the key facts above into a self-contained brief for a fresh spawn, no tool calls";

    // Five sequential SSE responses (the mock plays a script in request
    // order): orchestrator -> conway_ask tool_use; ephemeral ask child ->
    // brief; orchestrator -> conway_subagent spawn tool_use with the brief as
    // prompt; spawn child -> completion; orchestrator -> final text EndTurn.
    let mock = MockBackend::start(Script(vec![
        vec![
            Chunk::ToolCall {
                name: "conway_ask",
                args: serde_json::json!({ "prompt": ask_prompt }),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text(brief), Chunk::Finish("stop")],
        vec![
            Chunk::ToolCall {
                name: "conway_subagent",
                args: serde_json::json!({ "mode": "spawn", "prompt": brief }),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("spawn child done"), Chunk::Finish("stop")],
        vec![Chunk::Text("orchestrator all done"), Chunk::Finish("stop")],
    ]))
    .await;
    let fixture = write_fixture(&mock, 10);

    // `--allowed-tools conway_ask,conway_subagent` allowlists both delegate
    // tools by name (both are `Dangerous`; the default allowlist is
    // fail-closed, so without this the tool calls would be denied with feedback
    // and the run would loop until `max_steps`).
    let out = run_conway(
        &[
            "-p",
            PROMPT,
            "--allowed-tools",
            "conway_ask,conway_subagent",
        ],
        &fixture,
    );
    assert!(
        out.status.success(),
        "conway -p should succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Sanity: the orchestrator + two children drove exactly five
    // /chat/completions requests (orchestrator x3, ask child x1, spawn child
    // x1). Also keeps `MockHandle::requests` alive for this test binary so the
    // shared `common::mock_backend` harness does not trip dead-code warnings
    // (the same pattern `subcommands.rs` uses `#[allow(dead_code)]` for).
    assert_eq!(
        mock.requests().len(),
        5,
        "exactly five chat-completions requests (3 orchestrator + 1 ask child + 1 spawn child)"
    );

    let conway = open_conway(&fixture).await;
    let sessions = conway
        .sessions(SessionFilter {
            include_ephemeral: true,
            ..SessionFilter::default()
        })
        .await
        .expect("list sessions");

    // The orchestrator is the root session (no origin, non-ephemeral).
    let orchestrator_meta = sessions
        .iter()
        .find(|m| m.origin.is_none() && !m.ephemeral)
        .expect("the orchestrator (root) session must exist");
    let orchestrator_id = orchestrator_meta.id;

    // The ephemeral ask child: ephemeral, fork, parent = orchestrator.
    let ask_child_meta = sessions
        .iter()
        .find(|m| {
            m.ephemeral
                && m.origin.as_ref().map(|o| o.parent) == Some(orchestrator_id)
                && m.origin.as_ref().map(|o| o.mode) == Some(SubagentMode::Fork)
        })
        .expect("the ephemeral ask child must be present in the include_ephemeral listing");

    // The spawn child: non-ephemeral, spawn, parent = orchestrator.
    let spawn_child_meta = sessions
        .iter()
        .find(|m| {
            !m.ephemeral
                && m.origin.as_ref().map(|o| o.parent) == Some(orchestrator_id)
                && m.origin.as_ref().map(|o| o.mode) == Some(SubagentMode::Spawn)
        })
        .expect("the spawn child must be present in the listing");

    let orchestrator_records = read_session_records(&fixture, orchestrator_id);

    // ------------------------------------------------------------------
    // Assertion: the orchestrator's transcript records the `conway_ask` tool
    // call (as a `ContentBlock::ToolUse` inside an `Assistant` record -- the
    // real persisted shape; see `conway_ask.rs`'s own deviation note for why
    // this is not a separate `LogRecord::ToolCallRecord`) AND its
    // `ToolResultRecord` carrying the FULL brief, plus the `conway_subagent`
    // spawn tool call.
    // ------------------------------------------------------------------
    let ask_tool_use = orchestrator_records
        .iter()
        .find_map(|r| match r {
            LogRecord::Assistant { content, .. } => {
                content.iter().find(|b| matches!(b, ContentBlock::ToolUse { name, .. } if name.as_str() == "conway_ask"))
            }
            _ => None,
        })
        .expect("the orchestrator's transcript must record the conway_ask tool call");
    match ask_tool_use {
        ContentBlock::ToolUse { arguments, .. } => {
            assert_eq!(
                *arguments,
                serde_json::json!({ "prompt": ask_prompt }),
                "the conway_ask ToolUse must carry the ask prompt verbatim"
            );
        }
        _ => unreachable!("matched on ToolUse"),
    }

    let ask_result_text = orchestrator_records
        .iter()
        .find_map(|r| match r {
            LogRecord::ToolResultRecord { result, .. } if result.tool.as_str() == "conway_ask" => {
                result.blocks.iter().find_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
            }
            _ => None,
        })
        .expect("the orchestrator's transcript must carry a conway_ask ToolResultRecord");
    assert_eq!(
        ask_result_text, brief,
        "the conway_ask ToolResultRecord must carry the child's FULL reply verbatim"
    );
    assert!(
        ask_result_text.len() > 2000,
        "LOAD-BEARING: the orchestrator's conway_ask tool result must exceed 2000 chars (got {}), \
         proving it is AskOutcome.text, NOT AgentResult.summary",
        ask_result_text.len()
    );

    let spawn_tool_use = orchestrator_records
        .iter()
        .find_map(|r| match r {
            LogRecord::Assistant { content, .. } => {
                content.iter().find(|b| matches!(b, ContentBlock::ToolUse { name, .. } if name.as_str() == "conway_subagent"))
            }
            _ => None,
        })
        .expect("the orchestrator's transcript must record the conway_subagent tool call");
    match spawn_tool_use {
        ContentBlock::ToolUse { arguments, .. } => {
            assert_eq!(
                *arguments,
                serde_json::json!({ "mode": "spawn", "prompt": brief }),
                "the conway_subagent ToolUse must carry mode: spawn and the curated brief as prompt"
            );
        }
        _ => unreachable!("matched on ToolUse"),
    }

    // ------------------------------------------------------------------
    // Assertion: the orchestrator's transcript does NOT contain the curation
    // reasoning -- no `LogRecord::Assistant` carrying the brief sentinel. The
    // curation lives in the ephemeral ask child's own session.
    // ------------------------------------------------------------------
    let orchestrator_has_curation = orchestrator_records.iter().any(|r| match r {
        LogRecord::Assistant { content, .. } => content.iter().any(|b| match b {
            ContentBlock::Text { text } | ContentBlock::Thinking { text, .. } => {
                text.contains(BRIEF_SENTINEL)
            }
            _ => false,
        }),
        _ => false,
    });
    assert!(
        !orchestrator_has_curation,
        "the orchestrator's transcript must NOT carry the curation reply (the ephemeral child's \
         assistant turn); found an Assistant record containing the brief sentinel"
    );

    let ask_child_records = read_session_records(&fixture, ask_child_meta.id);
    let child_has_curation = ask_child_records.iter().any(|r| match r {
        LogRecord::Assistant { content, .. } => content.iter().any(|b| match b {
            ContentBlock::Text { text } | ContentBlock::Thinking { text, .. } => {
                text.contains(BRIEF_SENTINEL)
            }
            _ => false,
        }),
        _ => false,
    });
    assert!(
        child_has_curation,
        "the ephemeral ask child's own transcript MUST carry the curation reply (its assistant \
         turn containing the brief sentinel)"
    );

    // ------------------------------------------------------------------
    // Assertion: the spawn child's own first `UserTurn` is the curated brief,
    // verbatim -- the text `conway_ask` returned was passed verbatim as the
    // spawn's prompt (P-1 composition, end-to-end through the real binary).
    // ------------------------------------------------------------------
    let spawn_child_records = read_session_records(&fixture, spawn_child_meta.id);
    let spawn_first_user_turn = spawn_child_records
        .iter()
        .find_map(|r| match r {
            LogRecord::UserTurn { text, .. } => Some(text.clone()),
            _ => None,
        })
        .expect("the spawn child's own transcript must have a UserTurn");
    assert_eq!(
        spawn_first_user_turn, brief,
        "the spawn child's first UserTurn must be the curated brief verbatim (the text \
         conway_ask returned was passed verbatim as the spawn's prompt)"
    );
}