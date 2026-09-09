//! `conway routes explain <role>`: a pure formatter over
//! `Conway::explain_routing`.

use std::collections::HashMap;

use clap::{Args, Subcommand};
use conway::{
    BreakerKind, BreakerState, CacheReporting, ContextTokensSource, Conway, EntryOutcome,
    ExplainEntry, ExplainReport, RoutingReason, TokenCountFidelity,
};
use serde_json::{json, Value};

use crate::diag;
use crate::exit::ExitCode;
use crate::first_run::InstallFootprint;

#[derive(Args, Debug)]
pub struct RoutesArgs {
    #[command(subcommand)]
    pub action: RoutesAction,
}

#[derive(Subcommand, Debug)]
pub enum RoutesAction {
    /// Explain how `role` would be routed right now.
    Explain {
        role: String,
        #[arg(long)]
        json: bool,
    },
}

pub async fn run(args: &RoutesArgs, conway: &Conway) -> conway::Result<ExitCode> {
    match &args.action {
        RoutesAction::Explain { role, json } => {
            // Unknown-role detection reads `conway.config().roles` directly
            // -- the configuration's own source of truth for which roles
            // exist -- rather than inferring it from whether the report
            // came back with no entries at all. A `Router` supplied from
            // outside `conway-routing` (`ConwayBuilder::with_router`) makes
            // `Conway::explain_routing` fall back to
            // `conway_core::routing::MinimalRouter`'s honestly degenerate
            // report, which has no entries for an unconfigured role but is
            // not the only way it can end up that way in principle (an
            // empty-chain role is a second, distinct cause) -- inferring
            // "unknown role" from bare emptiness previously made that
            // fallback misreport every correctly-configured role as unknown.
            if !conway.config().roles.contains_key(role.as_str()) {
                // The merge floor bakes in an empty role named "default" so
                // an unconfigured `default_role` still validates, so listing
                // `roles` raw would name a role the operator never wrote --
                // and one that cannot route. `/settings`' own cycle list
                // filters it with this same predicate; this is the second
                // surface that presents `roles` to a person, and it must use
                // the one predicate rather than restate the check.
                let mut roles: Vec<&String> = conway
                    .config()
                    .roles
                    .iter()
                    .filter(|(name, entry)| !conway::config::is_baked_in_role_floor(name, entry))
                    .map(|(name, _)| name)
                    .collect();
                roles.sort();
                let roles_list = roles
                    .iter()
                    .map(|r| r.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                diag::error(format!(
                    "unknown role `{role}`; configured roles: {roles_list}"
                ));
                return Ok(ExitCode::Usage);
            }

            let report = conway.explain_routing(&conway::RoleAlias::new(role.as_str()));

            // A1c: the role's effective sampling params -- `[roles.<alias>
            // .params]` in `settings.json`, read straight off `ConwayConfig`
            // rather than threaded through `ExplainEntry`/`ExplainReport`.
            // `MinimalRouter::resolve` and `DeclarativeRouter::params_for`
            // both key this same value by role alone (never per-candidate),
            // so this is the identical value either producer would have
            // used to build a `Route`/`ExplainEntry` -- reading it here
            // avoids widening `ExplainEntry`'s wire shape (a change with a
            // second, non-owned construction site in
            // `conway-plugin-routing`) for a value this command can answer
            // just as honestly by asking config directly.
            //
            // WHAT MAKES THAT TRUE, and it is not permanent by itself:
            // `ConwayConfig::routing()` converts `RoleEntry.params` into
            // `RoleConfig.params` through a TOTAL, FIELD-FOR-FIELD `From`
            // impl -- no defaulting, no merging, no per-candidate
            // adjustment. That is the whole reason config and the resolved
            // route cannot disagree here. If a transformation is ever added
            // on that path -- a global params table roles inherit from, a
            // per-backend clamp, anything at all -- this read silently
            // starts reporting what was CONFIGURED while the request
            // carries something else, and an explain command that lies is
            // worse than one that says nothing. Thread the resolved value
            // through `ExplainEntry` at that point instead of patching here.
            let role_params = conway
                .config()
                .roles
                .get(role.as_str())
                .map(|entry| entry.params.clone())
                .unwrap_or_default();

            // A2a: the fixed, per-turn cost of the default install (tool
            // schemas, instruction fragments, one representative allowance
            // per configured MCP server) -- reused verbatim from guided
            // setup's own preflight rather than re-deriving it, per that
            // item's own disclosure that a raw-JSON MCP-count read would be
            // the worse option here now that `routes explain` already holds
            // a built `Conway`.
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let env: HashMap<String, String> = std::env::vars().collect();
            let mcp_count = conway.config().plugins.mcp.len();
            let footprint = crate::first_run::default_opinion_set_footprint(&cwd, &env, mcp_count);

            if *json {
                print_json(&report, &role_params, &footprint);
            } else {
                print_text(&report, &role_params, &footprint);
            }
            Ok(ExitCode::Completed)
        }
    }
}

fn print_text(
    report: &ExplainReport,
    role_params: &conway::config::schema::RoleParams,
    footprint: &InstallFootprint,
) {
    println!(
        "role: {}  (est_tokens={}, headroom_tokens={})",
        report.role, report.est_tokens, report.headroom_tokens
    );
    println!("params: {}", render_role_params(role_params));

    let width = report
        .entries
        .iter()
        .map(|e| e.model_ref.to_string().len())
        .max()
        .unwrap_or(0)
        + 2;

    for entry in &report.entries {
        let marker = match entry.chain_position {
            Some(position) => format!("[{position}]"),
            None => "[pin]".to_string(),
        };
        let (word, reason) = match &entry.outcome {
            EntryOutcome::Selected { reason } => ("SELECTED", render_reason(reason)),
            EntryOutcome::Skipped { reason } => ("SKIPPED", render_reason(reason)),
        };
        let model_ref = entry.model_ref.to_string();
        let breaker = render_breaker_state(&entry.breaker.state);
        let tokens = render_token_fidelity(entry.token_fidelity);
        let window_tokens = entry.capabilities.as_ref().map(|c| c.max_context_tokens);
        let window = render_window(window_tokens);
        let provenance = render_context_window_source(entry.context_window_source);
        let cache = render_cache_reporting(entry.cache_reporting);
        let fixed_cost = render_fixed_cost(footprint, &model_ref, window_tokens);
        println!(
            "  {marker} {model_ref:<width$}{word:<8} {reason}  (breaker: {breaker}, tokens: \
             {tokens}, window: {window} [{provenance}], cache: {cache}, fixed_cost: \
             {fixed_cost})",
            width = width,
        );
    }
}

fn print_json(
    report: &ExplainReport,
    role_params: &conway::config::schema::RoleParams,
    footprint: &InstallFootprint,
) {
    let chain: Vec<Value> = report
        .entries
        .iter()
        .map(|e| entry_json(e, footprint))
        .collect();
    let skipped: Vec<Value> = report
        .entries
        .iter()
        .filter(|e| matches!(e.outcome, EntryOutcome::Skipped { .. }))
        .map(|e| entry_json(e, footprint))
        .collect();
    let health: Vec<Value> = report
        .entries
        .iter()
        .map(|e| {
            json!({
                "model": e.model_ref.to_string(),
                "state": breaker_state_tag(&e.breaker.state),
            })
        })
        .collect();

    let obj = json!({
        "role": report.role.to_string(),
        "params": render_role_params(role_params),
        "chain": chain,
        "skipped": skipped,
        "health": health,
    });
    println!(
        "{}",
        serde_json::to_string(&obj).expect("explain report always serializes")
    );
}

fn entry_json(e: &ExplainEntry, footprint: &InstallFootprint) -> Value {
    let (outcome, reason) = match &e.outcome {
        EntryOutcome::Selected { reason } => ("selected", render_reason(reason)),
        EntryOutcome::Skipped { reason } => ("skipped", render_reason(reason)),
    };
    let model_ref = e.model_ref.to_string();
    let window_tokens = e.capabilities.as_ref().map(|c| c.max_context_tokens);
    // Computed before the `json!` literal below (rather than inline as one
    // of its values) so this never depends on the macro's field-evaluation
    // order relative to `model_ref` being moved into the `"model"` entry.
    let fixed_cost = render_fixed_cost(footprint, &model_ref, window_tokens);
    json!({
        "position": e.chain_position,
        "model": model_ref,
        "outcome": outcome,
        "reason": reason,
        "token_fidelity": render_token_fidelity(e.token_fidelity),
        "context_window_tokens": window_tokens,
        "context_window_source": render_context_window_source(e.context_window_source),
        "cache_reporting": render_cache_reporting(e.cache_reporting),
        "fixed_cost": fixed_cost,
    })
}

/// Renders a `RoutingReason` per the module's binding mapping. Every
/// variant known to this crate is matched explicitly; the wildcard arm
/// exists only for `#[non_exhaustive]` forward-compatibility and is not
/// expected to be exercised by this item's own unit tests -- a candidate
/// rendered through it (rather than one of the named arms below) is
/// treated as a bug.
fn render_reason(reason: &RoutingReason) -> String {
    match reason {
        RoutingReason::PinnedByApi => "pinned by API".to_string(),
        RoutingReason::PinnedByAgentDef => "pinned by agent definition".to_string(),
        RoutingReason::AliasPrimary { alias } => format!("primary for role `{alias}`"),
        RoutingReason::Fallback { position, after } => {
            let failures: Vec<String> = after
                .iter()
                .map(|f| format!("{}: {}", f.model, f.error))
                .collect();
            format!("fallback #{position} after: {}", failures.join(", "))
        }
        RoutingReason::CapabilitySkip { skipped, missing } => {
            format!("skipped `{skipped}`: missing {}", missing.join(", "))
        }
        RoutingReason::HealthSkip { skipped, breaker } => {
            let kind = breaker_kind_name(breaker);
            format!("skipped `{skipped}`: {kind} breaker open")
        }
        _ => format!("{reason:?}"),
    }
}

fn breaker_kind_name(kind: &BreakerKind) -> &'static str {
    match kind {
        BreakerKind::Transport => "transport",
        _ => "unknown",
    }
}

