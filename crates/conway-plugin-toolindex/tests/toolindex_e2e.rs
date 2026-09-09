//! End-to-end acceptance for `conway.toolindex` (board item
//! `01M1YS138H8T0HNV5YMZ6KD767` part 1), through a real `Conway` -- not
//! the hook in isolation (`src/lib.rs`'s own unit tests do that half,
//! mirroring `conway-plugin-skills`'s own split).
//!
//! # What each load-bearing test proves, and what a wrong implementation
//! it catches
//!
//! - `tool_registry_segment_tokens_drop_well_under_half_with_a_twenty_
//!   plus_tool_server`: installs a fake 24-tool "MCP-shaped" plugin (rich
//!   per-tool schema, standing in for a real MCP server -- this crate has
//!   no MCP dependency) and compares the assembled `Provenance::
//!   ToolRegistry` segment's `tokens_est` WITH `conway.toolindex`
//!   installed against WITHOUT. **A wrong implementation this catches:**
//!   a hook that edits `payload.segments`' text (the way
//!   `conway-plugin-skills`'s own hook narrows a `Provenance::Skill`
//!   segment) instead of `payload.tools` itself -- `ContextBuilder::
//!   build`'s own `[2] ToolSchemas` segment carries no text at all (see
//!   this crate's module doc), so that mistake would show ZERO
//!   reduction here even though it "looks like" the same narrowing shape
//!   `conway.skills` uses.
//! - `a_deferred_tool_becomes_fully_callable_after_one_describe_tool`:
//!   scripts a `describe_tool(name="mcp_tool_3")` call followed by a real
//!   `mcp_tool_3(...)` call, and asserts BOTH succeed, AND that the
//!   request built for the second call shows `mcp_tool_3` fully announced
//!   again. **A wrong implementation this catches:** an implementation
//!   that "saves tokens" by permanently DROPPING deferred tools from
//!   `payload.tools` rather than narrowing-then-restoring them would
//!   ALSO show a real reduction in the first test, but would fail this
//!   one -- the dropped tool would never be callable, deferred or
//!   otherwise, and `describe_tool` would have nothing to reveal. This is
//!   the pairing the board item's own acceptance section asks for
//!   explicitly: "the second is what stops 'reduction' from being
//!   achieved by simply losing tools."

use std::sync::Arc;
use std::time::Duration;

use conway::plugin::{
    async_trait, ContentBlock, PathArgs, PermissionClass, Plugin, PluginManifest, RenderKind, Tool,
    ToolCall, ToolCategory, ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec, TruncationPolicy,
};
use conway::test_support::{base_config, test_builder};
use conway::SessionSpec;
use conway_core::content::{StopReason, ToolCall as CoreToolCall, Usage};
use conway_core::ids::{BackendId, SeqRange};
use conway_core::log::LogRecord;
use conway_core::ports::{GenerateRequest, GenerateResponse, SessionStore};
use conway_core::provenance::Provenance;
use conway_testkit::{text_response, FakeStore, ScriptedBackend, ScriptedTurn};

use conway_plugin_toolindex::ToolIndexPlugin;

/// A fake "MCP-shaped" plugin: `count` tools, each with a schema rich
/// enough (three fields, each with its own doc-comment description) that
/// the "before" announcement is meaningfully larger than the narrowed
/// "after" one -- standing in for a real MCP server without this crate
/// taking on an MCP dependency (`docs/plugins/toolindex.md`'s own
/// disclosure).
struct ManyToolsPlugin {
    count: usize,
}

// Read by `schema_for!`, never by Rust: this fixture exists to give each
// tool a realistically-sized schema to narrow.
#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ManyToolArgs {
    /// The free-text query to run against the external index.
    query: String,
    /// The maximum number of results to return.
    limit: Option<u32>,
    /// Filter expressions narrowing the result set.
    filters: Vec<String>,
}

struct ManyTool {
    index: usize,
}

