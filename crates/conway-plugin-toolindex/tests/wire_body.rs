//! Wiremock, body-level proof that `conway.toolindex` genuinely bounds a
//! deferred tool's announced description on the real wire -- board item
//! `01M3TEKJH2HRK6QBTMJXSC1D77`'s own acceptance criterion.
//!
//! `tests/toolindex_e2e.rs` already proves the token-count reduction and
//! the describe-tool-then-call round trip, both through a
//! `conway_testkit::ScriptedBackend` -- which never serializes a real HTTP
//! request body at all (`GenerateRequest`s are captured as Rust values).
//! This file is the one load-bearing check that closes that gap: a real
//! `OpenAiCompatBackendFactory`-built backend, against a loopback
//! `wiremock` server, with the actual JSON bytes that server received
//! inspected -- mirroring `conway-plugin-backends`'
//! `tests/builder_end_to_end.rs` harness exactly (same `ConwayBuilder::
//! with_backend_factory` + `test_builder_without_router` shape), the
//! discriminating proof that this is the wire body a real backend sends,
//! not a value this test built and inspected without ever serializing it.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use conway::config::schema::{BackendEntry, ConwayConfig, RoleEntry};
use conway::plugin::{
    ContentBlock, PathArgs, PermissionClass, Plugin, PluginManifest, RenderKind, Tool, ToolCall,
    ToolCategory, ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec, TruncationPolicy,
};
use conway::test_support::{base_config, test_builder_without_router};
use conway::SessionSpec;
use conway_core::ids::RoleAlias;
use conway_plugin_backends::OpenAiCompatBackendFactory;
use conway_plugin_toolindex::ToolIndexPlugin;
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A real, multi-sentence MCP-tool-shaped description -- the exact shape
/// DOGFOOD 3 flagged (a real `list_directory` MCP tool's own description
/// ran to ~380 characters, several sentences, kept verbatim pre-fix).
const LONG_DESCRIPTION: &str = "Returns a listing of files and subdirectories within the given \
                                 path, each entry prefixed with [FILE] or [DIR]. Only the \
                                 top-level contents are shown; it does not recurse into \
                                 subdirectories. Use the search tool instead to find a file by \
                                 name across the whole project.";

struct LongDescriptionTool;

#[async_trait]
impl Tool for LongDescriptionTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("list_directory"),
            description: LONG_DESCRIPTION.to_string(),
            schema: schemars::schema_for!(ListDirectoryArgs),
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: "ok".to_string(),
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

#[allow(dead_code)]
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ListDirectoryArgs {
    /// The directory path to list.
    path: String,
}

struct LongDescriptionPlugin;

impl Plugin for LongDescriptionPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "test.long_description".to_string(),
            version: "0.1.0".to_string(),
            tools: vec![ToolName::new("list_directory")],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(LongDescriptionTool)]
    }
}

/// Mirrors `conway-plugin-backends`' own `builder_end_to_end.rs::config`.
fn config(base_url: String) -> ConwayConfig {
    let mut config = base_config();
    config.default_role = RoleAlias::new("coder");
    config.roles.insert(
        "coder".to_string(),
        RoleEntry {
            chain: vec!["mock/echo-model".to_string()],
            headroom_tokens: None,
            ..Default::default()
        },
    );
    config.backends.insert(
        "mock".to_string(),
        BackendEntry {
            kind: "openai-compat".to_string(),
            base_url,
            dialect: Some("openai".to_string()),
            ..BackendEntry::default()
        },
    );
    config
}

/// Mirrors `conway-plugin-backends`' own `builder_end_to_end.rs::sse_body`
/// -- the `"openai"` profile's `tool_calling` default selects the
/// streaming path regardless of whether this turn's response carries any
/// tool calls, so a real SSE body is required here, not a single JSON
/// document.
fn sse_body(events: &[Value]) -> String {
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(&event.to_string());
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deferred_tools_description_reaches_the_real_wire_bounded() {
    let server = MockServer::start().await;
    let body = sse_body(&[
        json!({"choices": [{"delta": {"content": "done"}, "finish_reason": null}]}),
        json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
    ]);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;

    let conway = test_builder_without_router(config(server.uri()))
        .with_backend_factory(Arc::new(OpenAiCompatBackendFactory))
        .with_plugin(Arc::new(LongDescriptionPlugin))
        .with_plugin(Arc::new(ToolIndexPlugin::new()))
        .build()
        .expect("build should succeed");

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .expect("new_session should succeed");
    let turn = handle.prompt("hi").await.expect("prompt should succeed");
    let _ = tokio::time::timeout(Duration::from_secs(10), turn.result())
        .await
        .expect("turn must not hang")
        .expect("turn should succeed");

    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 1, "exactly one real HTTP request");
    let body: Value = requests[0]
        .body_json()
        .expect("the real request body must be valid JSON");

    let tools = body["tools"]
        .as_array()
        .expect("a real `tools` array must be present on the wire");
    let list_directory = tools
        .iter()
        .find(|t| t["function"]["name"] == "list_directory")
        .expect("list_directory must be announced (narrowed, never dropped)");

    let wire_description = list_directory["function"]["description"]
        .as_str()
        .expect("description must be a string");

    assert!(
        wire_description.len() < LONG_DESCRIPTION.len(),
        "the real wire description ({} chars) must be shorter than the full description \
         ({} chars): {wire_description:?}",
        wire_description.len(),
        LONG_DESCRIPTION.len()
    );
    // The wire `description` IS the whole index entry (`name: <narrowed
    // description> (call describe_tool(...))`), not the bare description
    // alone -- `ToolIndexHook::before_request` replaces `spec.description`
    // with `index_entry`'s full line (`src/lib.rs`'s own doc). A real
    // untruncated index entry for `LONG_DESCRIPTION` would run to the
    // prefix/suffix overhead plus the full 287-character description --
    // well past 220 chars; the bound below is comfortably under that while
    // leaving room for the (short, truncated) description plus that same
    // fixed overhead.
    assert!(
        wire_description.len() <= 220,
        "the real wire description must be bounded to a short index entry, not an MCP \
         server's full multi-sentence text: {} chars: {wire_description:?}",
        wire_description.len()
    );
    assert!(
        !wire_description.contains("Only the top-level"),
        "the second sentence of the real description must never reach the wire: \
         {wire_description:?}"
    );
    assert!(
        wire_description.contains("describe_tool"),
        "the index entry must still point at describe_tool: {wire_description:?}"
    );

    // The real schema must be narrowed too -- not only the description.
    let wire_parameters = &list_directory["function"]["parameters"];
    assert_eq!(
        wire_parameters,
        &json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "title": "DeferredToolArgs",
            "type": "object",
        }),
        "the real wire parameters must be the trivial deferred placeholder, not \
         list_directory's own real schema: {wire_parameters}"
    );
}