fn render_breaker_state(state: &BreakerState) -> String {
    match state {
        BreakerState::Closed => "closed".to_string(),
        BreakerState::HalfOpen => "half-open".to_string(),
        BreakerState::Open { until, .. } => {
            format!(
                "open until {}",
                until.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            )
        }
        _ => "unknown".to_string(),
    }
}

fn breaker_state_tag(state: &BreakerState) -> &'static str {
    match state {
        BreakerState::Closed => "closed",
        BreakerState::HalfOpen => "half_open",
        BreakerState::Open { .. } => "open",
        _ => "unknown",
    }
}

/// Renders `ExplainEntry::token_fidelity` (board item
/// 01M0ASX466G3PW3SJJS3KGNS55) for both `print_text` and `entry_json` --
/// the operator-visible answer to "how much should I trust this backend's
/// token estimate?" that `Backend::token_fidelity` exists to force a
/// deliberate answer to. `None` (the producing `RoutingExplainer` could not
/// reach a live backend instance, e.g. `MinimalRouter`'s config-only
/// fallback) and an unrecognized future `TokenCountFidelity` variant (this
/// crate's dependency is `#[non_exhaustive]`) both render `"unknown"` --
/// deliberately the same string, since neither is a claim this call site can
/// tell apart from the operator's point of view.
fn render_token_fidelity(fidelity: Option<TokenCountFidelity>) -> &'static str {
    match fidelity {
        None => "unknown",
        Some(TokenCountFidelity::Exact) => "exact",
        Some(TokenCountFidelity::Calibrated) => "calibrated",
        Some(TokenCountFidelity::Heuristic) => "heuristic",
        Some(_) => "unknown",
    }
}

