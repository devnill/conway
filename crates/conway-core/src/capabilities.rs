//! The capability description model: what a `(backend, model)` pair can do,
//! and the content-free requirement/gating vocabulary the router uses to
//! filter candidates.
//!
//! Per architecture §4.1, transcribed exactly except for one deliberate
//! deviation: [`CacheMode::ExplicitBreakpoints`] carries `ttls: Vec<CacheTtl>`
//! rather than `&'static [CacheTtl]`, because every public type in this crate
//! must be `Deserialize` and a `'static` slice cannot be.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{ModelId, RoleAlias};
use crate::ports::Admission;
use crate::routing::RoutingConfig;

/// A backend/model's declared capabilities. Per-`(backend, model)`, not
/// per-backend: quantization and chat template change tool-call reliability
/// independent of the server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    pub tool_calling: ToolCallSupport,
    pub cache: CacheMode,
    pub parallel_tool_calls: bool,
    pub structured_output: StructuredOutput,
    pub max_context_tokens: u32,
    pub reasoning: bool,
    pub reliability_tier: ReliabilityTier,
}

/// How (and whether) a model supports tool/function calling.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallSupport {
    None,
    NonStreamingOnly,
    Streaming { validated: bool },
}

impl ToolCallSupport {
    /// A total ordering over tool-call support levels, so `RequiredCaps` can
    /// express "tool_calling >= NonStreamingOnly".
    ///
    /// `None` = 0, `NonStreamingOnly` = 1, `Streaming { validated: false }` =
    /// 2, `Streaming { validated: true }` = 3.
    pub fn rank(&self) -> u8 {
        match self {
            ToolCallSupport::None => 0,
            ToolCallSupport::NonStreamingOnly => 1,
            ToolCallSupport::Streaming { validated: false } => 2,
            ToolCallSupport::Streaming { validated: true } => 3,
        }
    }
}

impl PartialOrd for ToolCallSupport {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.rank().partial_cmp(&other.rank())
    }
}

/// How a backend caches shared prompt prefixes. Caching is economics, never
/// correctness-bearing: an adapter MAY ignore any hint derived from this
/// without changing assembled request content.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheMode {
    /// Anthropic: explicit addressable breakpoints.
    ExplicitBreakpoints {
        max_breakpoints: u8,
        ttls: Vec<CacheTtl>,
    },
    /// OpenAI / vLLM / Ollama: passive prefix matching. Hints become
    /// ORDERING guarantees only.
    ImplicitPrefix {
        min_prefix_tokens: u32,
    },
    /// Llama.cpp native slot save/restore (post-MVP adapter).
    SlotKv,
    None,
}

/// A cache breakpoint's time-to-live.
///
/// Defined here (not in `segment.rs`) because earlier work depends only on
/// and needs this type before the `segment.rs` exists.
/// earlier work MUST re-export this type from `segment.rs` rather than redefining
/// it.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheTtl {
    FiveMinutes,
    OneHour,
}

/// Whether a model supports structured/constrained output.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredOutput {
    None,
    JsonSchema,
    Grammar,
}

/// How well-vetted a backend/model pairing is.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReliabilityTier {
    Verified,
    Community,
    Unknown,
}

/// Tokens reserved for model output and reasoning when no caller supplies a
/// more specific value. See [`RequiredCaps::headroom_tokens`].
pub const DEFAULT_HEADROOM_TOKENS: u32 = 8_192;

fn default_headroom_tokens() -> u32 {
    DEFAULT_HEADROOM_TOKENS
}

/// Default divisor for `conway::config::schema::RoutingSection::
/// headroom_fraction`: `window / d`. `d = 10` (10%) is the exact figure
/// that field's own doc comment has illustrated since the fraction was
/// introduced (board item `01M1AVZPTRSWVE33G4DTJY7Q1B` finishes what an
/// earlier commit landed opt-in with this same worked example) -- reused
/// here rather than a fresh number, so the default this constant now drives
/// is not a silent departure from what the doc already promised: `8192`
/// fixed tokens is 25% of a 32K window but under 1% of a 976K one, and a
/// 10% reservation lands generously between those two extremes for most
/// configured models.
pub const DEFAULT_HEADROOM_FRACTION: u32 = 10;

