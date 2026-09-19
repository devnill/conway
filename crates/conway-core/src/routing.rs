//! The content-free routing request/response contract -- routing is
//! content-blind and predictable -- plus the routing
//! reason vocabulary, health/breaker state, the declarative routing config
//! types, the "why did this model run" explain-report shape, and a minimal
//! config-only `Router`/`RoutingExplainer` fallback (`MinimalRouter`) usable
//! without depending on `conway-routing` at all.
//!
//! `Router::resolve` (defined as a port trait in earlier work) never consults
//! request *content* — [`RouteRequest`] is constructed so that no field can
//! carry prompt text. This is a compile-time guarantee, not a convention: a
//! unit test below asserts the field set is exactly the five documented
//! fields.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::capabilities::{
    resolve_adaptive_headroom, Capabilities, ContextTokensSource, ReliabilityTier, RequiredCaps,
    StructuredOutput, ToolCallSupport, DEFAULT_HEADROOM_TOKENS,
};
use crate::content::SamplingParams;
use crate::error::RoutingError;
use crate::ids::{AgentId, BackendId, EndpointId, ModelId, ModelRef, RoleAlias};
use crate::ports::{
    Admission, CacheReporting, HealthRegistry, Router, RoutingExplainer, TokenCountFidelity,
};

fn default_headroom_tokens() -> u32 {
    DEFAULT_HEADROOM_TOKENS
}

/// A request to resolve a routing role to an ordered candidate list.
///
/// Deliberately has no field of type `String`, `Vec<ContentBlock>`,
/// `PromptSegment`, or `Message` that could carry prompt text, so routing
/// cannot become content-aware by accident. Reasoning-headroom rides on
/// `required.headroom_tokens`, not as a separate top-level field, so this
/// five-field guarantee stays mechanically checkable.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RouteRequest {
    pub role: RoleAlias,
    pub pin: Option<ModelRef>,
    pub required: RequiredCaps,
    pub est_tokens: u32,
    pub agent_id: AgentId,
}

impl RouteRequest {
    /// The total token window this request will occupy, so callers never
    /// recompute the est_tokens + headroom_tokens sum by hand.
    pub fn total_required(&self) -> u32 {
        self.required.total_required(self.est_tokens)
    }
}

/// A single resolved routing candidate.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Route {
    pub backend: BackendId,
    pub model: ModelId,
    pub params: SamplingParams,
    pub reason: RoutingReason,
}

/// Why a route was chosen, or why a candidate was skipped.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoutingReason {
    PinnedByApi,
    PinnedByAgentDef,
    AliasPrimary {
        alias: RoleAlias,
    },
    Fallback {
        position: u8,
        /// Candidates that were ATTEMPTED and failed, in the order the
        /// failures were discovered. A candidate that was never dialed at
        /// all does not belong here -- see `skipped`.
        after: Vec<AttemptFailure>,
        /// Candidates refused at ADMISSION -- rejected before any request
        /// was sent, so there is no attempt, no error, and no timestamp to
        /// record. Each entry is itself a skip reason
        /// ([`RoutingReason::HeadroomSkip`],
        /// [`RoutingReason::CapabilitySkip`],
        /// [`RoutingReason::HealthSkip`]) naming the candidate and why it
        /// was passed over.
        ///
        /// This field exists because `after` alone cannot answer the
        /// commonest real "why am I not on the model I chose": when the
        /// head of a chain is refused on window/headroom, nothing was
        /// attempted, so `after` is legitimately empty and the reason
        /// rendered as a bare `after:` with nothing following it. Filling
        /// `after` with a manufactured [`AttemptFailure`] would have made
        /// the record claim an attempt that never happened; this field
        /// records the skip as a skip instead.
        ///
        /// `#[serde(default)]`: a journal written before this field existed
        /// still deserializes, with an empty skip list.
        #[serde(default)]
        skipped: Vec<RoutingReason>,
    },
    CapabilitySkip {
        skipped: ModelRef,
        missing: Vec<String>,
    },
    HealthSkip {
        skipped: ModelRef,
        breaker: BreakerKind,
    },
    /// An admission-time skip on context size: `admission` did not fit
    /// inside the candidate's own declared window, so the candidate was
    /// passed over without ever being dialed.
    ///
    /// Sibling of [`RoutingReason::CapabilitySkip`]/
    /// [`RoutingReason::HealthSkip`], and deliberately distinct from them:
    /// those two say "this candidate cannot do what was asked" and "this
    /// endpoint is currently unhealthy"; this one says "this candidate is
    /// fine, the request is simply bigger than it can hold", and carries
    /// the three numbers that make that checkable rather than a rendered
    /// sentence. The arithmetic itself is [`Admission`]'s -- never restated
    /// by a renderer.
    HeadroomSkip {
        skipped: ModelRef,
        admission: Admission,
    },
}

impl RoutingReason {
    /// For a skip reason -- [`RoutingReason::HeadroomSkip`],
    /// [`RoutingReason::CapabilitySkip`], [`RoutingReason::HealthSkip`] --
    /// the candidate that was passed over plus a one-line, number-carrying
    /// explanation. `None` for every *selection* reason (`AliasPrimary`,
    /// `Fallback`, either pin), which names no skipped candidate at all.
    ///
    /// One implementation, shared by every surface that renders a skip
    /// (`conway routes explain`, the TUI's mid-turn fallback notice, and
    /// `/why`), so the three can never word the same fact differently.
    pub fn skip_detail(&self) -> Option<(&ModelRef, String)> {
        match self {
            RoutingReason::HeadroomSkip { skipped, admission } => Some((
                skipped,
                format!(
                    "window {} < required {} ({} prompt + {} headroom, short by {})",
                    admission.max_context_tokens,
                    admission.required_tokens(),
                    admission.est_tokens,
                    admission.headroom_tokens,
                    admission.shortfall_tokens(),
                ),
            )),
            RoutingReason::CapabilitySkip { skipped, missing } => {
                Some((skipped, format!("missing {}", missing.join(", "))))
            }
            RoutingReason::HealthSkip { skipped, breaker } => {
                Some((skipped, format!("{breaker:?} breaker open")))
            }
            _ => None,
        }
    }
}

/// A prior failed attempt, recorded as part of a `Fallback` reason.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttemptFailure {
    pub model: ModelRef,
    pub error: String,
    pub at: DateTime<Utc>,
}

