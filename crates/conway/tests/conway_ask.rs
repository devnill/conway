//! Acceptance tests for the `conway_ask` epic, item f: the thin-slice
//! end-to-end proof that `conway_ask` works through the REAL runtime + REAL
//! `SubagentPlugin` (including `AskTool`, item d), composing with
//! `conway_spawn` (). This is the capstone: it proves the epic's goal
//! -- out-of-band context curation, full text not truncated, curation
//! reasoning stays in the child, provenance preserved.
//!
//! One integration test drives the whole slice through the real
//! `Arc<Runtime>`-backed `Conway` (built from `conway_testkit` ports, the
//! same construction `ask.rs`/`session_handle_subagent.rs` use -- NOT a
//! `FakeSubagentHost`: `SubagentHost` is the real `Runtime` so `conway_ask`'s
//! `ctx.subagents.ask` goes through `Runtime::ask`'s subscribe-before-launch,
//! agent-id-checked TextDelta drain). It then inspects the persisted
//! transcripts (`FakeStore::read`) to prove every load-bearing property in
//! one place:
//!
//! - **Full text ():** the `ToolResultRecord` for `conway_ask` carries
//!   the child's FULL reply (`text.len() > 2000`, NOT truncated at
//!   `DEFAULT_SUMMARY_LIMIT`).
//! - **Curation stays in the child:** the parent's own log has NO
//!   `LogRecord::Assistant` carrying the curation reply; the ephemeral
//!   child's own log HAS it.
//! - **Provenance ():** the child's `SessionId` is reachable via the
//!   `EphemeralSessionRef` artifact's `id` (the `transcript_ref`), and
//!   `store.list(SessionFilter { include_ephemeral: true, .. })` contains it
//!   while `store.list(SessionFilter::default())` does NOT.
//! - **Composition ():** the parent's second turn drives
//!   `conway_ask` -> `conway_spawn { prompt: <brief> }`,
//!   and the spawn child's own first `UserTurn` is the brief verbatim.
//!
//! Gated on the `builtin-tools` feature (like `tests/interactive_tools.rs`'s
//! own gate, and `tests/gates.rs`'s `presets_builtin_plugins_matches_conway_tools`):
//! `conway_ask`/`conway_spawn` are registered from `presets::builtin_plugins()`,
//! which does not exist without this feature -- `build_conway`'s default
//! `ToolsConfig` would register no tools at all, and the whole slice this
//! file exercises has nothing meaningful to drive. Discovered by this
//! item's own verification pass (retire the backend
//! compile-time feature flags): this file predates that item and had no
//! such gate, but every combination that would have exposed the gap
//! (`conway-backends` referenced ungated with neither backend feature
//! enabled) failed to even *compile* until that item's fix, and the CI
//! feature matrix only ever ran `cargo check`, never `cargo test` -- so the
//! gap was invisible in both directions until now.
#![cfg(feature = "builtin-tools")]

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::{Conway, ConwayBuilder, SessionSpec};
use conway_core::agent::PermissionDecision;
use conway_core::content::{ContentBlock, StopReason, ToolCall, Usage};
use conway_core::ids::{BackendId, ModelId, ModelRef, SeqRange, ToolName};
use conway_core::log::{LogRecord, SessionFilter, SubagentMode};
use conway_core::ports::{Backend, GenerateResponse, SessionStore};
use conway_testkit::{FakeGate, FakeRouter, FakeStore, ScriptedBackend, ScriptedTurn};

// ---------------------------------------------------------------------
// Harness (mirrors `ask.rs`'s own helpers)
// ---------------------------------------------------------------------

fn fake_router() -> Arc<dyn conway_core::ports::Router> {
    Arc::new(FakeRouter::single(ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    }))
}