/// A declarative, config-time-resolved reservation of output/reasoning
/// tokens: a global default with per-role overrides.
///
/// Moved here from `conway-routing`'s `config.rs` -- enumerating every read of
/// [`HeadroomPolicy::resolve`] and every construction site found it is not
/// a total, drop-in replacement for [`RoutingConfig::headroom_for`] --
/// `conway-routing`'s `DeclarativeRouter::new` takes a `HeadroomPolicy` as a
/// caller-supplied sidecar and cross-checks its resolution against
/// `RoutingConfig::headroom_for` for every role at construction time,
/// rejecting a disagreement as `ConfigIssueKind::HeadroomSourcesDisagree`
/// rather than silently trusting either source. That check has no meaning
/// if there is only one source left to compare, so the type stays, moved
/// beside [`DEFAULT_HEADROOM_TOKENS`] rather than deleted.
///
/// Resolution ([`HeadroomPolicy::resolve`]) happens once, is total, and
/// never depends on anything but the policy and a role name -- no request
/// data, no runtime measurement. `per_role` is not populated by `serde`
/// (`#[serde(skip)]`): it is built explicitly by
/// [`HeadroomPolicy::from_routing_config`] from [`RoutingConfig`]'s own
/// per-role `headroom_tokens`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HeadroomPolicy {
    /// Mirrors the `[routing] default_headroom_tokens` config key.
    pub default_headroom_tokens: u32,
    #[serde(skip)]
    pub per_role: BTreeMap<RoleAlias, u32>,
}

impl Default for HeadroomPolicy {
    fn default() -> Self {
        Self {
            default_headroom_tokens: DEFAULT_HEADROOM_TOKENS,
            per_role: BTreeMap::new(),
        }
    }
}

impl HeadroomPolicy {
    /// The per-role override when present, else `default_headroom_tokens`.
    /// Total: never errors, never panics, including for a `role` absent
    /// from `per_role` (the global default is returned).
    pub fn resolve(&self, role: &RoleAlias) -> u32 {
        self.per_role
            .get(role)
            .copied()
            .unwrap_or(self.default_headroom_tokens)
    }

    /// Builds a `HeadroomPolicy` from a [`RoutingConfig`]: the global
    /// default from `RoutingConfig::default_headroom_tokens`, and the
    /// per-role table from every `RoleConfig::headroom_tokens` that is
    /// `Some(_)`.
    pub fn from_routing_config(config: &RoutingConfig) -> Self {
        let per_role = config
            .roles
            .iter()
            .filter_map(|(name, role)| {
                role.headroom_tokens
                    .map(|tokens| (RoleAlias::new(name.clone()), tokens))
            })
            .collect();
        Self {
            default_headroom_tokens: config.default_headroom_tokens,
            per_role,
        }
    }
}

/// The capability floor a routing candidate must clear, plus the reserved
/// output/reasoning budget ("headroom") that makes context-window gating
/// measure the whole turn rather than just the assembled prompt.
///
/// `max_context_tokens` in a backend's [`Capabilities`] is the *total*
/// window: prompt plus generated output plus reasoning tokens. Gating only
/// on assembled prompt size therefore admits requests that overflow
/// mid-generation, which surfaces as a `BackendError::ContextOverflow` after
/// the tokens are already paid for. `headroom_tokens` is the reserved
/// remainder: a candidate is compatible only if
/// `est_tokens + headroom_tokens <= caps.max_context_tokens`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RequiredCaps {
    pub tool_calling: Option<ToolCallSupport>,
    pub min_context: Option<u32>,
    pub structured_output: Option<StructuredOutput>,
    pub reasoning: Option<bool>,
    pub parallel_tool_calls: Option<bool>,
    pub min_reliability: Option<ReliabilityTier>,
    /// Tokens reserved for model output and reasoning. A candidate is
    /// compatible only if `est_tokens + headroom_tokens <=
    /// caps.max_context_tokens`. Never `Option`: a request with no reserved
    /// output space is always a configuration error, so the field carries
    /// [`DEFAULT_HEADROOM_TOKENS`] rather than "unspecified".
    #[serde(default = "default_headroom_tokens")]
    pub headroom_tokens: u32,
}

impl Default for RequiredCaps {
    // Written by hand (not derived) so the headroom default is explicit and
    // does not silently become `0` if a `#[derive(Default)]` were added
    // later without noticing this field.
    fn default() -> Self {
        Self {
            tool_calling: None,
            min_context: None,
            structured_output: None,
            reasoning: None,
            parallel_tool_calls: None,
            min_reliability: None,
            headroom_tokens: DEFAULT_HEADROOM_TOKENS,
        }
    }
}