/// The circuit breaker kind tracked per endpoint (Olla pattern).
///
/// A second, independent `Probe` variant — fed by a periodic health prober
/// decoupled from request traffic — used to exist here. It was retired, not
/// wired: the prober that would
/// have fed it had no production call site anywhere in this tree, and the
/// Transport breaker alone already handles recovery (a clock read takes it
/// half-open; the next real request retries), so wiring it would only have
/// shaved latency off the first request after an outage — an optimization
/// this project gates on a measured baseline that neither existed nor was
/// scheduled. `#[non_exhaustive]` is kept so a future breaker kind can be
/// added without a semver break.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakerKind {
    Transport,
}

/// A circuit breaker's current state.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BreakerState {
    Closed,
    Open {
        until: DateTime<Utc>,
        kind: BreakerKind,
    },
    HalfOpen,
}

/// A health observation fed to `HealthRegistry::record`.
///
/// `BadRequest`, `Auth`, and `ContextOverflow` deliberately have no
/// `Observation` representation (§8) — they are request problems, not
/// endpoint-health signals. Headroom exists specifically to convert most
/// would-be `ContextOverflow` failures into pre-flight `CapabilitySkip` /
/// `ContextTooLarge` decisions.
///
/// A `ProbeFail` variant, fed exclusively by the now-retired periodic health
/// prober, used to exist here. It
/// was removed along with its only producer rather than left unconstructible
/// beside a live variant.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Observation {
    Ok { latency_ms: u32 },
    TransportError,
    ServerError,
    RateLimited { retry_after_secs: Option<u64> },
}

/// A single breaker read at explain time. Carries the `HealthRegistry`
/// port's merged view (`state`), not an independent `{transport, probe}`
/// pair -- `conway-routing`'s `RoutingExplain` (this type's other producer)
/// documents why that split is unreachable through the port; `MinimalRouter`
/// below never has independent breaker state at all, so every entry it
/// produces carries `Closed`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BreakerSnapshot {
    pub state: BreakerState,
}

/// A read-only projection of a `(backend, model)` pair's `Capabilities`, for
/// rendering in an `ExplainEntry`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CapabilitySummary {
    pub tool_calling: ToolCallSupport,
    pub max_context_tokens: u32,
    pub structured_output: StructuredOutput,
    pub parallel_tool_calls: bool,
    pub reasoning: bool,
    pub reliability_tier: ReliabilityTier,
}

impl From<&Capabilities> for CapabilitySummary {
    fn from(caps: &Capabilities) -> CapabilitySummary {
        CapabilitySummary {
            tool_calling: caps.tool_calling,
            max_context_tokens: caps.max_context_tokens,
            structured_output: caps.structured_output,
            parallel_tool_calls: caps.parallel_tool_calls,
            reasoning: caps.reasoning,
            reliability_tier: caps.reliability_tier,
        }
    }
}

/// Whether a candidate was chosen, or skipped -- carrying the router's exact
/// `RoutingReason` either way.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EntryOutcome {
    Selected { reason: RoutingReason },
    Skipped { reason: RoutingReason },
}

/// One evaluated candidate: its place in the chain (or `None` for a pin),
/// whether it was selected or skipped and why, its capability summary (when
/// indexed), its breaker snapshot, and how much its backend's own token
/// estimate can be trusted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExplainEntry {
    pub model_ref: ModelRef,
    pub chain_position: Option<u8>,
    pub outcome: EntryOutcome,
    pub capabilities: Option<CapabilitySummary>,
    pub breaker: BreakerSnapshot,
    /// This candidate's backend's declared [`crate::ports::TokenCountFidelity`]
    /// (board item 01M0ASX466G3PW3SJJS3KGNS55) -- the operator-visible
    /// surface for `Backend::token_fidelity`. `None` when the producing
    /// `RoutingExplainer` cannot answer: today, only `MinimalRouter`'s
    /// config-only fallback, which never reaches a live `Backend` instance
    /// (see its own `explain` impl). `#[serde(default)]` so a report decoded
    /// from before this field existed still parses.
    #[serde(default)]
    pub token_fidelity: Option<TokenCountFidelity>,
    /// This candidate's resolved `capabilities.max_context_tokens`
    /// provenance (hosted OpenAI-compatible models item) -- the
    /// operator-visible surface for `Backend::context_window_source`, read
    /// via `CapabilityIndex::context_window_source` (the same index
    /// `capabilities` above is read from). `None` under the exact same
    /// conditions `capabilities` is `None` (this candidate has no
    /// capability-index entry at all -- an unindexed model, or
    /// `MinimalRouter`'s config-only fallback). `#[serde(default)]` so a
    /// report decoded from before this field existed still parses.
    #[serde(default)]
    pub context_window_source: Option<ContextTokensSource>,
    /// This candidate's backend's declared [`crate::ports::CacheReporting`]
    /// (board item A5.7, "prompt caching reads zero on every real
    /// session") -- the operator-visible surface for `Backend::
    /// cache_reporting`, read via `CapabilityIndex::cache_reporting` (keyed
    /// like `token_fidelity`, by backend id alone). `None` under the exact
    /// same conditions `token_fidelity` is `None` -- the producing
    /// `RoutingExplainer` could not reach a live `Backend` instance (e.g.
    /// `MinimalRouter`'s config-only fallback). `#[serde(default)]` so a
    /// report decoded from before this field existed still parses.
    #[serde(default)]
    pub cache_reporting: Option<CacheReporting>,
    /// The headroom THIS candidate was actually checked against (board item
    /// `01M2TVEWVMPP69TZ17XSGWEW82`). Since headroom now resolves per
    /// candidate -- an operator per-model value, else the role's, else a
    /// fraction of this candidate's OWN window -- the report-level
    /// [`ExplainReport::headroom_tokens`] is true for at most one row, and
    /// a reader comparing "needs N + headroom" against it would be reading
    /// a number no gate used.
    ///
    /// `None` when the producing `RoutingExplainer` ran no per-candidate
    /// resolution at all: today only [`MinimalRouter`], which does no
    /// admission checking whatsoever, so it has no honest per-row answer --
    /// the same reason its `capabilities` is `None`.
    /// `#[serde(default)]` so a report decoded from before this field
    /// existed still parses.
    #[serde(default)]
    pub headroom_tokens: Option<u32>,
}