/// Renders `ExplainEntry::capabilities.map(|c| c.max_context_tokens)` --
/// hosted OpenAI-compatible models item, acceptance criterion 1: "no
/// `unknown` anywhere" for a model this report will route to. `None` here
/// means this candidate has no capability-index entry at all (an
/// undeclared model, or the `MinimalRouter` config-only fallback), which is
/// a materially different fact from "a number exists but its source is
/// unconfirmed" -- `render_context_window_source` is what carries THAT
/// distinction; this function only ever answers "is there a number to
/// show".
fn render_window(max_context_tokens: Option<u32>) -> String {
    match max_context_tokens {
        Some(tokens) => tokens.to_string(),
        None => "not indexed".to_string(),
    }
}

/// Renders `ExplainEntry::context_window_source` -- the provenance marker
/// GP-14 requires beside every window number this report shows: `verified`
/// (a compiled-in, sourced metadata table entry, or Anthropic/`openai`'s own
/// documented per-family figure), `models.json` (an operator-editable
/// override -- config-level `ModelOverrides` or a `metadata_path` file,
/// either hand-edited or written by conway's own discover-or-ask setup
/// flow), `probed` (a live discovery result for this exact model), or
/// `floor (assumed)` (this dialect's own baseline governs, and that
/// baseline is not a sourced fact about any real model of this provider --
/// `ContextTokensSource::Unverified`). `None` (no capability-index entry at
/// all) and an unrecognized future variant (this crate's dependency is
/// `#[non_exhaustive]`) both render `"unknown"` -- the one case this
/// function is honestly unable to label, distinct from every named case
/// above, which it always can.
fn render_context_window_source(source: Option<ContextTokensSource>) -> &'static str {
    match source {
        None => "unknown",
        Some(ContextTokensSource::Override) => "models.json",
        Some(ContextTokensSource::Metadata) => "verified",
        Some(ContextTokensSource::Probed) => "probed",
        Some(ContextTokensSource::DialectDefaultFloor) => "verified",
        Some(ContextTokensSource::Unverified) => "floor (assumed)",
        Some(_) => "unknown",
    }
}

