//! The wire vocabulary this host speaks to a subprocess plugin: exactly two
//! request kinds -- `tool.spec/1` (manifest discovery) and `tool/1` (one
//! tool call) -- and the answers this host requires back. See `lib.rs`'s
//! own module doc for why these are the same point names `docs/plugins/
//! hooks.md` points 1/2 already use, and for the disclosed one-shot-exec
//! transport this crate builds alongside the persistent NDJSON transport
//! the later board item `01M03VJHG1WFECFJB4ZH3CKWDX` adds.
//!
//! **Deliberately explicit, never untagged.** A `tool/1` answer's success
//! and failure shapes are told apart by a REQUIRED `"ok"` boolean, not by
//! serde's `#[serde(untagged)]` guessing which variant an object matches --
//! an untagged enum whose success variant has every field optional would
//! happily parse an error envelope (`{"error": {...}}`) as an empty
//! success, silently discarding the error. `RawToolResult` below (an
//! ordinary struct, not an enum) is deserialized once and then
//! deterministically classified by its own `ok` field in
//! [`parse_tool_result`] -- there is no ambiguous JSON shape this parser
//! can misclassify.
//!
//! **Persistent framing reuses this vocabulary, does not parallel it.** The
//! persistent NDJSON transport (see `lib.rs`'s `session` module) carries
//! ONLY `tool/1` over the long-lived channel -- `tool.spec/1` discovery
//! stays one-shot, by design (the spec for item `01M03VJHG1WFECFJB4ZH3CKWDX`
//! says so explicitly: "keep it for discovery ... or as a fallback; the
//! persistent channel is for repeated `tool/1` calls"). That sidesteps the
//! one real wire collision a persistent envelope would otherwise force: a
//! JSON-RPC correlation `id` (a number) against the manifest's own `id`
//! field (the plugin's string identity). On the persistent channel the
//! request is the one-shot `tool/1` body (`op`, `tool`, `call_id`,
//! `arguments` -- the SAME field names [`Request::ToolV1`] already uses)
//! plus a JSON-RPC `id`, and the response is the one-shot `tool/1` answer
//! (`ok`, `blocks`, `is_error`, `artifacts`, `error` -- the SAME
//! [`RawToolResult`] fields) plus the echoed `id`; see
//! [`PersistentToolRequest`] / [`PersistentToolResponse`]. Nothing here
//! invents a second content-block or error vocabulary -- `blocks` and
//! `error` are reused verbatim from the one-shot shape.

use serde::{Deserialize, Serialize};

use conway::plugin::{Artifact, ContentBlock, PermissionClass, ToolCategory, ToolError};

/// One outgoing request this host ever sends, tagged by its own `"op"`
/// field on the wire -- `{"op":"tool.spec/1"}` or `{"op":"tool/1", ...}`.
/// Used by the one-shot path; the persistent path wraps the `tool/1` body
/// in [`PersistentToolRequest`] (same fields plus a JSON-RPC `id`).
#[derive(Serialize)]
#[serde(tag = "op")]
pub(crate) enum Request {
    #[serde(rename = "tool.spec/1")]
    ToolSpecV1,
    #[serde(rename = "tool/1")]
    ToolV1 {
        tool: String,
        call_id: String,
        arguments: serde_json::Value,
    },
}

/// A `tool/1` request framed for the persistent NDJSON transport -- the
/// one-shot [`Request::ToolV1`] body (`op`, `tool`, `call_id`,
/// `arguments` -- the SAME field names, NOT a parallel vocabulary) plus a
/// JSON-RPC `id` this host assigns for correlation. Serialized to one line
/// (`serde_json::to_vec` then `\n`) on the child's stdin; see `session`'s
/// own module doc for the NDJSON framing decision.
#[derive(Serialize)]
pub(crate) struct PersistentToolRequest {
    /// JSON-RPC correlation id, assigned monotonically by
    /// `PersistentSession`. Echoed back in [`PersistentToolResponse::id`];
    /// a response whose `id` does not match the outstanding request is a
    /// protocol error (the session is marked dead, fail-closed).
    pub id: u64,
    /// The one-shot `tool/1` op tag, emitted verbatim (`"tool/1"`) -- NOT a
    /// second op vocabulary, the literal value [`Request::ToolV1`] serializes.
    pub op: &'static str,
    pub tool: String,
    pub call_id: String,
    pub arguments: serde_json::Value,
}

impl PersistentToolRequest {
    /// The constant op tag this host emits for a persistent `tool/1`
    /// request -- the literal `"tool/1"`, the same value
    /// [`Request::ToolV1`] serializes via its `#[serde(rename = "tool/1")]`.
    pub const OP: &'static str = "tool/1";

