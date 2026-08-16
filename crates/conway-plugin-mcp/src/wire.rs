//! The JSON-RPC 2.0 wire vocabulary this MCP CLIENT speaks to an external MCP
//! SERVER over stdio (board item `01M03GPNF0KN59FHAEEAEY2JD3`): the four
//! method calls a minimal stdio-MCP client needs -- `initialize`,
//! `notifications/initialized` (a one-way notification, no reply),
//! `tools/list`, and `tools/call` -- and the shapes this client requires back.
//!
//! **Hand-rolled, NOT the official `rmcp` SDK or any MCP client library.**
//! MCP's wire protocol IS JSON-RPC 2.0 -- `serde_json` (already in the
//! workspace graph) is the whole codec. The spec's hard constraint and the
//! operator's memory both record that the official SDK pulls an
//! async-runtime/HTTP stack disproportionate to a stdio JSON-RPC codec, and
//! `cargo deny check` has caught an ungranted licence from exactly this kind
//! of addition before. The genuinely new engineering here is the persistent
//! transport process-lifecycle piece (see `session`'s own module doc), NOT
//! the wire format.
//!
//! **Shapes are the MCP stdio subset, cited from the spec.** The
//! `protocolVersion: "2024-11-05"` and the `initialize`/`notifications/
//! initialized`/`tools/list`/`tools/call` method names and params are the
//! well-known MCP 2024-11-05 stdio subset; this crate targets ONLY that
//! subset (HTTP+SSE transport is a SEPARATE item, explicitly out of scope).
//! A server returning a different `protocolVersion` is accepted (the version
//! is informational for this client -- we negotiate the `tools` capability
//! and proceed; we do not branch on the server's protocol version, only on
//! whether the `initialize` result is structurally valid and the `tools`
//! capability is present).
//!
//! **Correlation: JSON-RPC `id` on every REQUEST; no `id` on notifications.**
//! `initialize`, `tools/list`, and `tools/call` are JSON-RPC requests -- each
//! carries a numeric `id` and expects a reply with the echoed `id`.
//! `notifications/initialized` is a JSON-RPC NOTIFICATION -- it carries NO
//! `id` and the server does NOT reply. The reader task in `session` routes
//! inbound lines by `id`; a line with no `id` is an inbound server-initiated
//! notification (out of scope for this minimal client -- dropped with a
//! `tracing::warn!`, the session is NOT torn down: a notification an MCP
//! server pushes is observer-class, not a malformed frame).
//!
//! **Fail-closed on malformed REQUEST responses, fail-soft on unknown content
//! blocks.** `initialize`/`tools/list`/`tools/call` are PARTICIPANT-class
//! request/response (see `docs/plugins/compatibility.md`'s participant-vs-
//! observer distinction): a structurally-invalid response frame, an `id`
//! mismatch, or a JSON-RPC `error` response fails closed -- the session is
//! marked dead and a typed `McpPluginError` surfaces, never a hang and never a
//! silent retry. A `tools/call` RESULT's `content` array, by contrast, is
//! soft: an unknown content block TYPE (not `text`/`image`) is dropped, and a
//! `ContentBlock::Text` note naming the dropped type is appended (mirroring
//! `conway-plugin-subprocess`'s drop+count+surface discipline), UNLESS the
//! server already said `isError: true` -- in that case the error text blocks
//! are preserved and `is_error` is set without an extra note. The call still
//! SUCCEEDS (returns a `ToolOutput`), preserving the blocks the server DID
//! send; only a transport-level failure or a JSON-RPC `error` response fails
//! the call.
//!
//! **HTTP+SSE MCP transport is a SEPARATE item -- do NOT fold it in.** This
//! module is stdio only.

use serde_json::Value;

use conway::plugin::{Artifact, ContentBlock, PermissionClass, ToolCategory, ToolError};

/// The MCP protocol version this client puts on the wire in its `initialize`
/// request. The 2024-11-05 stdio subset (the well-known value; the shapes in
/// this module are drawn from that subset). A server answering with a
/// different `protocolVersion` is accepted -- this client does NOT branch on
/// the server's protocol version, only on structural validity and the
/// `tools` capability.
pub(crate) const CLIENT_PROTOCOL_VERSION: &str = "2024-11-05";

/// The `clientInfo.name` this client advertises in `initialize`.
pub(crate) const CLIENT_NAME: &str = "conway-mcp";

