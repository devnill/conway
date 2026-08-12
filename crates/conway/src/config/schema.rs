//! The `ConwayConfig` schema: the facade-owned wire shape for `settings.json`.
//!
//! Reconciliation note (disclosed in the WI-097 Self-Check): the binding
//! implementation notes say `[roles]` and `[health]` "deserialize directly
//! into `conway_core::RoutingConfig`. Do not duplicate the types." Two
//! properties of the already-committed `conway_core` types make a literal
//! reading of that instruction impossible:
//!
//! - `conway_core::routing::RoleConfig` has no `#[serde(default)]` on its
//!   `required`/`params` fields, so deserializing the documented minimal
//!   `{"roles": {"coder": {"chain": [...]}}}` directly into it fails
//!   ("missing field `required`").
//! - `conway_core::ids::ModelRef` derives a plain struct `Deserialize`
//!   (object wire shape `{backend, model}`), not a string-parsed one, so it
//!   cannot deserialize the documented `chain = ["local/qwen3-coder-80b",
//!   ...]` bare-string array.
//!
//! This module therefore defines its own [`RoleEntry`] (`chain: Vec<String>`,
//! `headroom_tokens: Option<u32>`, plus a per-role capability floor — see
//! [`RoleEntry`]'s own doc comment) matching the documented schema exactly,
//! and [`ConwayConfig::routing`] converts it into the authoritative
//! `conway_core::routing::RoutingConfig`/`RoleConfig` (parsing each chain
//! string via `ModelRef::from_str`, mapping the capability-floor fields into
//! `RequiredCaps`, and filling `params` with its `Default` value — the
//! documented schema never populates that sub-table).
//!
//! `[health]` does NOT embed `conway_core::routing::HealthConfig` directly,
//! even though that type has a container-level `#[serde(default)]`: it has
//! no `#[serde(deny_unknown_fields)]`, so embedding it verbatim would let a
//! typo'd `[health]` key (e.g. `transport_failures_to_opne`) parse
//! successfully and silently fall back to that field's default — precisely
//! the silent-misconfiguration failure mode this schema's fail-loud design
//! exists to prevent. This module instead defines its own [`HealthSection`],
//! mirroring `HealthConfig`'s seven fields and defaults exactly under
//! `#[serde(deny_unknown_fields, default)]`, matching the pattern already
//! used for `[roles]`/[`RoleEntry`]. [`ConwayConfig::routing`] converts
//! `HealthSection` into the authoritative `HealthConfig`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use conway_core::ids::RoleAlias;
use serde::{Deserialize, Serialize};

/// The complete, facade-owned `settings.json` schema.
///
/// `default_role` has no sensible built-in default (the binding config always
/// sets it explicitly), so it is the one field with no `#[serde(default)]`.
/// Every other field defaults per the documented schema in
/// `docs/embedding.md` (WI-097).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConwayConfig {
    pub default_role: RoleAlias,
    #[serde(default = "default_cwd")]
    pub cwd: PathBuf,
    #[serde(default)]
    pub session: SessionConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub permissions: PermissionsConfig,
    #[serde(default)]
    pub backends: BTreeMap<String, BackendEntry>,
    #[serde(default)]
    pub routing: RoutingSection,
    #[serde(default)]
    pub roles: BTreeMap<String, RoleEntry>,
    #[serde(default)]
    pub health: HealthSection,
    #[serde(default)]
    pub agents: AgentsConfig,
    #[serde(default)]
    pub models: ModelsConfig,
    /// `[tui]` (TUI-only options; the facade owns the schema, the
    /// `conway-cli` TUI consumes it). Currently `[tui.theme]` (T1) and
    /// `[tui.status_line]` (T3).
    #[serde(default)]
    pub tui: TuiSection,
    /// `[tools]` (board item: bash ships on by default and cannot be
    /// declined). Which built-in `conway-tools` plugins
    /// `ConwayBuilder::build` auto-registers -- see [`ToolsConfig`]'s own
    /// doc.
    #[serde(default)]
    pub tools: ToolsConfig,
    /// `[plugins]` (the first-party plugin tier, board item
    /// 01KZDC3JQ7W4DY1MG6MBCVB2DV) -- see [`PluginsConfig`]'s own doc for
    /// why this crate carries the wire shape but never itself acts on it.
    #[serde(default)]
    pub plugins: PluginsConfig,
    /// `[hooks]` (board item 01KZDC0RDRMMMJHX7SAFMM2Q5A, "declarative
    /// hooks"). **A `pre_tool_use` rule is dispatched ONLY IF an embedder
    /// injected a runner via `ConwayBuilder::with_hook_runner` (board item
    /// 01KZS00JP5QNBJSSHNFP9C47GM); every other `event` is still parsed and
    /// validated only.** That precondition is stated here rather than only
    /// in [`HooksConfig`] because this is the declaration site, and GP-14
    /// treats a declaration site as ONE artifact: a reader who stops at this
    /// field must not come away believing a rule they write here will run.
    /// `conway-cli` does not inject a runner today (board item
    /// 01KZVTTP492R3BDY33FAGYWDNW), so a `pre_tool_use` rule in a
    /// `settings.json` driving the CLI still parses, validates, and is never
    /// consulted. See [`HooksConfig`]'s own doc comment for the precise,
    /// per-event disclosure of what runs today and what remains a forward
    /// declaration.
    #[serde(default)]
    pub hooks: HooksConfig,
}

fn default_cwd() -> PathBuf {
    PathBuf::from(".")
}

impl ConwayConfig {
    /// The role's effective headroom: its own override if present,
    /// otherwise `routing.default_headroom_tokens`. Unknown roles get the
    /// global default rather than erroring — mirrors
    /// `conway_core::routing::RoutingConfig::headroom_for` exactly.
    pub fn headroom_for(&self, role: &RoleAlias) -> u32 {
        self.roles
            .get(role.as_str())
            .and_then(|r| r.headroom_tokens)
            .unwrap_or(self.routing.default_headroom_tokens)
    }