/// The full "why did this model run, and why not the others" answer for one
/// `RouteRequest`, including the effective headroom reservation used for
/// the admission check (see the amendment).
///
/// **Moved here from `conway-routing`,
/// replacing a dead, unreached second `ExplainReport` shape this module used
/// to declare on its own.** The type used to live only in `conway-routing`,
/// reachable exclusively through `RoutingExplain`'s projection of a concrete
/// `DeclarativeRouter` -- so a `Router` supplied from outside that crate
/// (`ConwayBuilder::with_router`) had no way to produce one, and
/// `Conway::explain_routing` fell back to a fabricated-empty report that
/// `conway routes explain` then misread as "unknown role" (a silent
/// inversion, not an honest degradation -- the bug this move exists to
/// close). It now lives here, where both `conway-routing::RoutingExplain`
/// (the rich, capability- and health-filtered answer) and `MinimalRouter`
/// (below; the honest degenerate answer core itself can produce with no
/// filtering at all) build the same shape. `conway-routing` re-exports these
/// five names for source compatibility.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExplainReport {
    pub role: RoleAlias,
    pub pin: Option<ModelRef>,
    pub est_tokens: u32,
    /// The EFFECTIVE requirement every `entries` outcome was actually
    /// checked against, when the producer performs a real check
    /// (`conway-routing::RoutingExplain`: the role's configured floor
    /// merged with `req.required`). `MinimalRouter::explain` performs no
    /// check at all, so its value here is only the role's configured floor
    /// plus resolved headroom (`RoutingConfig::required_caps_for`) --
    /// informational, not a claim that any entry was actually verified
    /// against it, so nothing claims a reach it does not have.
    pub required: RequiredCaps,
    /// The ROLE-wide headroom (`RoutingConfig::headroom_for`) -- the number
    /// a caller with no candidate in hand would have used. Since board item
    /// `01M2TVEWVMPP69TZ17XSGWEW82` this is no longer the number every row
    /// was checked against; [`ExplainEntry::headroom_tokens`] is, per
    /// candidate. Kept because it still answers "what does this role
    /// reserve by default", and because removing it would break every
    /// decoder of an already-serialized report.
    pub headroom_tokens: u32,
    pub entries: Vec<ExplainEntry>,
    pub generated_at: DateTime<Utc>,
}

impl ExplainReport {
    /// A stable, line-oriented rendering (see `docs/routing.md`'s "Asking
    /// why a route was chosen" section for the exact format). Two-space
    /// indent, `[<position>]` (or `[pin]`), the model ref right-padded to the
    /// longest ref in the report plus two spaces, `SELECTED`/`SKIPPED`
    /// padded to eight columns, then the reason. Timestamps are RFC 3339
    /// UTC. Trailing newline present. No ANSI codes -- rendering is the
    /// CLI's concern, not this crate's.
    pub fn render_text(&self) -> String {
        let mut out = format!(
            "role: {}  (est_tokens={}, headroom_tokens={})\n",
            self.role, self.est_tokens, self.headroom_tokens
        );

        let width = self
            .entries
            .iter()
            .map(|e| e.model_ref.to_string().len())
            .max()
            .unwrap_or(0)
            + 2;

        for entry in &self.entries {
            let marker = match entry.chain_position {
                Some(position) => format!("[{position}]"),
                None => "[pin]".to_string(),
            };
            let (word, reason) = match &entry.outcome {
                EntryOutcome::Selected { reason } => ("SELECTED", render_selected(reason)),
                EntryOutcome::Skipped { reason } => {
                    ("SKIPPED", render_skipped(reason, &entry.breaker))
                }
            };
            let model_ref = entry.model_ref.to_string();
            // Per-candidate headroom (board item
            // `01M2TVEWVMPP69TZ17XSGWEW82`): the reservation THIS row was
            // checked against, which the header's role-wide number is no
            // longer guaranteed to equal. Omitted entirely (not rendered as
            // `headroom=?`) when the producer ran no per-candidate
            // resolution, so no row ever displays a number nothing used.
            let headroom = match entry.headroom_tokens {
                Some(tokens) => format!("headroom={tokens}  "),
                None => String::new(),
            };
            let _ = writeln!(
                out,
                "  {marker} {model_ref:<width$}{word:<8} {headroom}{reason}",
                width = width,
            );
        }

        out
    }
}

/// Renders an `EntryOutcome::Selected` reason for `render_text`.
fn render_selected(reason: &RoutingReason) -> String {
    match reason {
        RoutingReason::PinnedByApi => "pinned(via=api)".to_string(),
        RoutingReason::PinnedByAgentDef => "pinned(via=agent_def)".to_string(),
        RoutingReason::AliasPrimary { alias } => format!("primary(role={alias})"),
        RoutingReason::Fallback { position, .. } => format!("fallback(position={position})"),
        _ => "selected".to_string(),
    }
}

/// Renders an `EntryOutcome::Skipped` reason for `render_text`. The health
/// case reads its `until` timestamp from `breaker` (the independent snapshot
/// taken at explain time), since `RoutingReason::HealthSkip` itself carries
/// only the breaker kind, not a timestamp.
fn render_skipped(reason: &RoutingReason, breaker: &BreakerSnapshot) -> String {
    match reason {
        RoutingReason::CapabilitySkip { missing, .. } => {
            format!("capability: {}", missing.join("; "))
        }
        // The numbers come from `RoutingReason::skip_detail` -- the one
        // shared skip renderer -- so this report cannot word the same
        // shortfall differently from the TUI notice or `/why`.
        RoutingReason::HeadroomSkip { .. } => match reason.skip_detail() {
            Some((_, detail)) => format!("headroom: {detail}"),
            None => "headroom: skipped".to_string(),
        },
        RoutingReason::HealthSkip { breaker: kind, .. } => {
            // `BreakerKind` is `#[non_exhaustive]` for OTHER crates; within
            // its own defining crate (this one, now that this type moved
            // here from conway-routing) every variant is already covered, so
            // a trailing wildcard would be unreachable dead code rather than
            // genuine forward-compatibility.
            let kind_name = match kind {
                BreakerKind::Transport => "transport",
            };
            match &breaker.state {
                BreakerState::Open { until, .. } => format!(
                    "health: {kind_name} breaker open until {}",
                    until.to_rfc3339_opts(SecondsFormat::Secs, true)
                ),
                _ => format!("health: {kind_name} breaker open"),
            }
        }
        _ => "skipped".to_string(),
    }
}

// ---------------------------------------------------------------------
// Config types. Types only: loading (TOML parsing, env resolution, path
// discovery) lives in the `conway` facade, not here.
// ---------------------------------------------------------------------