/// The `clientInfo.version` this client advertises in `initialize` (the
/// workspace version, the same versioning discipline every `conway-plugin-*`
/// crate carries).
pub(crate) const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The capabilities this client requests in `initialize`. Today just `tools`
/// -- this client calls `tools/list` and `tools/call` and nothing else. A
/// server that does NOT offer `tools` is refused at discover
/// ([`crate::McpPluginError::HandshakeFailed`]).
pub(crate) fn client_capabilities() -> Value {
    serde_json::json!({ "tools": {} })
}

/// The `clientInfo` object this client puts in `initialize`.
pub(crate) fn client_info() -> Value {
    serde_json::json!({ "name": CLIENT_NAME, "version": CLIENT_VERSION })
}

/// The full `initialize` request body, serialized as one JSON-RPC 2.0 line.
/// `id` is the caller's monotonic JSON-RPC id. Built here so the shape lives
/// in exactly one place.
pub(crate) fn initialize_request(id: u64) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": CLIENT_PROTOCOL_VERSION,
            "capabilities": client_capabilities(),
            "clientInfo": client_info(),
        }
    })
}

/// The `notifications/initialized` notification body -- a JSON-RPC 2.0
/// NOTIFICATION: NO `id`, NO `params` (per the 2024-11-05 subset), NO reply
/// expected. The server must not answer; if it does, the reader routes the
/// stray line by `id` (there is none) to the notification path and drops it.
pub(crate) fn initialized_notification() -> Value {
    serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
}

/// The `tools/list` request body.
pub(crate) fn tools_list_request(id: u64) -> Value {
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": "tools/list" })
}

/// The `tools/call` request body. `name` is the MCP tool name; `arguments` is
/// the call's already-schema-validated `ToolCall::arguments` (passed through
/// verbatim -- MCP's `inputSchema` is JSON Schema, the same shape conway's
/// `ToolSpec::schema` is).
pub(crate) fn tools_call_request(id: u64, name: &str, arguments: &Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    })
}

/// The parsed `initialize` result: the server's `protocolVersion`,
/// `capabilities`, and `serverInfo`. Only `capabilities.tools` is branched on
/// (the server MUST offer `tools`); the rest is surfaced in error messages.
#[derive(Debug, Clone)]
pub(crate) struct InitializeResult {
    pub protocol_version: String,
    pub server_name: String,
    pub server_version: String,
    pub offers_tools: bool,
}

/// Parses a JSON-RPC 2.0 `initialize` response. Fail-closed on every
/// structural problem: a response that is not a JSON object, a missing `id`,
/// an `id` mismatch, a JSON-RPC `error`, a `result` that is not an object, a
/// missing `capabilities`, or a server that does NOT offer `tools`. Returns
/// the typed `InitializeResult` on success, or a detail string for
/// `crate::McpPluginError::HandshakeFailed`.
pub(crate) fn parse_initialize_response(
    value: &Value,
    expected_id: u64,
) -> Result<InitializeResult, String> {
    let id = value.get("id").and_then(|v| v.as_u64());
    if id != Some(expected_id) {
        return Err(format!(
            "initialize response id {id:?} did not match request id {expected_id}"
        ));
    }
    if let Some(err) = value.get("error") {
        return Err(format!("initialize returned a JSON-RPC error: {err}"));
    }
    let result = value
        .get("result")
        .ok_or_else(|| "initialize response has no \"result\"".to_string())?;
    let protocol_version = result
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "initialize result has no string \"protocolVersion\"".to_string())?
        .to_string();
    let capabilities = result
        .get("capabilities")
        .ok_or_else(|| "initialize result has no \"capabilities\" object".to_string())?;
    let offers_tools = capabilities.get("tools").is_some();
    let server_info = result.get("serverInfo");
    let server_name = server_info
        .and_then(|s| s.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("<unknown>")
        .to_string();
    let server_version = server_info
        .and_then(|s| s.get("version"))
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>")
        .to_string();
    Ok(InitializeResult {
        protocol_version,
        server_name,
        server_version,
        offers_tools,
    })
}