/// Renders `ExplainEntry::cache_reporting` (board item A5.7, "prompt
/// caching reads zero on every real session") -- the operator-visible
/// answer to "does this candidate's backend have anywhere to report a
/// cache hit at all", printed BEFORE a single request is sent, so an
/// operator watching a `0%`/`not reported` status line can tell whether
/// that is because nothing is being hit, or because this backend's wire
/// dialect has no cache field to hit in the first place. `"reported"` and
/// `"not reported"` are DELIBERATELY distinct strings from `"unknown"`
/// (`None` -- the producing `RoutingExplainer` could not reach a live
/// `Backend` instance, e.g. `MinimalRouter`'s config-only fallback, or an
/// unrecognized future `CacheReporting` variant, this crate's dependency is
/// `#[non_exhaustive]`): a candidate this report could not ask is not the
/// same fact as one that answered "no."
fn render_cache_reporting(cache_reporting: Option<CacheReporting>) -> &'static str {
    match cache_reporting {
        None => "unknown",
        Some(CacheReporting::Reported) => "reported",
        Some(CacheReporting::NotReported) => "not reported",
        Some(_) => "unknown",
    }
}

/// Renders a role's effective `[roles.<alias>.params]` -- the operator-
/// visible answer to "what sampling params will conway actually send for
/// this role", read straight off `ConwayConfig::roles` (see this module's
/// `run` for why this bypasses `ExplainEntry` entirely). Every wire field
/// that is `Some`/non-empty is named `key=value`; `extra`'s entries are
/// named the same way (`reasoning_budget_tokens=8192`), since that map is
/// exactly where provider-specific keys like Anthropic extended thinking's
/// `reasoning_budget_tokens` live (`docs/routing.md`'s worked recipe).
/// Renders the literal string `"default"` -- never an empty string, and
/// never a place-holder that could be mistaken for a real (if unset) field
/// list -- when nothing at all is configured, so an operator can tell "this
/// role has no sampling overrides" apart from "the renderer produced
/// nothing to show."
fn render_role_params(params: &conway::config::schema::RoleParams) -> String {
    let mut parts = Vec::new();
    if let Some(temperature) = params.temperature {
        parts.push(format!("temperature={temperature}"));
    }
    if let Some(top_p) = params.top_p {
        parts.push(format!("top_p={top_p}"));
    }
    if let Some(max_tokens) = params.max_tokens {
        parts.push(format!("max_tokens={max_tokens}"));
    }
    if !params.stop.is_empty() {
        parts.push(format!("stop={:?}", params.stop));
    }
    if let Some(seed) = params.seed {
        parts.push(format!("seed={seed}"));
    }
    for (key, value) in &params.extra {
        parts.push(format!("{key}={value}"));
    }
    if parts.is_empty() {
        "default".to_string()
    } else {
        parts.join(", ")
    }
}