/// The declarative routing policy: per-role fallback chains plus health
/// tuning. `BTreeMap`, not `HashMap`, so serialized config is deterministically
/// ordered.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoutingConfig {
    pub roles: BTreeMap<String, RoleConfig>,
    pub health: HealthConfig,
    /// Global default reserved output/reasoning tokens, applied to any role
    /// without an override.
    #[serde(default = "default_headroom_tokens")]
    pub default_headroom_tokens: u32,
    /// Per-MODEL routing policy, keyed by the same `"backend/model"`
    /// convention [`RoleConfig::chain`] and `models.json` already use.
    /// Surfaced to an operator as `[routing].models."<backend>/<model>"`
    /// (board item `01M2TVEWVMPP69TZ17XSGWEW82`).
    ///
    /// **Deliberately not `models.json`.** That file is a facts catalogue
    /// (window, tier, tool support) written and rewritten by discovery and
    /// the provider-manage flow -- `conway::config::model_metadata`'s
    /// `set_context_window` reparses and reserializes the whole document
    /// and silently drops any field it does not know, so a headroom stored
    /// there would be erased by the next `first_run` or provider edit.
    /// Headroom is operator POLICY; a metadata refresh must not be able to
    /// overwrite a policy decision.
    #[serde(default)]
    pub models: BTreeMap<String, ModelHeadroom>,
    /// Divisor for the conway-DERIVED per-candidate headroom
    /// (`max(candidate_window / d, HEADROOM_FLOOR)`), mirroring the
    /// `[routing].headroom_fraction` config key. `None` (or `Some(0)`)
    /// disables derivation entirely, so
    /// [`Self::default_headroom_tokens`] becomes the fallback for every
    /// role with no override.
    ///
    /// **`None` in [`RoutingConfig::default`], even though the facade's own
    /// `[routing].headroom_fraction` defaults to `Some(10)`.** The facade
    /// (`conway::config::schema::ConwayConfig::routing`) passes its value
    /// through explicitly, so a real deployment still gets adaptive
    /// headroom by default. A `RoutingConfig` built as a Rust literal --
    /// every in-process router fixture in this workspace -- gets the
    /// pre-derivation behaviour unless it opts in, so turning derivation on
    /// is a config decision rather than something that silently changes
    /// what `default_headroom_tokens: 1_000` means in a unit test.
    #[serde(default)]
    pub headroom_fraction: Option<u32>,
}

/// Per-model routing policy: today, exactly one operator-authored knob.
///
/// Its own struct rather than a bare `BTreeMap<String, u32>` so the next
/// per-model routing knob is an added field here instead of a second
/// parallel table keyed the same way.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ModelHeadroom {
    /// The operator's explicit reservation for this exact
    /// `(backend, model)` pair. Honoured as written -- the most specific
    /// level of [`RoutingConfig::headroom_for_model`]'s ladder -- even when
    /// it makes the model unreachable; the operator is told so by name at
    /// config load (`conway::config::merge::validate`) rather than having
    /// their number quietly shrunk to fit.
    pub headroom_tokens: Option<u32>,
}

/// The one implementation of the headroom precedence ladder (board item
/// `01M2TVEWVMPP69TZ17XSGWEW82`), as a free function so the facade
/// (`conway::config::schema::ConwayConfig`, which holds its own
/// string-keyed schema types rather than a [`RoutingConfig`]) resolves the
/// SAME order the router does without restating it. Total.
///
/// Order, highest first:
///
/// 1. `model_override` -- operator, `[routing].models.<ref>.headroom_tokens`
/// 2. `role_override` -- operator, `roles.<alias>.headroom_tokens`
/// 3. conway-derived: `max(window / fraction, HEADROOM_FLOOR)`, needing
///    both a `fraction` and a known `window` for THIS candidate
/// 4. `default_headroom_tokens`
///
/// **A conway-derived value never beats an operator-written one**; among
/// operator-written values, the more specific wins. Level 3 sitting ABOVE
/// level 4 rather than below it is the load-bearing half: it is what stops
/// one role-wide number, sized against whichever chain member happened to
/// be biggest, from making a smaller sibling permanently unreachable.
///
/// Nothing here clamps: a level-1 or level-2 value is returned exactly as
/// the operator wrote it even when `window` is smaller, and the candidate
/// is then honestly skipped by the admission gate.
pub fn resolve_headroom(
    model_override: Option<u32>,
    role_override: Option<u32>,
    fraction: Option<u32>,
    window: Option<u32>,
    default_headroom_tokens: u32,
) -> u32 {
    if let Some(tokens) = model_override {
        return tokens;
    }
    if let Some(tokens) = role_override {
        return tokens;
    }
    if let (Some(fraction), Some(window)) = (fraction, window) {
        if let Some(derived) = resolve_adaptive_headroom(window, fraction) {
            return derived;
        }
    }
    default_headroom_tokens
}

impl RoutingConfig {
    /// Resolves the effective headroom for a role. Precedence, fixed and
    /// total: per-role `RoleConfig::headroom_tokens` overrides
    /// `default_headroom_tokens`, which overrides
    /// [`DEFAULT_HEADROOM_TOKENS`] (that constant is only reachable if
    /// `default_headroom_tokens` itself is absent from the config, which the
    /// serde default on this field prevents in practice). Unknown roles get
    /// the global default.
    ///
    /// A caller-supplied `RouteRequest.required.headroom_tokens` set by the
    /// runtime (e.g. from an agent def) sits above all of this — this method
    /// is only consulted when *constructing* a `RouteRequest`, never when
    /// interpreting one that already carries a value.
    pub fn headroom_for(&self, role: &RoleAlias) -> u32 {
        self.roles
            .get(role.as_str())
            .and_then(|r| r.headroom_tokens)
            .unwrap_or(self.default_headroom_tokens)
    }

    /// The operator's explicit per-model reservation for `model`, if any --
    /// level 1 of [`resolve_headroom`]'s ladder.
    pub fn model_headroom_override(&self, model: &ModelRef) -> Option<u32> {
        if self.models.is_empty() {
            return None;
        }
        self.models
            .get(&model.to_string())
            .and_then(|m| m.headroom_tokens)
    }

    /// The effective headroom for ONE candidate of `role`, resolved through
    /// [`resolve_headroom`]'s full four-level ladder against that
    /// candidate's own `window` (`None` when this process knows of no
    /// window for it at all, which skips only the derived level -- never
    /// the operator's own).
    ///
    /// This is the route-time answer. [`Self::headroom_for`] is the
    /// role-wide one, and remains what a caller with no candidate in hand
    /// (building a `RouteRequest`, rendering a report header) can honestly
    /// ask for; it is deliberately NOT redefined in terms of this method,
    /// because a role-wide question has no candidate window to derive from.
    pub fn headroom_for_model(
        &self,
        role: &RoleAlias,
        model: &ModelRef,
        window: Option<u32>,
    ) -> u32 {
        resolve_headroom(
            self.model_headroom_override(model),
            self.roles
                .get(role.as_str())
                .and_then(|r| r.headroom_tokens),
            self.headroom_fraction,
            window,
            self.default_headroom_tokens,
        )
    }

