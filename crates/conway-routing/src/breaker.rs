//! Dual per-endpoint circuit breakers (WI-033, Olla pattern): a `Transport`
//! breaker fed by request-path failures and an independent `Probe` breaker
//! fed by the health prober. All endpoint health *state* lives here; routing
//! *policy* never mutates it — `state`/`kind_state` are read-only by
//! construction (`&self`, read lock, expiry derived from the clock on read).

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
    probe: BreakerCell,
    last_latency_ms: Option<u32>,
}

/// The workspace's `HealthRegistry` implementation: two independent
/// breakers per endpoint, deterministic and clock-injectable.
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
    /// `Closed` without inserting an entry.
    pub fn kind_state(&self, ep: &EndpointId, kind: BreakerKind) -> BreakerState {
        let now = self.clock.now();
        let endpoints = self.endpoints.read().expect("breaker lock");
        match endpoints.get(ep) {
            None => BreakerState::Closed,
            Some(cells) => match kind {
                BreakerKind::Transport => cells.transport.state_at(now, BreakerKind::Transport),
                BreakerKind::Probe => cells.probe.state_at(now, BreakerKind::Probe),
                _ => BreakerState::Closed,
            },
        }
    }

    /// Merged view: `Open` with the later `until` if either breaker is
    /// open; else `HalfOpen` if either is half-open; else `Closed`.
    fn merged_state(&self, ep: &EndpointId) -> BreakerState {
        let transport = self.kind_state(ep, BreakerKind::Transport);
        let probe = self.kind_state(ep, BreakerKind::Probe);
        match (transport, probe) {
            (
                BreakerState::Open {
                    until: a, kind: ka, ..
                },
                BreakerState::Open { until: b, kind: kb },
            ) => {
                if a >= b {
                    BreakerState::Open { until: a, kind: ka }
                } else {
                    BreakerState::Open { until: b, kind: kb }
                }
            }
            (open @ BreakerState::Open { .. }, _) => open,
            (_, open @ BreakerState::Open { .. }) => open,
            (BreakerState::HalfOpen, _) | (_, BreakerState::HalfOpen) => BreakerState::HalfOpen,
            _ => BreakerState::Closed,
        }
    }

    /// Deterministic snapshot for reporting: sorted by (endpoint, kind).
    pub fn snapshot(&self) -> Vec<(EndpointId, BreakerKind, BreakerState)> {
        let now = self.clock.now();
        let endpoints = self.endpoints.read().expect("breaker lock");
        let mut out: Vec<(EndpointId, BreakerKind, BreakerState)> = Vec::new();
        for (ep, cells) in endpoints.iter() {
            out.push((
                ep.clone(),
                BreakerKind::Transport,
                cells.transport.state_at(now, BreakerKind::Transport),
            ));
            out.push((
                ep.clone(),
                BreakerKind::Probe,
                cells.probe.state_at(now, BreakerKind::Probe),
            ));
        }
        out.sort_by(|a, b| (&a.0, breaker_kind_rank(a.1)).cmp(&(&b.0, breaker_kind_rank(b.1))));
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

fn breaker_kind_rank(kind: BreakerKind) -> u8 {
    match kind {
        BreakerKind::Transport => 0,
        BreakerKind::Probe => 1,
        _ => 2,
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
                cells
                    .probe
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
            Observation::ProbeFail => {
                cells
                    .probe
                    .record_failure(now, self.config.probe_failures_to_open, open_for);
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
    fn breakers_are_fully_independent() {
        let (registry, _) = fresh_registry();
        let endpoint = ep("local");
        for _ in 0..10 {
            registry.record(&endpoint, Observation::ProbeFail);
        }
        assert_eq!(
            registry.kind_state(&endpoint, BreakerKind::Transport),
            BreakerState::Closed,
            "probe failures never touch the transport breaker"
        );
        let (registry2, _clock2) = fresh_registry();
        for _ in 0..10 {
            registry2.record(&endpoint, Observation::TransportError);
        }
        assert_eq!(
            registry2.kind_state(&endpoint, BreakerKind::Probe),
            BreakerState::Closed,
            "transport failures never touch the probe breaker"
        );
    }

    #[test]
    fn ok_resets_both_counters() {
        let (registry, _) = fresh_registry();
        let endpoint = ep("local");
        registry.record(&endpoint, Observation::TransportError);
        registry.record(&endpoint, Observation::TransportError);
        registry.record(&endpoint, Observation::ProbeFail);
        registry.record(&endpoint, Observation::Ok { latency_ms: 12 });
        // Counters reset: two more transport failures stay Closed.
        registry.record(&endpoint, Observation::TransportError);
        registry.record(&endpoint, Observation::TransportError);
        assert_eq!(
            registry.kind_state(&endpoint, BreakerKind::Transport),
            BreakerState::Closed
        );
        assert_eq!(registry.metrics(), vec![(endpoint, Some(12))]);
    }

    #[test]
    fn rate_limited_force_opens_without_counter_and_probe_untouched() {
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
        assert_eq!(
            registry.kind_state(&endpoint, BreakerKind::Probe),
            BreakerState::Closed
        );
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

    #[test]
    fn merged_state_prefers_later_until() {
        let (registry, _) = fresh_registry();
        let endpoint = ep("local");
        for _ in 0..3 {
            registry.record(&endpoint, Observation::TransportError);
        }
        for _ in 0..3 {
            registry.record(&endpoint, Observation::ProbeFail);
        }
        // Both opened at the same now with the same duration; force the
        // transport breaker later via RateLimited.
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
        registry.record(&ep("a"), Observation::ProbeFail);
        let snapshot = registry.snapshot();
        let keys: Vec<(String, BreakerKind)> = snapshot
            .iter()
            .map(|(e, k, _)| (e.to_string(), *k))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("a".to_string(), BreakerKind::Transport),
                ("a".to_string(), BreakerKind::Probe),
                ("b".to_string(), BreakerKind::Transport),
                ("b".to_string(), BreakerKind::Probe),
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