    /// Converts this facade schema into the authoritative
    /// `conway_core::routing::RoutingConfig`, parsing each chain entry's
    /// `"backend/model"` string via `ModelRef::from_str`. See the module
    /// doc comment for why this conversion (rather than direct shared-type
    /// deserialization) is necessary.
    pub fn routing(&self) -> Result<conway_core::routing::RoutingConfig, String> {
        let mut roles = BTreeMap::new();
        for (name, entry) in &self.roles {
            let mut chain = Vec::with_capacity(entry.chain.len());
            for raw in &entry.chain {
                let model_ref: conway_core::ids::ModelRef = raw.parse().map_err(|_| {
                    format!(
                        "role '{name}': invalid model reference '{raw}' (expected 'backend/model')"
                    )
                })?;
                chain.push(model_ref);
            }
            let required = conway_core::capabilities::RequiredCaps {
                tool_calling: entry.tool_calling.map(ToolCallSupportSpec::to_capability),
                structured_output: entry.structured_output,
                parallel_tool_calls: entry.parallel_tool_calls,
                reasoning: entry.reasoning,
                min_reliability: entry.min_reliability,
                min_context: entry.min_context,
                ..conway_core::capabilities::RequiredCaps::default()
            };
            roles.insert(
                name.clone(),
                conway_core::routing::RoleConfig {
                    chain,
                    required,
                    params: conway_core::content::SamplingParams::default(),
                    headroom_tokens: entry.headroom_tokens,
                },
            );
        }
        Ok(conway_core::routing::RoutingConfig {
            roles,
            health: self.health.into(),
            default_headroom_tokens: self.routing.default_headroom_tokens,
        })
    }
}

/// `[session]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SessionConfig {
    pub root: PathBuf,
    pub fsync: FsyncMode,
    pub fsync_interval_ms: u64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            root: PathBuf::from(".conway/sessions"),
            fsync: FsyncMode::Interval,
            fsync_interval_ms: 200,
        }
    }
}

/// `session.fsync`. A plain lowercase string tag, distinct from
/// `conway_core::config::FsyncPolicy`'s `{kind, millis}` tagged-enum wire
/// shape — `fsync_interval_ms` is carried as a sibling field per the
/// documented schema, not folded into the enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FsyncMode {
    Always,
    Interval,
    Never,
}

/// `[limits]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LimitsConfig {
    pub max_steps: u32,
    /// `0` = unlimited.
    pub max_tokens: u32,
    /// `0` = none.
    pub deadline_secs: u64,
    pub max_parallel_tools: u32,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_steps: 40,
            max_tokens: 0,
            deadline_secs: 0,
            max_parallel_tools: 4,
        }
    }
}

/// `[permissions]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PermissionsConfig {
    pub mode: PermissionMode,
    pub allowed_tools: Vec<String>,
    pub denied_tools: Vec<String>,
}

