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

//! **Graceful unknown-tag degradation, not fail-closed.** Board item
//! `01M03VJPRT8629CYR8JK4A8JPF` retrofits the per-enum degradation table in
//! `docs/plugins/compatibility.md` onto this slice's deserialization: an
//! unknown `ToolCategory` tag degrades to `Execute` (the category plan mode
//! already denies — the most restrictive), an unknown `PermissionClass` tag
//! degrades to `Dangerous`, and an unknown `ContentBlock` type in a `tool/1`
//! answer is dropped, counted, and surfaced (a summary `ContentBlock::Text`
//! naming each dropped block's type tag AND its parse reason is appended, and
//! `is_error` is set so the host knows the output is incomplete). Each
//! unknown tag is NAMED via a `tracing::warn!` at the point of degradation
//! so the convergence is auditable — `#[serde(other)]` is deliberately NOT
//! used because it would silently capture future variants this host SHOULD
//! refuse (these enums are `#[non_exhaustive]`), widening rather than
//! narrowing. The line, stated once here and at each custom deserializer:
//! **an unknown ENUM TAG degrades to the most restrictive value; a missing
//! or structurally-invalid FIELD (a non-string where a string was expected,
//! a missing required `ok`, an `ok:false` with no `error`, an empty
//! manifest id, a non-compiling schema) fails closed.** That is the
//! compatibility table's convergence rule, and it is what lets a host and
//! plugin co-evolve across versions.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use conway::plugin::{
    Artifact, ContentBlock, HostCapability, PermissionClass, ToolCategory, ToolError,
};

/// The wire-protocol major version this host speaks over the persistent
/// transport (board item `01M03VK7MRPSAVWMW7YNYPRPGT`). `major` covers the
/// frame vocabulary and envelope semantics (method names, error-code ranges)
/// -- a mismatch with a plugin's declared `major` is a hard refusal (the two
/// sides cannot agree on what a frame even means). Established here because
/// NO wire-version constants existed before this item; disclosed as `1`, the
/// first version. Bumping it is a breaking change that refuses every plugin
/// built against an older major on load.
pub(crate) const HOST_WIRE_MAJOR: u32 = 1;

/// The wire-protocol minor version this host speaks. `minor` is additive only
/// -- new methods, new optional fields, new capability names, new `Event`
/// variants (see `docs/plugins/compatibility.md`'s versioning table). A plugin
/// declares the minimum minor it requires (`minor_min`); this host accepts any
/// plugin whose `minor_min` is `<=` this constant. Established here at `1`,
/// the first version; bump it when this host gains a feature a plugin might
/// require.
pub(crate) const HOST_WIRE_MINOR: u32 = 1;

/// The `permission.policy/1` version this host speaks over the persistent
/// transport (board item `01M03VKJG7JJ0JEKY265WA7MJ7`). A plugin declares
/// its own version for this point in its `initialize/1` answer's `points`
/// array; this host consults that record via
/// `PersistentSession::point_version` to decide per-point
/// refuse-vs-degrade per `docs/plugins/compatibility.md`'s table. This point
/// is a PARTICIPANT point: a plugin that declares it at a version this host
/// does not support is REFUSED (not degraded), naming the version mismatch;
/// a plugin that does not declare it at all loads normally and contributes
/// no wire policy (advertising a point means the host speaks it, not that
/// the host requires it).
pub(crate) const HOST_PERMISSION_POLICY_VERSION: u32 = 1;

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
    /// Host capabilities this subprocess plugin requires the host to offer
    /// (board item `01M03VJXARFHSDAGHFXGCWKJTY`). `#[serde(default)]` so an
    /// existing plugin that omits the field parses as empty ("needs nothing
    /// the host might lack"). An UNKNOWN cap tag (a string this host's
    /// `HostCapability` enum does not recognize, sent by a NEWER plugin)
    /// FAILS CLOSED -- serde rejects it, the `WireManifest` fails to parse,
    /// and the plugin is refused (`SubprocessPluginError::UnparseableAnswer`)
    /// -- the NARROWING/safe direction, consistent with the unknown-tag item
    /// `01M03VJPRT8629CYR8JK4A8JPF`'s "structural malformation fails closed"
    /// line. No degrade path for unknown host-caps (unlike the
    /// `ToolCategory`/`PermissionClass`/`ContentBlock` degradation table):
    /// a capability requirement is a gate, and silently degrading a cap a
    /// plugin NEEDS into one it does not would load a plugin the host cannot
    /// support. The accepted cap values are mapped into
    /// [`conway::plugin::PluginManifest::required_host_caps`], which the
    /// `conway` builder consults at registration to refuse a plugin whose
    /// declared cap the host lacks (e.g. `persistent_transport` against a
    /// one-shot-only host).
    #[serde(default)]
    pub required_host_caps: Vec<HostCapability>,
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
    /// silently-applied default. An unknown enum TAG (a string this host
    /// does not recognize, sent by a NEWER plugin) degrades to `Dangerous`
    /// via `deserialize_permission_class` — the most restrictive value,
    /// never a silently-permissive one.
    #[serde(deserialize_with = "deserialize_permission_class")]
    pub permission: PermissionClass,
    /// Required for the identical reason `permission` is: a category is
    /// declarative metadata the runtime and any future UI already treat as
    /// meaningful (e.g. `ToolCategory::Delegate` gates fork/spawn-shaped
    /// behavior elsewhere in the tree), so an omission should fail loud,
    /// not silently resolve to whichever variant happens to be first. An
    /// unknown enum TAG (a string this host does not recognize, sent by a
    /// NEWER plugin) degrades to `Execute` via `deserialize_tool_category`
    /// — the category plan mode already denies, the most restrictive value.
    #[serde(deserialize_with = "deserialize_tool_category")]
    pub category: ToolCategory,
}