    /// Builds the filter input for a request: the role's `required` caps
    /// with `headroom_tokens` resolved from the override/default chain. An
    /// unknown role gets `RequiredCaps::default()` with headroom resolved
    /// the same way.
    pub fn required_caps_for(&self, role: &RoleAlias) -> RequiredCaps {
        let mut required = self
            .roles
            .get(role.as_str())
            .map(|r| r.required.clone())
            .unwrap_or_default();
        required.headroom_tokens = self.headroom_for(role);
        required
    }
}

/// Hand-written rather than derived, because `default_headroom_tokens` must
/// agree with the `#[serde(default = "default_headroom_tokens")]` attribute
/// on that field -- a derived `Default` would hand back `0` and silently
/// disagree with what deserializing a document that omits the key produces.
///
/// This exists so construction sites can write `..Default::default()` instead
/// of an exhaustive literal: adding a field here should not be a compile
/// break at every call site in the workspace. The values below are exactly
/// what every such site already spelled out.
impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            roles: BTreeMap::new(),
            health: HealthConfig::default(),
            default_headroom_tokens: default_headroom_tokens(),
            models: BTreeMap::new(),
            // `None`, not `Some(DEFAULT_HEADROOM_FRACTION)` -- see the
            // field's own doc for why the facade opts in explicitly
            // instead.
            headroom_fraction: None,
        }
    }
}

/// One role's fallback chain, capability floor, and sampling defaults.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RoleConfig {
    pub chain: Vec<ModelRef>,
    pub required: RequiredCaps,
    pub params: SamplingParams,
    /// Per-role override of [`RoutingConfig::default_headroom_tokens`].
    #[serde(default)]
    pub headroom_tokens: Option<u32>,
}

/// Circuit-breaker tuning for the endpoint's `Transport` breaker.
///
/// Every field has a serde default, so a config document omitting `[health]`
/// keys (or the whole table) deserializes to [`HealthConfig::default`].
///
/// **The `probe_*` fields (`probe_interval_secs`, `probe_timeout_secs`,
/// `probe_failures_to_open`, `probe_enabled`) that used to configure a
/// second, independent `Probe` breaker were removed , not merely left unwired.** The periodic
/// health prober that would have fed that breaker had no production call
/// site anywhere in this tree — the Transport breaker alone handles recovery
/// (it goes half-open on a clock read, and the next real request retries),
/// so the prober fixed no correctness gap; it would only have shaved latency
/// off the first request after an outage, which made wiring it an
/// optimization requiring a measured baseline that neither existed nor
/// was scheduled. **Breaking:** a `settings.json`/`RoutingConfig` document
/// naming any of the four removed keys under `[health]` now fails to
/// deserialize (`#[serde(deny_unknown_fields)]` on the facade's
/// `HealthSection` mirror) rather than silently accepting and ignoring them.
/// Do not confuse any of this with `[models].probe_on_startup`
/// (`conway::config::schema::ModelsConfig`), a different, already-wired
/// startup CAPABILITY probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HealthConfig {
    pub transport_failures_to_open: u32,
    pub open_duration_secs: u64,
    /// Consecutive successful observations required to close a half-open
    /// breaker.
    pub half_open_successes_to_close: u32,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            transport_failures_to_open: 3,
            open_duration_secs: 30,
            half_open_successes_to_close: 1,
        }
    }
}

/// One configured backend instance.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackendConfig {
    pub id: BackendId,
    pub kind: BackendKind,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub dialect: Option<String>,
    pub models: BTreeMap<String, ModelOverrides>,
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// The dialect family a backend adapter speaks.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    Anthropic,
    OpenAiCompat,
}

/// Per-model overrides layered onto a backend's declared capabilities.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelOverrides {
    pub stream_tools: Option<bool>,
    pub max_context_tokens: Option<u32>,
    pub reliability_tier: Option<ReliabilityTier>,
    /// Per-model override for the parallel-tool-calls capability
    /// (overrides > metadata > dialect defaults, per conway-plugin-backends'
    /// capability precedence).
    pub parallel_tool_calls: Option<bool>,
    /// A floor, not an override: `conway-routing` applies
    /// `effective = max(request.headroom_tokens, min_headroom_tokens.unwrap_or(0))`.
    /// A model that reasons heavily can insist on more reserved space than a
    /// role requests, but cannot reduce it.
    pub min_headroom_tokens: Option<u32>,
}

// ---------------------------------------------------------------------
// Minimal fallback implementations.
//
// `crate::ports`'s own module doc reserves `conway-core` for production
// fallbacks plus one no-op test-fixture constructor, "every other
// implementation lives in a dedicated crate" (the full test-double set now
// lives in `conway-testkit`, not here).
// These two types are the narrow, deliberate exception: `MinimalRouter` and
// `AlwaysClosedHealthRegistry` are production code, not test doubles -- they
// back `Conway::explain_routing`'s honest degenerate answer when the caller
// supplied its own `Router` (`ConwayBuilder::with_router`) and there is no
// concrete `conway_plugin_routing::DeclarativeRouter` left to project an
// `ExplainReport` through. Neither performs I/O, matching every other port
// implementation's constraint.
// ---------------------------------------------------------------------

/// A `HealthRegistry` that always reports `Closed` and records nothing.
/// Not a test double (`conway_testkit::FakeHealth` is that): this is the
/// honest production answer for a caller that has no real breaker state to
/// consult at all, per the module note above.
#[derive(Clone, Copy, Debug, Default)]
pub struct AlwaysClosedHealthRegistry;

impl HealthRegistry for AlwaysClosedHealthRegistry {
    fn state(&self, _ep: &EndpointId) -> BreakerState {
        BreakerState::Closed
    }

    fn record(&self, _ep: &EndpointId, _obs: Observation) {}
}

/// A minimal, config-only `Router` + `RoutingExplainer`: no capability
/// filtering, no health filtering, no invented values -- nothing claims a
/// capability it does not have. `resolve` returns a role's configured chain in
/// order (or a pin's single-element chain); `explain` answers with one
/// degenerate `ExplainEntry` per chain entry -- the first `Selected`, the rest
/// `Skipped`, `capabilities: None` (this type indexes no capabilities) and
/// `breaker: BreakerSnapshot { state: Closed }` (paired with
/// [`AlwaysClosedHealthRegistry`] -- this type tracks no real breaker state
/// either). See the module-note above this section for why these two live in
/// `conway-core` at all.
#[derive(Clone, Debug)]
pub struct MinimalRouter {
    config: RoutingConfig,
}