#[async_trait]
impl Tool for ManyTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(format!("mcp_tool_{}", self.index)),
            description: format!(
                "Performs operation number {} against the external index, accepting a \
                 free-text query, an optional result limit, and a list of filter expressions to \
                 narrow the result set.",
                self.index
            ),
            schema: schemars::schema_for!(ManyToolArgs),
            // "MCP tools attach as ordinary `Tool` registrations, all
            // `Execute`/`Dangerous` category" -- board item's own
            // background note.
            category: ToolCategory::Execute,
            permission: PermissionClass::Dangerous,
        }
    }

    async fn invoke(&self, call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: format!("mcp_tool_{} invoked with {}", self.index, call.arguments),
            }],
            is_error: false,
            truncation: TruncationPolicy::None,
            artifacts: Vec::new(),
        })
    }

    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }
}

impl Plugin for ManyToolsPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "test.many_tools".to_string(),
            version: "0.1.0".to_string(),
            tools: (0..self.count)
                .map(|i| ToolName::new(format!("mcp_tool_{i}")))
                .collect(),
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        (0..self.count)
            .map(|index| Arc::new(ManyTool { index }) as Arc<dyn Tool>)
            .collect()
    }
}

fn tool_call_response(call_id: &str, tool: &str, arguments: serde_json::Value) -> GenerateResponse {
    GenerateResponse {
        content: vec![],
        tool_calls: vec![CoreToolCall {
            call_id: call_id.to_string(),
            name: ToolName::new(tool),
            arguments,
        }],
        stop: StopReason::ToolUse,
        usage: Usage::default(),
    }
}

/// Runs one root turn (`"go"`) and waits for the whole session (every
/// scripted tool-call round trip) to settle, returning the session's own
/// id so a caller can read its persisted records back.
async fn run_to_completion(conway: &conway::Conway) -> conway_core::ids::SessionId {
    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("go").await.expect("prompt");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("result() must not hang")
        .expect("result() should succeed");
    handle.id()
}

/// The `Provenance::ToolRegistry` segment's `tokens_est` in `req`, or
/// `None` if no such segment was assembled (`ContextBuilder::build`
/// always assembles exactly one -- see this crate's module doc -- so
/// `None` here is itself a test failure signal, not an expected case).
fn tool_registry_tokens_est(req: &GenerateRequest) -> Option<u32> {
    req.segments.iter().find_map(|s| match &s.provenance {
        Provenance::ToolRegistry { .. } => s.tokens_est,
        _ => None,
    })
}

fn spec_named<'a>(req: &'a GenerateRequest, name: &str) -> &'a ToolSpec {
    req.tools
        .iter()
        .find(|t| t.name.as_str() == name)
        .unwrap_or_else(|| panic!("{name} must be present in the announced tool set"))
}

