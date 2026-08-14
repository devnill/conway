//! A per-endpoint circuit breaker (Olla pattern): a `Transport`
//! breaker fed by request-path failures. All endpoint health *state* lives
//! here; routing *policy* never mutates it — `state`/`kind_state` are
//! read-only by construction (`&self`, read lock, expiry derived from the
//! clock on read).
//!
//! **A second, independent `Probe` breaker fed by a periodic health prober
//! used to live alongside this one; it was retired, not wired ** — the prober that would have fed it had
//! no production call site, and the Transport breaker alone already handles
//! recovery (a clock read takes it half-open; the next real request
//! retries). `merged_state` below is a one-arm merge now, kept as its own
//! method rather than inlined into `state` so a second breaker kind can be
//! reintroduced later without re-deriving the merge policy from scratch.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Duration, Utc};
use conway_core::ids::EndpointId;
use conway_core::ports::HealthRegistry;
use conway_core::routing::{BreakerKind, BreakerState, HealthConfig, Observation};

/// Injectable time source so every breaker transition is deterministic in
/// tests: no `sleep`, no wall-clock dependence.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// The production clock.
#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Deterministic test clock.
#[cfg(any(test, feature = "test-clock"))]
#[derive(Debug)]
pub struct TestClock {
    now: RwLock<DateTime<Utc>>,
}

#[cfg(any(test, feature = "test-clock"))]
impl TestClock {
    pub fn at(start: DateTime<Utc>) -> Arc<TestClock> {
        Arc::new(TestClock {
            now: RwLock::new(start),
        })
    }

    pub fn advance(&self, by: Duration) {
        let mut now = self.now.write().expect("clock lock");
        *now += by;
    }

    pub fn set(&self, to: DateTime<Utc>) {
        *self.now.write().expect("clock lock") = to;
    }
}

#[cfg(any(test, feature = "test-clock"))]
impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.read().expect("clock lock")
    }
}

/// One breaker's mutable cell. `opened_until` is never cleared on expiry by
/// a read — `HalfOpen` is derived from the clock at read time.
#[derive(Debug, Default, Clone, Copy)]
struct BreakerCell {
    consecutive_failures: u32,
    half_open_successes: u32,
    opened_until: Option<DateTime<Utc>>,
}

impl BreakerCell {
    /// The cell's state as of `now`. Read-only derivation.
    fn state_at(&self, now: DateTime<Utc>, kind: BreakerKind) -> BreakerState {
        match self.opened_until {
            Some(until) if now < until => BreakerState::Open { until, kind },
            Some(_) => BreakerState::HalfOpen,
            None => BreakerState::Closed,
        }
    }

    fn is_half_open(&self, now: DateTime<Utc>) -> bool {
        matches!(self.opened_until, Some(until) if now >= until)
    }

    fn record_failure(&mut self, now: DateTime<Utc>, threshold: u32, open_for: Duration) {
        if self.is_half_open(now) {
            // HalfOpen --failure--> Open (fixed duration, no backoff).
            self.half_open_successes = 0;
            self.opened_until = Some(now + open_for);
            return;
        }
        if self.opened_until.is_some() {
            // Already Open: failures while open don't re-extend.
            return;
        }
        self.consecutive_failures += 1;
        if self.consecutive_failures >= threshold {
            self.opened_until = Some(now + open_for);
        }
    }

    fn record_success(&mut self, now: DateTime<Utc>, close_threshold: u32) {
        if self.is_half_open(now) {
            self.half_open_successes += 1;
            if self.half_open_successes >= close_threshold {
                *self = BreakerCell::default();
            }
            return;
        }
        self.consecutive_failures = 0;
        if self.opened_until.is_none() {
            self.half_open_successes = 0;
        }
    }

    fn force_open(&mut self, until: DateTime<Utc>) {
        // RateLimited: force-open without touching the failure counter.
        self.opened_until = Some(until);
        self.half_open_successes = 0;
    }
}