impl Default for PermissionsConfig {
    fn default() -> Self {
        Self {
            mode: PermissionMode::Prompt,
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionMode {
    Prompt,
    Allowlist,
    Deny,
}

/// `[backends.<id>]`. Facade-owned: distinct from
/// `conway_core::routing::BackendConfig` (which has no `api_key` or
/// `stream_tools` field — this crate's disambiguation between a literal
/// `api_key` and an `api_key_env` indirection, plus the per-backend
/// `stream_tools` default, only exist at the config-loading layer) and from
/// `conway_plugin_backends::{AnthropicConfig, OpenAiCompatConfig}` (the
/// concrete adapter configs constructed from this by
/// `conway_plugin_backends::factory`, board item 01KZHF270T3W8GZ7NM6DSNQ4MM).
/// Mirrors how those two adapter configs are already a third, distinct
/// shape from `conway_core::routing::BackendConfig` in the committed code.
///
/// **`kind` is an open name, not a closed enum** (board item
/// 01KZHF1E85MS1VF4YH8CDNCP9Z, cite decision 01KZHRPZ010R37411R3W1XR5TF —
/// the config break this represents is accepted, pre-1.0, not re-litigated
/// here). `crate::builder::resolve_backend_factory` resolves it against
/// every `conway_core::ports::BackendFactory` registered on the builder
/// (`ConwayBuilder::with_backend_factory`) -- ONLY: board item
/// 01KZHF270T3W8GZ7NM6DSNQ4MM removed the temporary compiled-in fallback to
/// `"anthropic"`/`"openai-compat"` that predecessor item deliberately left
/// standing, so every kind, including those two, is a registered factory
/// now (`conway_plugin_backends::factory`'s two `BackendFactory`s, attached
/// by default -- see [`PluginsConfig::default_backends`]'s own doc for what
/// makes them attach with no `settings.json` change). An unrecognised name
/// is a hard, named `build()` error, never a silent no-op (GP-14).
///
/// **Kind-specific keys: the catch-all shape, chosen over its two
/// alternatives, with its cost stated rather than papered over.** Three
/// shapes were on the table:
/// 1. *Chosen*: keep the five typed fields below and add `extra` — a
///    flattened catch-all for whatever else a third-party kind's entry
///    carries. Cost, accepted explicitly: `#[serde(flatten)]` cannot
///    coexist with `#[serde(deny_unknown_fields)]` (serde denies unknown
///    fields by rejecting them before the flatten target ever sees them,
///    which defeats the whole point of a catch-all), so this struct drops
///    that annotation — a typo in one of the five typed field names (e.g.
///    `base_ur1`) is no longer a parse error; it is silently captured into
///    `extra` and never read by either shipped adapter. This is a genuine
///    regression in a validation surface `tests/config_precedence.rs`'s
///    sibling `deny_unknown_fields` tests still enforce for every OTHER
///    section of `settings.json` — accepted here, and only here, because
///    the alternative (below) forecloses third-party kinds entirely. A
///    factory is free to validate its own `extra` keys inside `build()`
///    and reject its own typos there — `conway_core::ports::
///    BackendBuildContext::extra` (board item 01KZMM8ABQJQGHTDTP5S29P88C)
///    carries this same map, cloned verbatim by
///    `crate::builder::build_backend_context`, onward to every registered
///    `BackendFactory::build`, so this is genuinely reachable now, not
///    merely a follow-on concern; the facade itself still does not attempt
///    per-kind validation on a factory's behalf.
/// 2. *Rejected*: nest custom keys under one explicit sub-object (e.g.
///    `{"kind": "foo", "config": {...}}`), leaving the top level closed and
///    `deny_unknown_fields` intact. Rejected because it reintroduces
///    exactly the privileged-built-in asymmetry this item exists to
///    remove (GP-03): the five built-in-shaped keys would sit at one level
///    and a third party's own keys at another, a structural "built-ins are
///    first-class, everyone else is a guest" distinction with no technical
///    justification.
/// 3. *Rejected*: move every kind-specific key (including `dialect`, which
///    is already `openai-compat`-only) into the catch-all too, so nothing
///    but `kind` stays typed. Rejected because it is the largest break to
///    every existing `settings.json` and every example in
///    `docs/providers.md` for no benefit the chosen shape does not already
///    provide — `dialect` staying a typed, documented field for the
///    built-in `openai-compat` kind costs nothing since a third-party kind
///    is free to ignore or reuse it via `BackendBuildContext::dialect`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BackendEntry {
    pub kind: String,
    /// Mutually exclusive with `api_key_env` (enforced by
    /// `merge::validate`).
    pub api_key: String,
    pub api_key_env: String,
    pub base_url: String,
    pub dialect: Option<String>,
    pub stream_tools: Option<bool>,
    /// Every key this entry carries beyond the five typed fields above —
    /// where a third-party `kind`'s own configuration lives. See this
    /// struct's own doc comment for the typo-detection cost this flattened
    /// catch-all accepts.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Default for BackendEntry {
    fn default() -> Self {
        Self {
            kind: DEFAULT_BACKEND_KIND.to_string(),
            api_key: String::new(),
            api_key_env: String::new(),
            base_url: String::new(),
            dialect: None,
            stream_tools: None,
            extra: BTreeMap::new(),
        }
    }
}

/// `backends.<id>.kind`'s default when the key is absent — matches the
/// pre-open-name default (`BackendKind::Anthropic`'s
/// `#[serde(rename_all = "kebab-case")]` wire value) exactly, so an entry
/// that never set `kind` before this item behaves identically after it.
pub const DEFAULT_BACKEND_KIND: &str = "anthropic";

/// `[routing]` (headroom amendment). Just the global headroom default —
/// `[roles]` and `[health]` are separate top-level sections per the
/// documented schema, not nested under `[routing]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RoutingSection {
    pub default_headroom_tokens: u32,
}

impl Default for RoutingSection {
    fn default() -> Self {
        Self {
            default_headroom_tokens: DEFAULT_HEADROOM_TOKENS,
        }
    }
}

/// Disclosed reconciliation: the WI-097 amendment's prose says
/// `default_headroom_tokens` "defaults to `16000` when the key is absent."
/// The already-committed `conway_core::capabilities::DEFAULT_HEADROOM_TOKENS`
/// (consumed by `RequiredCaps::default()`,
/// `conway_core::routing::RoutingConfig`'s own serde default, and
/// `conway_core::capabilities::HeadroomPolicy::default()`) is `8_192`.
/// Introducing a third, disagreeing "default headroom" value at
/// the facade layer would mean the same omitted key resolves to three
/// different numbers depending which layer computed it — strictly worse
/// than deviating from the amendment's literal `16000`. This constant
/// therefore reuses the cross-crate-agreed value; the corresponding test
/// (`empty config -> default_headroom_tokens`) asserts `8_192`, not
/// `16_000`.
pub const DEFAULT_HEADROOM_TOKENS: u32 = conway_core::capabilities::DEFAULT_HEADROOM_TOKENS;

/// `[health]`. Facade-owned mirror of `conway_core::routing::HealthConfig`'s
/// three fields and defaults — see the module doc comment for why this
/// isn't a direct embed of `HealthConfig` (that type lacks
/// `#[serde(deny_unknown_fields)]`). Every field name, type, and default
/// value here must match `HealthConfig` exactly, or a valid setting would
/// silently diverge in meaning between the two types.
///
/// **BREAKING: `probe_enabled`/`probe_interval_secs`/`probe_timeout_secs`/
/// `probe_failures_to_open` were removed (board item
/// `01KZ802GSF692EKYKQ2TTVCJB8`), not merely left unimplemented.** They used
/// to configure a periodic health prober and the independent `Probe` breaker
/// it fed; the prober had no production call site anywhere in this tree —
/// the Transport breaker alone already handles recovery (a clock read takes
/// it half-open; the next real request retries) — so wiring it was an
/// optimization this project gates on a measured baseline that neither
/// existed nor was scheduled. Because this struct still carries
/// `#[serde(deny_unknown_fields)]`, a `settings.json` naming any of the four
/// removed keys under `[health]` now fails to load with that key named,
/// rather than silently accepting and ignoring it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HealthSection {
    pub transport_failures_to_open: u32,
    pub open_duration_secs: u64,
    pub half_open_successes_to_close: u32,
}

impl Default for HealthSection {
    fn default() -> Self {
        let d = conway_core::routing::HealthConfig::default();
        Self {
            transport_failures_to_open: d.transport_failures_to_open,
            open_duration_secs: d.open_duration_secs,
            half_open_successes_to_close: d.half_open_successes_to_close,
        }
    }
}

impl From<HealthSection> for conway_core::routing::HealthConfig {
    fn from(section: HealthSection) -> Self {
        Self {
            transport_failures_to_open: section.transport_failures_to_open,
            open_duration_secs: section.open_duration_secs,
            half_open_successes_to_close: section.half_open_successes_to_close,
        }
    }
}

/// `[roles.<alias>]`. `chain` is `Vec<String>` (`"backend/model"`), not
/// `Vec<ModelRef>` — see the module doc comment.
///
/// The six fields below are the role's capability floor: they map into
/// `conway_core::capabilities::RequiredCaps` (everything but
/// `headroom_tokens`, which has its own established path via
/// `RoleEntry::headroom_tokens` above and `ConwayConfig::headroom_for`).
/// Before this, `ConwayConfig::routing()` hardcoded `RequiredCaps::default()`
/// for every role, so a per-role floor enforced literally nothing — see
/// `docs/routing.md`'s "Capability matching" section for the pre-existing
/// disclosure this closes. Every field is optional and defaults to `None`
/// ("no requirement"), matching `RequiredCaps`'s own semantics exactly, so
/// an existing config with none of these keys parses and behaves
/// identically to before.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RoleEntry {
    pub chain: Vec<String>,
    pub headroom_tokens: Option<u32>,
    /// Minimum tool-calling support. Wire vocabulary: `"none"` |
    /// `"non_streaming"` | `"streaming"` | `"streaming_validated"` — see
    /// [`ToolCallSupportSpec`] for why this isn't
    /// `conway_core::capabilities::ToolCallSupport` directly.
    pub tool_calling: Option<ToolCallSupportSpec>,
    /// Minimum structured-output support. Wire vocabulary: `"none"` |
    /// `"json_schema"` | `"grammar"` (`conway_core::capabilities::StructuredOutput`'s
    /// own `rename_all = "snake_case"` shape — a plain enum, no struct
    /// variant, so no facade-local mirror is needed here).
    pub structured_output: Option<conway_core::capabilities::StructuredOutput>,
    pub parallel_tool_calls: Option<bool>,
    pub reasoning: Option<bool>,
    /// Minimum reliability tier. Wire vocabulary: `"verified"` |
    /// `"community"` | `"unknown"`.
    pub min_reliability: Option<conway_core::capabilities::ReliabilityTier>,
    /// An explicit context-window floor, independent of (and in addition
    /// to) the headroom-aware per-request gate — see
    /// `conway_core::capabilities::RequiredCaps::min_context`'s own doc.
    pub min_context: Option<u32>,
}