    /// Builds a persistent `tool/1` request from the same fields the one-shot
    /// path builds [`Request::ToolV1`] from, plus a correlation `id`.
    pub(crate) fn tool_v1(
        id: u64,
        tool: String,
        call_id: String,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id,
            op: Self::OP,
            tool,
            call_id,
            arguments,
        }
    }
}

/// The `tool.spec/1` answer: this subprocess's own declared identity and
/// tool set -- the wire projection of [`conway::plugin::PluginManifest`]
/// plus a [`conway::plugin::ToolSpec`] per declared tool, named `WireTool`
/// below.
#[derive(Clone, Debug, Deserialize)]
pub struct WireManifest {
    /// This plugin's manifest id -- becomes
    /// [`conway::plugin::PluginManifest::id`] verbatim. **Not** validated
    /// against the operator's own `config_id`
    /// ([`crate::SubprocessPluginSpec::config_id`]) -- the two are allowed
    /// to differ (an operator may key a `settings.json` entry however they
    /// like; the plugin's own manifest id is what every OTHER part of the
    /// system, e.g. an operator's `[plugins].install` list for the
    /// in-process tier, would use to refer to it), but see
    /// `crate::SubprocessPlugin::discover`'s own doc.
    pub id: String,
    /// This plugin's own version string, informational only -- becomes
    /// [`conway::plugin::PluginManifest::version`] verbatim, never
    /// compared against anything (this crate performs no semver
    /// compatibility check; a future item may add one).
    pub version: String,
    /// One or more declared tools. Empty is refused
    /// ([`crate::SubprocessPluginError::InvalidManifest`]) -- a plugin
    /// that declares nothing is indistinguishable from a plugin that is
    /// broken, and the honest answer to "an operator configured a
    /// subprocess plugin with nothing to offer" is a build-time error, not
    /// a silent no-op registration.
    pub tools: Vec<WireTool>,
}

/// One tool a [`WireManifest`] declares -- the wire projection of
/// [`conway::plugin::ToolSpec`]. `schema` is raw JSON (not
/// `schemars::schema::RootSchema` directly): a non-Rust plugin author has
/// no way to construct that Rust type, only to emit a JSON Schema document,
/// which is exactly what this field is -- `crate::SubprocessPlugin::
/// discover` parses it into `RootSchema` on this host's own side, the same
/// division of labor `docs/plugins/hooks.md` point 1's own "a schema that
/// fails to compile fails registry construction" rule already describes
/// for an in-process `Plugin`.
#[derive(Clone, Debug, Deserialize)]
pub struct WireTool {
    pub name: String,
    pub description: String,
    pub schema: serde_json::Value,
    /// Required, never defaulted: `docs/plugins/hooks.md`'s value-class
    /// table gives `PermissionClass` real teeth (`RequiresApproval`/
    /// `Dangerous` reach the operator's gate); defaulting an omitted field
    /// to `Safe` would let a plugin author's OMISSION silently grant the
    /// least-scrutinized class, exactly the "no paths declared defaults to
    /// allow" hazard `conway_core::ports::plugin::PathArgs`'s own doc
    /// argues against for a different field. A manifest that omits this is
    /// a parse error (`SubprocessPluginError::UnparseableAnswer`), not a
    /// silently-applied default.
    pub permission: PermissionClass,
    /// Required for the identical reason `permission` is: a category is
    /// declarative metadata the runtime and any future UI already treat as
    /// meaningful (e.g. `ToolCategory::Delegate` gates fork/spawn-shaped
    /// behavior elsewhere in the tree), so an omission should fail loud,
    /// not silently resolve to whichever variant happens to be first.
    pub category: ToolCategory,
}

/// The unclassified `tool/1` answer, deserialized once from stdout before
/// [`parse_tool_result`] decides which of [`WireToolResult`]'s two meanings
/// it carries -- see this module's own doc for why this two-step shape
/// (struct first, classify second) replaces an untagged enum.
#[derive(Deserialize)]
struct RawToolResult {
    ok: bool,
    #[serde(default)]
    blocks: Vec<ContentBlock>,
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    artifacts: Vec<Artifact>,
    #[serde(default)]
    error: Option<WireToolError>,
}

impl RawToolResult {
    /// Classifies this unclassified answer into a [`WireToolResult`]. The
    /// IDENTICAL logic [`parse_tool_result`] (one-shot) and
    /// [`parse_persistent_tool_response`] (persistent NDJSON) both run, just
    /// on different framings of the same `RawToolResult` body -- factored
    /// here so the two framings cannot drift apart.
    fn classify(self) -> Result<WireToolResult, String> {
        if self.ok {
            Ok(WireToolResult::Ok {
                blocks: self.blocks,
                is_error: self.is_error,
                artifacts: self.artifacts,
            })
        } else {
            match self.error {
                Some(err) => Ok(WireToolResult::Err(err)),
                None => Err("\"ok\": false was returned with no \"error\" object".to_string()),
            }
        }
    }
}