#[derive(Debug, Default)]
struct EndpointBreakers {
    transport: BreakerCell,
    last_latency_ms: Option<u32>,
}

/// The workspace's `HealthRegistry` implementation: a per-endpoint breaker,
/// deterministic and clock-injectable.
pub struct BreakerRegistry {
    config: HealthConfig,
    clock: Arc<dyn Clock>,
    endpoints: RwLock<HashMap<EndpointId, EndpointBreakers>>,
    /// Incremented on every mutation; `state()` must never change it.
    generation: AtomicU64,
}

impl std::fmt::Debug for BreakerRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BreakerRegistry")
            .field("config", &self.config)
            .field("generation", &self.generation.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl BreakerRegistry {
    pub fn new(config: HealthConfig) -> Arc<BreakerRegistry> {
        Self::with_clock(config, Arc::new(SystemClock))
    }

    pub fn with_clock(config: HealthConfig, clock: Arc<dyn Clock>) -> Arc<BreakerRegistry> {
        Arc::new(BreakerRegistry {
            config,
            clock,
            endpoints: RwLock::new(HashMap::new()),
            generation: AtomicU64::new(0),
        })
    }

    fn open_duration(&self) -> Duration {
        Duration::seconds(self.config.open_duration_secs as i64)
    }

    /// One breaker's state, independently. Unknown endpoints read as
    /// `Closed` without inserting an entry. `kind` is `#[non_exhaustive]`
    /// from outside its defining crate, so the wildcard arm below is a real
    /// compile-time requirement, not spare generality — it stays even
    /// though `Transport` is the only variant that exists today.
    pub fn kind_state(&self, ep: &EndpointId, kind: BreakerKind) -> BreakerState {
        let now = self.clock.now();
        let endpoints = self.endpoints.read().expect("breaker lock");
        match endpoints.get(ep) {
            None => BreakerState::Closed,
            Some(cells) => match kind {
                BreakerKind::Transport => cells.transport.state_at(now, BreakerKind::Transport),
                _ => BreakerState::Closed,
            },
        }
    }

    /// Merged view. A single breaker remains (the `Probe` breaker was
    /// retired —), so this is now a
    /// direct passthrough of the Transport breaker's own state.
    fn merged_state(&self, ep: &EndpointId) -> BreakerState {
        self.kind_state(ep, BreakerKind::Transport)
    }

    /// Deterministic snapshot for reporting: sorted by endpoint, one entry
    /// per endpoint (a single breaker kind survives).
    pub fn snapshot(&self) -> Vec<(EndpointId, BreakerKind, BreakerState)> {
        let now = self.clock.now();
        let endpoints = self.endpoints.read().expect("breaker lock");
        let mut out: Vec<(EndpointId, BreakerKind, BreakerState)> = endpoints
            .iter()
            .map(|(ep, cells)| {
                (
                    ep.clone(),
                    BreakerKind::Transport,
                    cells.transport.state_at(now, BreakerKind::Transport),
                )
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Last observed request latency per endpoint (no breaker influence in
    /// MVP — reporting only).
    pub fn metrics(&self) -> Vec<(EndpointId, Option<u32>)> {
        let endpoints = self.endpoints.read().expect("breaker lock");
        let mut out: Vec<(EndpointId, Option<u32>)> = endpoints
            .iter()
            .map(|(ep, cells)| (ep.clone(), cells.last_latency_ms))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    #[cfg(test)]
    fn entry_count(&self) -> usize {
        self.endpoints.read().expect("breaker lock").len()
    }

    #[cfg(test)]
    fn generation_count(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
}

impl HealthRegistry for BreakerRegistry {
    fn state(&self, ep: &EndpointId) -> BreakerState {
        self.merged_state(ep)
    }

    fn record(&self, ep: &EndpointId, obs: Observation) {
        let now = self.clock.now();
        let open_for = self.open_duration();
        let mut endpoints = self.endpoints.write().expect("breaker lock");
        let cells = endpoints.entry(ep.clone()).or_default();
        self.generation.fetch_add(1, Ordering::SeqCst);
        match obs {
            Observation::Ok { latency_ms } => {
                cells.last_latency_ms = Some(latency_ms);
                cells
                    .transport
                    .record_success(now, self.config.half_open_successes_to_close);
            }
            Observation::TransportError | Observation::ServerError => {
                cells.transport.record_failure(
                    now,
                    self.config.transport_failures_to_open,
                    open_for,
                );
            }
            Observation::RateLimited { retry_after_secs } => {
                let until = now
                    + retry_after_secs
                        .map(|s| Duration::seconds(s as i64))
                        .unwrap_or(open_for);
                cells.transport.force_open(until);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static_assertions::assert_impl_all!(BreakerRegistry: Send, Sync);

    fn start() -> DateTime<Utc> {
        "2026-07-20T00:00:00Z".parse().unwrap()
    }

    pub(super) fn fresh_registry() -> (Arc<BreakerRegistry>, Arc<TestClock>) {
        let clock = TestClock::at(start());
        let registry =
            BreakerRegistry::with_clock(HealthConfig::default(), Arc::clone(&clock) as _);
        (registry, clock)
    }

    fn ep(name: &str) -> EndpointId {
        EndpointId::new(name)
    }

    #[test]
    fn unknown_endpoint_is_closed_without_insertion() {
        let (registry, _) = fresh_registry();
        assert_eq!(registry.state(&ep("nope")), BreakerState::Closed);
        assert_eq!(
            registry.kind_state(&ep("nope"), BreakerKind::Transport),
            BreakerState::Closed
        );
        assert_eq!(registry.entry_count(), 0);
    }

    #[test]
    fn transport_opens_at_threshold_not_before() {
        let (registry, _) = fresh_registry();
        let endpoint = ep("local");
        // default transport_failures_to_open == 3
        registry.record(&endpoint, Observation::TransportError);
        registry.record(&endpoint, Observation::ServerError);
        assert_eq!(
            registry.kind_state(&endpoint, BreakerKind::Transport),
            BreakerState::Closed,
            "still closed at threshold - 1"
        );
        registry.record(&endpoint, Observation::TransportError);
        assert!(matches!(
            registry.kind_state(&endpoint, BreakerKind::Transport),
            BreakerState::Open {
                kind: BreakerKind::Transport,
                ..
            }
        ));
    }

    #[test]
    fn ok_resets_the_counter() {
        let (registry, _) = fresh_registry();
        let endpoint = ep("local");
        registry.record(&endpoint, Observation::TransportError);
        registry.record(&endpoint, Observation::TransportError);
        registry.record(&endpoint, Observation::Ok { latency_ms: 12 });
        // Counter reset: two more transport failures stay Closed.
        registry.record(&endpoint, Observation::TransportError);
        registry.record(&endpoint, Observation::TransportError);
        assert_eq!(
            registry.kind_state(&endpoint, BreakerKind::Transport),
            BreakerState::Closed
        );
        assert_eq!(registry.metrics(), vec![(endpoint, Some(12))]);
    }

    #[test]
    fn rate_limited_force_opens_without_counter() {
        let (registry, _) = fresh_registry();
        let endpoint = ep("local");
        registry.record(
            &endpoint,
            Observation::RateLimited {
                retry_after_secs: Some(7),
            },
        );
        match registry.kind_state(&endpoint, BreakerKind::Transport) {
            BreakerState::Open { until, .. } => {
                assert_eq!(until, start() + Duration::seconds(7));
            }
            other => panic!("expected Open, got {other:?}"),
        }
        // Fallback to open_duration when retry_after is None.
        let (registry2, _clock2) = fresh_registry();
        registry2.record(
            &endpoint,
            Observation::RateLimited {
                retry_after_secs: None,
            },
        );
        match registry2.kind_state(&endpoint, BreakerKind::Transport) {
            BreakerState::Open { until, .. } => {
                assert_eq!(
                    until,
                    start() + Duration::seconds(HealthConfig::default().open_duration_secs as i64)
                );
            }
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn open_becomes_half_open_on_read_without_mutation() {
        let (registry, clock) = fresh_registry();
        let endpoint = ep("local");
        for _ in 0..3 {
            registry.record(&endpoint, Observation::TransportError);
        }
        let generation = registry.generation_count();
        clock.advance(Duration::seconds(
            HealthConfig::default().open_duration_secs as i64,
        ));
        for _ in 0..1000 {
            assert_eq!(
                registry.kind_state(&endpoint, BreakerKind::Transport),
                BreakerState::HalfOpen
            );
            let _ = registry.state(&endpoint);
        }
        assert_eq!(
            registry.generation_count(),
            generation,
            "reads must never mutate breaker state"
        );
    }

    #[test]
    fn half_open_closes_after_successes_and_reopens_on_failure() {
        let (registry, clock) = fresh_registry();
        let endpoint = ep("local");
        for _ in 0..3 {
            registry.record(&endpoint, Observation::TransportError);
        }
        clock.advance(Duration::seconds(31));
        assert_eq!(
            registry.kind_state(&endpoint, BreakerKind::Transport),
            BreakerState::HalfOpen
        );
        // default half_open_successes_to_close == 1
        registry.record(&endpoint, Observation::Ok { latency_ms: 5 });
        assert_eq!(
            registry.kind_state(&endpoint, BreakerKind::Transport),
            BreakerState::Closed
        );

        // Reopen on a half-open failure, fixed duration.
        for _ in 0..3 {
            registry.record(&endpoint, Observation::TransportError);
        }
        clock.advance(Duration::seconds(31));
        assert_eq!(
            registry.kind_state(&endpoint, BreakerKind::Transport),
            BreakerState::HalfOpen
        );
        registry.record(&endpoint, Observation::TransportError);
        match registry.kind_state(&endpoint, BreakerKind::Transport) {
            BreakerState::Open { until, .. } => {
                assert_eq!(until, clock.now() + Duration::seconds(30));
            }
            other => panic!("expected Open after half-open failure, got {other:?}"),
        }
    }

    /// A single breaker survives (the `Probe` breaker was retired — board
    /// item), so the merged view is now a
    /// direct passthrough of the Transport breaker's own state.
    #[test]
    fn merged_state_passes_through_the_transport_breaker() {
        let (registry, _) = fresh_registry();
        let endpoint = ep("local");
        registry.record(
            &endpoint,
            Observation::RateLimited {
                retry_after_secs: Some(300),
            },
        );
        match registry.state(&endpoint) {
            BreakerState::Open { until, kind } => {
                assert_eq!(kind, BreakerKind::Transport);
                assert_eq!(until, start() + Duration::seconds(300));
            }
            other => panic!("expected merged Open, got {other:?}"),
        }
    }

    #[test]
    fn snapshot_is_sorted_and_complete() {
        let (registry, _) = fresh_registry();
        registry.record(&ep("b"), Observation::TransportError);
        registry.record(&ep("a"), Observation::TransportError);
        let snapshot = registry.snapshot();
        let keys: Vec<(String, BreakerKind)> = snapshot
            .iter()
            .map(|(e, k, _)| (e.to_string(), *k))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("a".to_string(), BreakerKind::Transport),
                ("b".to_string(), BreakerKind::Transport),
            ]
        );
    }

    #[test]
    fn concurrent_recording_is_safe_and_deterministic() {
        let (registry, _) = fresh_registry();
        let endpoint = ep("local");
        std::thread::scope(|scope| {
            for _ in 0..64 {
                let registry = Arc::clone(&registry);
                let endpoint = endpoint.clone();
                scope.spawn(move || {
                    for _ in 0..100 {
                        registry.record(&endpoint, Observation::TransportError);
                    }
                });
            }
        });
        // 6400 consecutive transport failures with no successes: Open.
        assert!(matches!(
            registry.kind_state(&endpoint, BreakerKind::Transport),
            BreakerState::Open { .. }
        ));
        assert_eq!(registry.generation_count(), 6400);
    }
}
