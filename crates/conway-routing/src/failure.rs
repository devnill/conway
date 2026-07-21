//! The authority on whether a `conway_core::error::BackendError` should
//! advance the fallback chain and/or feed a health observation (tension
//! T-2), as the three-class model the attempt loop consults.
//!
//! Relationship to `conway-core`'s coarse boolean helpers (S1, cycle-1
//! incremental review): `BackendError::is_health_signal()` is the two-way
//! projection of this table — `observation_for(e).is_some() ==
//! e.is_health_signal()` is pinned by a consistency test below, so the two
//! can never drift silently. `BackendError::is_failover_worthy()` answers a
//! DIFFERENT, narrower question (same-request retriability per §8) and
//! intentionally disagrees with `FailureClass::advances_chain` on
//! `BadRequest`: a bad request is never worth re-sending anywhere as-is,
//! but the chain still advances past it (`RequestIncompatible`) because a
//! different candidate model may accept the request (e.g. a larger context
//! window). Chain policy consults THIS module; transport-retry policy
//! consults `is_failover_worthy`.
//!
//! `BackendError::ContextOverflow` and `BackendError::BadRequest` are
//! request problems, not endpoint-health signals: they advance the chain
//! (the next candidate may well be able to serve the request) but never
//! produce an `Observation`, so a too-large prompt never trips a breaker
//! for an otherwise-healthy endpoint.

use conway_core::error::BackendError;
use conway_core::routing::Observation;

/// The three-way classification of a `BackendError` the runtime consults to
/// decide (a) whether to advance to the next candidate in the fallback
/// chain and (b) whether the error is even eligible to feed
/// `HealthRegistry::record` (see [`observation_for`], which is the
/// authoritative answer to (b)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// A transient endpoint problem. Advances the fallback chain and is
    /// eligible for a health observation via [`observation_for`].
    FailoverRetryable,
    /// The endpoint may be perfectly healthy; the request itself cannot be
    /// served here. Advances the fallback chain, never produces a health
    /// observation (T-2).
    RequestIncompatible,
    /// Not worth retrying anywhere in this turn. Does not advance the
    /// fallback chain, never produces a health observation.
    Fatal,
}

impl FailureClass {
    /// Whether the fallback chain should advance to the next candidate
    /// after an error of this class.
    pub fn advances_chain(self) -> bool {
        matches!(
            self,
            FailureClass::FailoverRetryable | FailureClass::RequestIncompatible
        )
    }
}

/// Classifies a `BackendError` per the T-2 table:
/// `Transport | ServerError | RateLimit` -> `FailoverRetryable`;
/// `ContextOverflow | BadRequest` -> `RequestIncompatible`;
/// `Auth | Cancelled | ToolParse` -> `Fatal`.
pub fn classify(err: &BackendError) -> FailureClass {
    match err {
        BackendError::Transport { .. }
        | BackendError::ServerError { .. }
        | BackendError::RateLimit { .. } => FailureClass::FailoverRetryable,

        BackendError::ContextOverflow { .. } | BackendError::BadRequest { .. } => {
            FailureClass::RequestIncompatible
        }

        BackendError::Auth { .. } | BackendError::Cancelled | BackendError::ToolParse { .. } => {
            FailureClass::Fatal
        }

        // `BackendError` is `#[non_exhaustive]`: an unrecognized future
        // variant is treated as the safest of the three classes -- no
        // chain advance, no health mutation -- until this crate is updated
        // to classify it explicitly.
        _ => FailureClass::Fatal,
    }
}