/// The unclassified `tool/1` answer, deserialized once from stdout before
/// [`parse_tool_result`] decides which of [`WireToolResult`]'s two meanings
/// it carries -- see this module's own doc for why this two-step shape
/// (struct first, classify second) replaces an untagged enum.
///
/// `blocks` is held as raw `serde_json::Value`s here, NOT as typed
/// `ContentBlock`s, so that [`RawToolResult::classify`] can partition known
/// blocks from unknown block types and SURFACE the dropped count (see
/// [`partition_blocks`]) -- a typed `Vec<ContentBlock>` here would silently
/// skip unknown variants via serde, losing the count the compatibility table
/// requires be surfaced. The top-level `ok`/`error` fields stay typed: a
/// missing `ok` or an `ok:false` with no `error` is STRUCTURAL malformation
/// and fails closed (see this module's own doc for the line).
#[derive(Deserialize)]
struct RawToolResult {
    ok: bool,
    #[serde(default)]
    blocks: Vec<serde_json::Value>,
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
    ///
    /// On the success path, `blocks` is partitioned into known
    /// `ContentBlock`s and the blocks that did not parse as one; any
    /// unparseable block is dropped, counted, and surfaced -- a
    /// `ContentBlock::Text` summary naming each dropped block's TYPE TAG and
    /// its parse REASON is APPENDED to the kept blocks, and `is_error` is set
    /// so the host knows the output is incomplete. The summary names the
    /// reason (not just the tag) so an operator sees the ACTUAL condition --
    /// an unknown block TYPE vs. a known type that missed a required field --
    /// rather than a blanket "unknown type" label that would misname a known
    /// tag like `text`. The call still SUCCEEDS (returns `WireToolResult::Ok`),
    /// preserving the known blocks the plugin DID send -- the compatibility
    /// table's "drop+count+surface" rule, not a whole-answer parse failure.
    fn classify(self) -> Result<WireToolResult, String> {
        if self.ok {
            let (known_blocks, dropped) = partition_blocks(self.blocks);
            let any_dropped = !dropped.is_empty();
            let is_error = self.is_error || any_dropped;
            let mut blocks = known_blocks;
            if any_dropped {
                // Surface the drop in the ONLY channel that reaches the
                // caller today: a summary content block in the kept output,
                // plus the `is_error` flag. The observe/1 wire point (a
                // dedicated status channel) is a LATER item; using it would
                // invent a parallel mechanism this slice does not have. The
                // summary NAMES each dropped block's type tag AND its parse
                // reason so the degradation is auditable in-band, not silent
                // -- and so a known type with a missing field is not misnamed
                // an "unknown type".
                let detail = dropped
                    .iter()
                    .map(|d| format!("{}: {}", d.tag, d.reason))
                    .collect::<Vec<_>>()
                    .join("; ");
                let summary = format!(
                    "subprocess plugin returned {} content block(s) that could not be \
                     parsed as a known content block and were dropped ({}); the known \
                     blocks are preserved",
                    dropped.len(),
                    detail
                );
                blocks.push(ContentBlock::Text { text: summary });
            }
            Ok(WireToolResult::Ok {
                blocks,
                is_error,
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

/// A block that could not be parsed as a known [`ContentBlock`], captured
/// for the surfaced summary: the `"type"` tag the plugin sent (or
/// `"<missing type>"` if absent) and the parse REASON. The reason
/// distinguishes an unknown block TYPE (a tag this host does not recognize,
/// the compatibility table's "drop+count+surface" case) from a KNOWN type
/// with structurally-invalid fields (a per-block shape issue, not a
/// whole-answer structural malformation) -- so the summary names the ACTUAL
/// condition instead of a blanket "unknown type" label that would misname a
/// known tag like `text` that merely missed a required field.
struct DroppedBlock {
    tag: String,
    reason: String,
}

/// Partitions raw JSON block values into known `ContentBlock`s and the
/// blocks that did not deserialize as one. A value that deserializes as a
/// `ContentBlock` is kept; a value that does not -- an unknown block TYPE
/// (the compatibility table's "drop+count+surface" case) OR a known type
/// with structurally-invalid fields (a per-block issue, not a whole-answer
/// structural malformation) -- is dropped and captured as a [`DroppedBlock`]
/// for the surfaced summary. Each dropped block is also NAMED via
/// `tracing::warn!` so the degradation is auditable out-of-band.
fn partition_blocks(raw: Vec<serde_json::Value>) -> (Vec<ContentBlock>, Vec<DroppedBlock>) {
    let mut known = Vec::with_capacity(raw.len());
    let mut dropped = Vec::new();
    for value in raw {
        match ContentBlock::deserialize(value.clone()) {
            Ok(block) => known.push(block),
            Err(err) => {
                let tag = value
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("<missing type>")
                    .to_string();
                // Trim the serde reason at ", expected" so the summary does
                // not carry the full list of known variant names (long, and
                // already implied by the tag). "unknown variant `quantum`"
                // and "missing field `text`" both survive the trim intact.
                let reason_full = err.to_string();
                let reason = reason_full
                    .split(", expected")
                    .next()
                    .unwrap_or(&reason_full)
                    .to_string();
                dropped.push(DroppedBlock { tag, reason });
                let last = dropped.last().expect("just pushed");
                tracing::warn!(
                    block_type = %last.tag,
                    dropped_so_far = dropped.len(),
                    reason = %last.reason,
                    "a content block from a subprocess plugin tool/1 answer could not be \
                     parsed as a known ContentBlock; dropping it and surfacing the count"
                );
            }
        }
    }
    (known, dropped)
}

// ----- Custom enum-tag deserializers: degrade unknown TAGS, fail closed on
//       structurally-invalid VALUES. See this module's own doc for the line.
//       `#[serde(other)]` is deliberately NOT used on these `#[non_exhaustive]`
//       enums: it would silently capture future variants this host SHOULD
//       refuse, widening rather than narrowing. Each deserializer below NAMES
//       the unknown tag via `tracing::warn!` so the degradation is auditable.

/// Deserializes a `ToolCategory` from its wire string, degrading an unknown
/// TAG to `ToolCategory::Execute` (the category plan mode already denies --
/// the most restrictive value, per `docs/plugins/compatibility.md`'s wire
/// table). A non-STRING value (null, a number, an object) is structural
/// malformation and fails closed, NOT degraded -- the line is "unknown enum
/// tag degrades; structurally-invalid field fails closed".
fn deserialize_tool_category<'de, D>(deserializer: D) -> Result<ToolCategory, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    let value = serde_json::Value::deserialize(deserializer)?;
    match ToolCategory::deserialize(value.clone()) {
        Ok(category) => Ok(category),
        Err(_) => {
            // Unknown TAG (a string this host does not recognize, sent by a
            // NEWER plugin) -> degrade to Execute, the most restrictive
            // value. A non-string (null/number/object/array) -> fail closed.
            if let Some(tag) = value.as_str() {
                tracing::warn!(
                    unknown_category = %tag,
                    degraded_to = "execute",
                    "unknown ToolCategory tag from a subprocess plugin manifest; \
                     degrading to the most restrictive value (Execute)"
                );
                Ok(ToolCategory::Execute)
            } else {
                Err(D::Error::custom(format!(
                    "expected a ToolCategory string, got non-string value: {value}"
                )))
            }
        }
    }
}

/// Deserializes a `PermissionClass` from its wire string, degrading an
/// unknown TAG to `PermissionClass::Dangerous` (the most restrictive value,
/// per `docs/plugins/compatibility.md`'s wire table). A non-STRING value
/// (null, a number, an object) is structural malformation and fails closed.
fn deserialize_permission_class<'de, D>(deserializer: D) -> Result<PermissionClass, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    let value = serde_json::Value::deserialize(deserializer)?;
    match PermissionClass::deserialize(value.clone()) {
        Ok(class) => Ok(class),
        Err(_) => {
            // Unknown TAG -> degrade to Dangerous, the most restrictive
            // value. A non-string -> fail closed.
            if let Some(tag) = value.as_str() {
                tracing::warn!(
                    unknown_permission = %tag,
                    degraded_to = "dangerous",
                    "unknown PermissionClass tag from a subprocess plugin manifest; \
                     degrading to the most restrictive value (Dangerous)"
                );
                Ok(PermissionClass::Dangerous)
            } else {
                Err(D::Error::custom(format!(
                    "expected a PermissionClass string, got non-string value: {value}"
                )))
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

// ----- initialize/1: the one-time version-negotiation handshake exchanged
//       ONCE at persistent-session open, BEFORE any tool/1 call (board item
//       `01M03VK7MRPSAVWMW7YNYPRPGT`). Rides the SAME id-correlated NDJSON
//       framing as tool/1 -- the request is one JSON-RPC object per line with
//       its own `id`, the response carries the echoed `id`, and the existing
//       reader task routes it by `id` through the SAME pending table (no
//       second reader). See `session`'s own module doc for the framing-reuse
//       decision.
//
// **Wire shape (disclosed here, the authority for this item):**
//
// Host -> plugin request (one NDJSON line):
//   {"id":N,"op":"initialize/1",
//    "host":{"name":"conway","version":"<conway crate version>"},
//    "wire_major":<HOST_WIRE_MAJOR>,"wire_minor":<HOST_WIRE_MINOR>,
//    "points":["tool/1"]}
//
// Plugin -> host response:
//   {"id":N,"ok":true,
//    "major":<P_MAJOR>,"minor_min":<P_MINOR_MIN>,
//    "points":[{"name":"tool/1","version":1},...]}
//
// `ok:false` carries an `"error"` string instead. Unknown fields in the
// plugin's answer are IGNORED-AND-COUNTED (the compatibility table's accept
// branch / forward-compat rule: a newer plugin's extra field does not break
// an older host), NEVER rejected -- the answer is deserialized as a
// `serde_json::Value` first, the known fields are pulled out, the remaining
// keys are counted and surfaced via `tracing::debug!`. A structurally-invalid
// answer (missing `ok`, `ok:false` with no error, a non-number where a number
// was expected) fails CLOSED -- mirroring `RawToolResult::classify`'s own
// "structural malformation fails closed; only KNOWN-shape unknown-FIELD is
// ignored-and-counted" line. `host.version` is put on the wire for the plugin
// to read but NEVER branched on by this host (informational only -- see
// `compatibility.md`'s versioning section).

/// The `host` object this host sends in its `initialize/1` request:
/// `{"name":"conway","version":"<crate version>"}`. `name` is the constant
/// `"conway"`; `version` is this crate's own `CARGO_PKG_VERSION` (the conway
/// workspace version), put on the wire for the plugin to read but NEVER
/// compared by this host -- a TUI-only release does not have to move the
/// protocol, and nothing here is size-of-conway-version-shaped.
#[derive(Serialize)]
pub(crate) struct InitializeHost {
    pub name: &'static str,
    pub version: &'static str,
}

/// An `initialize/1` request framed for the persistent NDJSON transport --
/// one JSON-RPC object per line, correlated by `id` like
/// [`PersistentToolRequest`]. Sent ONCE at session open, before any `tool/1`
/// call; see `session::PersistentSession::initialize`.
#[derive(Serialize)]
pub(crate) struct PersistentInitializeRequest {
    /// JSON-RPC correlation id, assigned by `PersistentSession::initialize`.
    /// Echoed back in the plugin's answer; a mismatch is a protocol error.
    pub id: u64,
    /// The constant op tag this host emits for an initialize request --
    /// `"initialize/1"`.
    pub op: &'static str,
    pub host: InitializeHost,
    pub wire_major: u32,
    pub wire_minor: u32,
    /// The wire points this host speaks over the persistent channel. Today
    /// `tool/1` and `permission.policy/1` -- `observe/1`, `status/1`, and
    /// `context.hook/1` are LATER items, so they are NOT advertised here. A
    /// plugin's per-point version records (see [`InitializeAnswer::points`])
    /// are consulted by those later items to decide per-point refuse-vs-
    /// degrade; this host advertises only what it speaks now. Advertising a
    /// point means the host SPEAKS it, not that the host REQUIRES it -- a
    /// plugin that declares a subset (e.g. `tool/1` only) loads normally
    /// and the absent point's behavior is "the plugin contributes nothing
    /// there"; the participant refusal is VERSION-gated (both speak the
    /// point at incompatible versions), not presence-gated.
    pub points: Vec<&'static str>,
}

impl PersistentInitializeRequest {
    /// The constant op tag this host emits for an `initialize/1` request.
    pub const OP: &'static str = "initialize/1";

    /// Builds the one-time `initialize/1` request this host sends at
    /// persistent-session open. `host.version` is this crate's own
    /// `CARGO_PKG_VERSION` -- informational only, never branched on. The
    /// advertised `points` is `["tool/1", "permission.policy/1"]` (the
    /// persistent wire points this host speaks today -- `permission.policy/1`
    /// added by board item `01M03VKJG7JJ0JEKY265WA7MJ7`); `wire_major`/
    /// `wire_minor` are [`HOST_WIRE_MAJOR`]/[`HOST_WIRE_MINOR`].
    pub(crate) fn new(id: u64) -> Self {
        Self {
            id,
            op: Self::OP,
            host: InitializeHost {
                name: "conway",
                version: env!("CARGO_PKG_VERSION"),
            },
            wire_major: HOST_WIRE_MAJOR,
            wire_minor: HOST_WIRE_MINOR,
            points: vec!["tool/1", "permission.policy/1"],
        }
    }
}

/// The plugin's `initialize/1` answer, parsed and classified from one NDJSON
/// line on stdout. Carries the plugin's own `major`, the minimum `minor` it
/// requires (`minor_min`), and the per-point versions it declares -- the
/// records later wire-point items (permission.policy, observe, status,
/// context.hook) consult to decide per-point refuse-vs-degrade WITHOUT
/// re-negotiating. `unknown_field_count` is the number of fields the plugin's
/// answer carried that this host did not recognize (forward-compat: a newer
/// plugin's extra field does not break an older host -- ignored-and-counted,
/// surfaced via `tracing::debug!` in the `initialize` caller
/// (`session::PersistentSession::initialize`), not in the parser itself).
#[derive(Debug)]
pub(crate) struct InitializeAnswer {
    pub id: u64,
    pub major: u32,
    pub minor_min: u32,
    /// The per-point versions the plugin declared, keyed by point name (e.g.
    /// `"tool/1"`). Stored on `PersistentSession` after a successful handshake
    /// so later items can read it without re-negotiating; see
    /// `PersistentSession::point_version`.
    pub points: HashMap<String, u32>,
    /// The number of unknown fields the plugin's answer carried -- ignored and
    /// counted, NOT rejected (the compatibility table's accept branch).
    pub unknown_field_count: usize,
}

/// The typed failure of [`parse_persistent_initialize_response`]. Splits the
/// two categorically-different failure modes the caller
/// (`PersistentSession::initialize`) maps onto two different
/// `crate::SubprocessPluginError` variants:
///
/// - [`InitializeParseError::Malformed`] -- the answer is structurally broken
///   (not JSON / not an object / missing or non-boolean `ok` / `ok:false` with
///   no `error` string / a non-number `id`/`major`/`minor_min` / a bad
///   `points` entry). The plugin is broken, not declining. Maps to
///   `HandshakeMalformed`.
/// - [`InitializeParseError::Refused`] -- the plugin DELIBERATELY answered
///   `ok:false` WITH an `error` string: it declined initialize. The plugin is
///   incompatible-by-choice, not broken. Maps to `HandshakeRefused`.
///
/// `From<String>` wraps any plain `String` error as `Malformed`, so the many
/// `.ok_or_else(|| "...".to_string())?` sites in the parser body need no
/// change -- `?` auto-wraps a `String` into `Malformed`. Only the `ok:false`
/// site constructs `Refused` explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InitializeParseError {
    Malformed(String),
    Refused(String),
}

impl From<String> for InitializeParseError {
    fn from(s: String) -> Self {
        Self::Malformed(s)
    }
}

/// Deserializes and classifies one persistent NDJSON `initialize/1` response
/// line. Mirrors [`parse_persistent_tool_response`]'s discipline, but splits
/// the failure mode the tool parser leaves undifferentiated: returns
/// [`InitializeParseError::Malformed`] for "not valid JSON", "missing `id`",
/// "missing/non-boolean `ok`", "`ok:false` with no `error` string", or a
/// structurally-invalid `points` entry (each a typed failure at the call site
/// -> `SubprocessPluginError::HandshakeMalformed`, fail-closed), and
/// [`InitializeParseError::Refused`] for a deliberate `ok:false` WITH an
/// `error` string (-> `HandshakeRefused`). Unknown FIELDS in the answer are
/// NOT an error: they are counted into
/// [`InitializeAnswer::unknown_field_count`] and surfaced via
/// `tracing::debug!` in the `initialize` caller, per the compatibility
/// table's accept branch.
pub(crate) fn parse_persistent_initialize_response(
    bytes: &[u8],
) -> Result<InitializeAnswer, InitializeParseError> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|err| err.to_string())?;
    let obj = value
        .as_object()
        .ok_or_else(|| "initialize answer is not a JSON object".to_string())?;

    // `ok` is REQUIRED -- a missing or non-boolean `ok` is structural
    // malformation, fail closed (mirroring RawToolResult::classify).
    let ok = obj
        .get("ok")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| "initialize answer missing or non-boolean `ok` field".to_string())?;
    if !ok {
        // `ok:false` is the plugin DELIBERATELY declining initialize -- a
        // refusal, categorically distinct from a structurally-broken answer.
        // It MUST carry an `error` string explaining why (mirroring
        // `RawToolResult::classify`'s "`ok:false` with no `error` object"
        // fail-closed line): a refusal WITH a reason is `Refused`, while
        // `ok:false` with no (or non-string) `error` is a contract violation
        // -- the plugin said no but broke the shape that says how -- and is
        // `Malformed`, fail-closed. The caller (`PersistentSession::initialize`)
        // maps `Refused` onto `HandshakeRefused` and `Malformed` onto
        // `HandshakeMalformed`, so the operator-facing variant honestly
        // distinguishes "the plugin declined" from "the plugin is broken".
        return Err(match obj.get("error").and_then(|v| v.as_str()) {
            Some(err) => InitializeParseError::Refused(format!("plugin refused initialize: {err}")),
            None => InitializeParseError::Malformed(
                "`ok:false` was returned with no `error` string".to_string(),
            ),
        });
    }

    let id = obj
        .get("id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "initialize answer missing or non-number `id` field".to_string())?;
    let major = obj
        .get("major")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .ok_or_else(|| "initialize answer missing or non-number `major` field".to_string())?;
    let minor_min = obj
        .get("minor_min")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .ok_or_else(|| "initialize answer missing or non-number `minor_min` field".to_string())?;

    // `points`: an array of {"name": string, "version": number}. Each entry
    // must be structurally valid (a missing/non-string `name` or a
    // missing/non-number `version` is structural malformation, fail closed).
    let points_arr = obj
        .get("points")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "initialize answer missing or non-array `points` field".to_string())?;
    let mut points = HashMap::with_capacity(points_arr.len());
    for (i, entry) in points_arr.iter().enumerate() {
        let pobj = entry
            .as_object()
            .ok_or_else(|| format!("initialize answer `points[{i}]` is not an object"))?;
        let name = pobj
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("initialize answer `points[{i}]` missing or non-string `name`"))?
            .to_string();
        let version = pobj
            .get("version")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32)
            .ok_or_else(|| {
                format!("initialize answer `points[{i}]` missing or non-number `version`")
            })?;
        points.insert(name, version);
    }

    // Unknown FIELDS are ignored-and-counted (the compatibility table's
    // accept branch / forward-compat rule). The set of known keys is fixed
    // by the wire shape above; any key outside it is a newer plugin's extra
    // field. The count is returned in [`InitializeAnswer::unknown_field_count`]
    // so the caller (`PersistentSession::initialize`) can surface it via
    // `tracing::debug!` with the plugin's `config_id` attached; it is NEVER
    // rejected here.
    let known = ["id", "ok", "major", "minor_min", "points"];
    let unknown_field_count = obj.keys().filter(|k| !known.contains(&k.as_str())).count();

    Ok(InitializeAnswer {
        id,
        major,
        minor_min,
        points,
        unknown_field_count,
    })
}

// ----- permission.policy/1: the one-time permission-policy declaration a
//       persistent plugin makes at session open, AFTER `initialize/1`
//       succeeds (board item `01M03VKJG7JJ0JEKY265WA7MJ7`). Rides the SAME
//       id-correlated NDJSON framing `initialize/1` and `tool/1` use (via
//       `PersistentSession::framed_round_trip`); NO second reader. The
//       plugin declares per-tool NARROWING verdicts (deny/prompt/abstain --
//       NO `allow`, by type construction: a plugin may only narrow, never
//       widen); the host stores them and the `conway` facade installs them
//       as `PatternOrigin::Plugin` deny/prompt rules in the
//       `PermissionBroker`, subordinate to the operator's own config.
//
// **Wire shape (disclosed here, the authority for this item):**
//
// Host -> plugin request (one NDJSON line):
//   {"id":N,"op":"permission.policy/1"}
//
// Plugin -> host response:
//   {"id":N,"ok":true,
//    "rules":[{"tool":"<name>","verdict":"deny|prompt|abstain","reason":"..."}, ...]}
//
// `ok:false` carries an `"error"` string instead. Unknown FIELDS in the
// answer are IGNORED-AND-COUNTED (the compatibility table's accept branch /
// forward-compat rule), never rejected -- the answer is deserialized as a
// `serde_json::Value` first, the known fields pulled out, the remaining keys
// counted and surfaced via `tracing::debug!` in the caller. An unknown
// `verdict` TAG (a string this host does not recognize, sent by a NEWER
// plugin) FAILS CLOSED as `Malformed` -- this point is a PARTICIPANT point
// (per `docs/plugins/compatibility.md`), and a verdict the host cannot
// classify is structural malformation, not a degrade-to-most-restrictive
// case (unlike the `ToolCategory`/`PermissionClass` degrade table): a
// plugin sending an unknown verdict is making a declaration the host cannot
// honestly interpret, and guessing `deny` would silence a real
// incompatibility rather than naming it. A structurally-invalid answer
// (missing `ok`, `ok:false` with no `error`, a non-array `rules`, a
// per-rule entry missing `tool`/`verdict`) fails CLOSED -- mirroring
// `parse_persistent_initialize_response`'s discipline.

/// A `permission.policy/1` request framed for the persistent NDJSON
/// transport -- one JSON-RPC object per line, correlated by `id` like
/// [`PersistentToolRequest`]/[`PersistentInitializeRequest`]. Sent ONCE at
/// session open, AFTER `initialize/1` succeeds and BEFORE any `tool/1` call;
/// see `session::PersistentSession::request_permission_policy`. Carries no
/// payload: the request asks "what is your policy?", and the plugin's
/// answer is a session-scoped static declaration (per-tool verdicts), not a
/// per-call evaluation.
#[derive(Serialize)]
pub(crate) struct PersistentPermissionPolicyRequest {
    /// JSON-RPC correlation id, assigned by
    /// `PersistentSession::request_permission_policy`. Echoed back in the
    /// plugin's answer; a mismatch is a protocol error.
    pub id: u64,
    /// The constant op tag this host emits for a permission-policy request
    /// -- `"permission.policy/1"`.
    pub op: &'static str,
}

impl PersistentPermissionPolicyRequest {
    /// The constant op tag this host emits for a `permission.policy/1`
    /// request.
    pub const OP: &'static str = "permission.policy/1";