/// Facade-local wire vocabulary for [`RoleEntry::tool_calling`]: `"none"` |
/// `"non_streaming"` | `"streaming"` | `"streaming_validated"`.
///
/// A distinct type from `conway_core::capabilities::ToolCallSupport`
/// because that enum's `Streaming { validated: bool }` variant is a struct
/// variant — awkward to hand-write in JSON (`{"streaming": {"validated":
/// true}}`) compared to a flat string tag. Structurally identical to
/// `conway_plugin_backends::model_metadata::ToolCallSupportSpec`, which
/// solves the exact same problem for `models.json`, but reusing that type
/// here is a separate refactor this item does not make: this crate does not
/// depend on `conway_plugin_backends` at all (board item
/// 01KZHF270T3W8GZ7NM6DSNQ4MM — that crate is a first-party plugin, never a
/// build dependency of this one), so unifying the two wire vocabularies
/// would mean either duplicating the type here anyway or reversing that
/// dependency direction, neither of which this item's own scope covers.
/// This stays a deliberate independent duplicate for now, the same shape of
/// tradeoff `crates/conway/src/config/model_metadata.rs` already makes for
/// its own, separate reason (that module's `ModelMetadata` stays local to
/// keep metadata loading network-free, disclosed in its own doc comment).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallSupportSpec {
    None,
    NonStreaming,
    Streaming,
    StreamingValidated,
}

impl ToolCallSupportSpec {
    pub fn to_capability(self) -> conway_core::capabilities::ToolCallSupport {
        use conway_core::capabilities::ToolCallSupport;
        match self {
            ToolCallSupportSpec::None => ToolCallSupport::None,
            ToolCallSupportSpec::NonStreaming => ToolCallSupport::NonStreamingOnly,
            ToolCallSupportSpec::Streaming => ToolCallSupport::Streaming { validated: false },
            ToolCallSupportSpec::StreamingValidated => {
                ToolCallSupport::Streaming { validated: true }
            }
        }
    }
}

/// `[agents]`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AgentsConfig {
    pub dir: PathBuf,
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            dir: PathBuf::from(".conway/agents"),
        }
    }
}

/// `[models]`. `probe_on_startup` is not shown in the WI-097 config snippet
/// but is required by WI-100's criteria (`config.models.probe_on_startup`,
/// default `false`); added here since WI-097 owns this file exclusively and
/// WI-100 depends on it existing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ModelsConfig {
    pub metadata_path: PathBuf,
    pub probe_on_startup: bool,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            metadata_path: PathBuf::from(".conway/models.json"),
            probe_on_startup: false,
        }
    }
}

/// `[tools]` (board item: bash ships on by default and cannot be declined).
///
/// `builtin_plugins` is a plain, explicit list of the
/// `conway-tools` built-in plugins to auto-register at `build()` time,
/// named by each one's own `PluginManifest::id` -- the SAME id space
/// `ConwayBuilder::with_builtin_plugins`'s `PluginSelection` filters by (a
/// `settings.json` edit and a library call express the identical policy,
/// in the identical vocabulary). The four built-in ids are `"conway.fs"`,
/// `"conway.shell"` (bash), `"conway.subagent"`, `"conway.report"`.
///
/// **Default: every built-in EXCEPT `"conway.shell"`.** conway's most
/// dangerous built-in (arbitrary shell execution) used to install itself
/// unconditionally, with no runtime way to decline it short of compiling
/// out `fs`/`subagent`/`report` too. Obtaining bash now requires a
/// deliberate act: add `"conway.shell"` to this list (the one-line
/// `settings.json` opt-in -- see `docs/getting-started.md`), call
/// `ConwayBuilder::with_builtin_plugins` directly, or (one-shot only) grant
/// it via `--allowed-tools` -- one-shot's own registration was never
/// gated by this key to begin with (see this crate's `presets::
/// default_permissions_for_one_shot` and `conway-cli`'s own wiring), only
/// its invocation was, and remains, gated by the permission mode.
///
/// `fs`/`subagent`/`report` staying on by default is a deliberate,
/// considered choice, not an oversight: none of the three is a
/// general-purpose arbitrary-code-execution primitive the way bash is (S0's
/// own threat model), each is load-bearing for conway's own out-of-the-box
/// usability (a `conway` with no filesystem tool cannot edit code; the
/// TUI's own dogfooding depends on `fs`), and each was already reachable
/// under the SAME `permissions.mode`/`--allowed-tools` invocation gate bash
/// always was -- registration was never the actual gap for those three.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ToolsConfig {
    pub builtin_plugins: Vec<String>,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            builtin_plugins: vec![
                "conway.fs".to_string(),
                "conway.subagent".to_string(),
                "conway.report".to_string(),
            ],
        }
    }
}