impl MinimalRouter {
    pub fn new(config: RoutingConfig) -> MinimalRouter {
        MinimalRouter { config }
    }

    /// The chain this request resolves against, and whether it came from a
    /// pin -- `None` only when `req` is unpinned and names a role absent
    /// from `self.config.roles`.
    fn chain_for(&self, req: &RouteRequest) -> Option<(Vec<ModelRef>, bool)> {
        match &req.pin {
            Some(pin) => Some((vec![pin.clone()], true)),
            None => self
                .config
                .roles
                .get(req.role.as_str())
                .map(|role| (role.chain.clone(), false)),
        }
    }

    fn reason_for(is_pin: bool, position: usize, role: &RoleAlias) -> RoutingReason {
        if is_pin {
            RoutingReason::PinnedByApi
        } else if position == 0 {
            RoutingReason::AliasPrimary {
                alias: role.clone(),
            }
        } else {
            RoutingReason::Fallback {
                position: position as u8,
                after: Vec::new(),
                // `MinimalRouter` filters nothing -- it holds no capability
                // index and no health state -- so it has genuinely skipped
                // nothing of its own to report. A candidate this router
                // hands back that is later refused by `Backend::admit`
                // gets its skip recorded by the attempt engine, which is
                // the layer that actually discovered it.
                skipped: Vec::new(),
            }
        }
    }
}

impl Router for MinimalRouter {
    fn resolve(&self, req: &RouteRequest) -> Result<Vec<Route>, RoutingError> {
        let Some((chain, is_pin)) = self.chain_for(req) else {
            return Err(RoutingError::UnknownRole {
                role: req.role.clone(),
            });
        };
        if chain.is_empty() {
            return Err(RoutingError::NoCandidate {
                role: req.role.clone(),
                considered: Vec::new(),
            });
        }

        let params = self
            .config
            .roles
            .get(req.role.as_str())
            .map(|role| role.params.clone())
            .unwrap_or_default();

        Ok(chain
            .iter()
            .enumerate()
            .map(|(position, model_ref)| Route {
                backend: model_ref.backend.clone(),
                model: model_ref.model.clone(),
                params: params.clone(),
                reason: Self::reason_for(is_pin, position, &req.role),
            })
            .collect())
    }

    fn known_roles(&self) -> Vec<RoleAlias> {
        self.config
            .roles
            .keys()
            .map(|name| RoleAlias::new(name.clone()))
            .collect()
    }
}

impl RoutingExplainer for MinimalRouter {
    fn explain(&self, req: &RouteRequest) -> ExplainReport {
        let generated_at = Utc::now();
        let (chain, is_pin) = self.chain_for(req).unwrap_or_default();

        let entries = chain
            .iter()
            .enumerate()
            .map(|(position, model_ref)| {
                let reason = Self::reason_for(is_pin, position, &req.role);
                let outcome = if position == 0 {
                    EntryOutcome::Selected { reason }
                } else {
                    EntryOutcome::Skipped { reason }
                };
                ExplainEntry {
                    model_ref: model_ref.clone(),
                    chain_position: if is_pin { None } else { Some(position as u8) },
                    outcome,
                    capabilities: None,
                    breaker: BreakerSnapshot {
                        state: BreakerState::Closed,
                    },
                    // `MinimalRouter` holds only `RoutingConfig` -- no
                    // `Arc<dyn Backend>`, so it has no honest answer, the
                    // same reason `capabilities` above is `None` here too.
                    token_fidelity: None,
                    context_window_source: None,
                    cache_reporting: None,
                    // This type performs no admission check at all, so it
                    // resolved no per-candidate headroom and has nothing
                    // honest to report per row -- same reason
                    // `capabilities` above is `None`. The role-wide value
                    // still rides on the report below.
                    headroom_tokens: None,
                }
            })
            .collect();

        ExplainReport {
            role: req.role.clone(),
            pin: req.pin.clone(),
            est_tokens: req.est_tokens,
            required: self.config.required_caps_for(&req.role),
            headroom_tokens: self.config.headroom_for(&req.role),
            entries,
            generated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_request_field_set_is_exactly_five_and_content_free() {
        let req = RouteRequest {
            role: RoleAlias::new("planner"),
            pin: None,
            required: RequiredCaps::default(),
            est_tokens: 100,
            agent_id: AgentId::new(),
        };
        let value = serde_json::to_value(&req).unwrap();
        let obj = value
            .as_object()
            .expect("RouteRequest must serialize to an object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["agent_id", "est_tokens", "pin", "required", "role"]
        );

        // No field could ever carry raw prompt text: nothing named `text`,
        // `content`, `prompt`, `message`, or `blocks`.
        for forbidden in ["text", "content", "prompt", "message", "blocks"] {
            assert!(
                !obj.contains_key(forbidden),
                "RouteRequest must not carry a {forbidden:?} field"
            );
        }
    }

    #[test]
    fn route_request_total_required_matches_required_caps() {
        let req = RouteRequest {
            role: RoleAlias::new("planner"),
            pin: None,
            required: RequiredCaps {
                headroom_tokens: 1_000,
                ..RequiredCaps::default()
            },
            est_tokens: 500,
            agent_id: AgentId::new(),
        };
        assert_eq!(req.total_required(), 1_500);
    }

    #[test]
    fn health_config_default_matches_documented_toml() {
        let h = HealthConfig::default();
        assert_eq!(h.transport_failures_to_open, 3);
        assert_eq!(h.open_duration_secs, 30);
        assert_eq!(h.half_open_successes_to_close, 1);
    }

    #[test]
    fn headroom_for_precedence_role_override_then_global_default() {
        let mut roles = BTreeMap::new();
        roles.insert(
            "planner".to_string(),
            RoleConfig {
                chain: vec![],
                headroom_tokens: Some(32_768),
                ..Default::default()
            },
        );
        roles.insert(
            "fast".to_string(),
            RoleConfig {
                chain: vec![],
                headroom_tokens: None,
                ..Default::default()
            },
        );
        let config = RoutingConfig {
            roles,
            default_headroom_tokens: 8_192,
            ..Default::default()
        };

        let planner: RoleAlias = "planner".parse().unwrap();
        let fast: RoleAlias = "fast".parse().unwrap();
        let unknown: RoleAlias = "unknown".parse().unwrap();

        // Per-role override wins.
        assert_eq!(config.headroom_for(&planner), 32_768);
        // No override -> global default.
        assert_eq!(config.headroom_for(&fast), 8_192);
        // Unknown role -> global default.
        assert_eq!(config.headroom_for(&unknown), 8_192);

        let required = config.required_caps_for(&planner);
        assert_eq!(required.headroom_tokens, 32_768);
    }

    /// Deserializes a JSON document shaped like the architecture
    /// §"conway-routing / Internal Design Notes" TOML snippet (roles.planner
    /// chain, roles.fast chain, health block), extended per the amendment
    /// with `default_headroom_tokens` and a per-role `headroom_tokens`
    /// override on `planner`. Field values use this crate's actual wire
    /// shapes (`ModelRef` is a `{backend, model}` object, not a
    /// `"backend/model"` string; `HealthConfig` uses plain integer-second
    /// fields, not humantime strings — both documented deviations from the
    /// doc's illustrative TOML). Round-trips through `serde_json`.
    #[test]
    fn routing_config_deserializes_reference_shape_and_round_trips() {
        let json = r#"
        {
          "roles": {
            "planner": {
              "chain": [
                {"backend": "anthropic", "model": "claude-sonnet-4-6"},
                {"backend": "ollama-cloud", "model": "glm-5.2"},
                {"backend": "local", "model": "qwen3-coder-80b"}
              ],
              "required": {},
              "params": {"stop": [], "extra": {}},
              "headroom_tokens": 32768
            },
            "fast": {
              "chain": [
                {"backend": "local", "model": "qwen3-coder-80b"},
                {"backend": "anthropic", "model": "claude-haiku-4-5"}
              ],
              "required": {},
              "params": {"stop": [], "extra": {}}
            }
          },
          "health": {
            "transport_failures_to_open": 3,
            "open_duration_secs": 30,
            "half_open_successes_to_close": 1
          },
          "default_headroom_tokens": 8192
        }
        "#;

        let config: RoutingConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.roles.len(), 2);
        assert_eq!(config.roles["planner"].chain.len(), 3);
        assert_eq!(config.roles["fast"].chain.len(), 2);
        assert_eq!(config.default_headroom_tokens, 8_192);
        assert_eq!(config.health, HealthConfig::default());

        let planner: RoleAlias = "planner".parse().unwrap();
        let fast: RoleAlias = "fast".parse().unwrap();
        assert_eq!(config.headroom_for(&planner), 32_768);
        assert_eq!(config.headroom_for(&fast), 8_192);

        // Round-trip.
        let reserialized = serde_json::to_string(&config).unwrap();
        let back: RoutingConfig = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            serde_json::to_value(&config).unwrap()
        );
    }