/// Whether `footprint`'s fixed install cost exceeds half of `window`
/// (`crate::first_run::runway_fixed_cost_warning`'s own
/// `INSTALL_FOOTPRINT_WARN_FRACTION` threshold, reused rather than
/// re-derived) -- `None` when there is nothing to compare against: `window`
/// is `None` (this candidate has no capability-index entry at all, the
/// exact condition `render_window` already reports as `"not indexed"`) or
/// `Some(0)`, `runway_fixed_cost_warning`'s own "nothing to compare
/// against" case.
fn fixed_cost_over_half_window(
    footprint: &InstallFootprint,
    model_key: &str,
    window: Option<u32>,
) -> Option<bool> {
    let window = window.filter(|w| *w > 0)?;
    Some(crate::first_run::runway_fixed_cost_warning(model_key, window, footprint, None).is_some())
}

/// Renders `footprint`'s fixed install cost against `window` -- A2a's own
/// preflight check, reused here so `conway routes explain` answers "will
/// the default install alone eat this candidate's window" beside the
/// existing breaker/tokens/window/cache columns rather than only at guided
/// setup time. `"unknown"` mirrors `render_token_fidelity`/
/// `render_cache_reporting`'s own precedent: `window` being absent is a
/// materially different fact from a real, sub-threshold percentage, and
/// must never collapse into the same string a real number would render.
fn render_fixed_cost(footprint: &InstallFootprint, model_key: &str, window: Option<u32>) -> String {
    match window.filter(|w| *w > 0) {
        None => "unknown".to_string(),
        Some(window) => {
            let total = footprint.total_tokens_est();
            let pct =
                ((u64::from(total) * 100) / u64::from(window)).min(u64::from(u32::MAX)) as u32;
            let over_half =
                fixed_cost_over_half_window(footprint, model_key, Some(window)).unwrap_or(false);
            let flag = if over_half {
                ", over half the window"
            } else {
                ""
            };
            format!("{total}/{window} tokens ({pct}%{flag})")
        }
    }
}

#[cfg(test)]
mod tests {
    use conway::{AttemptFailure, ModelRef};

    use super::*;

    #[test]
    fn every_known_reason_renders_a_specific_non_debug_string() {
        let alias = conway::RoleAlias::new("coder");
        let model: ModelRef = "backend/model".parse().expect("valid model ref");

        let cases: Vec<(RoutingReason, &str)> = vec![
            (RoutingReason::PinnedByApi, "pinned by API"),
            (
                RoutingReason::PinnedByAgentDef,
                "pinned by agent definition",
            ),
            (
                RoutingReason::AliasPrimary {
                    alias: alias.clone(),
                },
                "primary for role `coder`",
            ),
        ];
        for (reason, expected) in cases {
            assert_eq!(render_reason(&reason), expected);
        }

        let cap_skip = RoutingReason::CapabilitySkip {
            skipped: model.clone(),
            missing: vec!["tool_calling".to_string()],
        };
        assert_eq!(
            render_reason(&cap_skip),
            "skipped `backend/model`: missing tool_calling"
        );

        let health_skip = RoutingReason::HealthSkip {
            skipped: model.clone(),
            breaker: BreakerKind::Transport,
        };
        assert_eq!(
            render_reason(&health_skip),
            "skipped `backend/model`: transport breaker open"
        );

        let fallback = RoutingReason::Fallback {
            position: 2,
            after: vec![AttemptFailure {
                model: model.clone(),
                error: "connection refused".to_string(),
                at: chrono::Utc::now(),
            }],
        };
        assert_eq!(
            render_reason(&fallback),
            "fallback #2 after: backend/model: connection refused"
        );
    }

    #[test]
    fn breaker_state_renders_without_debug_fallback() {
        assert_eq!(render_breaker_state(&BreakerState::Closed), "closed");
        assert_eq!(render_breaker_state(&BreakerState::HalfOpen), "half-open");
        assert_eq!(breaker_state_tag(&BreakerState::Closed), "closed");
        assert_eq!(breaker_state_tag(&BreakerState::HalfOpen), "half_open");
    }

    /// Board item 01M0ASX466G3PW3SJJS3KGNS55: an operator asking "how much
    /// should I trust this backend's token estimate?" reads one of these
    /// three named answers, or `"unknown"` when the router could not reach
    /// a live backend instance at all -- never a bare `Debug` dump.
    #[test]
    fn token_fidelity_renders_every_declared_variant_and_none_as_unknown() {
        assert_eq!(
            render_token_fidelity(Some(TokenCountFidelity::Exact)),
            "exact"
        );
        assert_eq!(
            render_token_fidelity(Some(TokenCountFidelity::Calibrated)),
            "calibrated"
        );
        assert_eq!(
            render_token_fidelity(Some(TokenCountFidelity::Heuristic)),
            "heuristic"
        );
        assert_eq!(render_token_fidelity(None), "unknown");
    }

