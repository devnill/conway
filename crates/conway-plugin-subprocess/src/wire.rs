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

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use conway::plugin::{
    Artifact, CapabilityError, ContentBlock, HostCapability, PermissionClass, ResultStatus,
    ToolCategory, ToolError,
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

/// The `observe/1` version this host speaks over the persistent transport
/// (board item `01M03VKQ738DTGHHK2C4RWXC0E`). A plugin declares its own
/// version for this point in its `initialize/1` answer's `points` array; this
/// host consults that record via `PersistentSession::point_version` to decide
/// per-point engage-vs-degrade per `docs/plugins/compatibility.md`'s table.
/// This point is an OBSERVER point: a plugin that declares it at a version
/// this host does not support is DEGRADED (loaded WITHOUT the point, with a
/// `tracing::warn!` naming both versions) -- the observer rule, the OPPOSITE
/// of the participant refusal `permission.policy/1` uses. A plugin that does
/// not declare it at all loads normally and contributes no observe
/// subscription (advertising a point means the host speaks it, not that the
/// host requires it).
pub(crate) const HOST_OBSERVE_VERSION: u32 = 1;

/// The `status.declare/1` version this host speaks over the persistent
/// transport (board item `01M03VKQ738DTGHHK2C4RWXC0E`). Same
/// version-negotiation shape as [`HOST_OBSERVE_VERSION`]: an OBSERVER point,
/// DEGRADED on an unsupported version (load without the point, warn) rather
/// than refused. A plugin that does not declare it loads normally and the
/// host routes no `status/1` notifications for it.
pub(crate) const HOST_STATUS_VERSION: u32 = 1;

/// One outgoing request this host ever sends, tagged by its own `"op"`
/// field on the wire -- `{"op":"tool.spec/1"}`, `{"op":"tool/1", ...}`, or
/// `{"op":"capability/1", ...}`. Used by the one-shot path; the persistent
/// path wraps the `tool/1` body in [`PersistentToolRequest`] and the
/// `capability/1` body in [`PersistentCapabilityRequest`] (same fields plus
/// a JSON-RPC `id`).
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
    /// A capability call this host forwards to a subprocess plugin that
    /// declared `capability` in its `WireManifest::provides` -- the
    /// out-of-process leg of Edge B
    /// (`docs/vision/DESIGN-plugin-dependencies.md` §2), closed by board
    /// item `01M0XXXX3HK8914NE418P5GNRY`. `capability` is the SAME wire
    /// string [`HostCapability::as_wire_str`] produces (never a second name
    /// vocabulary); `payload` is opaque, whatever the calling plugin's own
    /// `CapabilityCallHandle::call` sent, unread and unvalidated by this
    /// host beyond ordinary JSON parsing -- the identical "this host does
    /// not interpret the body" posture [`Self::ToolV1`]'s own `arguments`
    /// already has for `tool/1`.
    #[serde(rename = "capability/1")]
    CapabilityV1 {
        capability: String,
        payload: serde_json::Value,
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

/// A `capability/1` request framed for the persistent NDJSON transport --
/// the one-shot [`Request::CapabilityV1`] body (`op`, `capability`,
/// `payload` -- the SAME field names, NOT a parallel vocabulary) plus a
/// JSON-RPC `id` this host assigns for correlation. Mirrors
/// [`PersistentToolRequest`] exactly, one level over: the SAME
/// `tool/1`-vs-`capability/1` reuse [`Request`]'s own doc states for the
/// one-shot forms, applied to the persistent framing too.
#[derive(Serialize)]
pub(crate) struct PersistentCapabilityRequest {
    /// JSON-RPC correlation id, assigned monotonically by
    /// `PersistentSession`. Echoed back in
    /// [`PersistentCapabilityResponse::id`]; a response whose `id` does not
    /// match the outstanding request is a protocol error (the session is
    /// marked dead, fail-closed -- the SAME posture
    /// [`PersistentToolRequest::id`]'s own doc states for `tool/1`).
    pub id: u64,
    /// The one-shot `capability/1` op tag, emitted verbatim
    /// (`"capability/1"`) -- NOT a second op vocabulary, the literal value
    /// [`Request::CapabilityV1`] serializes.
    pub op: &'static str,
    pub capability: String,
    pub payload: serde_json::Value,
}

impl PersistentCapabilityRequest {
    /// The constant op tag this host emits for a persistent `capability/1`
    /// request -- the literal `"capability/1"`, the same value
    /// [`Request::CapabilityV1`] serializes via its
    /// `#[serde(rename = "capability/1")]`.
    pub const OP: &'static str = "capability/1";

    /// Builds a persistent `capability/1` request from the same fields the
    /// one-shot path builds [`Request::CapabilityV1`] from, plus a
    /// correlation `id`.
    pub(crate) fn capability_v1(id: u64, capability: String, payload: serde_json::Value) -> Self {
        Self {
            id,
            op: Self::OP,
            capability,
            payload,
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
    /// the host might lack"). `HostCapability` is now an OPEN vocabulary
    /// (board item `01M0WWKA8K1E7JPK87J6RRQMZF`, which opened it from a
    /// closed two-variant enum): two core-blessed bare names plus a
    /// shape-checked `Named(String)` catch-all for anything else a plugin
    /// declares -- which splits "an unknown cap tag" into two DIFFERENT
    /// failure modes at two DIFFERENT seams, not one. A MALFORMED tag
    /// (empty, or failing `crate::event_name::validate_event_name`'s shape
    /// check) still FAILS CLOSED at parse -- serde rejects it, the
    /// `WireManifest` fails to parse, and the plugin is refused
    /// (`SubprocessPluginError::UnparseableAnswer`) -- unchanged, and
    /// consistent with the unknown-tag item
    /// `01M03VJPRT8629CYR8JK4A8JPF`'s "structural malformation fails
    /// closed" line. A WELL-FORMED but previously-unknown tag (sent by a
    /// NEWER plugin), by contrast, now PARSES -- resolving to
    /// `HostCapability::Named` -- and is refused LATER, at the
    /// host-capability gate (`conway::HostCaps::check_manifest`, consulted
    /// at registration), with the SAME `PluginError::MissingHostCapability`
    /// naming both the plugin and the cap (see
    /// `an_unknown_required_host_cap_is_refused_by_the_gate_not_by_the_parser`,
    /// `crates/conway-plugin-subprocess/tests/mechanism.rs`). Rejecting a
    /// well-formed name at parse would mean no third party could ever
    /// declare a capability the core has not blessed, defeating the point
    /// of opening the vocabulary -- the fail-closed guarantee is not
    /// weakened, it moved and got sharper. No degrade path for an unoffered
    /// host-cap in either case (unlike the
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
    /// Host capabilities this subprocess plugin would like to use but can
    /// work without -- the wire projection of
    /// [`conway::plugin::PluginManifest::optional_host_caps`] (board item
    /// `01M0WWKA8K1E7JPK87J6RRQMZF`), closed for the out-of-process tier by
    /// THIS item (`01M0XXXX3HK8914NE418P5GNRY`): until now `WireManifest`
    /// had no field to carry it, so `crate::SubprocessPlugin::discover`
    /// mapped it as an unconditional empty `Vec` regardless of what a
    /// subprocess plugin declared -- a real gap between the two plugin
    /// tiers this field closes, not a new capability.
    ///
    /// **Deserializes as [`HostCapability`], the SAME type
    /// [`Self::required_host_caps`] uses, on the SAME fail-closed
    /// boundary -- deliberately, not a lighter check for the optional
    /// case.** A MALFORMED tag still fails `WireManifest` parsing outright
    /// (`SubprocessPluginError::UnparseableAnswer`); a WELL-FORMED but
    /// previously-unknown tag still parses to `HostCapability::Named` (the
    /// sharpened boundary board item `01M0XKP5BWCPY3BHPJZHXKR4H3` put in
    /// [`Self::required_host_caps`]'s own doc, reused verbatim here rather
    /// than re-litigated). Only what happens with a WELL-FORMED name AFTER
    /// parsing differs from the required field: a required cap the host
    /// does not offer refuses the plugin at the host-capability gate
    /// (`PluginError::MissingHostCapability`); an optional one loads the
    /// plugin degraded, and the degradation is announced -- see
    /// [`conway::plugin::PluginManifest::optional_host_caps`]'s own doc for
    /// the two-channel announcement (`tracing::warn!` plus a
    /// `ConfigWarning { code: WarningCode::OptionalHostCapabilityMissing }`).
    /// That announce-vs-refuse split is `crates/conway/src/builder.rs`'s
    /// job, applied uniformly to every `PluginManifest::optional_host_caps`
    /// regardless of which plugin tier produced it -- `crate::
    /// SubprocessPlugin::discover` maps this field into that SAME
    /// `PluginManifest` field verbatim, never a parallel degrade path built
    /// for the out-of-process tier alone.
    ///
    /// `#[serde(default)]`, the same reason [`Self::required_host_caps`]
    /// has it: a manifest that predates this field (or simply omits it)
    /// parses as empty -- "nothing about this plugin degrades based on a
    /// host capability's absence" -- never a deserialization error. Test
    /// both directions: `crate::wire::tests::wire_manifest_without_
    /// optional_host_caps_key_defaults_to_empty` (an existing manifest
    /// parses unchanged) and
    /// `crate::wire::tests::wire_manifest_optional_host_caps_round_trips`
    /// (a declared one is carried through).
    #[serde(default)]
    pub optional_host_caps: Vec<HostCapability>,
    /// Capability NAMES this subprocess plugin registers a live provider
    /// for -- the wire declaration half of Edge B
    /// (`docs/vision/DESIGN-plugin-dependencies.md` §2,
    /// `crate::ports::capability`'s own module doc) for the out-of-process
    /// tier, closed by THIS item (`01M0XXXX3HK8914NE418P5GNRY`). Before this
    /// field existed, `WireManifest` had no way for a subprocess plugin to
    /// say "I answer capability calls for this name" at all -- Edge B's own
    /// channel (`CapabilityProvider`/`CapabilityRegistry`) is JSON-in/
    /// JSON-out and object-safe specifically so an out-of-process provider
    /// could implement it (see that module's own doc), but nothing carried
    /// a subprocess plugin's declaration onto the wire until now, which is
    /// the exact gap this item's own title names: "a plugin written in
    /// Python is quietly less capable than the identical plugin written in
    /// Rust."
    ///
    /// **Deserializes as [`HostCapability`] -- the SAME open, namespaced
    /// vocabulary [`Self::required_host_caps`]/[`Self::optional_host_caps`]
    /// already validate through [`HostCapability::named`] /
    /// `conway_core::event_name::validate_event_name`'s shared shape check
    /// (reused via `conway_core`, not reimplemented) -- on the SAME
    /// fail-closed boundary those two fields use: a MALFORMED name fails
    /// `WireManifest` parsing outright; a WELL-FORMED name (bare or
    /// `namespace.name`) parses, `Named` included. One vocabulary, one
    /// boundary, for all three fields on this struct -- the guard rail this
    /// item's own spec states explicitly ("`provides` must not take a
    /// different boundary from `required_host_caps` in the same
    /// struct").**
    ///
    /// **Does NOT map into [`conway::plugin::PluginManifest`] -- unlike every
    /// other field on this struct.** `PluginManifest` carries no `provides`
    /// field at all: `Plugin::capabilities() -> Vec<CapabilityRegistration>`
    /// is a TRAIT method returning live `Arc<dyn CapabilityProvider>`
    /// objects, not static manifest data (see that trait method's own doc,
    /// "Deliberately a trait method, not a `PluginManifest` field" --
    /// `PluginManifest` is a plain struct literal at three dozen call sites
    /// across the workspace, and a required field there breaks every one at
    /// once). `crate::SubprocessPlugin::discover` therefore reads this field
    /// to build `CapabilityRegistration`s directly (each wrapping a
    /// `SubprocessCapabilityProvider` that forwards a call across this
    /// plugin's own transport -- one-shot exec or the persistent session,
    /// whichever this plugin's `SubprocessPluginSpec::transport` selected),
    /// returned from `SubprocessPlugin`'s own `Plugin::capabilities` impl --
    /// never routed through `PluginManifest` at all. This is an
    /// IMPLEMENTATION of [`conway::plugin::CapabilityProvider`], the same
    /// existing trait an in-process provider implements, not a second,
    /// parallel registration path invented for the out-of-process tier (see
    /// this item's own spec: "this should be an implementation of an
    /// existing trait, not a new parallel path").
    ///
    /// A capability name repeated within ONE manifest is refused at
    /// `crate::SubprocessPlugin::discover`
    /// (`SubprocessPluginError::InvalidManifest`), fail-closed, mirroring
    /// this struct's own duplicate-tool-name check exactly (`discover`'s own
    /// doc): letting a self-duplicate slide would surface later as a
    /// same-plugin-vs-itself `DuplicateCapabilityProvider` at
    /// `CapabilityRegistry::from_registrations`, indistinguishable from a
    /// genuine cross-plugin conflict -- a confusing failure mode for a bug
    /// that is entirely local to this one manifest.
    ///
    /// `#[serde(default)]`: a manifest that predates this field (or simply
    /// omits it, the common case) parses as empty -- "provides nothing
    /// callable" -- never a deserialization error.
    #[serde(default)]
    pub provides: Vec<HostCapability>,
    /// Plugin ids this subprocess plugin's stated function cannot perform
    /// at all without -- the wire projection of
    /// [`conway::plugin::PluginManifest::requires`], carried verbatim
    /// (name-only, no version constraint; see that field's own doc).
    /// `#[serde(default)]` so an existing plugin manifest that predates this
    /// field (or simply omits it) parses as empty -- "depends on nothing" --
    /// never a deserialization error; `docs/plugins/compatibility.md`'s
    /// versioning table calls a new optional field a `minor`-compatible
    /// addition for exactly this reason. Mapped into
    /// [`conway::plugin::PluginManifest::requires`] by
    /// `crate::SubprocessPlugin::discover` and checked there by the SAME
    /// `ConwayBuilder::build` dependency-resolution code an in-process
    /// plugin's `requires` already goes through -- no parallel resolution
    /// path.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Plugin ids whose absence degrades only a presentation or convenience
    /// of this subprocess plugin -- the wire projection of
    /// [`conway::plugin::PluginManifest::optional`], carried verbatim
    /// (name-only, no version constraint; see that field's own doc).
    /// `#[serde(default)]`, the same reason [`Self::requires`] has it: a
    /// manifest predating this field parses as empty, never an error.
    #[serde(default)]
    pub optional: Vec<String>,
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

// ----- capability/1: the plugin-to-plugin capability CALL a subprocess
//       plugin answers when its own `WireManifest::provides` names a
//       capability another (in-process or out-of-process) plugin invokes
//       through `CapabilityCallHandle::call` -- the out-of-process leg of
//       Edge B, closed by board item `01M0XXXX3HK8914NE418P5GNRY`. Deliberately
//       the SAME two-shape discipline `tool/1` already established just above
//       (a REQUIRED `ok` boolean tells success and failure apart, never an
//       untagged guess; `RawCapabilityResult::classify` is the identical
//       "parse once, classify by `ok`" split `RawToolResult::classify` uses),
//       reused rather than reinvented for this second call kind:
//
// Success response (one-shot stdout, or a persistent NDJSON line):
//   {"ok": true, "result": <any JSON value, default null>}
// Declared-failure response:
//   {"ok": false, "error": {"message": "...", "detail": <any JSON value>}}
//
// `error` deserializes as [`CapabilityError`] directly -- the SAME
// `Serialize`/`Deserialize` type `crate::ports::capability`'s own module doc
// says a provider "constructs...from whatever its own wire answer carries"
// (that type's own doc, describing exactly this scenario), not a
// subprocess-only error shape invented in parallel. `ok:false` with no
// `error` object is a CONTRACT VIOLATION -- structural malformation, fails
// closed, mirroring `RawToolResult::classify`'s identical line for `tool/1`
// verbatim (this module's own doc states the line once: "a missing or
// structurally-invalid FIELD...fails closed").

/// The unclassified `capability/1` answer, deserialized once before
/// [`RawCapabilityResult::classify`] decides which of [`WireCapabilityResult`]'s
/// two meanings it carries -- mirrors [`RawToolResult`]'s own two-step shape
/// (struct first, classify second), one level over for the second call kind.
#[derive(Deserialize)]
struct RawCapabilityResult {
    ok: bool,
    /// The provider's successful answer -- opaque JSON, unread and
    /// unvalidated by this host: whatever shape THIS capability's own
    /// request/response contract defines, per
    /// `conway_core::ports::capability::CapabilityProvider::call`'s own
    /// doc ("undefined by this module; a future capability's own doc
    /// states its own request/response shape"). `#[serde(default)]` so an
    /// `ok:true` answer that omits `result`
    /// parses as `Value::Null` -- a provider with nothing to return (a
    /// void-shaped capability) still answers SOMETHING typed, rather than
    /// this host inventing a value, mirroring [`CapabilityError::detail`]'s
    /// own `Value::Null`-via-default precedent one type over.
    #[serde(default)]
    result: serde_json::Value,
    /// `Some` only on a declared failure (`ok:false`); deserializes as
    /// [`CapabilityError`] directly -- see this section's own doc.
    #[serde(default)]
    error: Option<CapabilityError>,
}

impl RawCapabilityResult {
    /// Classifies this unclassified answer into a [`WireCapabilityResult`] --
    /// the IDENTICAL split [`RawToolResult::classify`] runs for `tool/1`,
    /// reused for this second call kind rather than a parallel rule: `ok:true`
    /// succeeds with whatever `result` carries (default `Value::Null`);
    /// `ok:false` WITH an `error` object succeeds as the declared
    /// [`CapabilityError`]; `ok:false` with NO `error` object is a contract
    /// violation and fails closed.
    fn classify(self) -> Result<WireCapabilityResult, String> {
        if self.ok {
            Ok(WireCapabilityResult::Ok(self.result))
        } else {
            match self.error {
                Some(err) => Ok(WireCapabilityResult::Err(err)),
                None => Err("\"ok\": false was returned with no \"error\" object".to_string()),
            }
        }
    }
}

/// A `capability/1` response framed for the persistent NDJSON transport --
/// the one-shot [`RawCapabilityResult`] body (`ok`, `result`, `error` -- the
/// SAME fields, NOT a parallel vocabulary) plus the echoed JSON-RPC `id`.
/// Mirrors [`PersistentToolResponse`] exactly, one level over.
#[derive(Deserialize)]
pub(crate) struct PersistentCapabilityResponse {
    /// The echoed correlation id; must match the outstanding request's
    /// [`PersistentCapabilityRequest::id`] -- a mismatch is a protocol
    /// error, not silently re-routed (mirrors
    /// [`PersistentToolResponse::id`]'s own doc).
    pub id: u64,
    /// The one-shot `capability/1` answer body, flattened in so `ok`/
    /// `result`/`error` sit alongside `id` on the same JSON object.
    #[serde(flatten)]
    raw: RawCapabilityResult,
}

/// A `capability/1` call's classified answer -- either the provider's
/// successful `result` value, or its declared [`CapabilityError`]. Mirrors
/// [`WireToolResult`]'s own two-variant shape, one level over: this is the
/// wire projection `crate::SubprocessCapabilityProvider::call` classifies
/// into the `Result<serde_json::Value, CapabilityError>`
/// [`conway::plugin::CapabilityProvider::call`]'s own trait signature
/// requires.
///
/// `Debug` is derived here where [`WireToolResult`] does not derive it, for
/// one reason: this type's own parse tests assert on the FAILURE side with
/// `expect_err`, which requires the ok-side to be printable. Both members
/// (`serde_json::Value`, [`CapabilityError`]) already are, so the derive
/// costs nothing and buys a legible message when a fail-closed parse test
/// regresses.
#[derive(Debug)]
pub(crate) enum WireCapabilityResult {
    Ok(serde_json::Value),
    Err(CapabilityError),
}

/// Deserializes and classifies one `capability/1` answer (one-shot path).
/// `Err(String)` covers both "not valid JSON" and "valid JSON, `ok: false`,
/// but no `error` object" -- mirrors [`parse_tool_result`]'s own identical
/// two-cause `Err(String)`, reused rather than distinguished further here
/// either (the caller, `SubprocessCapabilityProvider::call`, maps both onto
/// one `CapabilityError`, the same way `SubprocessTool::invoke` maps
/// `parse_tool_result`'s onto one `ToolError::Internal`).
pub(crate) fn parse_capability_result(bytes: &[u8]) -> Result<WireCapabilityResult, String> {
    let raw: RawCapabilityResult = serde_json::from_slice(bytes).map_err(|err| err.to_string())?;
    raw.classify()
}

/// Deserializes and classifies one persistent NDJSON `capability/1` response
/// line -- the one-shot [`parse_capability_result`] shape plus a JSON-RPC
/// `id`. Returns the echoed `id` (for correlation against the outstanding
/// request) and the classified [`WireCapabilityResult`]. Mirrors
/// [`parse_persistent_tool_response`]'s own discipline exactly, one level
/// over: `Err(String)` covers "not valid JSON", "missing `id`", and
/// "`ok: false` with no `error` object" -- a malformed frame is a typed parse
/// error at the call site (`session::PersistentSession::capability_round_trip`
/// marks the session dead and reports `SubprocessPluginError::
/// MalformedFrame`, projected onto [`CapabilityError`] -- see that method's
/// own doc for which existing posture this reuses), never a deadlock.
pub(crate) fn parse_persistent_capability_response(
    bytes: &[u8],
) -> Result<(u64, WireCapabilityResult), String> {
    let resp: PersistentCapabilityResponse =
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
    /// `tool/1`, `permission.policy/1`, `observe/1`, and `status.declare/1`
    /// -- `context.hook/1` is a LATER item, so it is NOT advertised here. A
    /// plugin's per-point version records (see [`InitializeAnswer::points`])
    /// are consulted by the later wire-point items to decide per-point
    /// refuse-vs-degrade; this host advertises only what it speaks now.
    /// Advertising a point means the host SPEAKS it, not that the host
    /// REQUIRES it -- a plugin that declares a subset (e.g. `tool/1` only)
    /// loads normally and the absent point's behavior is "the plugin
    /// contributes nothing there"; the participant refusal is VERSION-gated
    /// (both speak the point at incompatible versions), not presence-gated,
    /// and the observer points (`observe/1`, `status.declare/1`) DEGRADE
    /// rather than refuse even on a version mismatch.
    pub points: Vec<&'static str>,
}

impl PersistentInitializeRequest {
    /// The constant op tag this host emits for an `initialize/1` request.
    pub const OP: &'static str = "initialize/1";

    /// Builds the one-time `initialize/1` request this host sends at
    /// persistent-session open. `host.version` is this crate's own
    /// `CARGO_PKG_VERSION` -- informational only, never branched on. The
    /// advertised `points` is `["tool/1", "permission.policy/1",
    /// "observe/1", "status.declare/1"]` (the persistent wire points this
    /// host speaks today -- `permission.policy/1` added by board item
    /// `01M03VKJG7JJ0JEKY265WA7MJ7`; `observe/1` and `status.declare/1` added
    /// by board item `01M03VKQ738DTGHHK2C4RWXC0E`); `wire_major`/
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
            points: vec![
                "tool/1",
                "permission.policy/1",
                "observe/1",
                "status.declare/1",
            ],
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

// ----- observe/1 + status.declare/1 / status/1 (board item
//       `01M03VKQ738DTGHHK2C4RWXC0E`). Two OBSERVER-class wire points over the
//       persistent NDJSON transport, both ONE-WAY (no JSON-RPC `id`, no
//       response) once engaged, both DEGRADE on an unsupported version (the
//       observer rule, the OPPOSITE of `permission.policy/1`'s participant
//       refusal). `observe/1` is host -> plugin (the host emits matching
//       `Event`s as outbound no-`id` notifications on the plugin's stdin);
//       `status.declare/1` / `status/1` is plugin -> host (the plugin pushes
//       status notifications as inbound no-`id` lines on its stdout). The
//       one-time engagement exchange for each rides the SAME id-correlated
//       NDJSON framing `initialize/1` and `tool/1` use (via
//       `PersistentSession::framed_round_trip`); NO second reader. The
//       one-way notifications themselves ride the RAW writer/reader, NOT
//       `framed_round_trip`.
//
// **Wire shapes (disclosed here, the authority for this item):**
//
// observe/1 engagement (host -> plugin request, plugin -> host response):
//   {"id":N,"op":"observe/1"}
//   {"id":N,"ok":true,"events":["turn_started",...] | ["*"]}
// `ok:false` carries an `"error"` string instead (the plugin declines to
// observe -- degrade, load without the point). Unknown FIELDS in the answer
// are ignored-and-counted (the compatibility table's accept branch). The
// `events` array is the plugin's SELECTOR: `["*"]` subscribes to every event;
// any other list subscribes to exactly the named `Event` tags. An unknown tag
// in the selector (one this host's `Event` enum does not produce) is IGNORED
// with a `tracing::warn!` -- the one enum-versioning case where "ignore" is
// correct, because an observer changes nothing by construction. A structurally
// invalid answer (missing `ok`, `ok:false` with no `error`, a non-array
// `events`, a non-string entry) DEGRADES too (warn, load without the point):
// an observer cannot fail the run by construction, so the host loads the
// plugin regardless and simply does not engage the point.
//
// observe/1 notification (host -> plugin, one-way, no `id`):
//   {"op":"observe/1",...the Event's own flattened fields...}\n
// The `Event` is serialized via its own `#[serde(tag = "event")]` shape and
// the `"op":"observe/1"` field is merged in alongside its fields, so one
// notification is one flat JSON object per line. The host filters by the
// plugin's declared selector BEFORE serializing; an `Event` whose tag the
// plugin did not select is never written. `Event::Lagged { skipped }` is
// forwarded regardless of the selector (it is the lossy-with-notice notice a
// slow consumer needs, mirroring `conway::EventStream`'s discipline).
//
// status.declare/1 engagement (host -> plugin request, plugin -> host
// response):
//   {"id":N,"op":"status.declare/1"}
//   {"id":N,"ok":true,"keys":[{"key":"build","max_len":80,"ttl_ms":5000},...]}
// `ok:false` carries an `"error"` string instead (decline -> degrade). Same
// ignore-and-count / degrade-on-malformation discipline as observe/1.
//
// status/1 notification (plugin -> host, one-way, no `id`):
//   {"op":"status/1","key":"<key>","status":"<ResultStatus tag>","value":"<text>"}\n
// The host's reader routes ANY no-`id` line to a bounded notification channel
// (drop+warn on overflow, never blocks the host turn); a handler task parses
// `op` and, for `op == "status/1"`, maps `status` to a `ResultStatus`. An
// unknown `ResultStatus` tag degrades to `ResultStatus::Failed` (the
// compatibility table's `ResultStatus` row, never `Completed`); a missing
// `op` / unknown `op` / structurally-invalid notification is dropped with a
// `tracing::warn!` (observer-class, degrade -- never fails the session).

/// An `observe/1` request framed for the persistent NDJSON transport -- one
/// JSON-RPC object per line, correlated by `id` like the other persistent
/// requests. Sent ONCE at session open, AFTER `initialize/1` and
/// `permission.policy/1` succeed; see
/// `session::PersistentSession::request_observe`. Carries no payload: the
/// request asks "what do you want to observe?", and the plugin's answer is its
/// selector.
#[derive(Serialize)]
pub(crate) struct PersistentObserveRequest {
    pub id: u64,
    pub op: &'static str,
}

impl PersistentObserveRequest {
    pub const OP: &'static str = "observe/1";

    pub(crate) fn new(id: u64) -> Self {
        Self { id, op: Self::OP }
    }
}

/// A `status.declare/1` request framed for the persistent NDJSON transport.
/// Sent ONCE at session open, AFTER `observe/1`; see
/// `session::PersistentSession::request_status_declare`. Carries no payload:
/// the request asks "what status keys will you push?", and the plugin's answer
/// is its per-key declaration metadata.
#[derive(Serialize)]
pub(crate) struct PersistentStatusDeclareRequest {
    pub id: u64,
    pub op: &'static str,
}

impl PersistentStatusDeclareRequest {
    pub const OP: &'static str = "status.declare/1";

    pub(crate) fn new(id: u64) -> Self {
        Self { id, op: Self::OP }
    }
}

/// The selector a plugin declares in its `observe/1` answer -- which host
/// `Event`s it wants to receive as notifications. `All` subscribes to every
/// event; `Tags` subscribes to exactly the named `Event` tags. An unknown tag
/// (one this host's `Event` enum does not produce) is KEPT in the set as
/// declared -- the host simply never has a matching event to forward, and the
/// selector entry is silently inert (warned once at engagement so the
/// degradation is auditable). This is the host-side half of "an unknown
/// `Event` tag is IGNORED": the plugin ASKED for something the host does not
/// produce, and the host loads normally without ever having a match to send.
#[derive(Clone, Debug)]
pub(crate) enum ObserveSelector {
    All,
    Tags(HashSet<String>),
}

impl ObserveSelector {
    /// `true` if an `Event` whose serialized `event` tag is `tag` should be
    /// forwarded to the plugin. `Event::Lagged` is ALWAYS forwarded
    /// regardless of the selector -- it is the lossy-with-notice notice a
    /// slow consumer needs, mirroring `conway::EventStream`'s own
    /// unconditional `Lagged` passthrough.
    pub(crate) fn matches(&self, tag: &str) -> bool {
        if tag == "lagged" {
            return true;
        }
        match self {
            ObserveSelector::All => true,
            ObserveSelector::Tags(set) => set.contains(tag),
        }
    }
}

/// The typed failure of [`parse_persistent_observe_response`]. Observer
/// points DEGRADE rather than fail closed, so the caller
/// (`PersistentSession::request_observe`) maps EVERY variant onto "load
/// without the point, `tracing::warn!`" -- there is no `Refused`-vs-
/// `Malformed` split to drive two different `SubprocessPluginError` variants
/// the way the participant points do. The split exists only so the warn
/// message can honestly distinguish "the plugin declined" (`Refused`) from
/// "the plugin is broken" (`Malformed`), the same honesty
/// `initialize/1`'s own split provides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ObserveParseError {
    /// The answer is structurally broken (not JSON / not an object / missing
    /// or non-boolean `ok` / `ok:false` with no `error` / a non-array
    /// `events` / a non-string entry). DEGRADE: load without the point.
    Malformed(String),
    /// The plugin DELIBERATELY answered `ok:false` WITH an `error` string: it
    /// declined to observe. DEGRADE: load without the point.
    Refused(String),
}

impl From<String> for ObserveParseError {
    fn from(s: String) -> Self {
        Self::Malformed(s)
    }
}

/// Deserializes and classifies one persistent NDJSON `observe/1` response
/// line. Returns the plugin's declared [`ObserveSelector`]. Unknown FIELDS are
/// ignored-and-counted (surfaced via `tracing::debug!` in the caller, not
/// here). EVERY failure mode DEGRADES: a structurally-invalid answer is
/// [`ObserveParseError::Malformed`], a deliberate `ok:false`-with-error is
/// [`ObserveParseError::Refused`], and the caller maps both onto "load without
/// the point, warn" -- an observer cannot fail the run by construction.
pub(crate) fn parse_persistent_observe_response(
    bytes: &[u8],
) -> Result<(ObserveSelector, usize), ObserveParseError> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|err| err.to_string())?;
    let obj = value
        .as_object()
        .ok_or_else(|| "observe/1 answer is not a JSON object".to_string())?;

    let ok = obj
        .get("ok")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| "observe/1 answer missing or non-boolean `ok` field".to_string())?;
    if !ok {
        return Err(match obj.get("error").and_then(|v| v.as_str()) {
            Some(err) => ObserveParseError::Refused(format!("plugin refused observe/1: {err}")),
            None => ObserveParseError::Malformed(
                "`ok:false` was returned with no `error` string".to_string(),
            ),
        });
    }

    // `events`: an array of string tags. `["*"]` -> All; any other list ->
    // Tags(set). An empty array is Tags(empty) -- a plugin that declares
    // observe/1 but selects nothing is saying "I want no events," an inert
    // but valid selector (not malformation). A non-string entry is structural
    // malformation -> degrade (Malformed).
    let events_arr = obj
        .get("events")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "observe/1 answer missing or non-array `events` field".to_string())?;
    let mut tags: HashSet<String> = HashSet::with_capacity(events_arr.len());
    let mut all = false;
    for (i, entry) in events_arr.iter().enumerate() {
        let tag = entry
            .as_str()
            .ok_or_else(|| format!("observe/1 answer `events[{i}]` is not a string"))?;
        if tag == "*" {
            all = true;
        } else {
            tags.insert(tag.to_string());
        }
    }
    let selector = if all {
        ObserveSelector::All
    } else {
        ObserveSelector::Tags(tags)
    };

    let known = ["id", "ok", "events"];
    let unknown_field_count = obj.keys().filter(|k| !known.contains(&k.as_str())).count();
    Ok((selector, unknown_field_count))
}

/// One per-key declaration in a `status.declare/1` answer -- the metadata a
/// plugin declares for a status key it will push via `status/1` notifications
/// (per `docs/plugins/hooks.md` point 12: `{ max_len, ttl_ms }`). The host
/// stores these for the facade surface; the ttl/expiry RENDER path itself
/// stays design-only (point 12's "Status" row).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct StatusDeclaration {
    pub key: String,
    #[serde(default)]
    pub max_len: Option<u64>,
    #[serde(default)]
    pub ttl_ms: Option<u64>,
}