/// `[plugins]` — the first-party plugin tier's install list (board item
/// 01KZDC3JQ7W4DY1MG6MBCVB2DV; see `PHILOSOPHY.md`'s "First-party plugins,
/// and why they are not defaults" and `docs/embedding.md`'s "First-party
/// plugin tier" section).
///
/// **Deliberately distinct from `[tools].builtin_plugins`, not a rename or
/// a shared list.** That key is documented and named for the CLOSED
/// candidate set this crate itself compiles in behind the `builtin-tools`
/// feature (`presets::builtin_plugins()`'s four `conway-tools` candidates)
/// and filters at `build()` time (see [`ToolsConfig`]'s own doc) — an
/// unknown id there is a hard config error precisely because the full
/// candidate set is known here, at compile time
/// (`config::merge::validate`'s check 8). A first-party plugin (dynamic
/// routing, compaction, memory, skills, MCP — the six named in
/// `PHILOSOPHY.md`) is never such a candidate: THIS crate does not, and
/// must never, depend on any of them (GP-03/P-6 — a first-party plugin
/// sits on the exact same footing as a third-party one from `conway`'s own
/// point of view, never a privileged one folded into the built-in
/// candidate set). Folding the two lists together would make
/// `builtin_plugins`'s existing name a lie in the other direction the
/// first day a real first-party plugin lands in it.
///
/// **This crate carries the wire shape and does nothing else with it.**
/// `install` is inert data as far as `ConwayBuilder::build` is concerned —
/// it never reads this field, exactly like [`TuiSection`] immediately
/// below is carried here but consumed only by `conway-cli`'s TUI. Whatever
/// binary or embedder actually links a given first-party plugin crate
/// reads this list itself, via [`crate::ConwayBuilder::config`], and calls
/// `ConwayBuilder::with_plugin`/`with_backend`/`with_router` for every id
/// it recognizes *before* calling `build()` — see
/// `crates/conway-cli/src/first_party_plugins.rs` for the worked example
/// this item ships (`conway-cli` links `conway-plugin-skeleton` and
/// resolves ids from this list against it). An id the reading binary does
/// not recognize is that binary's own config error to raise (GP-14): a
/// name silently doing nothing here would be exactly the rung-1 lie
/// CONTRIBUTING's declaration rule exists to prevent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PluginsConfig {
    /// Manifest ids (`PluginManifest::id`) to install, resolved by whatever
    /// binary or embedder links the named plugin crate(s). Empty by
    /// default: no first-party plugin is ever installed unless named here
    /// (or attached directly via `ConwayBuilder::with_plugin` in library
    /// code) — the tier's whole point is that nothing in it runs unasked.
    pub install: Vec<String>,
    /// The one deliberate exception to `install`'s "empty by default, the
    /// tier's whole point" rule (owner decision 01KZHRPZ010R37411R3W1XR5TF,
    /// board item 01KZHF270T3W8GZ7NM6DSNQ4MM): the `BackendFactory` kind ids
    /// a reading binary/embedder attaches WITHOUT this array (or `install`)
    /// naming them at all. **Default: `["anthropic", "openai-compat"]`** --
    /// `conway_plugin_backends`'s two published `BackendFactory::id()`
    /// values, so an operator's existing `[backends.<id>]` entries (which
    /// already name `kind` as one of these two strings) keep resolving with
    /// zero `settings.json` changes the moment this facade stopped
    /// compiling either dialect in.
    ///
    /// **Why this one pair, and not every first-party kind, defaults to
    /// on:** every OTHER first-party mechanism (a `Plugin`, a
    /// `RouterFactory`) has an honest absent-configuration fallback --
    /// `conway_core::routing::MinimalRouter` when no router factory is
    /// installed, simply no extra tool when no plugin is -- so leaving
    /// `install` empty by default costs a capability, never all of them. A
    /// `[backends.<id>]` entry with no matching `BackendFactory` has no such
    /// fallback (`ConwayBuilder::build` hard-errors: "no backends
    /// configured"): a backend absent from a fresh install does not narrow
    /// what `conway` can do, it leaves `conway` unable to reach a model at
    /// all. That asymmetry, not a general exception to the tier's
    /// opt-in-by-default rule, is why this ships attached.
    ///
    /// **Resolved in the SAME pass as `install`** (`crates/conway-cli/src/
    /// first_party_plugins.rs`'s `install` function): a binary/embedder
    /// unions this list with `install`'s ids before resolving each one
    /// against every linked plugin/router-factory/backend-factory bundle,
    /// so an id here and an id in `install` are handled identically once
    /// resolution starts -- only WHERE each id comes from differs. Removing
    /// an entry from this list -- the decline mechanism a later board item
    /// designs the operator-facing UX for -- is already possible today by
    /// editing `settings.json` directly (this is an ordinary,
    /// `#[serde(default)]`-backed `Vec<String>` field, not special-cased
    /// machinery); that later item's job is discoverability and validation
    /// around doing so, not this field's existence.
    pub default_backends: Vec<String>,
}

impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            install: Vec::new(),
            default_backends: vec!["anthropic".to_string(), "openai-compat".to_string()],
        }
    }
}

/// `[tui]` (TUI-only options). The facade owns the wire shape so the same
/// discovery/precedence/`deny_unknown_fields` machinery that governs every
/// other section governs this one too; the `conway-cli` TUI reads
/// `conway.config().tui.theme` at startup and builds a ratatui `Theme` from
/// it (see `crates/conway-cli/src/tui/view/theme.rs`). The facade itself
/// never names ratatui types -- `ThemeConfig` is a string-keyed shape so the
/// facade need not depend on the render crate.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TuiSection {
    #[serde(default)]
    pub theme: ThemeConfig,
    /// `[tui.status_line]` (T3): declarative status-line field order +
    /// visibility. `fields` is the ordered list of field names to render; a
    /// field absent from the list is hidden, and the list's order is the
    /// render order. Unknown names are silently dropped at render time
    /// (P-10: config is untrusted input, never a panic). Default = the
    /// Lean line
    /// `["session","lineage","mode","model","ctx","tokens","activity","hint"]`.
    /// See `docs/interactive.md`'s "The status line" section for the
    /// full field list.
    #[serde(default)]
    pub status_line: StatusLineConfig,
    /// `[tui.tool_preview_lines]` (T5): the cap on collapsed tool-preview
    /// lines in the TUI transcript. A tool entry whose stored `preview` has
    /// more physical lines than this renders the first N lines + a dim
    /// `… (+M lines, Ctrl-E to expand)` affordance while the entry's
    /// `expanded` flag is `false`; the full preview renders while `true`.
    /// The stored preview is never truncated -- the cap is render-time only.
    /// `None` (the default) means the TUI's built-in default of 3. The TUI
    /// clamps a loaded value to `1..=200` with a fallback to 3 on a
    /// missing/out-of-range/bad value (P-10: config is untrusted input,
    /// never a panic). `CONWAY_TUI__TOOL_PREVIEW_LINES=10` overrides via
    /// env.
    #[serde(default)]
    pub tool_preview_lines: Option<u32>,
    /// `[tui.history_size]` (T8): the cap on the persisted input-history
    /// FIFO (`~/.conway/history`, or `$XDG_CONFIG_HOME/conway/history` when
    /// set -- see `conway::config::discovery::history_file_path`). Loaded at
    /// startup and appended to on every submit; oldest entries are evicted
    /// once the cap is exceeded. `None` (the default) means the TUI's
    /// built-in default of 500. The TUI clamps a loaded value to
    /// `1..=100_000` with a fallback to 500 on a missing/out-of-range/bad
    /// value (P-10: config is untrusted input, never a panic).
    /// `CONWAY_TUI__HISTORY_SIZE=1000` overrides via env.
    #[serde(default)]
    pub history_size: Option<u32>,
}