/// One tool an MCP server declared in its `tools/list` answer: name,
/// description, and `inputSchema` (a JSON Schema, kept raw so
/// `McpPlugin::discover` can compile it into a `schemars::schema::RootSchema`
/// the SAME way `conway-plugin-subprocess` compiles a wire-declared schema).
#[derive(Debug, Clone)]
pub(crate) struct ListedTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Parses a JSON-RPC 2.0 `tools/list` response. Fail-closed on structural
/// problems (no `result`, `result.tools` not an array, a tool missing `name`,
/// a duplicate name, an `inputSchema` that is not an object); each tool's
/// `description` defaults to the empty string when absent (MCP makes it
/// optional), and `inputSchema` is kept raw for the caller to compile.
pub(crate) fn parse_tools_list_response(
    value: &Value,
    expected_id: u64,
) -> Result<Vec<ListedTool>, String> {
    let id = value.get("id").and_then(|v| v.as_u64());
    if id != Some(expected_id) {
        return Err(format!(
            "tools/list response id {id:?} did not match request id {expected_id}"
        ));
    }
    if let Some(err) = value.get("error") {
        return Err(format!("tools/list returned a JSON-RPC error: {err}"));
    }
    let result = value
        .get("result")
        .ok_or_else(|| "tools/list response has no \"result\"".to_string())?;
    let tools = result
        .get("tools")
        .ok_or_else(|| "tools/list result has no \"tools\" array".to_string())?;
    let arr = tools
        .as_array()
        .ok_or_else(|| "tools/list result \"tools\" is not an array".to_string())?;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(arr.len());
    for (i, t) in arr.iter().enumerate() {
        let name = t
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or_else(|| format!("tools/list tool[{i}] has no string \"name\""))?
            .to_string();
        if name.is_empty() {
            return Err(format!("tools/list tool[{i}] has an empty name"));
        }
        if !seen.insert(name.clone()) {
            return Err(format!(
                "tools/list declared tool name '{name}' is duplicated"
            ));
        }
        let description = t
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .to_string();
        let input_schema = t
            .get("inputSchema")
            .ok_or_else(|| format!("tools/list tool '{name}' has no \"inputSchema\""))?
            .clone();
        if !input_schema.is_object() {
            return Err(format!(
                "tools/list tool '{name}' inputSchema is not an object"
            ));
        }
        out.push(ListedTool {
            name,
            description,
            input_schema,
        });
    }
    Ok(out)
}

/// The parsed `tools/call` result: the content blocks (already mapped to
/// conway `ContentBlock`s, with unknown block types dropped+surfaced), the
/// `isError` flag, and any artifacts (MCP has no artifacts field -- always
/// empty today, kept here so the caller can thread it into `ToolOutput`
/// without a second shape).
#[derive(Debug, Clone)]
pub(crate) struct CallResult {
    pub blocks: Vec<ContentBlock>,
    pub is_error: bool,
    pub artifacts: Vec<Artifact>,
}

/// Parses a JSON-RPC 2.0 `tools/call` response. Returns:
/// - `Ok(CallResult)` on a `result` with a `content` array (the success path
///   -- `isError: true` is still a RESULT, surfaced as `CallResult` with
///   `is_error: true`, NOT a JSON-RPC `error`; the distinction is load-bearing
///   in MCP: `isError` is a tool-level failure the caller reads, a JSON-RPC
///   `error` is a protocol-level failure the transport fails closed on).
/// - `Err(CallError::JsonRpc(code, message))` on a JSON-RPC `error` response
///   (a protocol-level failure -- the session dies, fail-closed).
/// - `Err(CallError::Malformed(detail))` on a structurally-invalid frame or an
///   `id` mismatch (the session dies, fail-closed).
pub(crate) enum CallOutcome {
    Ok(CallResult),
    JsonRpcError { code: i64, message: String },
    Malformed(String),
}

/// A content block that could not be parsed as a known `ContentBlock`,
/// captured for the surfaced note: the `"type"` tag the server sent (or
/// `"<missing type>"`) and the parse reason. Mirrors
/// `conway-plugin-subprocess`'s `DroppedBlock` exactly.
struct DroppedBlock {
    tag: String,
    reason: String,
}

/// Partitions raw JSON content-block values into known `ContentBlock`s and
/// dropped blocks, mirroring `conway-plugin-subprocess::wire::partition_blocks`
/// exactly. MCP content blocks are `{"type":"text","text":"..."}` (-> `Text`)
/// and `{"type":"image","data":"...","mimeType":"..."}` (-> `Image`; MCP uses
/// `data`/`mimeType`, conway's `ContentBlock::Image` uses `data_base64`/
/// `media_type`, so the mapping is explicit, not a straight deserialize).
/// Every other type is dropped+warned+surfaced.
fn partition_content(raw: Vec<Value>) -> (Vec<ContentBlock>, Vec<DroppedBlock>) {
    let mut known = Vec::with_capacity(raw.len());
    let mut dropped = Vec::new();
    for value in raw {
        let tag = value
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("<missing type>")
            .to_string();
        match tag.as_str() {
            "text" => {
                let text = value
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                known.push(ContentBlock::Text { text });
            }
            "image" => {
                let data_base64 = value
                    .get("data")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                let media_type = value
                    .get("mimeType")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_string();
                known.push(ContentBlock::Image {
                    data_base64,
                    media_type,
                });
            }
            _ => {
                // Unknown content block type: drop+warn. The surfaced note is
                // appended by the caller so the caller can decide whether to
                // add it (it does NOT add it when the server already said
                // `isError: true` -- the error text is the relevant output).
                let reason = format!("unknown MCP content block type `{tag}`");
                tracing::warn!(%tag, "dropping an unknown MCP content block type (drop+count+surface: the known blocks are preserved)");
                dropped.push(DroppedBlock { tag, reason });
            }
        }
    }
    (known, dropped)
}