impl RequiredCaps {
    /// Total window the request will occupy. Saturating.
    pub fn total_required(&self, est_tokens: u32) -> u32 {
        est_tokens.saturating_add(self.headroom_tokens)
    }

    /// How far `total_required(est_tokens)` exceeds `caps.max_context_tokens`
    /// (`0` if it does not exceed it). Saturating. Delegates to
    /// `Admission::shortfall_tokens` (the headroom arithmetic itself, in one place
    /// lives in exactly one place, `conway_core::ports::check_admission`'s
    /// `Admission`, not restated here).
    pub fn shortfall(&self, caps: &Capabilities, est_tokens: u32) -> u32 {
        self.admission(caps, est_tokens).shortfall_tokens()
    }

    /// Builds the `Admission` this requirement's headroom gate reduces to,
    /// against `caps` for `est_tokens`. Private: `satisfied_by` and
    /// `shortfall` are the only callers, and both only need `Admission`'s
    /// derived numbers, never the value itself.
    fn admission(&self, caps: &Capabilities, est_tokens: u32) -> Admission {
        Admission {
            est_tokens,
            headroom_tokens: self.headroom_tokens,
            max_context_tokens: caps.max_context_tokens,
        }
    }

    /// Checks `caps` against every set requirement, given the current
    /// request's estimated prompt size. Returns one human-readable string
    /// per unmet requirement (used verbatim in `RoutingReason::CapabilitySkip`
    /// and `RoutingError::NoCandidate`), checked in this fixed order:
    /// tool_calling, context (headroom-aware), min_context (explicit floor),
    /// structured_output, reasoning, parallel_tool_calls, min_reliability.
    ///
    /// All arithmetic is saturating; no `u32` overflow path can panic.
    pub fn satisfied_by(&self, caps: &Capabilities, est_tokens: u32) -> Result<(), Vec<String>> {
        let mut missing = Vec::new();

        if let Some(required) = &self.tool_calling {
            if caps.tool_calling.rank() < required.rank() {
                missing.push(format!(
                    "tool_calling: requires {required:?}, model provides {:?}",
                    caps.tool_calling
                ));
            }
        }

        // Context: headroom-aware, the load-bearing check. The fit
        // comparison itself is `Admission::fits`/`shortfall_tokens` (
        // one headroom arithmetic in the workspace, `conway_core::ports::
        // check_admission`) -- this method's own contribution is only its
        // distinct message wording/ordering, which `capability.rs`
        // (`conway-routing`)'s `satisfies` -- the router-facing sibling
        // formulation of this same admission decision, pinned to agree with
        // this one by `satisfies_agrees_with_core_on_accept_reject` -- keeps
        // deliberately different (CLI-facing golden strings it cannot
        // change; see that module's own doc).
        let admission = self.admission(caps, est_tokens);
        if !admission.fits() {
            missing.push(format!(
                "context: needs {est_tokens} prompt + {} headroom = {} tokens, model provides {} (short by {})",
                self.headroom_tokens,
                admission.required_tokens(),
                caps.max_context_tokens,
                admission.shortfall_tokens()
            ));
        }

        // min_context: an independent, coarser floor a role may set to
        // exclude small models regardless of the current request size.
        if let Some(required) = self.min_context {
            if required > caps.max_context_tokens {
                missing.push(format!(
                    "min_context: requires {required} tokens, model provides {}",
                    caps.max_context_tokens
                ));
            }
        }

        if let Some(required) = &self.structured_output {
            if *required != caps.structured_output {
                missing.push(format!(
                    "structured_output: requires {required:?}, model provides {:?}",
                    caps.structured_output
                ));
            }
        }

        if let Some(required) = self.reasoning {
            if required && !caps.reasoning {
                missing.push(format!(
                    "reasoning: requires {required:?}, model provides {:?}",
                    caps.reasoning
                ));
            }
        }

        if let Some(required) = self.parallel_tool_calls {
            if required && !caps.parallel_tool_calls {
                missing.push(format!(
                    "parallel_tool_calls: requires {required:?}, model provides {:?}",
                    caps.parallel_tool_calls
                ));
            }
        }

        if let Some(required) = &self.min_reliability {
            if caps.reliability_tier != *required {
                missing.push(format!(
                    "min_reliability: requires {required:?}, model provides {:?}",
                    caps.reliability_tier
                ));
            }
        }

        if missing.is_empty() {
            Ok(())
        } else {
            Err(missing)
        }
    }
}

