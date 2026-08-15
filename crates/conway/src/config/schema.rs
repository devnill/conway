//! The `ConwayConfig` schema: the facade-owned wire shape for `settings.json`.
//!
//! Reconciliation note (disclosed): the binding
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
/// `docs/embedding.md`.
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
    /// `[tools]` (bash ships on by default and cannot be
    /// declined). Which built-in `conway-tools` plugins
    /// `ConwayBuilder::build` auto-registers -- see [`ToolsConfig`]'s own
    /// doc.
    #[serde(default)]
    pub tools: ToolsConfig,
    /// `[plugins]` (the first-party plugin tier
    ///) -- see [`PluginsConfig`]'s own doc for
    /// why this crate carries the wire shape but never itself acts on it.
    #[serde(default)]
    pub plugins: PluginsConfig,
    /// `[hooks]` (, "declarative
    /// hooks"). **A `pre_tool_use` rule is dispatched ONLY IF a runner has
    /// been injected -- either via `ConwayBuilder::with_hook_runner` (board
    /// item) directly, or via
    /// `ConwayBuilder::with_default_hook_runner`, the convenience that
    /// supplies this workspace's own in-tree default; every other `event` is
    /// still parsed and validated only.** That precondition is stated here
    /// rather than only in [`HooksConfig`] because this is the declaration
    /// site, and a declaration site is ONE artifact -- nothing may claim to be
    /// reached that isn't: a reader
    /// who stops at this field must not come away believing a rule they
    /// write here will run. `conway-cli` DOES inject a runner -- `build_conway` calls `with_default_
    /// hook_runner` unconditionally), so a `pre_tool_use` rule in a
    /// `settings.json` driving the CLI fires. A third party that links this
    /// crate directly, without calling either method itself, still gets
    /// nothing -- the CLI's own choice to opt in is not inherited by every
    /// embedder. See [`HooksConfig`]'s own doc comment for the precise,
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
    /// Ceiling on tool calls DISPATCHED per turn (per user turn, for a
    /// keep-alive session). `0` = unlimited.
    ///
    /// The counterpart to `Budget::max_tool_calls`, added when that
    /// dimension started being enforced: the other three limits all had a
    /// config counterpart, and leaving this one reachable only from the
    /// library API would put a capability in one consumption mode and not
    /// the others.
    pub max_tool_calls: u32,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_steps: 40,
            max_tokens: 0,
            deadline_secs: 0,
            max_parallel_tools: 4,
            max_tool_calls: 0,
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
/// `conway_plugin_backends::factory`).
/// Mirrors how those two adapter configs are already a third, distinct
/// shape from `conway_core::routing::BackendConfig` in the committed code.
///
/// **`kind` is an open name, not a closed enum** (///, cite —
/// the config break this represents is accepted, pre-1.0, not re-litigated
/// here). `crate::builder::resolve_backend_factory` resolves it against
/// every `conway_core::ports::BackendFactory` registered on the builder
/// (`ConwayBuilder::with_backend_factory`) -- ONLY:
/// removed the temporary compiled-in fallback to
/// `"anthropic"`/`"openai-compat"` that predecessor item deliberately left
/// standing, so every kind, including those two, is a registered factory
/// now (`conway_plugin_backends::factory`'s two `BackendFactory`s, attached
/// by default -- see [`PluginsConfig::default_backends`]'s own doc for what
/// makes them attach with no `settings.json` change). An unrecognised name
/// is a hard, named `build()` error, never a silent no-op -- a key that
/// claims to install something must install it.
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
///    BackendBuildContext::extra`
///    carries this same map, cloned verbatim by
///    `crate::builder::build_backend_context`, onward to every registered
///    `BackendFactory::build`, so this is genuinely reachable now, not
///    merely a follow-on concern; the facade itself still does not attempt
///    per-kind validation on a factory's behalf.
/// 2. *Rejected*: nest custom keys under one explicit sub-object (e.g.
///    `{"kind": "foo", "config": {...}}`), leaving the top level closed and
///    `deny_unknown_fields` intact. Rejected because it reintroduces
///    exactly the privileged-built-in asymmetry this item exists to
///    remove, since there is exactly one extension mechanism: the five
///    built-in-shaped keys would sit at one level
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

