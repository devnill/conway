//! `conway routes explain <role>`: a pure formatter over
//! `Conway::explain_routing`.

use clap::{Args, Subcommand};
use conway::{
    BreakerKind, BreakerState, Conway, EntryOutcome, ExplainEntry, ExplainReport, RoutingReason,
};
use serde_json::{json, Value};

use crate::diag;
use crate::exit::ExitCode;

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
            // fallback misreport every correctly-configured role as unknown
            //.
            if !conway.config().roles.contains_key(role.as_str()) {
                let mut roles: Vec<&String> = conway.config().roles.keys().collect();
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

            if *json {
                print_json(&report);
            } else {
                print_text(&report);
            }
            Ok(ExitCode::Completed)
        }
    }
}

fn print_text(report: &ExplainReport) {
    println!(
        "role: {}  (est_tokens={}, headroom_tokens={})",
        report.role, report.est_tokens, report.headroom_tokens
    );

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
        println!(
            "  {marker} {model_ref:<width$}{word:<8} {reason}  (breaker: {breaker})",
            width = width,
        );
    }
}

fn print_json(report: &ExplainReport) {
    let chain: Vec<Value> = report.entries.iter().map(entry_json).collect();
    let skipped: Vec<Value> = report
        .entries
        .iter()
        .filter(|e| matches!(e.outcome, EntryOutcome::Skipped { .. }))
        .map(entry_json)
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
        "chain": chain,
        "skipped": skipped,
        "health": health,
    });
    println!(
        "{}",
        serde_json::to_string(&obj).expect("explain report always serializes")
    );
}

fn entry_json(e: &ExplainEntry) -> Value {
    let (outcome, reason) = match &e.outcome {
        EntryOutcome::Selected { reason } => ("selected", render_reason(reason)),
        EntryOutcome::Skipped { reason } => ("skipped", render_reason(reason)),
    };
    json!({
        "position": e.chain_position,
        "model": e.model_ref.to_string(),
        "outcome": outcome,
        "reason": reason,
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
}