/// Tokens a single tool result may occupy in an assembled request before
/// [`crate::provenance::Provenance::ToolResult`] admission (board item
/// `01M1AVZPTRSWVE33G4DTJY7Q1B`) replaces its content with a not-admitted
/// note, when no more specific bound is configured or computable. `0` is the
/// distinct "unlimited" sentinel this constant is never itself set to --
/// `ToolResultBoundPolicy::resolve` returns it verbatim only when a role, or
/// the global default, is explicitly configured to `0`.
///
/// Also doubles as the fixed cap a fraction-computed bound
/// (`conway::config::schema::RoutingSection::tool_result_bound_fraction`) is
/// clamped to, so a 1M-window model does not admit a huge single tool result
/// that would still degrade attention even though it is a small fraction of
/// the window. `8_192` tokens is about 32KB of text under this crate's own
/// `chars/4` estimator (`conway_runtime::context::builder::
/// estimate_block_tokens`) -- the same "32K bytes" figure this board item's
/// own spec names as an example cap, chosen deliberately rather than an
/// unrelated round number: large enough to admit a typical file read or
/// search result, small enough that one oversized result cannot alone
/// consume a meaningful fraction of even a mid-sized window. This is a
/// CEILING, not evidence that every reachable window is at least this
/// large -- `docs/providers.md` documents Ollama alone serving windows from
/// 4K to 1M tokens, well under this cap on the small end, which is exactly
/// why [`DEFAULT_TOOL_RESULT_BOUND_FLOOR`] exists: the fraction-computed
/// bound below this cap still needs its own floor.
pub const DEFAULT_TOOL_RESULT_BOUND_CAP_TOKENS: u32 = 8_192;

/// Floor a fraction-computed tool-result bound (`window / fraction`) is
/// raised to before the [`DEFAULT_TOOL_RESULT_BOUND_CAP_TOKENS`] cap is
/// applied -- mirrors the sibling `conway::config::schema::HEADROOM_FLOOR`
/// at the same value, for the same reason: without it, a small enough
/// window divides down to a bound
/// that is not a size safety valve but a role-breaking default (a 4096-token
/// window at the default 1/5 fraction computes to 819 tokens -- under the
/// content of a routine file read or grep result), and integer division can
/// land on exactly `0`, which collides with `ToolResultBoundPolicy::
/// resolve`'s own "0 means unlimited" sentinel and silently INVERTS intent
/// (an unreachably strict bound becoming no bound at all) instead of merely
/// being too strict. `2_048` tokens (~8KB) is chosen to match the headroom
/// floor's own already-reviewed number rather than invent a second one; see
/// that constant's doc for the "does not starve a small model" reasoning,
/// which applies here identically since the concern (a computed value on a
/// tiny window must stay usable) is the same shape.
pub const DEFAULT_TOOL_RESULT_BOUND_FLOOR: u32 = 2_048;

/// Default divisor for the adaptive tool-result bound: `window / d`. `d = 5`
/// (20%) sits at the generous end of this board item's own suggested 10-20%
/// range -- move 1's gate exists to catch one outlier result blowing the
/// whole window, not to ration ordinary large-but-legitimate results (a big
/// file read, a wide search), so this leaves more room per-result than
/// [`crate::capabilities`]'s headroom fraction (10%, reserved for the
/// model's OWN output across the whole turn, a stingier budget by
/// necessity). Combined with [`DEFAULT_TOOL_RESULT_BOUND_CAP_TOKENS`] via
/// `min`, so a huge window still cannot admit an unreasonably large single
/// result.
pub const DEFAULT_TOOL_RESULT_BOUND_FRACTION: u32 = 5;