    /// Builds the one-time `permission.policy/1` request this host sends
    /// after `initialize/1` succeeds.
    pub(crate) fn new(id: u64) -> Self {
        Self { id, op: Self::OP }
    }
}

/// The verdict vocabulary a `permission.policy/1` answer carries -- the
/// wire form of [`conway::plugin::PluginPermissionVerdict`]. NARROWING-only
/// by construction: there is no `allow` variant, so a plugin declaring its
/// policy over the wire can never widen what the operator authorized. An
/// unknown TAG (a string this host does not recognize, sent by a NEWER
/// plugin) FAILS CLOSED in the parser -- see the module-level doc above for
/// why this participant point does not degrade an unknown verdict to the
/// most restrictive value the way the `ToolCategory`/`PermissionClass` table
/// does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WirePermissionVerdict {
    Deny,
    Prompt,
    Abstain,
}

/// One per-tool rule in a `permission.policy/1` answer -- the wire form of
/// [`conway::plugin::PluginPermissionRule`]. `tool` is matched exactly (the
/// identical match `Select::Tools([tool])` uses); `reason` is the
/// operator-readable text carried into the broker's rendered denial for a
/// `deny` verdict (unused for `abstain`).
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct WirePermissionRule {
    pub tool: String,
    pub verdict: WirePermissionVerdict,
    /// Defaulted to empty so a plugin that omits it parses -- a `deny`
    /// without a reason is still a valid (if unhelpful) denial, and
    /// defaulting the field is the safe direction (the verdict narrows
    /// either way). An `abstain` carrying a `reason` simply ignores it.
    #[serde(default)]
    pub reason: String,
}