    /// Hosted OpenAI-compatible models item, acceptance criterion 1:
    /// `conway routes explain` shows a window and provenance for every
    /// candidate; `render_context_window_source` renders every declared
    /// `ContextTokensSource` variant with a DISTINCT, named label -- GP-14,
    /// "verified", "models.json"/"declared", "probed", and "floor
    /// (assumed)" are different facts and must never collapse into the
    /// same string.
    #[test]
    fn render_context_window_source_names_every_declared_variant_distinctly() {
        assert_eq!(
            render_context_window_source(Some(ContextTokensSource::Override)),
            "models.json"
        );
        assert_eq!(
            render_context_window_source(Some(ContextTokensSource::Metadata)),
            "verified"
        );
        assert_eq!(
            render_context_window_source(Some(ContextTokensSource::Probed)),
            "probed"
        );
        assert_eq!(
            render_context_window_source(Some(ContextTokensSource::DialectDefaultFloor)),
            "verified"
        );
        assert_eq!(
            render_context_window_source(Some(ContextTokensSource::Unverified)),
            "floor (assumed)"
        );
        assert_eq!(render_context_window_source(None), "unknown");

        // The two "no real fact was declared" variants must NEVER render
        // the same string as a genuinely sourced one -- the exact
        // distinction the reported incident's `unknown`-everywhere output
        // erased.
        assert_ne!(
            render_context_window_source(Some(ContextTokensSource::Unverified)),
            render_context_window_source(Some(ContextTokensSource::Metadata)),
        );
        assert_ne!(
            render_context_window_source(Some(ContextTokensSource::Unverified)),
            render_context_window_source(Some(ContextTokensSource::Override)),
        );
    }

    #[test]
    fn render_window_shows_the_number_or_names_that_none_was_indexed() {
        assert_eq!(render_window(Some(1_048_576)), "1048576");
        assert_eq!(render_window(None), "not indexed");
    }

    /// Board item A5.7: `render_cache_reporting` renders every declared
    /// `CacheReporting` variant with a DISTINCT, named label -- "reported"
    /// and "not reported" are different facts a backend actually declared,
    /// and both must render differently from `"unknown"` (the router could
    /// not reach a live backend at all) -- the exact conflation this board
    /// item exists to prevent.
    #[test]
    fn render_cache_reporting_names_every_declared_variant_distinctly() {
        assert_eq!(
            render_cache_reporting(Some(CacheReporting::Reported)),
            "reported"
        );
        assert_eq!(
            render_cache_reporting(Some(CacheReporting::NotReported)),
            "not reported"
        );
        assert_eq!(render_cache_reporting(None), "unknown");
        assert_ne!(
            render_cache_reporting(Some(CacheReporting::NotReported)),
            render_cache_reporting(Some(CacheReporting::Reported)),
        );
        assert_ne!(
            render_cache_reporting(None),
            render_cache_reporting(Some(CacheReporting::NotReported)),
        );
    }

    // -------------------------------------------------------------------
    // A1c: `routes explain` shows a role's effective sampling params.
    // -------------------------------------------------------------------

    /// **The "fires" half.** A role with `temperature` and an `extra` key
    /// configured (`reasoning_budget_tokens`, the exact Anthropic extended
    /// thinking key `docs/routing.md`'s worked recipe names) must show both
    /// -- not collapse to the same `"default"` string an unconfigured role
    /// renders. Paired with the "never" test below: together they catch a
    /// printer that always shows `"default"` regardless of what is
    /// configured, which this test alone could not.
    #[test]
    fn render_role_params_shows_configured_temperature_and_extra_key() {
        let mut extra = serde_json::Map::new();
        extra.insert("reasoning_budget_tokens".to_string(), json!(8_192));
        let params = conway::config::schema::RoleParams {
            temperature: Some(0.4),
            extra,
            ..Default::default()
        };
        let rendered = render_role_params(&params);
        assert!(rendered.contains("temperature=0.4"), "{rendered}");
        assert!(
            rendered.contains("reasoning_budget_tokens=8192"),
            "{rendered}"
        );
        assert_ne!(rendered, "default");
    }