/// The typed failure of [`parse_persistent_status_declare_response`].
/// Identical discipline to [`ObserveParseError`]: every variant DEGRADES (load
/// without the point, warn); the split only distinguishes "declined"
/// (`Refused`) from "broken" (`Malformed`) for an honest warn message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StatusDeclareParseError {
    Malformed(String),
    Refused(String),
}

impl From<String> for StatusDeclareParseError {
    fn from(s: String) -> Self {
        Self::Malformed(s)
    }
}

/// Deserializes and classifies one persistent NDJSON `status.declare/1`
/// response line. Returns the per-key declarations and the unknown-field
/// count. Every failure mode DEGRADES (observer point): a structurally
/// invalid answer is [`StatusDeclareParseError::Malformed`], a deliberate
/// `ok:false`-with-error is [`StatusDeclareParseError::Refused`], and the
/// caller maps both onto "load without the point, warn".
pub(crate) fn parse_persistent_status_declare_response(
    bytes: &[u8],
) -> Result<(Vec<StatusDeclaration>, usize), StatusDeclareParseError> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|err| err.to_string())?;
    let obj = value
        .as_object()
        .ok_or_else(|| "status.declare/1 answer is not a JSON object".to_string())?;

    let ok = obj
        .get("ok")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| "status.declare/1 answer missing or non-boolean `ok` field".to_string())?;
    if !ok {
        return Err(match obj.get("error").and_then(|v| v.as_str()) {
            Some(err) => {
                StatusDeclareParseError::Refused(format!("plugin refused status.declare/1: {err}"))
            }
            None => StatusDeclareParseError::Malformed(
                "`ok:false` was returned with no `error` string".to_string(),
            ),
        });
    }

    // `keys`: an array of {key, max_len?, ttl_ms?}. An empty array is VALID --
    // a plugin that declares the point but contributes no keys is saying "I
    // have no status to push," the same inert-but-valid shape an empty
    // observe selector takes. Each entry must carry a non-empty `key` string;
    // a missing/empty/non-string `key` is structural malformation -> degrade.
    let keys_arr = obj
        .get("keys")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "status.declare/1 answer missing or non-array `keys` field".to_string())?;
    let mut decls = Vec::with_capacity(keys_arr.len());
    for (i, entry) in keys_arr.iter().enumerate() {
        let decl: StatusDeclaration = serde_json::from_value(entry.clone()).map_err(|err| {
            let reason_full = err.to_string();
            let reason = reason_full
                .split(", expected")
                .next()
                .unwrap_or(&reason_full)
                .to_string();
            format!("status.declare/1 answer `keys[{i}]` is malformed: {reason}")
        })?;
        if decl.key.is_empty() {
            return Err(format!("status.declare/1 answer `keys[{i}]` has an empty `key`").into());
        }
        decls.push(decl);
    }

    let known = ["id", "ok", "keys"];
    let unknown_field_count = obj.keys().filter(|k| !known.contains(&k.as_str())).count();
    Ok((decls, unknown_field_count))
}