fn base_config() -> ConwayConfig {
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
        default_role: conway_core::ids::RoleAlias::new("default"),
        cwd: std::path::PathBuf::from("."),
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

fn build_conway(store: Arc<dyn SessionStore>, backend: Arc<dyn Backend>) -> Conway {
    let gate = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
    ConwayBuilder::from_parts(base_config())
        .with_backend(backend)
        .with_session_store(store)
        .with_permission_gate(gate)
        .with_router(fake_router())
        .build()
        .expect("build should succeed with every port injected")
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

fn tool_call_response(call_id: &str, tool: &str, args: serde_json::Value) -> GenerateResponse {
    text_and_tool_call_response("", call_id, tool, args)
}

/// A response carrying BOTH a text content block (the distinctive fact the
/// parent establishes in its transcript) AND a tool call, with stop=ToolUse
/// so the agent loop continues into the tool execution. This is the shape
/// approach (b) of the spec takes when the fact-establishing text and the
/// `conway_ask` invocation are driven by a single parent turn (necessary
/// because a `start_root` agent's task exits after `EndTurn` -- a second
/// `handle.prompt` on the same handle would hang waiting for an agent task
/// that has already returned; see `ask.rs`'s own "resumed continuation" note).
fn text_and_tool_call_response(
    text: &str,
    call_id: &str,
    tool: &str,
    args: serde_json::Value,
) -> GenerateResponse {
    let mut content = Vec::new();
    if !text.is_empty() {
        content.push(ContentBlock::Text {
            text: text.to_string(),
        });
    }
    GenerateResponse {
        content,
        tool_calls: vec![ToolCall {
            call_id: call_id.to_string(),
            name: ToolName::new(tool),
            arguments: args,
        }],
        stop: StopReason::ToolUse,
        usage: Usage::default(),
    }
}

/// The distinctive fact the parent establishes in its first turn -- the
/// ephemeral ask child inherits the parent's context, so its curation reply
/// is expected to reference this fact.
const PARENT_FACT: &str = "DISTINCTIVE_PARENT_FACT_42819: the project wires modules A, B, and C.";

/// A sentinel embedded at the head of the curated brief so the "curation
/// reasoning stays in the child" assertion can grep for it without pinning
/// the brief's exact wording.
const BRIEF_SENTINEL: &str = "CURATED_BRIEF_SENTINEL_72611";

/// A self-contained brief longer than `DEFAULT_SUMMARY_LIMIT` (2000 chars) so
/// the load-bearing `text.len() > 2000` assertion proves the tool result
/// carries the FULL reply, not `AgentResult::summary` (which truncates at
/// 2000). The sentinel at the head lets the parent/child-reasoning split
/// assertion grep for it without matching the parent's own short turns.
fn long_brief() -> String {
    let mut s = String::from(BRIEF_SENTINEL);
    s.push_str(" Self-contained brief for a fresh spawn: ");
    s.push_str(PARENT_FACT);
    s.push_str(" Component A parses input; B transforms; C renders. ");
    // Pad with distinctive, non-repetitive-enough prose to clear 2000 chars.
    // Each clause is a real "fact" a curator might emit, so the brief reads
    // as genuine context, not filler -- the assertion is on length, not
    // wording, but a realistic payload keeps the test honest.
    let clauses = [
        "The runtime subscribes before launching the child (no race).",
        "AskOutcome.text is the concatenated TextDelta stream, untruncated.",
        "The EphemeralSessionRef artifact names the child session for provenance.",
        "store.list with include_ephemeral surfaces the fork; the default filter hides it.",
        "conway_ask composes with conway_spawn -- the model-facing pattern.",
        "Curation reasoning lives in the child's own session, not the parent's context.",
        "The parent sees only the final text via the ToolResultRecord.",
        "Fork semantics: the child inherits the parent's full context, role, and tools.",
        "full text: the tool result is NOT the AgentResult.summary.",
        "composition: ask is not a third primitive, it composes SubagentHost::ask.",
        "provenance: the artifact carries the transcript_ref.",
        "fork-only: no mode parameter on conway_ask.",
        "The agent-id-checked drain ignores sibling TextDeltas.",
        "subscribe-before-launch guarantees no TextDelta is missed.",
        "The child's AgentFinished carries ephemeral: true.",
    ];
    let mut i = 0;
    while s.len() <= 2000 {
        s.push_str(clauses[i % clauses.len()]);
        s.push(' ');
        i += 1;
    }
    s
}

// ---------------------------------------------------------------------
// The slice
// ---------------------------------------------------------------------

/// The `conway_ask` end-to-end slice through the real `Runtime`. One test,
/// six load-bearing assertions (numbered below to match the spec). Parent
/// vs. child backend turns are distinguished by CALL ORDER: the
/// `ScriptedBackend` plays its script sequentially, and the fork/spawn
/// children share the parent's backend (`SubagentHost::start` reuses the
/// runtime's registered backend), so the request order is deterministic --
/// parent turn 1, parent turn 2's first call (conway_ask tool_use), the ask
/// child's turn, parent turn 2's second call (conway_spawn tool_use), the
/// spawn child's turn, parent turn 2's final call. This mirrors how `ask.rs`
/// itself distinguishes parent/child (it scripts two turns and asserts
/// `backend.calls().last()` is the child) -- the sequential `ScriptedBackend`
/// IS the turn counter.
#[tokio::test]
async fn conway_ask_end_to_end_slice_through_the_real_runtime() {
    let brief = long_brief();
    assert!(
        brief.len() > 2000,
        "test fixture: the brief must exceed 2000 chars to prove NOT-truncated; got {}",
        brief.len()
    );
    let ask_prompt = "Summarize the key facts above into a self-contained brief for a fresh spawn, no tool calls";

    let store: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            // 1. Parent turn 1, first backend call: establishes a distinctive
            //    fact in the parent's transcript (approach (b) -- a TextDelta
            //    carrying the fact, in the SAME turn that proposes
            //    `conway_ask`) AND proposes `conway_ask`. The fact is appended
            //    to the parent's log (as the assistant's content) before the
            //    tool executes, so the fork child inherits it.
            ScriptedTurn::Respond(text_and_tool_call_response(
                PARENT_FACT,
                "call_ask_1",
                "conway_ask",
                serde_json::json!({ "prompt": ask_prompt }),
            )),
            // 2. The ephemeral ask-child's one turn: the FULL curated brief
            //    (> 2000 chars). This is the curation reasoning that must
            //    stay in the CHILD's session, not the parent's.
            ScriptedTurn::Respond(text_response(&brief)),
            // 3. Parent turn 1, second backend call: proposes
            //    `conway_spawn` with the curated brief as its prompt (:
            //    ask -> spawn composition).
            ScriptedTurn::Respond(tool_call_response(
                "call_spawn_1",
                "conway_spawn",
                serde_json::json!({ "prompt": brief }),
            )),
            // 4. The spawn child's one turn: completes so the parent's
            //    `conway_spawn` await resolves.
            ScriptedTurn::Respond(text_response("spawn child done")),
            // 5. Parent turn 1, final backend call: a plain text reply that
            //    ends the turn (EndTurn) so the parent's `TurnHandle::result`
            //    resolves cleanly.
            ScriptedTurn::Respond(text_response("parent all done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let conway = build_conway(store.clone(), backend);

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");

    // The whole slice -- fact + conway_ask + conway_spawn -- driven
    // by a single parent prompt. A `start_root` agent's task runs exactly one
    // prompt-to-completion cycle before exiting (`agent_loop.rs`: an `EndTurn`
    // is a `return`, not a loop-back), so a second `handle.prompt` on the same
    // handle would hang waiting for an agent task that has already returned;
    // `ask.rs`'s own "resumed continuation" test rebuilds a fresh `Conway` and
    // uses `conway::resume` for its second turn. Combining the fact and the
    // `conway_ask` invocation into one turn sidesteps that entirely while
    // still satisfying approach (b)'s load-bearing requirement: the
    // distinctive fact IS in the parent's transcript (as the assistant's own
    // text content) before the fork child runs, so the child inherits it.
    let turn = handle
        .prompt("research the project, then use conway_ask to summarize the key facts above into a brief for a fresh spawn, then spawn it")
        .await
        .expect("prompt should succeed");
    let turn_outcome = tokio::time::timeout(Duration::from_secs(10), turn.result()).await;
    turn_outcome
        .expect("turn result must not hang")
        .expect("turn result should succeed");

    let parent_id = handle.id();
    let parent_records = store
        .read(&parent_id, SeqRange::full())
        .await
        .expect("read parent records");

    // ------------------------------------------------------------------
    // Assertion 3 (load-bearing): the `conway_ask` ToolResultRecord carries
    // the FULL reply text (> 2000 chars), NOT a truncated summary.
    // ------------------------------------------------------------------
    let ask_result = parent_records
        .iter()
        .find_map(|r| match r {
            LogRecord::ToolResultRecord { result, .. } if result.tool.as_str() == "conway_ask" => {
                Some(result)
            }
            _ => None,
        })
        .expect("the parent's log must contain a ToolResultRecord for conway_ask");
    let ask_result_text = ask_result
        .blocks
        .iter()
        .find_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .expect("the conway_ask tool result must carry a Text block");
    assert_eq!(
        ask_result_text, brief,
        "the conway_ask tool result must be the child's FULL reply, verbatim"
    );
    assert!(
        ask_result_text.len() > 2000,
        "LOAD-BEARING: the tool result text must exceed 2000 chars (got {}), proving it is \
         AskOutcome.text, NOT AgentResult::summary (which truncates at DEFAULT_SUMMARY_LIMIT=2000)",
        ask_result_text.len()
    );

    // ------------------------------------------------------------------
    // Assertion 4a: the parent's log records the `conway_ask` invocation, as
    // a `ContentBlock::ToolUse` INSIDE the `Assistant` record -- the durable
    // shape for a model-proposed tool call (`LogRecord::ToolCallRecord` was
    // removed; see). The `ToolResultRecord`
    // for the reply IS a separate record (asserted below).
    // ------------------------------------------------------------------
    let ask_call = parent_records
        .iter()
        .find_map(|r| match r {
            LogRecord::Assistant { content, .. } => {
                content.iter().find(|b| matches!(b, ContentBlock::ToolUse { name, .. } if name.as_str() == "conway_ask"))
            }
            _ => None,
        })
        .expect("the parent's log must record the conway_ask tool call (as a ToolUse block)");
    match ask_call {
        ContentBlock::ToolUse { arguments, .. } => {
            assert_eq!(
                *arguments,
                serde_json::json!({ "prompt": ask_prompt }),
                "the conway_ask ToolUse must carry AskArgs {{ prompt, .. }} verbatim"
            );
        }
        _ => unreachable!("matched on ToolUse"),
    }

    // ------------------------------------------------------------------
    // Assertion 4b: curation reasoning stays in the CHILD. The parent's own
    // log has NO `LogRecord::Assistant` carrying the curation reply; the
    // ephemeral child's own log HAS it. (Persisted-record form of the spec's
    // "ThinkingDelta/TextDelta": the child's assistant turn is recorded as a
    // `LogRecord::Assistant` with `ContentBlock::Text`/`Thinking` blocks --
    // the spec's "TextDelta/ThinkingDelta" wording names the live Event
    // variants of the same content.)
    // ------------------------------------------------------------------
    let parent_has_curation = parent_records.iter().any(|r| match r {
        LogRecord::Assistant { content, .. } => content.iter().any(|b| match b {
            ContentBlock::Text { text } | ContentBlock::Thinking { text, .. } => {
                text.contains(BRIEF_SENTINEL)
            }
            _ => false,
        }),
        _ => false,
    });
    assert!(
        !parent_has_curation,
        "the parent's own log must NOT carry the curation reply (the child's assistant turn); \
         found an Assistant record containing the brief sentinel"
    );

    // Locate the ephemeral ask child via the session catalog. The
    // `EphemeralSessionRef` artifact's `id` is the child's `transcript_ref`
    // (); the catalog lookup (`SessionFilter { include_ephemeral: true }`)
    // is the other route to the same `SessionId`. Both are exercised below.
    let with_ephemeral = store
        .list(SessionFilter {
            include_ephemeral: true,
            ..SessionFilter::default()
        })
        .await
        .expect("list with ephemeral should succeed");
    let ask_child_meta = with_ephemeral
        .iter()
        .find(|m| {
            m.ephemeral
                && m.origin.as_ref().map(|o| o.parent) == Some(parent_id)
                && m.origin.as_ref().map(|o| o.mode) == Some(SubagentMode::Fork)
        })
        .expect("the ephemeral ask child must be present in the include_ephemeral listing");
    let ask_child_session = ask_child_meta.id;

    let ask_child_records = store
        .read(&ask_child_session, SeqRange::full())
        .await
        .expect("read ask child records");
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
        "the ephemeral ask child's own log MUST carry the curation reply (its assistant turn \
         containing the brief sentinel); got: {ask_child_records:?}"
    );

    // ------------------------------------------------------------------
    // Assertion 5: catalog hiding. `store.list(SessionFilter::default())`
    // (exclude-ephemeral) does NOT contain the ask child's `SessionId`;
    // `store.list(SessionFilter { include_ephemeral: true, .. })` DOES.
    // (`include_ephemeral` is the real `SessionFilter` field name -- item b's
    // design relies on it.)
    // ------------------------------------------------------------------
    let default_listing = store
        .list(SessionFilter::default())
        .await
        .expect("default list should succeed");
    assert!(
        !default_listing.iter().any(|m| m.id == ask_child_session),
        "the ephemeral ask child must NOT appear in the default (exclude-ephemeral) listing"
    );
    assert!(
        with_ephemeral.iter().any(|m| m.id == ask_child_session),
        "the ephemeral ask child MUST appear in the include_ephemeral listing"
    );

    // ------------------------------------------------------------------
    // Assertion 6 ( composition): `conway_ask` -> `conway_spawn`.
    // The parent's log has the spawn call as a `ContentBlock::ToolUse` inside
    // its `Assistant` record; the spawn child's own first `UserTurn` is the
    // curated brief, verbatim (the text `conway_ask` returned was passed
    // verbatim as the spawn's prompt); and the parent awaited the spawn (the
    // spawn child completed, so the tool returned).
    // ------------------------------------------------------------------
    let spawn_call = parent_records
        .iter()
        .find_map(|r| match r {
            LogRecord::Assistant { content, .. } => {
                content.iter().find(|b| matches!(b, ContentBlock::ToolUse { name, .. } if name.as_str() == "conway_spawn"))
            }
            _ => None,
        })
        .expect("the parent's log must record the conway_spawn tool call (as a ToolUse block)");
    match spawn_call {
        ContentBlock::ToolUse { arguments, .. } => {
            assert_eq!(
                *arguments,
                serde_json::json!({ "prompt": brief }),
                "the conway_spawn ToolUse must carry the curated brief as prompt"
            );
        }
        _ => unreachable!("matched on ToolUse"),
    }

    // The parent awaited the spawn: a ToolResultRecord for conway_spawn
    // exists in the parent's log (it would not be there if the await had not
    // resolved -- the tool only returns after `wait_for_result` completes).
    let spawn_result_present = parent_records
        .iter()
        .any(|r| matches!(r, LogRecord::ToolResultRecord { result, .. } if result.tool.as_str() == "conway_spawn"));
    assert!(
        spawn_result_present,
        "the parent's log must contain a ToolResultRecord for conway_spawn, proving the \
         parent awaited the spawn child's completion ( composition)"
    );

    // The spawn child is non-ephemeral, so it appears in the DEFAULT listing.
    let spawn_child_meta = default_listing
        .iter()
        .find(|m| {
            !m.ephemeral
                && m.origin.as_ref().map(|o| o.parent) == Some(parent_id)
                && m.origin.as_ref().map(|o| o.mode) == Some(SubagentMode::Spawn)
        })
        .expect("the spawn child must appear in the default (non-ephemeral) listing");
    let spawn_child_session = spawn_child_meta.id;

    let spawn_child_records = store
        .read(&spawn_child_session, SeqRange::full())
        .await
        .expect("read spawn child records");
    // The spawn child's own first UserTurn (after the Header) is the spawn
    // prompt -- the curated brief, verbatim.
    let spawn_first_user_turn = spawn_child_records.iter().find_map(|r| match r {
        LogRecord::UserTurn { text, .. } => Some(text.clone()),
        _ => None,
    });
    let spawn_first_user_turn =
        spawn_first_user_turn.expect("the spawn child's own log must have a UserTurn");
    assert_eq!(
        spawn_first_user_turn, brief,
        "the spawn child's first UserTurn must be the curated brief verbatim (the text \
         conway_ask returned was passed verbatim as the spawn's prompt)"
    );

    // Let any lingering child tasks drain so they do not outlive the test.
    let _ = tokio::time::timeout(
        Duration::from_secs(5),
        handle.await_agent(ask_child_meta.agent_id),
    )
    .await;
    let _ = tokio::time::timeout(
        Duration::from_secs(5),
        handle.await_agent(spawn_child_meta.agent_id),
    )
    .await;
}