/// A resolved bound on how many tokens a single tool result may occupy in
/// assembled context before admission replaces it with a not-admitted note
/// (board item `01M1AVZPTRSWVE33G4DTJY7Q1B`, "refuse-to-admit", per the
/// operator's 2026-09-01 ruling -- NOT silent truncation). Shape mirrors
/// [`HeadroomPolicy`] (a global default plus a `per_role` table, resolved
/// once and total), but is deliberately NOT built by a `from_routing_config`
/// sibling the way [`HeadroomPolicy`] is: this bound is a context-ASSEMBLY
/// concern, never consulted by routing/admission (`RequiredCaps`,
/// `RoutingConfig`), so it has no reason to live on
/// `conway_core::routing::RoleConfig` the way `headroom_tokens` does, and
/// every reason not to -- adding a field there would touch `RoutingConfig`'s
/// and `RoleConfig`'s many existing call sites across `conway-plugin-routing`
/// and every crate that constructs a `RouteRequest`, for a value routing
/// itself never reads. `conway::builder::ConwayBuilder::build` (the sole
/// production constructor) builds this directly from `RoutingSection::
/// tool_result_bound_fraction`/`tool_result_bound_cap_tokens` and each
/// role's own chain against model metadata -- the identical
/// smallest-reachable-window technique `headroom_fraction` already uses,
/// just not routed back through `RoutingConfig` to get there.
///
/// `0` means unlimited (the gate never fires) -- an explicit operator
/// choice, at either the global or the per-role level, never a value this
/// policy computes on its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ToolResultBoundPolicy {
    /// Mirrors the `[routing] tool_result_bound_cap_tokens`/
    /// `tool_result_bound_fraction` config keys' resolved fallback (used
    /// whenever a role has no adaptively-computed or explicitly-overridden
    /// entry in `per_role`).
    pub default_bound_tokens: u32,
    #[serde(skip)]
    pub per_role: BTreeMap<RoleAlias, u32>,
}

impl Default for ToolResultBoundPolicy {
    fn default() -> Self {
        Self {
            default_bound_tokens: DEFAULT_TOOL_RESULT_BOUND_CAP_TOKENS,
            per_role: BTreeMap::new(),
        }
    }
}

impl ToolResultBoundPolicy {
    /// The per-role entry when present, else `default_bound_tokens`. Total:
    /// never errors, never panics. `0` (from either source) means
    /// unlimited.
    pub fn resolve(&self, role: &RoleAlias) -> u32 {
        self.per_role
            .get(role)
            .copied()
            .unwrap_or(self.default_bound_tokens)
    }
}

/// `window / fraction`, floored then capped -- the pure arithmetic
/// `conway::builder::ConwayBuilder::build` uses to compute one role's
/// adaptive per-`per_role` entry in [`ToolResultBoundPolicy`], pulled out
/// here so it is unit-testable without going through the whole builder
/// (which needs a real `ConwayConfig` plus model metadata to reach it).
/// `fraction == 0` (adaptive disabled) and "no model metadata reachable for
/// this role's chain" are both handled by the caller BEFORE this is called
/// -- this function's only job is the three-number arithmetic once a
/// `smallest_window` is already known and `fraction > 0`; it never returns
/// `0` given a nonzero `floor`, which is the point: `window / fraction`
/// alone can integer-divide down to exactly `0` on a small enough window,
/// colliding with [`ToolResultBoundPolicy::resolve`]'s own "0 means
/// unlimited" sentinel and silently inverting intent. `floor` is applied
/// BEFORE `cap`, so an operator who sets `cap < floor` still gets `cap`
/// (the cap is the harder ceiling either way) rather than a bound that
/// exceeds their own configured cap.
pub fn resolve_adaptive_tool_result_bound(
    smallest_window: u32,
    fraction: u32,
    cap: u32,
    floor: u32,
) -> u32 {
    debug_assert!(fraction > 0, "caller must handle fraction == 0 itself");
    (smallest_window / fraction).max(floor).min(cap)
}

#[cfg(test)]
mod tool_result_bound_tests {
    use super::*;

    #[test]
    fn a_tiny_window_is_raised_to_the_floor_not_left_near_zero() {
        // 4096 / 5 = 819, well under a usable bound -- the floor must win.
        let bound = resolve_adaptive_tool_result_bound(
            4_096,
            5,
            DEFAULT_TOOL_RESULT_BOUND_CAP_TOKENS,
            DEFAULT_TOOL_RESULT_BOUND_FLOOR,
        );
        assert_eq!(bound, DEFAULT_TOOL_RESULT_BOUND_FLOOR);
    }

    #[test]
    fn a_window_smaller_than_the_fraction_never_computes_to_the_unlimited_sentinel() {
        // Integer division alone would land on exactly 0 here (3 / 5 == 0),
        // which collides with `resolve`'s own "0 means unlimited" sentinel.
        let bound = resolve_adaptive_tool_result_bound(
            3,
            5,
            DEFAULT_TOOL_RESULT_BOUND_CAP_TOKENS,
            DEFAULT_TOOL_RESULT_BOUND_FLOOR,
        );
        assert_ne!(bound, 0);
        assert_eq!(bound, DEFAULT_TOOL_RESULT_BOUND_FLOOR);
    }