/// The parsed `permission.policy/1` answer: the per-tool rules the plugin
/// declared. Stored on `PersistentSession` for the session's lifetime and
/// read by `SubprocessPlugin::permission_rules` (the `Plugin` trait method
/// the `conway` facade consults to install `PatternOrigin::Plugin` rules).
#[derive(Debug)]
pub(crate) struct PermissionPolicyAnswer {
    pub id: u64,
    pub rules: Vec<WirePermissionRule>,
    /// The number of unknown fields the plugin's answer carried -- ignored
    /// and counted, NOT rejected (the compatibility table's accept branch).
    pub unknown_field_count: usize,
}

/// The typed failure of [`parse_persistent_permission_policy_response`].
/// Mirrors [`InitializeParseError`]'s split so the caller
/// (`PersistentSession::request_permission_policy`) maps the two
/// categorically-different failure modes onto two different
/// `crate::SubprocessPluginError` variants:
///
/// - [`PermissionPolicyParseError::Malformed`] -- the answer is structurally
///   broken (not JSON / not an object / missing or non-boolean `ok` /
///   `ok:false` with no `error` string / a non-number `id` / a non-array
///   `rules` / a per-rule entry missing `tool` or `verdict` / an unknown
///   `verdict` tag). FAILS CLOSED as `HandshakeMalformed` -- a plugin
///   sending a malformed policy answer cannot be trusted to recover, and
///   silently no-op-ing would hide a real incompatibility (acceptance
///   criterion 3: fail-closed, never silently no-op).
/// - [`PermissionPolicyParseError::Refused`] -- the plugin DELIBERATELY
///   answered `ok:false` WITH an `error` string: it declined to declare a
///   policy. Maps to `HandshakeRefused`.
///
/// `From<String>` wraps any plain `String` error as `Malformed`, so the many
/// `.ok_or_else(|| "...".to_string())?` sites in the parser body need no
/// change -- `?` auto-wraps a `String` into `Malformed`. Only the `ok:false`
/// site constructs `Refused` explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PermissionPolicyParseError {
    Malformed(String),
    Refused(String),
}