    /// **The "never" half.** A role with nothing configured (every field
    /// `None`/empty) must show the literal `"default"` sentinel, not an
    /// empty string or a debug dump of six absent fields. Paired with the
    /// test above: together they catch a printer that always emits the
    /// same field list regardless of whether anything was actually
    /// configured, which this test alone could not.
    #[test]
    fn render_role_params_with_nothing_configured_shows_default() {
        let params = conway::config::schema::RoleParams::default();
        assert_eq!(render_role_params(&params), "default");
    }

    // -------------------------------------------------------------------
    // A2a: `routes explain` shows window vs. the install's fixed cost.
    // -------------------------------------------------------------------

    /// The exact fixture `first_run.rs`'s own
    /// `runway_fixed_cost_warning_fires_and_names_both_numbers_and_both_offers`
    /// / `..._stays_silent_under_the_fraction` pair uses (24.0k total: 9.5k
    /// tool schemas + 500 instructions + 14k command-prompt allowance),
    /// reused here rather than a fresh one so this test and that one are
    /// provably checking the identical threshold.
    fn fixture_footprint() -> InstallFootprint {
        InstallFootprint {
            tool_schema_tokens_est: 9_500,
            instruction_fragment_tokens_est: 500,
            command_prompt_allowance_tokens_est: 14_000,
        }
    }

    /// **The "fires" half.** 24,000 of 32,768 tokens is 73%, well past the
    /// 50% line -- `render_fixed_cost` must name both numbers and flag the
    /// over-threshold state, and `fixed_cost_over_half_window` must answer
    /// `Some(true)`. Paired with the "silent" test below (identical
    /// fixture, larger window): together they catch an implementation that
    /// always reports "over half" regardless of the window, which this
    /// test alone could not.
    #[test]
    fn render_fixed_cost_flags_over_half_window_on_a_small_window() {
        let footprint = fixture_footprint();
        let rendered = render_fixed_cost(&footprint, "ollama/glm-5.2", Some(32_768));
        assert!(rendered.contains("24000/32768"), "{rendered}");
        assert!(rendered.contains("73%"), "{rendered}");
        assert!(rendered.contains("over half"), "{rendered}");
        assert_eq!(
            fixed_cost_over_half_window(&footprint, "ollama/glm-5.2", Some(32_768)),
            Some(true)
        );
    }

    /// **The "silent" half.** The identical fixture against a 200,000-token
    /// window (24.0k is 12%) must NOT flag the over-threshold state --
    /// `render_fixed_cost` must not contain the warning phrase, and
    /// `fixed_cost_over_half_window` must answer `Some(false)`. Paired with
    /// the "fires" test above: together they catch an implementation that
    /// never flags the over-threshold state regardless of the window,
    /// which this test alone could not.
    #[test]
    fn render_fixed_cost_stays_under_threshold_on_a_large_window_identical_fixture() {
        let footprint = fixture_footprint();
        let rendered = render_fixed_cost(&footprint, "anthropic/claude-haiku-4-5", Some(200_000));
        assert!(rendered.contains("24000/200000"), "{rendered}");
        assert!(rendered.contains("12%"), "{rendered}");
        assert!(!rendered.contains("over half"), "{rendered}");
        assert_eq!(
            fixed_cost_over_half_window(&footprint, "anthropic/claude-haiku-4-5", Some(200_000)),
            Some(false)
        );
    }

    /// `render_fixed_cost`'s `"unknown"` case: no capability-index entry at
    /// all (the same condition `render_window` reports as `"not indexed"`)
    /// must render distinctly from any real percentage, and
    /// `fixed_cost_over_half_window` must answer `None` -- not `Some(_)`,
    /// which would claim a comparison this call site could not honestly
    /// make.
    #[test]
    fn render_fixed_cost_with_no_indexed_window_reads_unknown() {
        let footprint = fixture_footprint();
        assert_eq!(render_fixed_cost(&footprint, "local/m1", None), "unknown");
        assert_eq!(
            fixed_cost_over_half_window(&footprint, "local/m1", None),
            None
        );
    }
}