/// One status contribution parsed from an inbound no-`id` `status/1`
/// notification line -- the wire form of [`conway::plugin::PluginStatusContribution`].
/// `status` is the already-degraded [`ResultStatus`] (an unknown wire tag
/// became `ResultStatus::Failed` at parse time, per the compatibility table).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WireStatusContribution {
    pub key: String,
    pub status: ResultStatus,
    pub value: String,
}

/// Maps a wire `status` string tag to a [`ResultStatus`], degrading an
/// UNKNOWN tag to `ResultStatus::Failed` (the compatibility table's
/// `ResultStatus` row -- never `Completed`). The `value` string populates the
/// variant's own text field where one exists (`Failed::error`,
/// `Cancelled::reason`, `BudgetExceeded::limit`); for `Completed` and
/// `Rejected` it is carried alongside unchanged. `#[serde(other)]` is
/// deliberately NOT used on `ResultStatus` (it is `#[non_exhaustive]`): an
/// unknown tag is NAMED via the `Failed` variant's `error` string so the
/// degradation is auditable, rather than silently captured.
pub(crate) fn parse_status_notification(
    value: &serde_json::Value,
) -> Result<WireStatusContribution, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| "status/1 notification is not a JSON object".to_string())?;
    let key = obj
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "status/1 notification missing or non-string `key`".to_string())?
        .to_string();
    if key.is_empty() {
        return Err("status/1 notification has an empty `key`".to_string());
    }
    let status_tag = obj
        .get("status")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "status/1 notification missing or non-string `status`".to_string())?;
    let value_str = obj
        .get("value")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let status = match status_tag {
        "completed" => ResultStatus::Completed,
        "failed" => ResultStatus::Failed {
            error: value_str.clone(),
        },
        "cancelled" => ResultStatus::Cancelled {
            reason: value_str.clone(),
        },
        "budget_exceeded" => ResultStatus::BudgetExceeded {
            limit: value_str.clone(),
        },
        "rejected" => ResultStatus::Rejected {
            missing: Vec::new(),
        },
        unknown => {
            tracing::warn!(
                unknown_status = %unknown,
                degraded_to = "failed",
                key = %key,
                "unknown ResultStatus tag in a status/1 notification from a subprocess plugin; \
                 degrading to Failed (never Completed), per the compatibility table"
            );
            ResultStatus::Failed {
                error: format!("unknown status tag: {unknown}"),
            }
        }
    };
    Ok(WireStatusContribution {
        key,
        status,
        value: value_str,
    })
}