impl From<String> for PermissionPolicyParseError {
    fn from(s: String) -> Self {
        Self::Malformed(s)
    }
}

/// Deserializes and classifies one persistent NDJSON `permission.policy/1`
/// response line. Mirrors [`parse_persistent_initialize_response`]'s
/// discipline: unknown FIELDS are ignored-and-counted (the compatibility
/// table's accept branch); structural malformation (missing `ok`, `ok:false`
/// with no `error`, a non-number `id`, a non-array `rules`, a per-rule entry
/// missing `tool`/`verdict`, an unknown `verdict` tag) fails CLOSED as
/// `Malformed`; a deliberate `ok:false` WITH an `error` string is `Refused`.
pub(crate) fn parse_persistent_permission_policy_response(
    bytes: &[u8],
) -> Result<PermissionPolicyAnswer, PermissionPolicyParseError> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|err| err.to_string())?;
    let obj = value
        .as_object()
        .ok_or_else(|| "permission.policy answer is not a JSON object".to_string())?;

    let ok = obj
        .get("ok")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| "permission.policy answer missing or non-boolean `ok` field".to_string())?;
    if !ok {
        return Err(match obj.get("error").and_then(|v| v.as_str()) {
            Some(err) => PermissionPolicyParseError::Refused(format!(
                "plugin refused permission.policy/1: {err}"
            )),
            None => PermissionPolicyParseError::Malformed(
                "`ok:false` was returned with no `error` string".to_string(),
            ),
        });
    }

    let id = obj
        .get("id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "permission.policy answer missing or non-number `id` field".to_string())?;

    // `rules`: an array of {tool, verdict, reason?}. An empty array is
    // VALID -- a plugin that declares `permission.policy/1` but contributes
    // no rules is saying "I have no per-tool policy," the same as abstaining
    // on every tool; it is NOT malformation. Each entry must be structurally
    // valid (a missing/non-string `tool` or a missing/non-string `verdict`
    // is structural malformation, fail closed); an unknown `verdict` TAG
    // fails closed at the per-entry `WirePermissionRule` deserialize (serde
    // rejects the unknown variant -- the NARROWING direction, naming the
    // incompatibility rather than guessing).
    let rules_arr = obj
        .get("rules")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "permission.policy answer missing or non-array `rules` field".to_string())?;
    let mut rules = Vec::with_capacity(rules_arr.len());
    for (i, entry) in rules_arr.iter().enumerate() {
        let rule: WirePermissionRule = serde_json::from_value(entry.clone()).map_err(|err| {
            // Trim the serde reason at ", expected" so the detail does not
            // carry the full list of known variant names (long) -- mirroring
            // `partition_blocks`'s own trim. "unknown variant `quantum`" and
            // "missing field `tool`" both survive the trim intact.
            let reason_full = err.to_string();
            let reason = reason_full
                .split(", expected")
                .next()
                .unwrap_or(&reason_full)
                .to_string();
            format!("permission.policy answer `rules[{i}]` is malformed: {reason}")
        })?;
        if rule.tool.is_empty() {
            return Err(
                format!("permission.policy answer `rules[{i}]` has an empty `tool`").into(),
            );
        }
        rules.push(rule);
    }

    let known = ["id", "ok", "rules"];
    let unknown_field_count = obj.keys().filter(|k| !known.contains(&k.as_str())).count();

    Ok(PermissionPolicyAnswer {
        id,
        rules,
        unknown_field_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A well-formed accept-branch answer with an unknown extra field parses
    /// successfully, and the unknown field is COUNTED (not rejected) -- the
    /// compatibility table's accept branch / forward-compat rule. This is the
    /// load-bearing assertion for acceptance criterion 4: the count is
    /// surfaced in a debug/log path (`tracing::debug!` in
    /// `PersistentSession::initialize`, not in this parser), and the answer
    /// is ACCEPTED.
    #[test]
    fn initialize_answer_with_unknown_field_is_accepted_and_counted() {
        let bytes = br#"{"id":1,"ok":true,"major":1,"minor_min":1,"points":[{"name":"tool/1","version":1}],"future_field":"bonus","another":42}"#;
        let answer = parse_persistent_initialize_response(bytes).expect("accept-branch answer");
        assert_eq!(answer.id, 1);
        assert_eq!(answer.major, 1);
        assert_eq!(answer.minor_min, 1);
        assert_eq!(
            answer.points.get("tool/1").copied(),
            Some(1),
            "the tool/1 point version is recorded"
        );
        assert_eq!(
            answer.unknown_field_count, 2,
            "the two unknown fields (`future_field`, `another`) are counted, not rejected"
        );
    }

    /// A missing `ok` is structural malformation -- fail closed, not accepted,
    /// and classified `Malformed` (the plugin is broken, not declining).
    #[test]
    fn initialize_answer_missing_ok_fails_closed_as_malformed() {
        let bytes = br#"{"id":1,"major":1,"minor_min":1,"points":[]}"#;
        let err = parse_persistent_initialize_response(bytes).expect_err("missing ok fails closed");
        match err {
            InitializeParseError::Malformed(detail) => assert!(
                detail.contains("ok"),
                "the malformed detail names the missing field: {detail}"
            ),
            other => panic!("missing ok is Malformed, got {other:?}"),
        }
    }

    /// `ok:false` with no `error` string is a CONTRACT VIOLATION (the shape
    /// requires `ok:false` to carry `error`) -- structural malformation, fail
    /// closed, classified `Malformed` (the plugin said no but broke the shape
    /// that says how). Mirrors `RawToolResult::classify`'s identical line.
    #[test]
    fn initialize_answer_ok_false_with_no_error_is_malformed() {
        let bytes = br#"{"id":1,"ok":false,"major":1,"minor_min":1,"points":[]}"#;
        let err =
            parse_persistent_initialize_response(bytes).expect_err("ok:false with no error fails");
        match err {
            InitializeParseError::Malformed(detail) => assert!(
                detail.contains("error"),
                "the malformed detail names the missing error string: {detail}"
            ),
            other => panic!("ok:false with no error is Malformed, got {other:?}"),
        }
    }

    /// `ok:false` WITH a valid `error` string is the plugin DELIBERATELY
    /// declining initialize -- a refusal, not malformation. Classified `Refused`
    /// carrying the plugin's stated reason, so the caller can surface it as
    /// `HandshakeRefused` (the plugin is incompatible-by-choice, not broken).
    #[test]
    fn initialize_answer_ok_false_with_error_is_refused() {
        let bytes =
            br#"{"id":1,"ok":false,"error":"I do not support initialize/1","major":1,"minor_min":1,"points":[]}"#;
        let err = parse_persistent_initialize_response(bytes)
            .expect_err("ok:false with an error is a refusal");
        match err {
            InitializeParseError::Refused(detail) => {
                assert!(
                    detail.contains("refused initialize"),
                    "the refused detail names the refusal: {detail}"
                );
                assert!(
                    detail.contains("I do not support initialize/1"),
                    "the refused detail carries the plugin's stated reason: {detail}"
                );
            }
            other => panic!("ok:false with error is Refused, got {other:?}"),
        }
    }

    /// A non-number `major` is structural malformation -- fail closed.
    #[test]
    fn initialize_answer_non_number_major_fails_closed() {
        let bytes = br#"{"id":1,"ok":true,"major":"nope","minor_min":1,"points":[]}"#;
        parse_persistent_initialize_response(bytes).expect_err("a non-number major fails closed");
    }

    // ----- permission.policy/1 parser tests (board item
    //       `01M03VKJG7JJ0JEKY265WA7MJ7`). Mirrors the initialize-parser
    //       test discipline: accept-branch-with-unknown-field counts;
    //       structural malformation fails closed; `ok:false`-with-error is
    //       Refused; an unknown verdict tag fails closed (participant point,
    //       not a degrade case).

    /// A well-formed answer with deny/prompt/abstain rules and an unknown
    /// extra field parses, the verdicts are classified correctly, and the
    /// unknown field is COUNTED (not rejected) -- the compatibility table's
    /// accept branch.
    #[test]
    fn permission_policy_answer_with_rules_parses_and_counts_unknown_fields() {
        let bytes = br#"{"id":2,"ok":true,"rules":[{"tool":"greet","verdict":"deny","reason":"no"},{"tool":"bash","verdict":"prompt"},{"tool":"read","verdict":"abstain"}],"future":"bonus"}"#;
        let answer =
            parse_persistent_permission_policy_response(bytes).expect("well-formed answer");
        assert_eq!(answer.id, 2);
        assert_eq!(answer.rules.len(), 3);
        assert_eq!(answer.rules[0].tool, "greet");
        assert_eq!(answer.rules[0].verdict, WirePermissionVerdict::Deny);
        assert_eq!(answer.rules[0].reason, "no");
        assert_eq!(answer.rules[1].verdict, WirePermissionVerdict::Prompt);
        assert_eq!(
            answer.rules[1].reason, "",
            "omitted reason defaults to empty"
        );
        assert_eq!(answer.rules[2].verdict, WirePermissionVerdict::Abstain);
        assert_eq!(
            answer.unknown_field_count, 1,
            "the one unknown field is counted"
        );
    }

    /// An empty `rules` array is VALID -- a plugin declaring the point but
    /// contributing no rules is saying "I have no per-tool policy," the same
    /// as abstaining on every tool. Not malformation.
    #[test]
    fn permission_policy_answer_with_empty_rules_is_accepted() {
        let bytes = br#"{"id":2,"ok":true,"rules":[]}"#;
        let answer = parse_persistent_permission_policy_response(bytes).expect("empty rules ok");
        assert_eq!(answer.rules.len(), 0);
    }

    /// `ok:false` WITH an `error` string is the plugin DELIBERATELY
    /// declining -- `Refused`, not `Malformed`.
    #[test]
    fn permission_policy_answer_ok_false_with_error_is_refused() {
        let bytes = br#"{"id":2,"ok":false,"error":"I do not speak permission.policy/1"}"#;
        let err = parse_persistent_permission_policy_response(bytes)
            .expect_err("ok:false with error is a refusal");
        match err {
            PermissionPolicyParseError::Refused(detail) => {
                assert!(
                    detail.contains("refused permission.policy/1"),
                    "names the point: {detail}"
                );
                assert!(
                    detail.contains("I do not speak permission.policy/1"),
                    "carries the plugin's reason: {detail}"
                );
            }
            other => panic!("ok:false with error is Refused, got {other:?}"),
        }
    }

    /// `ok:false` with NO `error` string is a contract violation --
    /// `Malformed`, fail closed.
    #[test]
    fn permission_policy_answer_ok_false_with_no_error_is_malformed() {
        let bytes = br#"{"id":2,"ok":false,"rules":[]}"#;
        let err = parse_persistent_permission_policy_response(bytes)
            .expect_err("ok:false with no error fails closed");
        assert!(matches!(err, PermissionPolicyParseError::Malformed(_)));
    }

    /// An unknown `verdict` tag fails CLOSED as `Malformed` -- this is a
    /// PARTICIPANT point, and a verdict the host cannot classify is
    /// structural malformation, not a degrade-to-most-restrictive case.
    #[test]
    fn permission_policy_answer_unknown_verdict_tag_fails_closed() {
        let bytes = br#"{"id":2,"ok":true,"rules":[{"tool":"greet","verdict":"quantum"}]}"#;
        let err = parse_persistent_permission_policy_response(bytes)
            .expect_err("an unknown verdict tag must fail closed");
        match err {
            PermissionPolicyParseError::Malformed(detail) => {
                assert!(
                    detail.contains("rules[0]"),
                    "names the offending entry: {detail}"
                );
            }
            other => panic!("unknown verdict is Malformed, got {other:?}"),
        }
    }

    /// A per-rule entry missing `tool` is structural malformation -- fail
    /// closed.
    #[test]
    fn permission_policy_answer_rule_missing_tool_fails_closed() {
        let bytes = br#"{"id":2,"ok":true,"rules":[{"verdict":"deny"}]}"#;
        parse_persistent_permission_policy_response(bytes)
            .expect_err("a rule missing `tool` must fail closed");
    }

    /// An empty `tool` string is structural malformation -- a rule that
    /// names no tool cannot be installed and is not silently dropped.
    #[test]
    fn permission_policy_answer_rule_with_empty_tool_fails_closed() {
        let bytes = br#"{"id":2,"ok":true,"rules":[{"tool":"","verdict":"deny"}]}"#;
        parse_persistent_permission_policy_response(bytes)
            .expect_err("an empty tool name must fail closed");
    }

    /// A non-array `rules` is structural malformation -- fail closed.
    #[test]
    fn permission_policy_answer_non_array_rules_fails_closed() {
        let bytes = br#"{"id":2,"ok":true,"rules":"not-an-array"}"#;
        parse_persistent_permission_policy_response(bytes)
            .expect_err("a non-array rules field must fail closed");
    }
}