    #[test]
    fn a_huge_window_is_clamped_to_the_cap_not_left_at_twenty_percent() {
        // 1_000_000 / 5 = 200_000, far past the cap.
        let bound = resolve_adaptive_tool_result_bound(
            1_000_000,
            5,
            DEFAULT_TOOL_RESULT_BOUND_CAP_TOKENS,
            DEFAULT_TOOL_RESULT_BOUND_FLOOR,
        );
        assert_eq!(bound, DEFAULT_TOOL_RESULT_BOUND_CAP_TOKENS);
    }

    #[test]
    fn a_mid_sized_window_is_governed_by_neither_the_floor_nor_the_cap() {
        // 30_000 / 5 = 6_000: above the 2_048 floor, below the 8_192 cap --
        // the fraction's own arithmetic must be what wins.
        let bound = resolve_adaptive_tool_result_bound(
            30_000,
            5,
            DEFAULT_TOOL_RESULT_BOUND_CAP_TOKENS,
            DEFAULT_TOOL_RESULT_BOUND_FLOOR,
        );
        assert_eq!(bound, 6_000);
    }

    #[test]
    fn an_operators_cap_below_the_floor_still_wins_as_the_harder_ceiling() {
        let bound =
            resolve_adaptive_tool_result_bound(4_096, 5, 1_000, DEFAULT_TOOL_RESULT_BOUND_FLOOR);
        assert_eq!(bound, 1_000);
    }
}