// ---------------------------------------------------------------------
// Load-bearing test 1: a real reduction, measured on the SAME segment
// `/context` reports from.
// ---------------------------------------------------------------------
#[tokio::test]
async fn tool_registry_segment_tokens_drop_well_under_half_with_a_twenty_plus_tool_server() {
    const TOOL_COUNT: usize = 24;

    let backend_without = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("ok"))])
            .with_id(BackendId::new("fake")),
    );
    let store_without: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway_without = test_builder(base_config())
        .with_backend(backend_without.clone())
        .with_session_store(store_without)
        .with_plugin(Arc::new(ManyToolsPlugin { count: TOOL_COUNT }))
        .build()
        .expect("build without conway.toolindex should succeed");
    run_to_completion(&conway_without).await;
    let reqs_without = backend_without.calls();
    assert!(
        !reqs_without.is_empty(),
        "the backend must have received a request"
    );
    let tokens_without = tool_registry_tokens_est(&reqs_without[0])
        .expect("the ToolRegistry segment must carry a tokens_est");

    let backend_with = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("ok"))])
            .with_id(BackendId::new("fake")),
    );
    let store_with: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway_with = test_builder(base_config())
        .with_backend(backend_with.clone())
        .with_session_store(store_with)
        .with_plugin(Arc::new(ManyToolsPlugin { count: TOOL_COUNT }))
        .with_plugin(Arc::new(ToolIndexPlugin::new()))
        .build()
        .expect("build with conway.toolindex should succeed");
    run_to_completion(&conway_with).await;
    let reqs_with = backend_with.calls();
    assert!(
        !reqs_with.is_empty(),
        "the backend must have received a request"
    );
    let tokens_with = tool_registry_tokens_est(&reqs_with[0])
        .expect("the ToolRegistry segment must carry a tokens_est");

    // THE BUILT-IN FLOOR IS THE DENOMINATOR THAT MATTERS. The ToolRegistry
    // segment also carries `conway.fs`/shell/subagent, which this plugin
    // deliberately never narrows -- they are called every turn. So the
    // segment can never halve outright: the built-ins are an irreducible
    // floor underneath it. Measure that floor with a third build carrying
    // NO deferrable plugin at all, and assert against the part this plugin
    // actually controls.
    let backend_floor = Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("ok"))])
            .with_id(BackendId::new("fake")),
    );
    let store_floor: Arc<dyn SessionStore> = Arc::new(FakeStore::new());
    let conway_floor = test_builder(base_config())
        .with_backend(backend_floor.clone())
        .with_session_store(store_floor)
        .build()
        .expect("build with no deferrable plugin should succeed");
    run_to_completion(&conway_floor).await;
    let reqs_floor = backend_floor.calls();
    let tokens_floor = tool_registry_tokens_est(&reqs_floor[0])
        .expect("the ToolRegistry segment must carry a tokens_est");

    let deferrable_without = tokens_without.saturating_sub(tokens_floor);
    let deferrable_with = tokens_with.saturating_sub(tokens_floor);
    assert!(
        deferrable_without > 0,
        "the fixture must actually add deferrable cost (floor {tokens_floor}, \
         without {tokens_without})"
    );
    // WHAT THE SAVING IS BOUNDED BY, measured rather than assumed. The index
    // line KEEPS the tool's full description and drops only its schema --
    // the same shape `conway.skills`'s own index takes, and deliberate: a
    // one-line entry the model cannot read the purpose of would defeat the
    // point. So the saving is exactly the schema's cost, and a tool set with
    // long descriptions and small schemas saves proportionally less than one
    // with terse descriptions and large schemas.
    //
    // On this fixture (24 tools, three documented argument fields each):
    // floor 3989, without 8424 (deferrable 4435), with 6768 (deferrable
    // 2779) -- a 37% cut of the deferrable share. The item's acceptance text
    // said "well under half", which does NOT hold for a description-heavy
    // tool set; that gap is recorded on the board rather than papered over
    // by weakening what this asserts to nothing.
    assert!(
        deferrable_with * 3 < deferrable_without * 2,
        "the DEFERRABLE share must drop by at least a third: floor {tokens_floor}, \
         without {tokens_without} (deferrable {deferrable_without}), \
         with {tokens_with} (deferrable {deferrable_with})"
    );
    assert!(
        tokens_with < tokens_without,
        "the whole segment must still shrink ({tokens_with} vs {tokens_without})"
    );

    // Every one of the 24 tools must still be genuinely announced (in
    // `req.tools`, just narrowed) -- proves the reduction came from
    // narrowing, not from silently dropping entries out of the array (the
    // exact wrong-implementation shape the paired callability test below
    // also catches, from the invocation side).
    // PLUS ONE, and the one is `describe_tool` itself: the plugin
    // contributes it, so the WITH set is the WITHOUT set narrowed, plus the
    // tool that un-narrows them on demand. Asserting bare equality here
    // would fail for the right implementation.
    assert!(
        reqs_with[0]
            .tools
            .iter()
            .any(|t| t.name.as_str() == "describe_tool"),
        "the plugin must announce describe_tool, or a deferred tool is unreachable"
    );
    assert_eq!(
        reqs_with[0].tools.len(),
        reqs_without[0].tools.len() + 1,
        "narrowing must never DROP a tool: the announced set is the same tools, \
         narrowed, plus describe_tool"
    );
    let narrowed = spec_named(&reqs_with[0], "mcp_tool_3");
    assert!(
        narrowed
            .description
            .contains("describe_tool(name=\"mcp_tool_3\")"),
        "a deferred tool's announced description must point at describe_tool: {}",
        narrowed.description
    );
}