impl CallOutcome {
    /// Builds the outcome from a routed raw JSON-RPC 2.0 value, correlated
    /// against `expected_id`. The classification lives here so the session's
    /// `framed_round_trip` stays shape-agnostic.
    pub(crate) fn from_value(value: &Value, expected_id: u64) -> Self {
        let id = value.get("id").and_then(|v| v.as_u64());
        if id != Some(expected_id) {
            return CallOutcome::Malformed(format!(
                "tools/call response id {id:?} did not match request id {expected_id}"
            ));
        }
        if let Some(err) = value.get("error") {
            let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
            let message = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("<no message>")
                .to_string();
            return CallOutcome::JsonRpcError { code, message };
        }
        let result = match value.get("result") {
            Some(r) => r,
            None => {
                return CallOutcome::Malformed("tools/call response has no \"result\"".to_string())
            }
        };
        let content = result.get("content");
        let content_arr = match content.and_then(|c| c.as_array()) {
            Some(arr) => arr.clone(),
            None => {
                // `content` is required by MCP; a missing/non-array content is
                // a malformed frame, fail-closed.
                return CallOutcome::Malformed(
                    "tools/call result has no \"content\" array".to_string(),
                );
            }
        };
        let is_error = result
            .get("isError")
            .and_then(|e| e.as_bool())
            .unwrap_or(false);
        let (blocks, dropped) = partition_content(content_arr);
        let any_dropped = !dropped.is_empty();
        let mut blocks = blocks;
        // Surface the drop ONLY when the server did NOT already flag an error:
        // an `isError: true` result's error text blocks ARE the relevant
        // output, and appending a drop-note there would bury the error under a
        // secondary complaint. The drop is still warned above either way. A
        // drop flips `is_error` to true (the output is incomplete) -- mirroring
        // `conway-plugin-subprocess::wire::RawToolResult::classify`'s own
        // `is_error = self.is_error || any_dropped`.
        if any_dropped && !is_error {
            let detail = dropped
                .iter()
                .map(|d| format!("{}: {}", d.tag, d.reason))
                .collect::<Vec<_>>()
                .join("; ");
            let note = format!(
                "MCP server returned {} content block(s) that could not be \
                 mapped and were dropped ({}); the known blocks are preserved",
                dropped.len(),
                detail
            );
            blocks.push(ContentBlock::Text { text: note });
        }
        CallOutcome::Ok(CallResult {
            blocks,
            is_error: is_error || any_dropped,
            // MCP has no artifacts field; always empty today.
            artifacts: Vec::new(),
        })
    }
}

/// The conservative category/permission an MCP tool gets by default. An MCP
/// tool is OPAQUE to conway -- its `inputSchema` is arbitrary JSON Schema this
/// host never introspects beyond compiling it, and MCP carries NO conway
/// `category`/`permission` field (the way a conway-subprocess `tool.spec/1`
/// manifest does). The conservative default is therefore the MOST RESTRICTIVE
/// pair, mirroring `conway-plugin-subprocess`'s unknown-tag degradation
/// (`ToolCategory::Execute` -- the category plan mode already denies; and
/// `PermissionClass::Dangerous` -- the widest permission class). An operator
/// who wants a narrower class for a specific MCP tool can narrow it through
/// the ordinary `permissions.json`/`PermissionMode` path, the same way they
/// narrow any other tool.
pub(crate) const DEFAULT_CATEGORY: ToolCategory = ToolCategory::Execute;
pub(crate) const DEFAULT_PERMISSION: PermissionClass = PermissionClass::Dangerous;

/// Maps a JSON-RPC `error` response (a protocol-level failure) onto a
/// `ToolError` -- a `tools/call` that returns a JSON-RPC `error` is a
/// transport/protocol failure, not a tool-level `isError`, so it surfaces as
/// `ToolError::Internal` carrying the error code and message so an operator
/// can tell a broken MCP server apart from a tool that legitimately declined.
pub(crate) fn jsonrpc_error_to_tool_error(code: i64, message: String) -> ToolError {
    ToolError::Internal {
        detail: format!("MCP tools/call JSON-RPC error (code {code}): {message}"),
    }
}