    #[test]
    fn model_overrides_round_trip() {
        let mo = ModelOverrides {
            stream_tools: Some(true),
            max_context_tokens: Some(131_072),
            reliability_tier: Some(ReliabilityTier::Verified),
            parallel_tool_calls: None,
            min_headroom_tokens: Some(16_384),
        };
        let json = serde_json::to_string(&mo).unwrap();
        let back: ModelOverrides = serde_json::from_str(&json).unwrap();
        assert_eq!(mo, back);
    }

    #[test]
    fn backend_config_round_trips_with_btreemap_models() {
        let mut models = BTreeMap::new();
        models.insert(
            "qwen3-coder-80b".to_string(),
            ModelOverrides {
                stream_tools: None,
                max_context_tokens: None,
                reliability_tier: None,
                parallel_tool_calls: None,
                min_headroom_tokens: None,
            },
        );
        let cfg = BackendConfig {
            id: BackendId::new("local"),
            kind: BackendKind::OpenAiCompat,
            base_url: Some("http://localhost:8080".into()),
            api_key_env: None,
            dialect: None,
            models,
            extra: serde_json::Map::new(),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: BackendConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.models.len(), 1);
        assert_eq!(back.id, cfg.id);
    }

    // -------------------------------------------------------------------
    // `MinimalRouter`/`ExplainReport`
    // -------------------------------------------------------------------

    fn model_ref(backend: &str, model: &str) -> ModelRef {
        ModelRef {
            backend: BackendId::new(backend),
            model: ModelId::new(model),
        }
    }

    fn two_entry_chain_config() -> RoutingConfig {
        let mut roles = BTreeMap::new();
        roles.insert(
            "planner".to_string(),
            RoleConfig {
                chain: vec![
                    model_ref("anthropic", "claude-sonnet-4-6"),
                    model_ref("local", "qwen3-coder-80b"),
                ],
                ..Default::default()
            },
        );
        RoutingConfig {
            roles,
            default_headroom_tokens: 4_096,
            ..Default::default()
        }
    }

    fn request(role: &str) -> RouteRequest {
        RouteRequest {
            role: RoleAlias::new(role),
            pin: None,
            required: RequiredCaps::default(),
            est_tokens: 0,
            agent_id: AgentId::new(),
        }
    }

    #[test]
    fn minimal_router_explain_over_two_entry_chain_is_honestly_degenerate() {
        let router = MinimalRouter::new(two_entry_chain_config());
        let report = router.explain(&request("planner"));

        assert_eq!(report.entries.len(), 2);
        assert!(matches!(
            report.entries[0].outcome,
            EntryOutcome::Selected { .. }
        ));
        assert!(matches!(
            report.entries[1].outcome,
            EntryOutcome::Skipped { .. }
        ));
        for entry in &report.entries {
            assert_eq!(entry.capabilities, None);
            assert_eq!(entry.token_fidelity, None);
            assert_eq!(
                entry.breaker,
                BreakerSnapshot {
                    state: BreakerState::Closed
                }
            );
        }
    }

    /// `token_fidelity` (board item 01M0ASX466G3PW3SJJS3KGNS55) must
    /// `#[serde(default)]`: an `ExplainEntry` encoded before this field
    /// existed has no `token_fidelity` key at all, and must still decode --
    /// to `None`, not a decode error. Built by serializing a real entry and
    /// stripping the key, rather than a hand-written literal, so this test
    /// does not silently drift from the other fields' actual wire shape.
    #[test]
    fn explain_entry_without_token_fidelity_key_decodes_to_none() {
        let entry = ExplainEntry {
            model_ref: "local/m1".parse().unwrap(),
            chain_position: Some(0),
            outcome: EntryOutcome::Selected {
                reason: RoutingReason::PinnedByApi,
            },
            capabilities: None,
            breaker: BreakerSnapshot {
                state: BreakerState::Closed,
            },
            token_fidelity: Some(TokenCountFidelity::Calibrated),
            context_window_source: None,
            cache_reporting: None,
            headroom_tokens: None,
        };
        let mut value = serde_json::to_value(&entry).unwrap();
        value
            .as_object_mut()
            .expect("ExplainEntry serializes to an object")
            .remove("token_fidelity")
            .expect("token_fidelity key present before removal");

        let decoded: ExplainEntry =
            serde_json::from_value(value).expect("pre-field-existing shape still decodes");
        assert_eq!(decoded.token_fidelity, None);
    }