/// Disclosed reconciliation: the amendment's prose says
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
/// `probe_failures_to_open` were removed , not merely left unimplemented.** They used
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
/// depend on `conway_plugin_backends` at all (/// — that crate is a first-party plugin, never a
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

/// `[models]`. `probe_on_startup` is not shown in the config snippet
/// but is required by the criteria (`config.models.probe_on_startup`,
/// default `false`); added here since earlier work owns this file exclusively and
/// depends on it existing.
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

/// `[tools]` (bash ships on by default and cannot be declined).
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

/// `[plugins]` — the first-party plugin tier's install list (///; see `PHILOSOPHY.md`'s "First-party plugins,
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
/// must never, depend on any of them — a first-party plugin
/// sits on the exact same footing as a third-party one from `conway`'s own
/// point of view, never a privileged one folded into the built-in
/// candidate set. Folding the two lists together would make
/// `builtin_plugins`'s existing name a lie in the other direction the
/// first day a real first-party plugin lands in it.
///
/// **This crate carries the wire shape and does nothing else with it.**
/// `install` is inert data as far as `ConwayBuilder::build` is concerned —
/// it never reads this field. Unlike `[tui]`, which used to be carried
/// here on the same "wire shape only, no behavior" footing (Stage 2a moved
/// it out entirely — see `crates/conway-cli/src/tui/config.rs` — because
/// `[tui]` is a presentation-only vocabulary a headless embedder should
/// never have to parse; `[plugins]` stays here because every consumption
/// mode, TUI included, resolves it the same way through
/// `ConwayBuilder::install_selected`). Whatever
/// binary or embedder actually links a given first-party plugin crate
/// reads this list itself, via [`crate::ConwayBuilder::config`], and calls
/// `ConwayBuilder::with_plugin`/`with_backend`/`with_router` for every id
/// it recognizes *before* calling `build()` — see
/// `crates/conway-cli/src/first_party_plugins.rs` for the worked example
/// this item ships (`conway-cli` links `conway-plugin-skeleton` and
/// resolves ids from this list against it). An id the reading binary does
/// not recognize is that binary's own config error to raise, since nothing may claim to
/// be reached that isn't: a
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
    /// tier's whole point" rule (owner,
    ///): the `BackendFactory` kind ids
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
    /// an entry from this list -- the decline mechanism a later
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