/// This crate's single authority on whether a `BackendError` should feed
/// `HealthRegistry::record`, and with which `Observation`. Returns `None`
/// for every error whose [`classify`] result is `RequestIncompatible` or
/// `Fatal`; returns `Some(_)` for every `FailoverRetryable` variant.
pub fn observation_for(err: &BackendError) -> Option<Observation> {
    match err {
        BackendError::Transport { .. } => Some(Observation::TransportError),
        BackendError::ServerError { .. } => Some(Observation::ServerError),
        BackendError::RateLimit { retry_after_secs } => Some(Observation::RateLimited {
            retry_after_secs: *retry_after_secs,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_variants() -> Vec<BackendError> {
        vec![
            BackendError::Transport { detail: "x".into() },
            BackendError::RateLimit {
                retry_after_secs: Some(7),
            },
            BackendError::RateLimit {
                retry_after_secs: None,
            },
            BackendError::Auth { detail: "x".into() },
            BackendError::BadRequest { detail: "x".into() },
            BackendError::ServerError {
                status: 503,
                detail: "x".into(),
            },
            BackendError::ContextOverflow {
                required_tokens: 2,
                max_context_tokens: 1,
            },
            BackendError::ToolParse { detail: "x".into() },
            BackendError::Cancelled,
        ]
    }

    #[test]
    fn classify_matches_t2_table() {
        use FailureClass::*;
        let cases = [
            (
                BackendError::Transport { detail: "x".into() },
                FailoverRetryable,
            ),
            (
                BackendError::ServerError {
                    status: 500,
                    detail: "x".into(),
                },
                FailoverRetryable,
            ),
            (
                BackendError::RateLimit {
                    retry_after_secs: None,
                },
                FailoverRetryable,
            ),
            (
                BackendError::ContextOverflow {
                    required_tokens: 1,
                    max_context_tokens: 1,
                },
                RequestIncompatible,
            ),
            (
                BackendError::BadRequest { detail: "x".into() },
                RequestIncompatible,
            ),
            (BackendError::Auth { detail: "x".into() }, Fatal),
            (BackendError::Cancelled, Fatal),
            (BackendError::ToolParse { detail: "x".into() }, Fatal),
        ];
        for (err, expected) in cases {
            assert_eq!(classify(&err), expected, "unexpected class for {err:?}");
        }
    }

    /// Exhaustive over every `BackendError` variant: `observation_for` is
    /// `None` iff the class is `RequestIncompatible`/`Fatal`, `Some(_)` iff
    /// the class is `FailoverRetryable`.
    #[test]
    fn observation_for_is_exhaustive_over_all_variants() {
        for err in all_variants() {
            let class = classify(&err);
            let obs = observation_for(&err);
            match class {
                FailureClass::FailoverRetryable => {
                    assert!(obs.is_some(), "expected Some(_) for {err:?}")
                }
                FailureClass::RequestIncompatible | FailureClass::Fatal => {
                    assert!(obs.is_none(), "expected None for {err:?}")
                }
            }
        }
    }

    #[test]
    fn observation_for_maps_specific_variants() {
        assert_eq!(
            observation_for(&BackendError::Transport { detail: "x".into() }),
            Some(Observation::TransportError)
        );
        assert_eq!(
            observation_for(&BackendError::ServerError {
                status: 500,
                detail: "x".into()
            }),
            Some(Observation::ServerError)
        );
        assert_eq!(
            observation_for(&BackendError::RateLimit {
                retry_after_secs: Some(9)
            }),
            Some(Observation::RateLimited {
                retry_after_secs: Some(9)
            })
        );
        assert_eq!(
            observation_for(&BackendError::RateLimit {
                retry_after_secs: None
            }),
            Some(Observation::RateLimited {
                retry_after_secs: None
            })
        );
    }

    #[test]
    fn observation_for_is_none_for_request_incompatible_and_fatal() {
        assert_eq!(
            observation_for(&BackendError::ContextOverflow {
                required_tokens: 2,
                max_context_tokens: 1
            }),
            None
        );
        assert_eq!(
            observation_for(&BackendError::BadRequest { detail: "x".into() }),
            None
        );
        assert_eq!(
            observation_for(&BackendError::Auth { detail: "x".into() }),
            None
        );
        assert_eq!(observation_for(&BackendError::Cancelled), None);
        assert_eq!(
            observation_for(&BackendError::ToolParse { detail: "x".into() }),
            None
        );
    }

    #[test]
    fn advances_chain_matches_table() {
        assert!(FailureClass::FailoverRetryable.advances_chain());
        assert!(FailureClass::RequestIncompatible.advances_chain());
        assert!(!FailureClass::Fatal.advances_chain());
    }

    /// S1 pin: the routing-side table and conway-core's boolean projections
    /// can never drift silently. Exhaustive over every `BackendError`
    /// variant.
    #[test]
    fn classification_is_consistent_with_core_projections() {
        for e in all_variants() {
            assert_eq!(
                observation_for(&e).is_some(),
                e.is_health_signal(),
                "health-signal projection drifted for {e:?}"
            );
            match classify(&e) {
                FailureClass::FailoverRetryable => {
                    assert!(
                        e.is_failover_worthy(),
                        "retryable but not failover-worthy: {e:?}"
                    );
                }
                FailureClass::Fatal => {
                    assert!(!e.is_failover_worthy(), "fatal but failover-worthy: {e:?}");
                }
                FailureClass::RequestIncompatible => {
                    // ContextOverflow is failover-worthy (a bigger-window
                    // candidate helps); BadRequest deliberately is not (the
                    // request itself is malformed) — both still advance the
                    // chain. Documented divergence; no assertion beyond
                    // advances_chain.
                    assert!(classify(&e).advances_chain());
                }
            }
        }
    }
}