/// A `tool/1` response framed for the persistent NDJSON transport -- the
/// one-shot [`RawToolResult`] body (`ok`, `blocks`, `is_error`, `artifacts`,
/// `error` -- the SAME fields, NOT a parallel vocabulary) plus the echoed
/// JSON-RPC `id`. Deserialized from one `\n`-delimited line on the child's
/// stdout; see [`parse_persistent_tool_response`].
#[derive(Deserialize)]
pub(crate) struct PersistentToolResponse {
    /// The echoed correlation id; must match the outstanding request's
    /// [`PersistentToolRequest::id`] -- a mismatch is a protocol error,
    /// not silently re-routed.
    pub id: u64,
    /// The one-shot `tool/1` answer body, flattened in so `ok`/`blocks`/
    /// `is_error`/`artifacts`/`error` sit alongside `id` on the same JSON
    /// object (the wire shape `{"id":N, "ok":true, "blocks":[...], ...}`).
    #[serde(flatten)]
    raw: RawToolResult,
}

/// A `tool/1` call's classified answer -- success (a
/// [`conway::plugin::ToolOutput`]'s own three fields, minus truncation:
/// `crate::SubprocessTool::invoke` always applies
/// [`conway::plugin::TruncationPolicy::None`], since a subprocess plugin's
/// own output has no natural head/tail split this host could apply
/// generically -- a future item may let a manifest declare one) or a typed
/// failure ([`WireToolError`]).
pub enum WireToolResult {
    Ok {
        blocks: Vec<ContentBlock>,
        is_error: bool,
        artifacts: Vec<Artifact>,
    },
    Err(WireToolError),
}

/// A `tool/1` call's declared failure: `kind` maps onto a specific
/// [`conway::plugin::ToolError`] variant
/// (`WireToolErrorKind::into_tool_error`), `detail` is a free-text
/// explanation.
#[derive(Deserialize)]
pub struct WireToolError {
    pub kind: WireToolErrorKind,
    #[serde(default)]
    pub detail: String,
    /// Only meaningful when `kind == Timeout`; ignored otherwise. Default
    /// `0` when the subprocess omits it -- a plugin that reports its own
    /// internal timeout without a duration still reports SOMETHING typed,
    /// rather than this host inventing a number.
    #[serde(default)]
    pub after_secs: u64,
}

/// `tool/1`'s declared failure vocabulary -- deliberately the same shape
/// `conway::plugin::ToolError`'s own non-exhaustive set already offers an
/// in-process `Tool`, so a subprocess plugin author can report exactly the
/// same failures a Rust one could, no narrower.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireToolErrorKind {
    InvalidArguments,
    Denied,
    Cancelled,
    Timeout,
    Io,
    Internal,
}

impl WireToolErrorKind {
    pub(crate) fn into_tool_error(self, detail: String, after_secs: u64) -> ToolError {
        match self {
            Self::InvalidArguments => ToolError::InvalidArguments { detail },
            Self::Denied => ToolError::Denied { reason: detail },
            Self::Cancelled => ToolError::Cancelled,
            Self::Timeout => ToolError::Timeout { after_secs },
            Self::Io => ToolError::Io { detail },
            Self::Internal => ToolError::Internal { detail },
        }
    }
}

/// Deserializes and classifies one `tool/1` answer (one-shot path).
/// `Err(String)` covers both "not valid JSON" and "valid JSON, `ok: false`,
/// but no `error` object" -- both are the identical caller-facing outcome
/// (`SubprocessPluginError::UnparseableAnswer`/`ToolError::Internal`, per
/// call site), so this function does not distinguish them further.
pub(crate) fn parse_tool_result(bytes: &[u8]) -> Result<WireToolResult, String> {
    let raw: RawToolResult = serde_json::from_slice(bytes).map_err(|err| err.to_string())?;
    raw.classify()
}

/// Deserializes and classifies one persistent NDJSON `tool/1` response line
/// -- the one-shot [`parse_tool_result`] shape plus a JSON-RPC `id`. Returns
/// the echoed `id` (for correlation against the outstanding request) and
/// the classified [`WireToolResult`]. `Err(String)` covers "not valid
/// JSON", "missing `id`", and "`ok: false` with no `error` object" -- each a
/// typed failure at the call site (a malformed frame is a parse error, not
/// a deadlock, per item `01M03VJHG1WFECFJB4ZH3CKWDX`'s acceptance criterion 4).
pub(crate) fn parse_persistent_tool_response(
    bytes: &[u8],
) -> Result<(u64, WireToolResult), String> {
    let resp: PersistentToolResponse =
        serde_json::from_slice(bytes).map_err(|err| err.to_string())?;
    Ok((resp.id, resp.raw.classify()?))
}