/// `[hooks]` -- an operator-declared list of "when this event happens, run
/// this command" rules (, "declarative
/// hooks"; see `docs/plugins/hooks.md` point 13 and `docs/plugins/scripts.md`
/// for the design this shape is drawn from).
///
/// **Reachability disclosure, PER EVENT -- read this before adding a rule.**
/// Nothing here may claim to be reached that isn't. This
/// section itself, and every type it names, always parses and validates
/// (that part was never event-conditional). Whether a rule actually RUNS is:
///
/// - **`event == "pre_tool_use"`, `enabled: true`: DISPATCHED** -- `ConwayBuilder::build` filters
///   `rules` to exactly these and hands them to
///   `conway_runtime::permission::PermissionBroker::decide`, which invokes
///   each via the injected [`crate::plugin::HookRunner`] at the SAME tier
///   as a `deny` pattern rule -- before the mode gate, the cache, pattern
///   allows, and `AutoAllow`, so a denial is enforced under every
///   permission mode. **This still requires an actual runner.**
///   `ConwayBuilder::with_hook_runner` (mirroring `with_permission_gate`/
///   `with_context_hook`: not called at all is the default) is what
///   supplies one -- `conway-runtime` never constructs one itself -- it must not depend on `conway-tools` to
///   reach one).
///
///   **`conway-cli` supplies one.**
///   `build_conway` calls `ConwayBuilder::with_default_hook_runner` (a
///   convenience over `with_hook_runner` that constructs this workspace's
///   own `conway_tools::hook_runner::ProcessHookRunner`) unconditionally, so
///   a `pre_tool_use` rule written in a real `settings.json` and driven
///   through the CLI actually fires. **A third party embedding this crate
///   directly gets none of that automatically** -- the CLI's choice to call
///   `with_default_hook_runner` is that binary's own opt-in, not a default
///   this crate applies on a caller's behalf; an embedder must call
///   `with_hook_runner` or `with_default_hook_runner` itself, exactly like
///   every other optional port on `ConwayBuilder`. A `pre_tool_use` rule
///   declared here with no runner ever injected (the embedder case) still
///   parses, validates, and is silently never consulted -- exactly the same
///   gap this doc used to disclose for the whole section, now narrowed to
///   that one precondition.
/// - **`post_tool_use`, `session_starting`, `child_spawned`: DISPATCHED,
///   observation-only**. Same
///   injected-runner precondition as `pre_tool_use` above. These cannot
///   deny anything and they fail OPEN: a hook that errors or times out is
///   logged and the operation it observed is unaffected, which is the
///   opposite of `pre_tool_use` and is deliberate — the observed thing has
///   already happened, so breaking it because a logging script misfired
///   would be the wrong direction. Dispatched by
///   `conway_runtime::hook_dispatch::HookDispatcher::dispatch`.
/// - **`prompt_submitted`: DISPATCHED, may DENY but never MODIFY** (board
///   item). Fires at both prompt-submission
///   sites before the text reaches the agent loop, and fails CLOSED like
///   `pre_tool_use`. It cannot rewrite a word of what the user typed, and
///   that is a TYPE guarantee rather than an unwired path: the dispatch
///   reads only `HookPermissionVerdict`, which has no variant capable of
///   carrying replacement text.
/// - **`child_reported`: DISPATCHED, observation-only**. Same
///   injected-runner precondition and same fail-OPEN posture as
///   `post_tool_use`/`session_starting`/`child_spawned` above -- dispatched
///   by the identical `conway_runtime::hook_dispatch::HookDispatcher::
///   dispatch`, through the SAME runner. Fires once per agent that HAS a
///   parent (never for a root's own finish), for both a normal completion
///   (`AgentLoop::finish`) and a supervisor-synthesized terminal result
///   (`conway_runtime::supervisor`: a panic, or a task still unresponsive
///   past its grace window) -- gated on the same publish-race winner
///   `Event::AgentFinished` already uses at each site, so it fires exactly
///   once per agent regardless of which side wins.
/// - **`request_assembled` and `context_overflow`: DISPATCHED,
///   CONTEXT-EDITING** (board item `01KZRZZP6A4A27R3EN0HQAENBS`,
///   correcting this doc's own earlier "observation-only" claim for
///   `request_assembled`). Same injected-runner precondition as every other
///   event above, and still fails OPEN like the observation tier -- a
///   failing/timing-out/malformed script contributes nothing, logged
///   (`tracing::warn!`) rather than failing the turn -- but a subscribed
///   hook's `HookAnswer.context` (`conway_core::hook::ContextDelta`) is now
///   READ and APPLIED, via `conway_runtime::hook_dispatch::HookDispatcher::
///   dispatch_context` rather than `Self::dispatch`. **Append-only, by the
///   TYPE `ContextDelta` itself**: a hook may append a new segment (stamped
///   with `Provenance::SystemNote { reason: "script_hook:<its id>" }`,
///   naming the configured hook per the ACCEPTANCE's provenance
///   requirement) or exclude an existing one BY ID -- there is no field
///   anywhere capable of expressing in-place edit, reorder, or wholesale
///   replace (`conway_runtime::context::script_hook`'s own module doc has
///   the full type-level argument). An existing `request_assembled` rule
///   written purely for observation (its answer never sets `context`) is
///   UNAFFECTED: applying an empty `ContextDelta` is a no-op, so this is not
///   a breaking change to a shipped config surface.
///
///   `request_assembled` fires once per turn, from `conway_runtime::
///   agent_loop::AgentLoop::run_inner`, after `ContextBuilder::build` (and,
///   if one is registered, `ContextHook::before_request`'s own edit) and
///   before that turn's route/attempt call; `context_overflow` is the
///   script-hook counterpart of `ContextHook::on_overflow` (point 4 of
///   `docs/plugins/hooks.md`) -- it fires ONLY when routing/the attempt
///   engine rejects with `RoutingError::ContextTooLarge` (every candidate
///   failed SOLELY on headroom), never for a mixed `RoutingError::
///   NoCandidate` rejection; this boundary is unchanged and unwidened by
///   this event's addition. Both payloads are a SUMMARY plus per-segment
///   METADATA (id, role, provenance, estimated tokens) -- an id is what
///   `ContextDelta::excludes` needs to name a target, and
///   role/provenance is enough for a policy decision, but segment
///   `content` is never shipped: a verbatim context dump on every turn is
///   an unbounded, content-proportional cost this item's own design
///   question declined to pay unconditionally (`crate::hook_dispatch::
///   HookDispatcher::dispatch_context`'s own doc has the reasoning).
///
///   A Rust `ContextHook` (`ConwayBuilder::with_context_hook`) and a
///   configured script hook on the SAME event coexist: both are evaluated
///   independently against the SAME pre-edit payload (decision
///   `01KYTQVYPJW0PAAXRBEMAKZY0V`, "no chaining between context-editing
///   hooks") and their edits compose -- exclusions union, appends
///   concatenate in configured order. Two or more script hooks on the same
///   event compose the identical way. Every edit, Rust or script, is run
///   through the SAME tool-call/result coherence guard
///   (`conway_runtime::context::hook_guard::ensure_hook_payload_coherent`,
///   board item `01M00RGARPESWXYAVY960KDE7S`) before it can reach a
///   request -- a script hook that orphans a tool call/result pair is
///   refused, never repaired, exactly like the Rust `ContextHook` path
///   already was.
/// - **A namespaced `event` (`plugin_id.event_name`): DISPATCHED,
///   observation-only, IF AND ONLY IF an installed plugin actually
///   declares it** ( --
///   `PHILOSOPHY.md` §5's open vocabulary: "A plugin declares the events
///   it emits"). `ConwayBuilder::build` resolves every installed plugin's
///   own declared events (`conway_runtime::hook_dispatch::
///   declared_plugin_events`) and unions the well-formed, ACTUALLY-DECLARED
///   ones into the same dispatch table `post_tool_use` etc. already use --
///   same runner precondition, same fail-open posture, never deny-capable
///   (there is no plugin-event equivalent of `pre_tool_use`). A `match` on
///   a plugin event whose OWN declaration says its payload carries no tool
///   name is the identical typed, build-time error a core event without
///   one already gets. A namespaced `event` naming no installed plugin's
///   declared event parses, validates, and is silently never dispatched --
///   the SAME gap a typo'd core event name has always had.
///
/// **Default: an empty rule list.** This is the part of that rule which
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
    /// operator-visibility item (umbrella) uses
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
/// (`conway_tools::hook_runner::ProcessHookRunner`
///).
///
/// **Deliberately minimal.** The only fields beyond the five the owning
/// names are the ones already covered by those five --
/// specifically, no `cwd` override and no `args`-vs-`command` split were
/// added: `command` already being an argv vector (program, then its
/// arguments) settles the args-vs-command question by construction (the
/// first element IS the program, the rest ARE its arguments -- a separate
/// `args` field would just restate that split redundantly), and a `cwd`
/// override is left out because nothing runs yet to need one -- the runner
/// item is the one that knows what "current
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
    /// The event name this rule fires on -- either a bare core event (e.g.
    /// `"pre_tool_use"`) or, as of, a
    /// plugin-declared `"plugin_id.event_name"` (`PHILOSOPHY.md` §5: "That
    /// list is open rather than fixed"). `merge::validate`'s own event-shape
    /// check enforces the bare-vs-namespaced convention itself
    /// (`conway_core::event_name::validate_event_name(event, None)`) --
    /// this crate has no access to the resolved plugin set at that point,
    /// so whether a namespaced `event` names an ACTUALLY-declared plugin
    /// event is checked separately, at `ConwayBuilder::build`, which does.
    /// A namespaced `event` naming no installed plugin's declared event is
    /// tolerated exactly like a typo'd core event name always was: the
    /// rule parses, validates, and is silently never dispatched -- see
    /// [`HooksConfig`]'s own reachability doc.
    pub event: String,
    /// The tool-name matcher this rule fires for. `"match"` on the wire -- the exact
    /// spelling `PHILOSOPHY.md` §5's own example uses
    /// (`{"match": "bash", "run": "..."}`) -- while the Rust field is
    /// `match_tool` because `match` is a reserved word; that item's own "A
    /// design decision you must make and record" explicitly names
    /// `match_tool` as an acceptable choice.
    ///
    /// **The config-shape decision, recorded here rather than left
    /// ambiguous across two documents:** `PHILOSOPHY.md` is the 1.0
    /// specification (ruling of 2026-08-13) and the tree converges toward
    /// it, but convergence is field-by-field, not a wholesale reshape onto
    /// the page's illustrative event-keyed JSON. `HooksConfig::rules`
    /// stays a FLAT list keyed by an `event` field per entry (unchanged --
    /// see this struct's own doc) rather than moving to the page's nested
    /// `{"pre_tool_use": [...], "post_tool_use": [...]}` map, and `command`
    /// stays an argv `Vec<String>` rather than the page's single `run`
    /// shell string -- that argv-vs-`run` divergence was already a
    /// DELIBERATE, previously-recorded decision (see `command`'s own doc,
    /// immediately below) predating this item, not something this item
    /// reopens. What this item DOES converge is the part that was a true
    /// GAP rather than a considered divergence: the page's vocabulary had
    /// no matcher counterpart in the shipped schema at all
    /// (`pre_tool_use`/`post_tool_use` fired for every tool, unconditionally
    /// -- unusable for the page's own canonical example, "run the formatter
    /// after a write"). Adding `match` with the page's own spelling, while
    /// leaving the flat/argv shape as-is, is the smallest change that turns
    /// the gap into parity without a breaking reshape of every existing
    /// `[hooks]` block -- a full move to the nested shape remains available
    /// as later, purely additive work if a future item makes the case for
    /// it; nothing here forecloses it.
    ///
    /// `None` (the field omitted, the default) preserves TODAY's
    /// fire-for-every-tool-call behavior byte-for-byte -- an existing
    /// `settings.json` with no `match` key behaves identically before and
    /// after this item, which is the compatibility half of the ACCEPTANCE.
    /// `Some(pattern)` NARROWS which calls consult this rule; there is no
    /// way to WIDEN past "every call for this event", matching every other
    /// hook-narrowing guarantee in this section (`HookPermissionVerdict` has
    /// no `Allow` variant, for the identical reason).
    ///
    /// Applies only to `event`s that carry a tool name --
    /// `"pre_tool_use"`/`"post_tool_use"` -- enforced by `merge::validate`'s
    /// hooks check: a rule that sets `match` on any OTHER `event` is a
    /// surfaced, typed config error naming the rule's `id`, never silently
    /// ignored. `conway_core::hook::tool_matcher_matches` is the actual
    /// matching function this field's value is checked with once dispatch
    /// reads it (`conway_runtime::permission::PreToolUseHookSpec::matcher`,
    /// `conway_runtime::hook_dispatch::HookSpec::matcher`) -- exact string
    /// equality, or (if `pattern` contains `*`) a `*`-only glob against the
    /// tool's whole name; see that function's own doc for why exact-plus-
    /// glob is the full vocabulary and a regex dialect was declined.
    #[serde(rename = "match")]
    pub match_tool: Option<String>,
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
    /// item); still only READ, never enforced,
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
    /// asserts nothing about whether a runner exists (neither
    /// `ConwayBuilder::with_hook_runner` nor `with_default_hook_runner` may
    /// have been called -- true for a direct embedder of this crate, though
    /// no longer true for `conway-cli` itself, which now calls the latter
    /// unconditionally; see [`HooksConfig`]'s own doc for that precondition
    /// in full); it only says
    /// "don't treat the rule I just wrote as disabled", ordinary
    /// boolean-flag convention. `enabled: false` on a `pre_tool_use` rule
    /// is now genuinely load-bearing: `ConwayBuilder::build`'s filter into
    /// `PreToolUseHookSpec` drops a disabled rule before `PermissionBroker::
    /// decide` ever sees it.
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
            match_tool: None,
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
            "match": "bash",
            "command": ["/usr/local/bin/audit-hook", "--strict"],
            "timeout_ms": 3000,
            "enabled": true
          }
        ]
      }
    }
    "#;

    /// ACCEPTANCE: a well-formed `[hooks]` block with one entry parses and
    /// round-trips (serialize back, re-parse, equal). Includes `"match"`
    ///, spelled exactly as
    /// `PHILOSOPHY.md` §5's own example spells it, decoding into
    /// `HookEntry::match_tool`.
    #[test]
    fn hooks_block_round_trips() {
        let cfg: ConwayConfig = serde_json::from_str(HOOKS_BLOCK).expect("must parse");
        assert_eq!(cfg.hooks.rules.len(), 1);
        let rule = &cfg.hooks.rules[0];
        assert_eq!(rule.id, "audit-bash");
        assert_eq!(rule.event, "pre_tool_use");
        assert_eq!(rule.match_tool.as_deref(), Some("bash"));
        assert_eq!(
            rule.command,
            vec![
                "/usr/local/bin/audit-hook".to_string(),
                "--strict".to_string()
            ]
        );
        assert_eq!(rule.timeout_ms, 3000);
        assert!(rule.enabled);

        let reserialized = serde_json::to_string(&cfg).expect("must serialize");
        assert!(
            reserialized.contains("\"match\""),
            "the wire key must be literally \"match\", matching PHILOSOPHY.md §5's own \
             spelling: {reserialized}"
        );
        let cfg2: ConwayConfig = serde_json::from_str(&reserialized).expect("must re-parse");
        assert_eq!(cfg, cfg2);
    }

    /// ACCEPTANCE: an absent `"match"` preserves today's fire-for-every-tool
    /// behavior -- `match_tool` defaults to `None`, not `Some("")` or any
    /// other value that could accidentally narrow anything.
    #[test]
    fn hook_entry_with_no_match_key_defaults_to_none() {
        let json = r#"
        {
          "default_role": "coder",
          "roles": { "coder": { "chain": [] } },
          "hooks": {
            "rules": [
              { "id": "x", "event": "post_tool_use", "command": ["echo", "hi"] }
            ]
          }
        }
        "#;
        let cfg: ConwayConfig = serde_json::from_str(json).expect("must parse");
        assert_eq!(cfg.hooks.rules[0].match_tool, None);
    }

    /// A glob `"match"` (`*`) round-trips exactly like an exact one --
    /// `HookEntry::match_tool` itself does not distinguish the two shapes;
    /// that distinction lives entirely in
    /// `conway_core::hook::tool_matcher_matches`.
    #[test]
    fn hook_entry_glob_match_round_trips() {
        let json = r#"
        {
          "default_role": "coder",
          "roles": { "coder": { "chain": [] } },
          "hooks": {
            "rules": [
              { "id": "x", "event": "post_tool_use", "match": "fs.*", "command": ["cargo", "fmt"] }
            ]
          }
        }
        "#;
        let cfg: ConwayConfig = serde_json::from_str(json).expect("must parse");
        assert_eq!(cfg.hooks.rules[0].match_tool.as_deref(), Some("fs.*"));
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
        let err = result
            .expect_err("typo'd entry key must be rejected")
            .to_string();
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