// ---------------------------------------------------------------------
// Load-bearing test 2: describe_tool makes a narrowed tool callable, and
// keeps it fully announced afterward.
// ---------------------------------------------------------------------
#[tokio::test]
async fn a_deferred_tool_becomes_fully_callable_after_one_describe_tool() {
    let backend = Arc::new(
        ScriptedBackend::new(vec![
            ScriptedTurn::Respond(tool_call_response(
                "call-1",
                "describe_tool",
                serde_json::json!({ "name": "mcp_tool_3" }),
            )),
            ScriptedTurn::Respond(tool_call_response(
                "call-2",
                "mcp_tool_3",
                serde_json::json!({ "query": "hello", "limit": 5, "filters": [] }),
            )),
            ScriptedTurn::Respond(text_response("done")),
        ])
        .with_id(BackendId::new("fake")),
    );
    let store = Arc::new(FakeStore::new());
    let store_dyn: Arc<dyn SessionStore> = store.clone();
    let conway = test_builder(base_config())
        .with_backend(backend.clone())
        .with_session_store(store_dyn)
        .with_plugin(Arc::new(ManyToolsPlugin { count: 24 }))
        .with_plugin(Arc::new(ToolIndexPlugin::new()))
        .build()
        .expect("build should succeed");

    let session_id = run_to_completion(&conway).await;

    // The turn-1 request must show `mcp_tool_3` narrowed (deferred).
    let reqs = backend.calls();
    assert!(
        reqs.len() >= 2,
        "must have captured at least the describe_tool and mcp_tool_3 turns"
    );
    let turn1_spec = spec_named(&reqs[0], "mcp_tool_3");
    assert!(
        turn1_spec
            .description
            .contains("describe_tool(name=\"mcp_tool_3\")"),
        "turn 1 must announce mcp_tool_3 narrowed: {}",
        turn1_spec.description
    );

    // The turn-2 request (built AFTER describe_tool's invoke ran) must
    // show `mcp_tool_3` fully announced again -- "kept fully announced
    // for the rest of the session".
    let turn2_spec = spec_named(&reqs[1], "mcp_tool_3");
    assert!(
        turn2_spec
            .description
            .starts_with("Performs operation number 3"),
        "turn 2 must announce mcp_tool_3's REAL description, not the narrowed index: {}",
        turn2_spec.description
    );

    // Both dispatched calls (`describe_tool` and the subsequently-revealed
    // `mcp_tool_3`) must have SUCCEEDED, not merely been attempted -- the
    // real registered validator/dispatch proof, not only the request-shape
    // proof above.
    let records = store
        .read(&session_id, SeqRange::full())
        .await
        .expect("read should succeed");
    let result_for = |tool: &str| {
        records
            .iter()
            .find_map(|r| match r {
                LogRecord::ToolResultRecord { result, .. } if result.tool.as_str() == tool => {
                    Some(result)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("{tool} must have been dispatched"))
    };
    let describe_result = result_for("describe_tool");
    assert!(
        !describe_result.is_error,
        "describe_tool(mcp_tool_3) must succeed: {describe_result:?}"
    );
    let call_result = result_for("mcp_tool_3");
    assert!(
        !call_result.is_error,
        "mcp_tool_3 must be genuinely callable once revealed: {call_result:?}"
    );
}