/// The result of a `Backend::probe` liveness/readiness check.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProbeReport {
    pub ok: bool,
    pub latency_ms: u32,
    pub models: Vec<ModelId>,
    pub detail: Option<String>,
    pub at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(max_context_tokens: u32) -> Capabilities {
        Capabilities {
            tool_calling: ToolCallSupport::NonStreamingOnly,
            cache: CacheMode::None,
            parallel_tool_calls: false,
            structured_output: StructuredOutput::None,
            max_context_tokens,
            reasoning: false,
            reliability_tier: ReliabilityTier::Community,
        }
    }

    // -----------------------------------------------------------------
    // HeadroomPolicy (moved here from conway-routing's config.rs, board
    // item)
    // -----------------------------------------------------------------

    #[test]
    fn headroom_policy_default_matches_core_constant_and_empty() {
        let policy = HeadroomPolicy::default();
        assert_eq!(policy.default_headroom_tokens, DEFAULT_HEADROOM_TOKENS);
        assert!(policy.per_role.is_empty());
    }

    #[test]
    fn headroom_policy_deserializes_the_routing_table_shape() {
        let policy: HeadroomPolicy = serde_json::from_str(r#"{"default_headroom_tokens":4096}"#)
            .expect("valid HeadroomPolicy json");
        assert_eq!(policy.default_headroom_tokens, 4_096);
        assert!(policy.per_role.is_empty());
    }

    #[test]
    fn headroom_policy_omitting_the_field_deserializes_to_default() {
        let policy: HeadroomPolicy =
            serde_json::from_str("{}").expect("empty document uses default");
        assert_eq!(policy, HeadroomPolicy::default());
    }

    #[test]
    fn headroom_resolve_per_role_hit() {
        let mut per_role = BTreeMap::new();
        per_role.insert(RoleAlias::new("planner"), 16_384);
        let policy = HeadroomPolicy {
            default_headroom_tokens: 4_096,
            per_role,
        };
        assert_eq!(policy.resolve(&RoleAlias::new("planner")), 16_384);
    }

    #[test]
    fn headroom_resolve_per_role_miss_falls_back_to_global_default() {
        let mut per_role = BTreeMap::new();
        per_role.insert(RoleAlias::new("planner"), 16_384);
        let policy = HeadroomPolicy {
            default_headroom_tokens: 4_096,
            per_role,
        };
        // "fast" has no override.
        assert_eq!(policy.resolve(&RoleAlias::new("fast")), 4_096);
    }

    #[test]
    fn headroom_resolve_empty_policy_returns_default() {
        let policy = HeadroomPolicy::default();
        assert_eq!(
            policy.resolve(&RoleAlias::new("anything")),
            DEFAULT_HEADROOM_TOKENS
        );
        // Total: even a role absent from every table never panics/errors.
        assert_eq!(
            policy.resolve(&RoleAlias::new("unknown-role")),
            DEFAULT_HEADROOM_TOKENS
        );
    }

    #[test]
    fn headroom_policy_from_routing_config_reads_per_role_override_and_global_default() {
        use crate::ids::{BackendId, ModelRef};
        use crate::routing::{HealthConfig, RoleConfig};

        let mut roles = BTreeMap::new();
        roles.insert(
            "planner".to_string(),
            RoleConfig {
                chain: vec![ModelRef {
                    backend: BackendId::new("anthropic"),
                    model: ModelId::new("claude-sonnet-4-6"),
                }],
                headroom_tokens: Some(16_384),
                ..Default::default()
            },
        );
        roles.insert(
            "fast".to_string(),
            RoleConfig {
                chain: vec![ModelRef {
                    backend: BackendId::new("local"),
                    model: ModelId::new("qwen3-coder-80b"),
                }],
                headroom_tokens: None,
                ..Default::default()
            },
        );
        let routing_config = RoutingConfig {
            roles,
            health: HealthConfig::default(),
            default_headroom_tokens: 4_096,
        };

        let policy = HeadroomPolicy::from_routing_config(&routing_config);
        assert_eq!(policy.default_headroom_tokens, 4_096);
        assert_eq!(policy.resolve(&RoleAlias::new("planner")), 16_384);
        assert_eq!(policy.resolve(&RoleAlias::new("fast")), 4_096);
    }

    #[test]
    fn tool_call_support_rank_and_order() {
        assert_eq!(ToolCallSupport::None.rank(), 0);
        assert_eq!(ToolCallSupport::NonStreamingOnly.rank(), 1);
        assert_eq!(ToolCallSupport::Streaming { validated: false }.rank(), 2);
        assert_eq!(ToolCallSupport::Streaming { validated: true }.rank(), 3);

        assert!(ToolCallSupport::None < ToolCallSupport::NonStreamingOnly);
        assert!(
            ToolCallSupport::NonStreamingOnly < ToolCallSupport::Streaming { validated: false }
        );
        assert!(
            ToolCallSupport::Streaming { validated: false }
                < ToolCallSupport::Streaming { validated: true }
        );
    }

    #[test]
    fn required_caps_default_has_default_headroom() {
        let rc = RequiredCaps::default();
        assert!(rc.tool_calling.is_none());
        assert_eq!(rc.headroom_tokens, DEFAULT_HEADROOM_TOKENS);
    }

    #[test]
    fn headroom_tokens_round_trips_and_serializes_key() {
        let rc: RequiredCaps = serde_json::from_str(r#"{"headroom_tokens":8192}"#).unwrap();
        assert_eq!(rc.headroom_tokens, 8192);

        let default_json = serde_json::to_value(RequiredCaps::default()).unwrap();
        assert!(
            default_json.get("headroom_tokens").is_some(),
            "default RequiredCaps must serialize the headroom_tokens key: {default_json:?}"
        );
        assert_eq!(default_json["headroom_tokens"], DEFAULT_HEADROOM_TOKENS);
    }

    #[test]
    fn headroom_default_applies_when_field_omitted() {
        let rc: RequiredCaps = serde_json::from_str("{}").unwrap();
        assert_eq!(rc.headroom_tokens, DEFAULT_HEADROOM_TOKENS);
    }

    #[test]
    fn min_context_error_names_both_numbers() {
        let required = RequiredCaps {
            min_context: Some(200_000),
            ..RequiredCaps::default()
        };
        // A tiny headroom and large max_context so only min_context fails.
        let model_caps = Capabilities {
            max_context_tokens: 32_768,
            ..caps(32_768)
        };
        let err = required.satisfied_by(&model_caps, 0).unwrap_err();
        let joined = err.join(" | ");
        assert!(joined.contains("200000"), "missing required: {joined}");
        assert!(joined.contains("32768"), "missing available: {joined}");
        assert!(joined.contains("min_context"), "missing label: {joined}");
    }

    #[test]
    fn context_error_names_est_headroom_max_and_shortfall() {
        let required = RequiredCaps {
            headroom_tokens: 8_192,
            ..RequiredCaps::default()
        };
        let model_caps = caps(32_768);
        let err = required.satisfied_by(&model_caps, 30_000).unwrap_err();
        let joined = err.join(" | ");
        for needle in ["30000", "8192", "38192", "32768", "5424"] {
            assert!(joined.contains(needle), "missing {needle} in {joined}");
        }
    }

    #[test]
    fn context_check_ok_when_within_budget() {
        let required = RequiredCaps {
            headroom_tokens: 8_192,
            ..RequiredCaps::default()
        };
        let model_caps = caps(32_768);
        assert!(required.satisfied_by(&model_caps, 20_000).is_ok());
    }

    #[test]
    fn context_boundary_exact_fit_passes_one_over_fails() {
        let required = RequiredCaps {
            headroom_tokens: 8_192,
            ..RequiredCaps::default()
        };
        let model_caps = caps(32_768);
        // est + headroom == max_context_tokens: passes.
        assert!(required.satisfied_by(&model_caps, 24_576).is_ok());
        // one token over: fails.
        assert!(required.satisfied_by(&model_caps, 24_577).is_err());
    }

    #[test]
    fn saturating_arithmetic_never_panics_at_u32_max() {
        let required = RequiredCaps {
            headroom_tokens: 8_192,
            ..RequiredCaps::default()
        };
        let model_caps = caps(32_768);
        let err = required.satisfied_by(&model_caps, u32::MAX).unwrap_err();
        assert!(!err.is_empty());
        assert_eq!(required.total_required(u32::MAX), u32::MAX);
        assert_eq!(required.shortfall(&model_caps, u32::MAX), u32::MAX - 32_768);
    }

    // -----------------------------------------------------------------
    // ToolResultBoundPolicy (board item 01M1AVZPTRSWVE33G4DTJY7Q1B, move 1)
    // -----------------------------------------------------------------

    #[test]
    fn tool_result_bound_policy_default_is_the_disclosed_cap_and_empty_per_role() {
        let policy = ToolResultBoundPolicy::default();
        assert_eq!(
            policy.default_bound_tokens,
            DEFAULT_TOOL_RESULT_BOUND_CAP_TOKENS
        );
        assert!(policy.per_role.is_empty());
    }

    #[test]
    fn tool_result_bound_resolve_per_role_hit_and_miss() {
        let mut per_role = BTreeMap::new();
        per_role.insert(RoleAlias::new("planner"), 2_000);
        let policy = ToolResultBoundPolicy {
            default_bound_tokens: 8_192,
            per_role,
        };
        assert_eq!(policy.resolve(&RoleAlias::new("planner")), 2_000);
        // "fast" has no override -> falls back to the global default.
        assert_eq!(policy.resolve(&RoleAlias::new("fast")), 8_192);
    }

    #[test]
    fn tool_result_bound_zero_means_unlimited_at_either_layer() {
        let global_unlimited = ToolResultBoundPolicy {
            default_bound_tokens: 0,
            per_role: BTreeMap::new(),
        };
        assert_eq!(global_unlimited.resolve(&RoleAlias::new("anything")), 0);

        let mut per_role = BTreeMap::new();
        per_role.insert(RoleAlias::new("planner"), 0);
        let per_role_unlimited = ToolResultBoundPolicy {
            default_bound_tokens: 8_192,
            per_role,
        };
        assert_eq!(per_role_unlimited.resolve(&RoleAlias::new("planner")), 0);
        assert_eq!(per_role_unlimited.resolve(&RoleAlias::new("fast")), 8_192);
    }

    #[test]
    fn tool_result_bound_policy_deserializes_and_omitting_the_field_uses_default() {
        let policy: ToolResultBoundPolicy =
            serde_json::from_str(r#"{"default_bound_tokens":4096}"#).unwrap();
        assert_eq!(policy.default_bound_tokens, 4_096);
        assert!(policy.per_role.is_empty());

        let policy: ToolResultBoundPolicy = serde_json::from_str("{}").unwrap();
        assert_eq!(policy, ToolResultBoundPolicy::default());
    }

    #[test]
    fn probe_report_round_trips() {
        let report = ProbeReport {
            ok: true,
            latency_ms: 42,
            models: vec![ModelId::new("claude-sonnet-4-6")],
            detail: None,
            at: "2026-07-20T00:00:00Z".parse().unwrap(),
        };
        let json = serde_json::to_string(&report).unwrap();
        let back: ProbeReport = serde_json::from_str(&json).unwrap();
        assert_eq!(report, back);
    }
}