/// Builds the outbound no-`id` `observe/1` notification line for one `Event`:
/// `{"op":"observe/1",...the Event's own flattened fields...}\n`. The `Event`
/// is serialized via its own `#[serde(tag = "event")]` shape, the `"event"`
/// tag is checked against `selector` (an `Event` whose tag the plugin did not
/// select returns `None` -- the caller drops it silently; `Event::Lagged` is
/// ALWAYS forwarded regardless of the selector, per [`ObserveSelector::
/// matches`]), then `"op"` is merged into the resulting object so one
/// notification is one flat JSON object per line. Returns `None` if the
/// `Event` is filtered out OR fails to serialize (should not happen for any
/// constructed `Event`, but fail-safe: drop at the caller rather than
/// panicking the host turn).
pub(crate) fn build_observe_notification(
    event: &conway::plugin::Event,
    selector: &ObserveSelector,
) -> Option<Vec<u8>> {
    let mut value = serde_json::to_value(event).ok()?;
    let tag = value.get("event").and_then(|v| v.as_str()).unwrap_or("");
    if !selector.matches(tag) {
        return None;
    }
    if let Some(obj) = value.as_object_mut() {
        // `op` is inserted LAST so it never collides with an `Event` field
        // name (no `Event` variant carries a field named `op`).
        obj.insert("op".to_string(), serde_json::json!("observe/1"));
    }
    let mut bytes = serde_json::to_vec(&value).ok()?;
    bytes.push(b'\n');
    Some(bytes)
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

    // ----- observe/1 + status.declare/1 / status/1 parser tests (board item
    //       `01M03VKQ738DTGHHK2C4RWXC0E`). Every failure mode DEGRADES (the
    //       parsers classify Malformed vs Refused so the caller's warn message
    //       is honest, but the caller maps BOTH onto "load without the point");
    //       an unknown `ResultStatus` wire tag degrades to `Failed` (never
    //       `Completed`); the selector keeps an unknown tag inert (the
    //       host-side half of "an unknown `Event` tag is IGNORED").

    /// A `["*"]` selector parses to `ObserveSelector::All`; unknown extra
    /// fields are counted (accept branch / forward-compat), not rejected.
    #[test]
    fn observe_answer_star_selector_is_all_with_unknown_fields_counted() {
        let bytes = br#"{"id":3,"ok":true,"events":["*"],"future":"x"}"#;
        let (selector, unknown) =
            parse_persistent_observe_response(bytes).expect("accept-branch observe answer");
        assert!(matches!(selector, ObserveSelector::All), "['*'] -> All");
        assert_eq!(unknown, 1, "the one unknown field is counted, not rejected");
    }

    /// A tag-list selector parses to `ObserveSelector::Tags` with exactly the
    /// named tags; `Event::Lagged` is always matched regardless of the set.
    #[test]
    fn observe_answer_tag_list_selector_keeps_named_tags_and_matches_lagged() {
        let bytes = br#"{"id":3,"ok":true,"events":["turn_started","agent_finished"]}"#;
        let (selector, _) =
            parse_persistent_observe_response(bytes).expect("tag-list observe answer");
        match &selector {
            ObserveSelector::Tags(set) => {
                assert!(set.contains("turn_started"));
                assert!(set.contains("agent_finished"));
                assert_eq!(set.len(), 2);
            }
            other => panic!("expected Tags, got {other:?}"),
        }
        // Lagged is ALWAYS forwarded (lossy-with-notice notice), even when the
        // selector did not name it.
        assert!(selector.matches("lagged"));
        assert!(selector.matches("turn_started"));
        assert!(!selector.matches("model_decision"));
        // An unknown tag the plugin named but the host does not produce is
        // kept in the set -- the host simply never has a match to forward, so
        // it is silently inert (the host-side half of "ignore").
        let bytes2 = br#"{"id":3,"ok":true,"events":["quantum_event"]}"#;
        let (selector2, _) =
            parse_persistent_observe_response(bytes2).expect("unknown-tag selector");
        match &selector2 {
            ObserveSelector::Tags(set) => assert!(set.contains("quantum_event")),
            other => panic!("expected Tags, got {other:?}"),
        }
        assert!(!selector2.matches("turn_started"), "unknown tag is inert");
    }

    /// `ok:false` WITH an `error` string is a deliberate decline ->
    /// `Refused` (the caller degrades, loading without the point).
    #[test]
    fn observe_answer_ok_false_with_error_is_refused() {
        let bytes = br#"{"id":3,"ok":false,"error":"I do not observe"}"#;
        let err = parse_persistent_observe_response(bytes).expect_err("ok:false is not Ok");
        match err {
            ObserveParseError::Refused(detail) => {
                assert!(
                    detail.contains("I do not observe"),
                    "carries the plugin's reason: {detail}"
                )
            }
            other => panic!("ok:false-with-error is Refused, got {other:?}"),
        }
    }

    /// `ok:false` with NO `error` is a contract violation -> `Malformed`.
    #[test]
    fn observe_answer_ok_false_with_no_error_is_malformed() {
        let bytes = br#"{"id":3,"ok":false}"#;
        let err = parse_persistent_observe_response(bytes).expect_err("ok:false no error");
        assert!(matches!(err, ObserveParseError::Malformed(_)));
    }

    /// A non-array `events` is structural malformation -> `Malformed`.
    #[test]
    fn observe_answer_non_array_events_is_malformed() {
        let bytes = br#"{"id":3,"ok":true,"events":"turn_started"}"#;
        parse_persistent_observe_response(bytes)
            .expect_err("a non-array events field is malformed");
    }

    /// A status.declare/1 answer with two keys parses both declarations.
    #[test]
    fn status_declare_answer_parses_per_key_declarations() {
        let bytes = br#"{"id":4,"ok":true,"keys":[{"key":"build","max_len":80,"ttl_ms":5000},{"key":"lint"}]}"#;
        let (decls, _) = parse_persistent_status_declare_response(bytes)
            .expect("accept-branch status.declare answer");
        assert_eq!(decls.len(), 2);
        assert_eq!(decls[0].key, "build");
        assert_eq!(decls[0].max_len, Some(80));
        assert_eq!(decls[0].ttl_ms, Some(5000));
        assert_eq!(decls[1].key, "lint");
        assert_eq!(decls[1].max_len, None, "max_len is optional");
        assert_eq!(decls[1].ttl_ms, None, "ttl_ms is optional");
    }

    /// A status.declare/1 `ok:false`-with-error is a deliberate decline ->
    /// `Refused` (the caller degrades).
    #[test]
    fn status_declare_answer_ok_false_with_error_is_refused() {
        let bytes = br#"{"id":4,"ok":false,"error":"no status to push"}"#;
        let err = parse_persistent_status_declare_response(bytes).expect_err("ok:false is not Ok");
        match err {
            StatusDeclareParseError::Refused(detail) => {
                assert!(
                    detail.contains("no status to push"),
                    "carries reason: {detail}"
                )
            }
            other => panic!("ok:false-with-error is Refused, got {other:?}"),
        }
    }

    /// A status.declare/1 entry with an empty `key` is structural malformation
    /// -> `Malformed`.
    #[test]
    fn status_declare_answer_empty_key_is_malformed() {
        let bytes = br#"{"id":4,"ok":true,"keys":[{"key":""}]}"#;
        parse_persistent_status_declare_response(bytes).expect_err("an empty key is malformed");
    }

    /// A known `ResultStatus` tag maps to the matching variant; the `value`
    /// string populates the variant's own text field.
    #[test]
    fn status_notification_known_tag_maps_to_variant() {
        let val =
            serde_json::json!({"op":"status/1","key":"build","status":"failed","value":"red"});
        let contrib = parse_status_notification(&val).expect("known failed tag");
        assert_eq!(contrib.key, "build");
        assert_eq!(contrib.value, "red");
        assert_eq!(
            contrib.status,
            ResultStatus::Failed {
                error: "red".into()
            }
        );

        let val2 =
            serde_json::json!({"op":"status/1","key":"lint","status":"completed","value":"green"});
        let contrib2 = parse_status_notification(&val2).expect("known completed tag");
        assert_eq!(contrib2.status, ResultStatus::Completed);
    }

    /// An UNKNOWN `ResultStatus` tag degrades to `ResultStatus::Failed`
    /// (never `Completed`), carrying the unknown tag in the `error` string so
    /// the degradation is auditable -- the compatibility table's
    /// `ResultStatus` row.
    #[test]
    fn status_notification_unknown_tag_degrades_to_failed() {
        let val = serde_json::json!({"op":"status/1","key":"build","status":"quantum","value":"?"});
        let contrib = parse_status_notification(&val).expect("unknown tag degrades, not errors");
        match contrib.status {
            ResultStatus::Failed { error } => {
                assert!(error.contains("quantum"), "names the unknown tag: {error}")
            }
            other => panic!("unknown tag degrades to Failed, got {other:?} (never Completed)"),
        }
    }

    /// A status/1 notification missing `key` or `status` is structurally
    /// invalid -> `Err` (the caller drops it with a warn, observer-class).
    #[test]
    fn status_notification_missing_field_is_err() {
        let no_key = serde_json::json!({"op":"status/1","status":"completed","value":"ok"});
        parse_status_notification(&no_key).expect_err("missing key is invalid");
        let no_status = serde_json::json!({"op":"status/1","key":"build","value":"ok"});
        parse_status_notification(&no_status).expect_err("missing status is invalid");
    }

    /// `build_observe_notification` produces a flat object with `op` merged
    /// alongside the `Event`'s own `event`-tagged fields, `\n`-terminated; a
    /// selector that does not match the tag returns `None`.
    #[test]
    fn build_observe_notification_merges_op_and_filters_by_selector() {
        let event = conway::plugin::Event::TurnStarted { turn: 7 };
        let all = ObserveSelector::All;
        let line = build_observe_notification(&event, &all).expect("All selector forwards");
        let s = std::str::from_utf8(&line).unwrap();
        assert!(s.ends_with('\n'), "newline-terminated");
        assert!(s.contains("\"op\":\"observe/1\""), "op merged in: {s}");
        assert!(
            s.contains("\"event\":\"turn_started\""),
            "event tag retained: {s}"
        );
        assert!(s.contains("\"turn\":7"), "event field retained: {s}");

        // A selector that did not name `turn_started` filters it out.
        let tags =
            ObserveSelector::Tags(std::collections::HashSet::from(["agent_finished".into()]));
        assert!(
            build_observe_notification(&event, &tags).is_none(),
            "filtered out"
        );

        // Lagged is ALWAYS forwarded regardless of the selector.
        let lagged = conway::plugin::Event::Lagged { skipped: 3 };
        let line = build_observe_notification(&lagged, &tags).expect("Lagged always forwarded");
        let s = std::str::from_utf8(&line).unwrap();
        assert!(
            s.contains("\"event\":\"lagged\""),
            "lagged tag present: {s}"
        );
        assert!(s.contains("\"skipped\":3"), "skipped field present: {s}");
    }

    // ----- `WireManifest::optional_host_caps`/`::provides` parser tests
    //       (board item `01M0XXXX3HK8914NE418P5GNRY`, acceptance criterion 1).
    //       Mirrors `required_host_caps`' own untested-but-established
    //       discipline: `#[serde(default)]` so an existing manifest parses
    //       unchanged, and the SAME `HostCapability` fail-closed-malformed /
    //       accept-well-formed-unknown boundary applies to both new fields.

    /// A manifest predating `optional_host_caps` (no such key in the JSON)
    /// parses unchanged, with the field defaulting to empty -- acceptance
    /// criterion 1's first half.
    #[test]
    fn wire_manifest_without_optional_host_caps_key_defaults_to_empty() {
        let json = r#"{
            "id": "acme.greet",
            "version": "0.1.0",
            "tools": []
        }"#;
        let manifest: WireManifest = serde_json::from_str(json).expect("predates the field");
        assert_eq!(manifest.optional_host_caps, Vec::new());
        assert_eq!(
            manifest.provides,
            Vec::new(),
            "provides also defaults to empty for a manifest predating it"
        );
    }

    /// A manifest that DOES declare `optional_host_caps` carries the
    /// declared caps through -- acceptance criterion 1's second half ("Test
    /// both").
    #[test]
    fn wire_manifest_optional_host_caps_round_trips() {
        let json = r#"{
            "id": "acme.greet",
            "version": "0.1.0",
            "tools": [],
            "optional_host_caps": ["persistent_transport", "acme.greet.extra"]
        }"#;
        let manifest: WireManifest = serde_json::from_str(json).expect("well-formed caps parse");
        assert_eq!(
            manifest.optional_host_caps,
            vec![
                HostCapability::PersistentTransport,
                HostCapability::named("acme.greet.extra").unwrap(),
            ]
        );
    }

    /// A manifest declaring `provides` carries the declared capability names
    /// through, normalizing a core-blessed bare name and preserving a
    /// namespaced one -- acceptance criterion 3's parsing half.
    #[test]
    fn wire_manifest_provides_parses_declared_capability_names() {
        let json = r#"{
            "id": "acme.greet",
            "version": "0.1.0",
            "tools": [],
            "provides": ["acme.greet.checkbox", "subagent"]
        }"#;
        let manifest: WireManifest =
            serde_json::from_str(json).expect("well-formed provides parse");
        assert_eq!(
            manifest.provides,
            vec![
                HostCapability::named("acme.greet.checkbox").unwrap(),
                HostCapability::Subagent,
            ]
        );
    }

    /// A MALFORMED capability name in `provides` fails `WireManifest`
    /// parsing outright -- the SAME fail-closed boundary
    /// `required_host_caps`/`optional_host_caps` already enforce, not a
    /// different one for `provides` (this item's own guard rail).
    #[test]
    fn wire_manifest_provides_malformed_name_fails_closed() {
        let json = r#"{
            "id": "acme.greet",
            "version": "0.1.0",
            "tools": [],
            "provides": [".bad"]
        }"#;
        let err = serde_json::from_str::<WireManifest>(json)
            .expect_err("a malformed capability name must fail WireManifest parsing");
        // Asserted on what the message must DO for a plugin author -- name
        // the offending value and say it is not well formed -- rather than
        // on a particular phrasing. `validate_event_name` owns the wording
        // and is shared by three call sites; pinning its exact words here
        // would make this test fail on an improvement to any of them.
        let msg = err.to_string();
        assert!(
            msg.contains(".bad") && msg.to_lowercase().contains("well-formed"),
            "the parse error should name the offending value and the shape violation: {err}"
        );
    }

    /// A WELL-FORMED but previously-unknown `provides` name PARSES (resolving
    /// to `HostCapability::Named`) -- the sharpened fail-closed boundary board
    /// item `01M0XKP5BWCPY3BHPJZHXKR4H3` put in `required_host_caps`' own
    /// doc, reused verbatim for `provides` rather than re-litigated: opening
    /// the vocabulary means a third party's own capability name is not
    /// rejected merely for being unrecognized.
    #[test]
    fn wire_manifest_provides_well_formed_unknown_name_parses_as_named() {
        let json = r#"{
            "id": "acme.greet",
            "version": "0.1.0",
            "tools": [],
            "provides": ["acme.greet.brand_new_capability"]
        }"#;
        let manifest: WireManifest =
            serde_json::from_str(json).expect("a well-formed but unknown name still parses");
        assert_eq!(
            manifest.provides,
            vec![HostCapability::Named(
                "acme.greet.brand_new_capability".to_string()
            )]
        );
    }

    // ----- capability/1 parser tests (board item `01M0XXXX3HK8914NE418P5GNRY`).
    //       Mirrors the tool/1 parser test discipline directly above:
    //       ok:true succeeds (default `result` when omitted); ok:false WITH
    //       an `error` object succeeds as the declared `CapabilityError`;
    //       ok:false with NO `error` object fails closed.

    /// A well-formed `ok:true` answer classifies as `Ok`, carrying whatever
    /// `result` the provider sent.
    #[test]
    fn capability_result_ok_true_parses_result_value() {
        let bytes = br#"{"ok": true, "result": {"echoed": 42}}"#;
        let result = parse_capability_result(bytes).expect("well-formed success answer");
        match result {
            WireCapabilityResult::Ok(value) => {
                assert_eq!(value, serde_json::json!({"echoed": 42}));
            }
            WireCapabilityResult::Err(err) => panic!("expected Ok, got Err({err:?})"),
        }
    }

    /// An `ok:true` answer that OMITS `result` defaults to `Value::Null` --
    /// a provider with nothing to return still answers successfully, never a
    /// parse error.
    #[test]
    fn capability_result_ok_true_missing_result_defaults_to_null() {
        let bytes = br#"{"ok": true}"#;
        let result = parse_capability_result(bytes).expect("omitted result defaults, not errors");
        match result {
            WireCapabilityResult::Ok(value) => assert_eq!(value, serde_json::Value::Null),
            WireCapabilityResult::Err(err) => panic!("expected Ok, got Err({err:?})"),
        }
    }

    /// An `ok:false` answer WITH an `error` object classifies as `Err`,
    /// carrying the provider's own [`CapabilityError`] verbatim.
    #[test]
    fn capability_result_ok_false_with_error_parses_capability_error() {
        let bytes =
            br#"{"ok": false, "error": {"message": "acme.greet.checkbox denied", "detail": {"code": "denied"}}}"#;
        let result = parse_capability_result(bytes).expect("a declared failure still parses");
        match result {
            WireCapabilityResult::Err(err) => {
                assert_eq!(err.message, "acme.greet.checkbox denied");
                assert_eq!(err.detail, serde_json::json!({"code": "denied"}));
            }
            WireCapabilityResult::Ok(value) => panic!("expected Err, got Ok({value:?})"),
        }
    }

    /// An `ok:false` answer with NO `error` object is a contract violation --
    /// fails closed, the SAME line `RawToolResult::classify` draws for
    /// `tool/1` verbatim.
    #[test]
    fn capability_result_ok_false_without_error_fails_closed() {
        let bytes = br#"{"ok": false}"#;
        let err = parse_capability_result(bytes)
            .expect_err("ok:false with no error object must fail closed");
        assert!(
            err.contains("error"),
            "the parse error names the missing error object: {err}"
        );
    }

    /// A non-boolean/missing `ok` is structural malformation -- fails closed
    /// at the JSON-shape level (the `ok` field is required, not defaulted).
    #[test]
    fn capability_result_missing_ok_fails_closed() {
        let bytes = br#"{"result": 1}"#;
        parse_capability_result(bytes).expect_err("a missing ok field must fail closed");
    }

    /// The persistent `capability/1` response parser returns the echoed `id`
    /// alongside the classified result -- the same correlation shape
    /// `parse_persistent_tool_response` proves for `tool/1`.
    #[test]
    fn persistent_capability_response_correlates_id_and_classifies() {
        let bytes = br#"{"id": 7, "ok": true, "result": "hello"}"#;
        let (id, result) =
            parse_persistent_capability_response(bytes).expect("well-formed persistent answer");
        assert_eq!(id, 7);
        match result {
            WireCapabilityResult::Ok(value) => assert_eq!(value, serde_json::json!("hello")),
            WireCapabilityResult::Err(err) => panic!("expected Ok, got Err({err:?})"),
        }
    }
}