/// `[tui.status_line]`: declarative status-line field order + visibility
/// (T3). The `fields` list is the ordered set of field names the TUI
/// renders, left to right; a field not in the list is hidden, and the list
/// order is the render order. Unknown names are dropped at render time
/// (P-10: never a panic). Defaults to the Lean line
/// `["session","lineage","mode","model","ctx","tokens","activity","hint"]`.
///
/// `session`/`lineage` were added by the item that corrected a requirement
/// miss in the TUI's T6 scroll affordance: T6 put `session`/agent-lineage
/// content on a scroll-triggered overlay, which is application chrome, not
/// scroll-position-dependent information, so both moved here instead (see
/// `crates/conway-cli/src/tui/view/header.rs`'s module doc for the full
/// story). **A pinned `fields` list written before that item shipped keeps
/// working unchanged, but will not gain either new field** -- unknown/
/// missing names are never an error, so an older config simply renders
/// without them.
///
/// Available field names (see `docs/interactive.md`): `session`,
/// `lineage`, `mode`, `model`, `ctx`, `tokens`, `activity`, `hint`, `git`,
/// `cwd`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StatusLineConfig {
    /// Ordered field names to render. Default = Lean line.
    pub fields: Vec<String>,
}

impl Default for StatusLineConfig {
    fn default() -> Self {
        Self {
            fields: vec![
                "session".to_string(),
                "lineage".to_string(),
                "mode".to_string(),
                "model".to_string(),
                "ctx".to_string(),
                "tokens".to_string(),
                "activity".to_string(),
                "hint".to_string(),
            ],
        }
    }
}

/// `[tui.theme]`: a per-named-style override table. Each entry is an
/// `Option<ThemeStyleConfig>` -- `None` (the default for every slot) means
/// "use the TUI's built-in default for this named style"; `Some` overlays
/// `fg`/`bg`/`modifiers` on top of the default. The TUI resolves the
/// strings to ratatui `Color`/`Modifier` values and maps any unparseable
/// or out-of-range value back to the default for that slot (P-10: config
/// is untrusted input, never a panic). Every field is `Option` so a user
/// can override just one named style without restating the rest.
///
/// Field names match the `Theme` slot names in
/// `crates/conway-cli/src/tui/view/theme.rs` one-for-one.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ThemeConfig {
    pub user: Option<ThemeStyleConfig>,
    pub assistant: Option<ThemeStyleConfig>,
    pub assistant_marker: Option<ThemeStyleConfig>,
    pub reasoning: Option<ThemeStyleConfig>,
    /// T4: the `HH:MM ` timestamp prefix prepended to each entry's first
    /// rendered line while `show_timestamps` is on.
    pub timestamp: Option<ThemeStyleConfig>,
    pub tool_proposed: Option<ThemeStyleConfig>,
    pub tool_awaiting: Option<ThemeStyleConfig>,
    pub tool_running: Option<ThemeStyleConfig>,
    pub tool_done: Option<ThemeStyleConfig>,
    pub tool_failed: Option<ThemeStyleConfig>,
    pub agent_starting: Option<ThemeStyleConfig>,
    pub agent_running: Option<ThemeStyleConfig>,
    pub agent_awaiting: Option<ThemeStyleConfig>,
    pub agent_finished: Option<ThemeStyleConfig>,
    pub agent_failed: Option<ThemeStyleConfig>,
    pub agent_cancelled: Option<ThemeStyleConfig>,
    pub notice: Option<ThemeStyleConfig>,
    pub error: Option<ThemeStyleConfig>,
    pub fatal_error: Option<ThemeStyleConfig>,
    pub dim: Option<ThemeStyleConfig>,
    pub focused: Option<ThemeStyleConfig>,
    pub selected: Option<ThemeStyleConfig>,
    pub emphasized: Option<ThemeStyleConfig>,
    pub border_normal: Option<ThemeStyleConfig>,
    pub border_warning: Option<ThemeStyleConfig>,
    pub border_danger: Option<ThemeStyleConfig>,
    pub border_accent: Option<ThemeStyleConfig>,
    pub status_mode: Option<ThemeStyleConfig>,
    pub status_dim: Option<ThemeStyleConfig>,
    pub spinner: Option<ThemeStyleConfig>,
    /// T6: the sticky context header shown above the transcript while it
    /// overflows the viewport (`session · focused agent · model · ctx%`).
    pub header: Option<ThemeStyleConfig>,
    /// T6: the floating "jump to bottom" footer pill shown over the bottom
    /// row of the transcript while scrolled up (`!follow_tail`).
    pub scroll_footer: Option<ThemeStyleConfig>,
    /// T7: the `/help` keybinding overlay's block border.
    pub help_border: Option<ThemeStyleConfig>,
    /// T7: the key/chord column in the `/help` keybinding overlay's rows
    /// (e.g. `Ctrl-E`, `PageUp/PageDown`).
    pub help_key: Option<ThemeStyleConfig>,
}

/// One `[tui.theme.<name>]` entry: foreground/background color names plus a
/// modifier tag list. All fields are optional -- a `None`/empty field means
/// "leave the named style's default for that channel untouched". The TUI
/// parses `fg`/`bg` as ratatui color names (`"cyan"`, `"dark_gray"`,
/// `"#ff00ff"`, ...) and `modifiers` as ratatui modifier names
/// (`"bold"`, `"dim"`, `"italic"`, `"reversed"`, ...); any unrecognized
/// value falls back to the default (P-10), never a panic.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ThemeStyleConfig {
    pub fg: Option<String>,
    pub bg: Option<String>,
    #[serde(default)]
    pub modifiers: Vec<String>,
}

