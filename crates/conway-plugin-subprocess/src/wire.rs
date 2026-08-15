//! The wire vocabulary this host speaks to a subprocess plugin: exactly two
//! request kinds -- `tool.spec/1` (manifest discovery) and `tool/1` (one
//! tool call) -- and the answers this host requires back. See `lib.rs`'s
//! own module doc for why these are the same point names `docs/plugins/
//! hooks.md` points 1/2 already use, and for the disclosed one-shot-exec
//! transport this crate builds instead of the design's persistent
//! connection.
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

use serde::{Deserialize, Serialize};

use conway::plugin::{Artifact, ContentBlock, PermissionClass, ToolCategory, ToolError};

/// One outgoing request this host ever sends, tagged by its own `"op"`
/// field on the wire -- `{"op":"tool.spec/1"}` or `{"op":"tool/1", ...}`.
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

/// Deserializes and classifies one `tool/1` answer. `Err(String)` covers
/// both "not valid JSON" and "valid JSON, `ok: false`, but no `error`
/// object" -- both are the identical caller-facing outcome
/// (`SubprocessPluginError::UnparseableAnswer`/`ToolError::Internal`, per
/// call site), so this function does not distinguish them further.
pub(crate) fn parse_tool_result(bytes: &[u8]) -> Result<WireToolResult, String> {
    let raw: RawToolResult = serde_json::from_slice(bytes).map_err(|err| err.to_string())?;
    if raw.ok {
        Ok(WireToolResult::Ok {
            blocks: raw.blocks,
            is_error: raw.is_error,
            artifacts: raw.artifacts,
        })
    } else {
        match raw.error {
            Some(err) => Ok(WireToolResult::Err(err)),
            None => Err("\"ok\": false was returned with no \"error\" object".to_string()),
        }
    }
}