    /// Board item `01M2TVFXE2X1SHWZS54HG9DX5A`: a session journal written
    /// before `RoutingReason::Fallback::skipped` existed still decodes,
    /// with an empty skip list -- the `#[serde(default)]` that field's own
    /// doc promises. Built by serializing a live value and stripping the
    /// key, rather than a hand-written literal, so this cannot drift from
    /// the variant's actual wire shape.
    #[test]
    fn fallback_without_skipped_key_decodes_to_an_empty_list() {
        let reason = RoutingReason::Fallback {
            position: 1,
            after: Vec::new(),
            skipped: vec![RoutingReason::HeadroomSkip {
                skipped: "local/m1".parse().unwrap(),
                admission: Admission {
                    est_tokens: 91_808,
                    headroom_tokens: 8_192,
                    max_context_tokens: 32_768,
                },
            }],
        };
        let mut value = serde_json::to_value(&reason).unwrap();
        value
            .as_object_mut()
            .expect("RoutingReason serializes to an object")
            .remove("skipped")
            .expect("skipped key present before removal");

        let decoded: RoutingReason =
            serde_json::from_value(value).expect("pre-field-existing shape still decodes");
        assert_eq!(
            decoded,
            RoutingReason::Fallback {
                position: 1,
                after: Vec::new(),
                skipped: Vec::new(),
            }
        );
    }

    /// The one shared skip renderer names the candidate's own window, what
    /// the request actually required, both halves of that requirement, and
    /// the shortfall -- the numbers `conway routes explain` already printed
    /// for the same candidate, now carried to the moment of the skip.
    #[test]
    fn headroom_skip_detail_names_the_window_and_the_requirement() {
        let reason = RoutingReason::HeadroomSkip {
            skipped: "local/qwen3.8:27b-mlx".parse().unwrap(),
            admission: Admission {
                est_tokens: 91_808,
                headroom_tokens: 8_192,
                max_context_tokens: 32_768,
            },
        };
        let (model, detail) = reason.skip_detail().expect("a skip carries a detail");
        assert_eq!(model.to_string(), "local/qwen3.8:27b-mlx");
        assert_eq!(
            detail,
            "window 32768 < required 100000 (91808 prompt + 8192 headroom, short by 67232)"
        );

        // A SELECTION reason names no skipped candidate and must not
        // pretend to.
        assert!(RoutingReason::PinnedByApi.skip_detail().is_none());
        assert!(RoutingReason::Fallback {
            position: 1,
            after: Vec::new(),
            skipped: Vec::new(),
        }
        .skip_detail()
        .is_none());
    }

    #[test]
    fn explain_entry_token_fidelity_round_trips() {
        let entry = ExplainEntry {
            model_ref: "local/m1".parse().unwrap(),
            chain_position: Some(0),
            outcome: EntryOutcome::Selected {
                reason: RoutingReason::PinnedByApi,
            },
            capabilities: None,
            breaker: BreakerSnapshot {
                state: BreakerState::Closed,
            },
            token_fidelity: Some(TokenCountFidelity::Calibrated),
            context_window_source: None,
            cache_reporting: None,
            headroom_tokens: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: ExplainEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, entry);
    }

    /// `cache_reporting` (board item A5.7) must `#[serde(default)]`,
    /// mirroring `token_fidelity`'s own precedent: an `ExplainEntry` encoded
    /// before this field existed has no `cache_reporting` key at all, and
    /// must still decode -- to `None`, not a decode error.
    #[test]
    fn explain_entry_without_cache_reporting_key_decodes_to_none() {
        let entry = ExplainEntry {
            model_ref: "local/m1".parse().unwrap(),
            chain_position: Some(0),
            outcome: EntryOutcome::Selected {
                reason: RoutingReason::PinnedByApi,
            },
            capabilities: None,
            breaker: BreakerSnapshot {
                state: BreakerState::Closed,
            },
            token_fidelity: None,
            context_window_source: None,
            cache_reporting: Some(CacheReporting::Reported),
            headroom_tokens: None,
        };
        let mut value = serde_json::to_value(&entry).unwrap();
        value
            .as_object_mut()
            .expect("ExplainEntry serializes to an object")
            .remove("cache_reporting")
            .expect("cache_reporting key present before removal");

        let decoded: ExplainEntry =
            serde_json::from_value(value).expect("pre-field-existing shape still decodes");
        assert_eq!(decoded.cache_reporting, None);
    }

    #[test]
    fn explain_entry_cache_reporting_round_trips() {
        let entry = ExplainEntry {
            model_ref: "local/m1".parse().unwrap(),
            chain_position: Some(0),
            outcome: EntryOutcome::Selected {
                reason: RoutingReason::PinnedByApi,
            },
            capabilities: None,
            breaker: BreakerSnapshot {
                state: BreakerState::Closed,
            },
            token_fidelity: None,
            context_window_source: None,
            cache_reporting: Some(CacheReporting::NotReported),
            headroom_tokens: None,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: ExplainEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, entry);
    }

    #[test]
    fn minimal_router_resolve_returns_configured_chain_in_order() {
        let router = MinimalRouter::new(two_entry_chain_config());
        let routes = router
            .resolve(&request("planner"))
            .expect("configured role resolves");
        assert_eq!(routes.len(), 2);
        assert!(matches!(
            routes[0].reason,
            RoutingReason::AliasPrimary { .. }
        ));
        assert!(matches!(
            routes[1].reason,
            RoutingReason::Fallback { position: 1, .. }
        ));
    }

    #[test]
    fn minimal_router_resolve_unconfigured_role_is_unknown_role() {
        let router = MinimalRouter::new(two_entry_chain_config());
        let err = router
            .resolve(&request("no-such-role"))
            .expect_err("unconfigured role must not resolve");
        assert!(matches!(err, RoutingError::UnknownRole { .. }));
    }

    #[test]
    fn minimal_router_explain_unconfigured_role_is_empty_not_invented() {
        let router = MinimalRouter::new(two_entry_chain_config());
        let report = router.explain(&request("no-such-role"));
        assert!(report.entries.is_empty());
    }
}