/// `[hooks]` -- an operator-declared list of "when this event happens, run
/// this command" rules (board item 01KZDC0RDRMMMJHX7SAFMM2Q5A, "declarative
/// hooks"; see `docs/plugins/hooks.md` point 13 and `docs/plugins/scripts.md`
/// for the design this shape is drawn from).
///
/// **GP-14 disclosure, PER EVENT -- read this before adding a rule.** This
/// section itself, and every type it names, always parses and validates
/// (that part was never event-conditional). Whether a rule actually RUNS is:
///
/// - **`event == "pre_tool_use"`, `enabled: true`: DISPATCHED** (board item
///   01KZS00JP5QNBJSSHNFP9C47GM) -- `ConwayBuilder::build` filters
///   `rules` to exactly these and hands them to
///   `conway_runtime::permission::PermissionBroker::decide`, which invokes
///   each via the injected [`crate::plugin::HookRunner`] at the SAME tier
///   as a `deny` pattern rule -- before the mode gate, the cache, pattern
///   allows, and `AutoAllow`, so a denial is enforced under every
///   permission mode. **This still requires an actual runner.**
///   `ConwayBuilder::with_hook_runner` (mirroring `with_permission_gate`/
///   `with_context_hook`: not called at all is the default) is what
///   supplies one -- `conway-runtime` never constructs one itself (decision
///   01KZT642CEZ20K92DYWBTPE2XZ: it must not depend on `conway-tools` to
///   reach one). A `pre_tool_use` rule declared here with no runner ever
///   injected parses, validates, and is silently never consulted --
///   exactly the same gap this doc used to disclose for the whole section,
///   now narrowed to this one precondition.
/// - **Every OTHER `event` value: still forward-declared, unchanged.**
///   Nothing in this crate, or anywhere else in the tree, spawns a process,
///   dispatches an event, or otherwise acts on a rule whose `event` is not
///   `"pre_tool_use"`. Writing one gets you a config that parses and
///   rejects a typo'd key exactly like every other section -- and, for
///   now, nothing more. **01KZRZY1MNM872BZ6AKEBG3SKE** is the general
///   script-runner port ([`crate::plugin::HookRunner`]) this item's
///   `pre_tool_use` dispatch is the FIRST consumer of, not the last: a
///   later item wiring a second event reuses the same runner, the same
///   [`HookEntry::timeout_ms`]/[`HookEntry::enabled`] enforcement, and adds
///   only its own event-specific dispatch call site.
///
/// **Default: an empty rule list.** This is the part of GP-14's rule that
/// is easiest to get backwards (its own named precedent, `probe_enabled`,
/// got exactly this wrong -- see [`HealthSection`]'s doc comment): the
/// default here must not assert that any dispatch happens, and an empty
/// list asserts nothing at all. An operator who never writes `[hooks]`
/// observes zero behavior change from before this item existed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HooksConfig {
    /// Individually named, individually revocable rules -- "rules", not
    /// "entries" or "hooks", to match the vocabulary the later
    /// operator-visibility item (umbrella 01KZDC0RDRMMMJHX7SAFMM2Q5A) uses
    /// when it lists and revokes one by [`HookEntry::id`].
    pub rules: Vec<HookEntry>,
}

/// One `[hooks].rules[]` entry: "when `event` happens, run `command`."
///
/// **Dispatched when `event == "pre_tool_use"` (and a runner is injected --
/// see [`HooksConfig`]'s own doc for the exact precondition); parsed and
/// validated only for every other `event` value.** This crate itself never
/// constructs a `std::process::Command`/`tokio::process::Command` from a
/// value of this type either way -- `ConwayBuilder::build` only ever hands
/// this data to an injected `Arc<dyn `[`crate::plugin::HookRunner`]`>`,
/// which is where the actual process spawn lives
/// (`conway_tools::hook_runner::ProcessHookRunner`, board item
/// 01KZRZY1MNM872BZ6AKEBG3SKE).
///
/// **Deliberately minimal.** The only fields beyond the five the owning
/// board item names are the ones already covered by those five --
/// specifically, no `cwd` override and no `args`-vs-`command` split were
/// added: `command` already being an argv vector (program, then its
/// arguments) settles the args-vs-command question by construction (the
/// first element IS the program, the rest ARE its arguments -- a separate
/// `args` field would just restate that split redundantly), and a `cwd`
/// override is left out because nothing runs yet to need one -- the runner
/// item (01KZRZY1MNM872BZ6AKEBG3SKE) is the one that knows what "current
/// directory" should mean for a spawned hook (the invoking agent's cwd? the
/// project root? see `crates/conway-tools/src/shell/bash.rs`'s own `cwd`
/// field for the shape precedent if it wants one), and adding the field now,
/// uninformed by that answer, risks shipping a name nothing reads correctly
/// once something finally does. Per this item's own instructions: easier to
/// add an optional field later than remove one from a shipped schema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HookEntry {
    /// This rule's stable, operator-chosen identity. Required, must be
    /// non-empty, and must be unique across every rule in the merged file
    /// (`merge::validate`'s hooks check) -- enforced there rather than by a
    /// serde-level required field, matching this schema's established
    /// pattern of a lenient parse (missing key -> this type's own default)
    /// plus a named semantic check for the actual domain invariant (e.g.
    /// `permissions.mode = "allowlist"` requiring non-empty
    /// `allowed_tools`).
    ///
    /// Load-bearing for the later operator-visibility item, which lists
    /// hook rules individually and revokes one by name: deriving this from
    /// list position instead would silently rename a rule the moment the
    /// list is reordered, so it is never `Option<String>` and never
    /// inferred from an index.
    pub id: String,
    /// The event name this rule fires on (e.g. `"pre_tool_use"`).
    ///
    /// FOLLOW-UP LANDING SPOT: this item does NOT validate the
    /// bare-vs-namespaced event-name convention -- a sibling board item is
    /// deciding that rule. When it lands, its check belongs here (or in
    /// `merge::validate`, validating this field against whatever vocabulary
    /// it settles on). Not blocked on here.
    pub event: String,
    /// The command to run, as an argv vector (program, then its arguments)
    /// -- never a single shell string, so no shell-quoting ambiguity exists
    /// in config. Contrast `crates/conway-tools/src/shell/bash.rs`'s
    /// `BashArgs::command`, a single string handed to `bash -c` -- that
    /// tool's whole point is running an arbitrary shell command, so a shell
    /// string is the right shape there; a declaratively-configured hook has
    /// no such reason to need a shell at all, so argv is the right shape
    /// here.
    pub command: Vec<String>,
    /// Milliseconds an injected [`crate::plugin::HookRunner`] will allow
    /// this command before killing it -- enforced for real, for a
    /// `pre_tool_use` rule, by `ConwayBuilder::build`'s translation into
    /// `conway_runtime::permission::PreToolUseHookSpec::timeout_ms` (board
    /// item 01KZS00JP5QNBJSSHNFP9C47GM); still only READ, never enforced,
    /// for any other `event` (see [`HooksConfig`]'s own doc). Default
    /// 5000ms, chosen the same way `crates/conway-tools/src/shell/bash.rs`'s
    /// own `DEFAULT_TIMEOUT_MS` was: long enough for a typical local script
    /// (lint, format-check, a small HTTP call) to finish, short enough that
    /// a hung hook cannot silently stall an agent turn indefinitely.
    #[serde(default = "default_hook_timeout_ms")]
    pub timeout_ms: u64,
    /// Whether this rule is active. Defaults to `true`.
    ///
    /// **Why default-`true` does not repeat `probe_enabled`'s mistake:**
    /// `probe_enabled` defaulted `true` on a section every config already
    /// had a value for, so the bare default alone asserted periodic
    /// probing was happening for every operator, including ones who never
    /// touched `[health]`. This field only has any effect once an operator
    /// has already hand-written a `[hooks].rules[]` entry -- the default
    /// rule list is empty (see [`HooksConfig`]'s own doc) -- so there is no
    /// rule for `enabled` to apply to until the operator deliberately
    /// creates one. `enabled: true` on a rule an operator just wrote
    /// asserts nothing about whether a runner exists (`ConwayBuilder::
    /// with_hook_runner` may still not have been called -- see
    /// [`HooksConfig`]'s own doc for that precondition); it only says
    /// "don't treat the rule I just wrote as disabled", ordinary
    /// boolean-flag convention. `enabled: false` on a `pre_tool_use` rule
    /// is now genuinely load-bearing: `ConwayBuilder::build`'s filter into
    /// `PreToolUseHookSpec` drops a disabled rule before `PermissionBroker::
    /// decide` ever sees it (board item 01KZS00JP5QNBJSSHNFP9C47GM).
    /// `enabled` on any OTHER `event` stays exactly as inert as every other
    /// field here.
    #[serde(default = "default_hook_enabled")]
    pub enabled: bool,
}

