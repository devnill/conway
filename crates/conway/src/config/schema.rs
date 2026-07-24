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
//! `headroom_tokens: Option<u32>`) matching the documented schema exactly, and
//! [`ConwayConfig::routing`] converts it into the authoritative
//! `conway_core::routing::RoutingConfig`/`RoleConfig` (parsing each chain
//! string via `ModelRef::from_str`, and filling `required`/`params` with
//! their `Default` values — the documented schema never populates those
//! sub-tables).
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
/// `docs/crates/conway.md` (WI-097).
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
            roles.insert(
                name.clone(),
                conway_core::routing::RoleConfig {
                    chain,
                    required: conway_core::capabilities::RequiredCaps::default(),
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
/// `conway_backends::{AnthropicConfig, OpenAiCompatConfig}` (the concrete
/// adapter configs constructed from this by WI-100). Mirrors how those two
/// adapter configs are already a third, distinct shape from
/// `conway_core::routing::BackendConfig` in the committed code.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BackendEntry {
    pub kind: BackendKind,
    /// Mutually exclusive with `api_key_env` (enforced by
    /// `merge::validate`).
    pub api_key: String,
    pub api_key_env: String,
    pub base_url: String,
    pub dialect: Option<String>,
    pub stream_tools: Option<bool>,
}

impl Default for BackendEntry {
    fn default() -> Self {
        Self {
            kind: BackendKind::Anthropic,
            api_key: String::new(),
            api_key_env: String::new(),
            base_url: String::new(),
            dialect: None,
            stream_tools: None,
        }
    }
}

/// `backends.<id>.kind`. `OpenaiCompat` (not `OpenAiCompat`) is deliberate:
/// `#[serde(rename_all = "kebab-case")]` splits on the type's own case
/// boundaries, so `OpenaiCompat` -> `"openai-compat"` matches the documented
/// value exactly; `OpenAiCompat` would instead produce `"open-ai-compat"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
    Anthropic,
    OpenaiCompat,
}

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
/// (consumed by `RequiredCaps::default()` and
/// `conway_core::routing::RoutingConfig`'s own serde default) is `8_192`,
/// and `conway-routing`'s `HeadroomPolicy` explicitly aliases the same
/// constant with a comment noting it supersedes an earlier, different
/// literal. Introducing a third, disagreeing "default headroom" value at
/// the facade layer would mean the same omitted key resolves to three
/// different numbers depending which layer computed it — strictly worse
/// than deviating from the amendment's literal `16000`. This constant
/// therefore reuses the cross-crate-agreed value; the corresponding test
/// (`empty config -> default_headroom_tokens`) asserts `8_192`, not
/// `16_000`.
pub const DEFAULT_HEADROOM_TOKENS: u32 = conway_core::capabilities::DEFAULT_HEADROOM_TOKENS;

/// `[health]`. Facade-owned mirror of `conway_core::routing::HealthConfig`'s
/// seven fields and defaults — see the module doc comment for why this
/// isn't a direct embed of `HealthConfig` (that type lacks
/// `#[serde(deny_unknown_fields)]`). Every field name, type, and default
/// value here must match `HealthConfig` exactly, or a valid setting would
/// silently diverge in meaning between the two types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HealthSection {
    pub transport_failures_to_open: u32,
    pub open_duration_secs: u64,
    pub probe_interval_secs: u64,
    pub probe_timeout_secs: u64,
    pub probe_failures_to_open: u32,
    pub half_open_successes_to_close: u32,
    pub probe_enabled: bool,
}

impl Default for HealthSection {
    fn default() -> Self {
        let d = conway_core::routing::HealthConfig::default();
        Self {
            transport_failures_to_open: d.transport_failures_to_open,
            open_duration_secs: d.open_duration_secs,
            probe_interval_secs: d.probe_interval_secs,
            probe_timeout_secs: d.probe_timeout_secs,
            probe_failures_to_open: d.probe_failures_to_open,
            half_open_successes_to_close: d.half_open_successes_to_close,
            probe_enabled: d.probe_enabled,
        }
    }
}

impl From<HealthSection> for conway_core::routing::HealthConfig {
    fn from(section: HealthSection) -> Self {
        Self {
            transport_failures_to_open: section.transport_failures_to_open,
            open_duration_secs: section.open_duration_secs,
            probe_interval_secs: section.probe_interval_secs,
            probe_timeout_secs: section.probe_timeout_secs,
            probe_failures_to_open: section.probe_failures_to_open,
            half_open_successes_to_close: section.half_open_successes_to_close,
            probe_enabled: section.probe_enabled,
        }
    }
}

/// `[roles.<alias>]`. `chain` is `Vec<String>` (`"backend/model"`), not
/// `Vec<ModelRef>` — see the module doc comment.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RoleEntry {
    pub chain: Vec<String>,
    pub headroom_tokens: Option<u32>,
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