impl Default for HookEntry {
    fn default() -> Self {
        Self {
            id: String::new(),
            event: String::new(),
            command: Vec::new(),
            timeout_ms: default_hook_timeout_ms(),
            enabled: default_hook_enabled(),
        }
    }
}

fn default_hook_timeout_ms() -> u64 {
    5000
}

fn default_hook_enabled() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOOKS_BLOCK: &str = r#"
    {
      "default_role": "coder",
      "roles": { "coder": { "chain": [] } },
      "hooks": {
        "rules": [
          {
            "id": "audit-bash",
            "event": "pre_tool_use",
            "command": ["/usr/local/bin/audit-hook", "--strict"],
            "timeout_ms": 3000,
            "enabled": true
          }
        ]
      }
    }
    "#;

    /// ACCEPTANCE: a well-formed `[hooks]` block with one entry parses and
    /// round-trips (serialize back, re-parse, equal).
    #[test]
    fn hooks_block_round_trips() {
        let cfg: ConwayConfig = serde_json::from_str(HOOKS_BLOCK).expect("must parse");
        assert_eq!(cfg.hooks.rules.len(), 1);
        let rule = &cfg.hooks.rules[0];
        assert_eq!(rule.id, "audit-bash");
        assert_eq!(rule.event, "pre_tool_use");
        assert_eq!(
            rule.command,
            vec!["/usr/local/bin/audit-hook".to_string(), "--strict".to_string()]
        );
        assert_eq!(rule.timeout_ms, 3000);
        assert!(rule.enabled);

        let reserialized = serde_json::to_string(&cfg).expect("must serialize");
        let cfg2: ConwayConfig = serde_json::from_str(&reserialized).expect("must re-parse");
        assert_eq!(cfg, cfg2);
    }

    /// ACCEPTANCE: a typo'd key INSIDE an entry (`"evnet"`, `"comand"`)
    /// fails to parse with a serde error naming the unrecognized field --
    /// proving the strictness is on the entry (`HookEntry`), not just the
    /// container (`HooksConfig`).
    #[test]
    fn typo_d_hook_entry_key_is_rejected_by_deny_unknown_fields() {
        let json = r#"
        {
          "default_role": "coder",
          "roles": { "coder": { "chain": [] } },
          "hooks": {
            "rules": [
              { "id": "x", "evnet": "pre_tool_use", "comand": ["echo"] }
            ]
          }
        }
        "#;
        let result: Result<ConwayConfig, _> = serde_json::from_str(json);
        let err = result.expect_err("typo'd entry key must be rejected").to_string();
        // Assert on the field NAME specifically, not on the generic phrase.
        // An `|| err.contains("unknown field")` fallback would still pass if a
        // future serde dropped the offending key from the message, which is the
        // half an operator actually needs -- being told a key is wrong without
        // being told WHICH key is barely better than silence.
        assert!(
            err.contains("evnet"),
            "error must name the unrecognized field: {err}"
        );
    }

    /// ACCEPTANCE: omitting `[hooks]` entirely still parses, with an empty
    /// rule list -- every existing config file in the repo and in `docs/`
    /// is unaffected by this item.
    #[test]
    fn hooks_omitted_entirely_parses_with_an_empty_rule_list() {
        let json = r#"
        {
          "default_role": "coder",
          "roles": { "coder": { "chain": [] } }
        }
        "#;
        let cfg: ConwayConfig = serde_json::from_str(json).expect("must parse without [hooks]");
        assert_eq!(cfg.hooks.rules, Vec::<HookEntry>::new());
    }

    /// `HookEntry::timeout_ms`/`enabled` each have their own defaults when
    /// omitted from an entry, independent of `HooksConfig`'s own
    /// container-level default (which only fires when `rules` itself is
    /// entirely absent).
    #[test]
    fn hook_entry_defaults_apply_when_only_id_event_command_are_given() {
        let json = r#"
        {
          "default_role": "coder",
          "roles": { "coder": { "chain": [] } },
          "hooks": {
            "rules": [
              { "id": "x", "event": "pre_tool_use", "command": ["echo", "hi"] }
            ]
          }
        }
        "#;
        let cfg: ConwayConfig = serde_json::from_str(json).expect("must parse");
        let rule = &cfg.hooks.rules[0];
        assert_eq!(rule.timeout_ms, 5000);
        assert!(rule.enabled);
    }
}
