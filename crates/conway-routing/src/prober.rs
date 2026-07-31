//! `HealthProber` (WI-035): a periodic, per-endpoint [`Backend::probe`] loop
//! that keeps the Probe breaker fed with liveness data independently of
//! request traffic, so a dead local server is distinguishable from a slow
//! one before a request is ever routed to it.
//!
//! This module only *writes* health state (`HealthRegistry::record`); it
//! never reads it (`HealthRegistry::state` is not called anywhere here) --
//! routing policy is `router.rs`'s concern, not this loop's.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use futures::future::{self, FutureExt};
use tokio::sync::watch;
use tokio::task::{JoinError, JoinSet};
use tokio::time::{Instant, MissedTickBehavior};

use conway_core::error::BackendError;
use conway_core::ids::{BackendId, EndpointId};
use conway_core::ports::{Backend, HealthRegistry};
use conway_core::routing::{HealthConfig, Observation};

use crate::failure::{self, FailureClass};

/// Handle to a spawned [`HealthProber`] task.
///
/// Dropping the handle *without* calling [`shutdown`](ProberHandle::shutdown)
/// does **not** abort the task: the loop is simply detached and keeps
/// probing on its own schedule. Call `shutdown` explicitly to stop it.
pub struct ProberHandle {
    cancel: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl ProberHandle {
    /// Signal the loop to stop after its current round. Idempotent (safe to
    /// call more than once) and non-blocking (only flips a flag).
    pub fn shutdown(&self) {
        let _ = self.cancel.send(true);
    }

    /// Wait for the loop to actually stop.
    pub async fn join(self) -> Result<(), JoinError> {
        self.task.await
    }
}

/// Owns the periodic probe loop. See the module docs above (WI-035) for
/// the binding spec.
///
/// Signature deviation from the module spec's
/// `HealthProber::spawn(Vec<Arc<dyn Backend>>, HealthConfig)`: recording an
/// observation requires somewhere to record it, so a `health: Arc<dyn
/// HealthRegistry>` parameter is added. Flagged in the module spec's own
/// assumption list rather than worked around silently.
pub struct HealthProber;

impl HealthProber {
    pub fn spawn(
        backends: Vec<Arc<dyn Backend>>,
        health: Arc<dyn HealthRegistry>,
        config: HealthConfig,
    ) -> ProberHandle {
        let (cancel_tx, cancel_rx) = watch::channel(false);

        if !config.probe_enabled {
            // No observations, ever: the task exits on its first poll.
            let task = tokio::spawn(async {});
            return ProberHandle {
                cancel: cancel_tx,
                task,
            };
        }

        let probe_interval = Duration::from_secs(config.probe_interval_secs);
        let probe_timeout = Duration::from_secs(config.probe_timeout_secs);
        let task = tokio::spawn(run_loop(
            backends,
            health,
            probe_interval,
            probe_timeout,
            cancel_rx,
        ));

        ProberHandle {
            cancel: cancel_tx,
            task,
        }
    }
}

async fn run_loop(
    backends: Vec<Arc<dyn Backend>>,
    health: Arc<dyn HealthRegistry>,
    probe_interval: Duration,
    probe_timeout: Duration,
    cancel_rx: watch::Receiver<bool>,
) {
    // `tokio::time::interval`'s first tick fires immediately -- intentional,
    // so startup health is known before the first turn (WI-035 notes). No
    // jitter: determinism under `tokio::time::pause` outweighs
    // thundering-herd avoidance at this scale.
    let mut ticker = tokio::time::interval(probe_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    // `cancel` becomes `None` once the sending `ProberHandle` is dropped
    // without calling `shutdown`: the watch channel closes, and from then
    // on this loop only answers to the ticker (a `Some` branch that keeps
    // resolving `Err` would busy-loop the `select!` instead of sleeping).
    let mut cancel: Option<watch::Receiver<bool>> = Some(cancel_rx);

    loop {
        let cancelled = match &mut cancel {
            Some(rx) => rx.changed().left_future(),
            None => future::pending().right_future(),
        };
        tokio::select! {
            result = cancelled => {
                match result {
                    Ok(()) if *cancel.as_ref().unwrap().borrow() => break,
                    Ok(()) => {}
                    Err(_) => cancel = None,
                }
            }
            _ = ticker.tick() => {
                probe_round(&backends, &health, probe_timeout).await;
            }
        }
    }
}

/// One round: every backend probed concurrently, each wrapped in a timeout
/// and a panic guard so one bad backend never affects another or the loop
/// itself. Outcome mapping is exhaustive and total (WI-035 notes table),
/// except that a probe `Err` no longer always yields an observation — see
/// [`probe_error_observation`] (WI-124).
async fn probe_round(
    backends: &[Arc<dyn Backend>],
    health: &Arc<dyn HealthRegistry>,
    timeout: Duration,
) {
    let mut set = JoinSet::new();
    for backend in backends {
        let backend = Arc::clone(backend);
        let health = Arc::clone(health);
        set.spawn(async move {
            let endpoint = endpoint_of(&backend.id());
            let started = Instant::now();
            let outcome = AssertUnwindSafe(tokio::time::timeout(timeout, backend.probe()))
                .catch_unwind()
                .await;
            let observation = match outcome {
                Ok(Ok(Ok(_report))) => Some(Observation::Ok {
                    latency_ms: started.elapsed().as_millis() as u32,
                }),
                Ok(Ok(Err(err))) => probe_error_observation(&err),
                Ok(Err(_elapsed)) => Some(Observation::ProbeFail),
                Err(_panic) => Some(Observation::ProbeFail),
            };
            if let Some(observation) = observation {
                health.record(&endpoint, observation);
            }
        });
    }
    while set.join_next().await.is_some() {}
}

/// Maps a probe's `BackendError` to a health observation, reusing this
/// crate's `failure::classify` table so probe and request-path health
/// signals never diverge on what counts as an endpoint problem versus a
/// request problem (WI-124). Only `FailureClass::FailoverRetryable`
/// (`Transport`/`ServerError`/`RateLimit`) trips the Probe breaker via
/// `Observation::ProbeFail`; `RequestIncompatible` (e.g. a `BadRequest` from
/// a 404 on a liveness path this dialect doesn't serve) and `Fatal` errors
/// yield no observation at all, leaving breaker state untouched — "this
/// path isn't served here" must not be counted the same as "the server is
/// down".
fn probe_error_observation(err: &BackendError) -> Option<Observation> {
    match failure::classify(err) {
        FailureClass::FailoverRetryable => Some(Observation::ProbeFail),
        FailureClass::RequestIncompatible | FailureClass::Fatal => None,
    }
}

/// `EndpointId` per backend: mirrors `router::endpoint_of`'s `ModelRef.backend
/// -> EndpointId` mapping (endpoint identity is 1:1 with backend identity for
/// MVP), applied directly to `Backend::id()` since the prober probes per
/// endpoint, not per model.
fn endpoint_of(id: &BackendId) -> EndpointId {
    EndpointId::new(id.as_str())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use async_trait::async_trait;
    use chrono::Utc;
    use conway_core::capabilities::{
        CacheMode, Capabilities, ProbeReport, ReliabilityTier, StructuredOutput, ToolCallSupport,
    };
    use conway_core::error::BackendError;
    use conway_core::ids::ModelId;
    use conway_core::ports::{BoxStream, GenerateRequest, GenerateResponse, StreamChunk};
    use conway_core::routing::BreakerState;

    use super::*;

    fn test_capabilities() -> Capabilities {
        Capabilities {
            tool_calling: ToolCallSupport::None,
            cache: CacheMode::None,
            parallel_tool_calls: false,
            structured_output: StructuredOutput::None,
            max_context_tokens: 128_000,
            reasoning: false,
            reliability_tier: ReliabilityTier::Community,
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ProbeScript {
        Ok,
        Err,
        /// A request-incompatible probe failure (e.g. `BadRequest` from a
        /// 404 on a liveness path the dialect doesn't serve) — WI-124:
        /// distinct from `Err`'s `Transport` failure, which is a genuine
        /// endpoint-health signal.
        UnsupportedPath,
        Hang,
        Panic,
    }

    /// Scripted-outcome fake backend whose `probe` calls are counted.
    /// `generate`/`stream` are never exercised by these tests.
    struct CountingProbeBackend {
        id: BackendId,
        calls: AtomicUsize,
        script: Mutex<VecDeque<ProbeScript>>,
        hang_for: Duration,
    }

    impl CountingProbeBackend {
        fn new(id: &str, script: Vec<ProbeScript>) -> Arc<Self> {
            Arc::new(Self {
                id: BackendId::new(id),
                calls: AtomicUsize::new(0),
                script: Mutex::new(script.into()),
                hang_for: Duration::from_secs(3600),
            })
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Backend for CountingProbeBackend {
        fn id(&self) -> BackendId {
            self.id.clone()
        }

        fn capabilities(&self, _model: &ModelId) -> Capabilities {
            test_capabilities()
        }

        async fn generate(&self, _req: GenerateRequest) -> Result<GenerateResponse, BackendError> {
            Err(BackendError::Transport {
                detail: "unsupported by prober test double".into(),
            })
        }

        async fn stream(
            &self,
            _req: GenerateRequest,
        ) -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError> {
            Err(BackendError::Transport {
                detail: "unsupported by prober test double".into(),
            })
        }

        async fn probe(&self) -> Result<ProbeReport, BackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let next = self
                .script
                .lock()
                .expect("script lock")
                .pop_front()
                .unwrap_or(ProbeScript::Ok);
            match next {
                ProbeScript::Ok => Ok(ProbeReport {
                    ok: true,
                    latency_ms: 1,
                    models: vec![],
                    detail: None,
                    at: Utc::now(),
                }),
                ProbeScript::Err => Err(BackendError::Transport {
                    detail: "scripted probe failure".into(),
                }),
                ProbeScript::UnsupportedPath => Err(BackendError::BadRequest {
                    detail: "scripted unsupported liveness path".into(),
                }),
                ProbeScript::Hang => {
                    tokio::time::sleep(self.hang_for).await;
                    unreachable!("a hung probe must be abandoned by its timeout first")
                }
                ProbeScript::Panic => panic!("scripted probe panic"),
            }
        }
    }

    #[derive(Default)]
    struct RecordingRegistry {
        calls: Mutex<Vec<(EndpointId, Observation)>>,
    }

    impl RecordingRegistry {
        fn observations(&self) -> Vec<(EndpointId, Observation)> {
            self.calls.lock().expect("calls lock").clone()
        }

        fn count(&self) -> usize {
            self.calls.lock().expect("calls lock").len()
        }
    }

    impl HealthRegistry for RecordingRegistry {
        fn state(&self, _ep: &EndpointId) -> BreakerState {
            // Never called by the prober; a fixed answer is enough to
            // satisfy the trait.
            BreakerState::Closed
        }

        fn record(&self, ep: &EndpointId, obs: Observation) {
            self.calls
                .lock()
                .expect("calls lock")
                .push((ep.clone(), obs));
        }
    }

    fn config(interval_secs: u64, timeout_secs: u64) -> HealthConfig {
        HealthConfig {
            probe_interval_secs: interval_secs,
            probe_timeout_secs: timeout_secs,
            probe_enabled: true,
            ..HealthConfig::default()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn disabled_prober_records_nothing_and_exits_immediately() {
        let backend = CountingProbeBackend::new("b", vec![]);
        let health = Arc::new(RecordingRegistry::default());
        let cfg = HealthConfig {
            probe_enabled: false,
            ..HealthConfig::default()
        };
        let handle = HealthProber::spawn(vec![backend.clone() as _], health.clone() as _, cfg);

        handle.join().await.expect("task exits cleanly");

        tokio::time::advance(Duration::from_secs(3600)).await;
        assert_eq!(backend.call_count(), 0);
        assert_eq!(health.count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn ok_probe_records_ok_observation_with_latency() {
        let backend = CountingProbeBackend::new("b", vec![ProbeScript::Ok]);
        let health = Arc::new(RecordingRegistry::default());
        let handle = HealthProber::spawn(
            vec![backend.clone() as _],
            health.clone() as _,
            config(15, 2),
        );

        // First tick fires immediately.
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;

        assert_eq!(backend.call_count(), 1);
        let observations = health.observations();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].0, EndpointId::new("b"));
        assert!(matches!(observations[0].1, Observation::Ok { .. }));

        handle.shutdown();
        handle.join().await.expect("task exits cleanly");
    }

    #[tokio::test(start_paused = true)]
    async fn err_probe_records_probe_fail() {
        let backend = CountingProbeBackend::new("b", vec![ProbeScript::Err]);
        let health = Arc::new(RecordingRegistry::default());
        let handle = HealthProber::spawn(
            vec![backend.clone() as _],
            health.clone() as _,
            config(15, 2),
        );

        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;

        assert_eq!(
            health.observations(),
            vec![(EndpointId::new("b"), Observation::ProbeFail)]
        );

        handle.shutdown();
        handle.join().await.expect("task exits cleanly");
    }

    /// WI-124 criterion 2: an unsupported liveness path (classified as
    /// `BackendError::BadRequest`, e.g. a 404 on a path this dialect doesn't
    /// serve) must not be counted as a health failure — no observation at
    /// all is recorded, and in particular the Probe breaker does not open.
    #[tokio::test(start_paused = true)]
    async fn unsupported_liveness_path_records_no_observation() {
        let backend = CountingProbeBackend::new("b", vec![ProbeScript::UnsupportedPath]);
        let health = Arc::new(RecordingRegistry::default());
        let handle = HealthProber::spawn(
            vec![backend.clone() as _],
            health.clone() as _,
            config(15, 2),
        );

        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;

        assert_eq!(backend.call_count(), 1, "the probe still ran");
        assert_eq!(
            health.observations(),
            vec![],
            "an unsupported-path failure must not feed the breaker"
        );

        handle.shutdown();
        handle.join().await.expect("task exits cleanly");
    }

    /// A round that yields no observation must not disturb later rounds:
    /// the loop keeps ticking and a subsequent genuine failure still trips
    /// the Probe breaker normally.
    #[tokio::test(start_paused = true)]
    async fn unsupported_liveness_path_does_not_block_later_probe_fail_observations() {
        let backend =
            CountingProbeBackend::new("b", vec![ProbeScript::UnsupportedPath, ProbeScript::Err]);
        let health = Arc::new(RecordingRegistry::default());
        let handle = HealthProber::spawn(
            vec![backend.clone() as _],
            health.clone() as _,
            config(15, 2),
        );

        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(health.observations(), vec![]);

        tokio::time::advance(Duration::from_secs(15)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            health.observations(),
            vec![(EndpointId::new("b"), Observation::ProbeFail)],
            "a later genuine failure still records ProbeFail"
        );

        handle.shutdown();
        handle.join().await.expect("task exits cleanly");
    }

    #[tokio::test(start_paused = true)]
    async fn hung_probe_is_abandoned_and_records_probe_fail_exactly_once() {
        let backend = CountingProbeBackend::new("b", vec![ProbeScript::Hang]);
        let health = Arc::new(RecordingRegistry::default());
        // A long interval keeps the next tick well outside this test's
        // window, isolating "does the abandoned probe resolve late" from
        // "did the next scheduled tick fire".
        let handle = HealthProber::spawn(
            vec![backend.clone() as _],
            health.clone() as _,
            config(10_000, 2),
        );

        // Let the first tick start the hanging probe, then let it time out.
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::task::yield_now().await;

        assert_eq!(
            health.observations(),
            vec![(EndpointId::new("b"), Observation::ProbeFail)]
        );

        // The abandoned sleep (scripted to hang for 3600s) never
        // independently completes into a second (Ok) observation once its
        // enclosing timeout has already fired and dropped it.
        tokio::time::advance(Duration::from_secs(3700)).await;
        assert_eq!(
            health.count(),
            1,
            "the abandoned probe must not record a second, later observation"
        );

        handle.shutdown();
        handle.join().await.expect("task exits cleanly");
    }

    #[tokio::test(start_paused = true)]
    async fn backends_are_probed_concurrently_within_a_tick() {
        // N backends that each hang for exactly the timeout: if probed
        // sequentially, draining them would take N * timeout; concurrently,
        // one `timeout` advance is enough for all of them to be abandoned.
        let n = 6;
        let backends: Vec<_> = (0..n)
            .map(|i| CountingProbeBackend::new(&format!("b{i}"), vec![ProbeScript::Hang]))
            .collect();
        let health = Arc::new(RecordingRegistry::default());
        let handle = HealthProber::spawn(
            backends.iter().map(|b| b.clone() as _).collect(),
            health.clone() as _,
            config(1000, 2),
        );

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::task::yield_now().await;

        assert_eq!(
            health.count(),
            n,
            "one probe_timeout advance abandons every backend, not just one"
        );

        handle.shutdown();
        handle.join().await.expect("task exits cleanly");
    }

    #[tokio::test(start_paused = true)]
    async fn panicking_probe_is_caught_and_subsequent_ticks_still_run() {
        let backend = CountingProbeBackend::new("b", vec![ProbeScript::Panic, ProbeScript::Ok]);
        let health = Arc::new(RecordingRegistry::default());
        let handle = HealthProber::spawn(
            vec![backend.clone() as _],
            health.clone() as _,
            config(15, 2),
        );

        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            health.observations(),
            vec![(EndpointId::new("b"), Observation::ProbeFail)]
        );

        tokio::time::advance(Duration::from_secs(15)).await;
        tokio::task::yield_now().await;
        let observations = health.observations();
        assert_eq!(
            observations.len(),
            2,
            "the panic did not kill the prober loop"
        );
        assert!(matches!(observations[1].1, Observation::Ok { .. }));
        assert_eq!(backend.call_count(), 2);

        handle.shutdown();
        handle.join().await.expect("task exits cleanly");
    }

    #[tokio::test(start_paused = true)]
    async fn probes_once_per_interval_with_no_drift() {
        let backend = CountingProbeBackend::new(
            "b",
            vec![
                ProbeScript::Ok,
                ProbeScript::Ok,
                ProbeScript::Ok,
                ProbeScript::Ok,
                ProbeScript::Ok,
            ],
        );
        let health = Arc::new(RecordingRegistry::default());
        let handle = HealthProber::spawn(
            vec![backend.clone() as _],
            health.clone() as _,
            config(15, 2),
        );

        // Immediate first tick.
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(backend.call_count(), 1);

        for expected in 2..=5 {
            tokio::time::advance(Duration::from_secs(15)).await;
            tokio::task::yield_now().await;
            assert_eq!(
                backend.call_count(),
                expected,
                "no drift-induced extra calls"
            );
        }

        handle.shutdown();
        handle.join().await.expect("task exits cleanly");
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_stops_further_observations() {
        let backend =
            CountingProbeBackend::new("b", std::iter::repeat_n(ProbeScript::Ok, 20).collect());
        let health = Arc::new(RecordingRegistry::default());
        let handle = HealthProber::spawn(
            vec![backend.clone() as _],
            health.clone() as _,
            config(15, 2),
        );

        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(health.count(), 1);

        handle.shutdown();
        handle.join().await.expect("task exits cleanly");

        tokio::time::advance(Duration::from_secs(15 * 10)).await;
        assert_eq!(
            health.count(),
            1,
            "no observations after shutdown, even across 10 more intervals"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_the_handle_without_shutdown_does_not_abort_the_task() {
        let backend =
            CountingProbeBackend::new("b", std::iter::repeat_n(ProbeScript::Ok, 20).collect());
        let health = Arc::new(RecordingRegistry::default());
        let handle = HealthProber::spawn(
            vec![backend.clone() as _],
            health.clone() as _,
            config(15, 2),
        );

        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(health.count(), 1);

        drop(handle);

        tokio::time::advance(Duration::from_secs(15)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            health.count(),
            2,
            "the loop keeps running after the handle is dropped"
        );
    }
}
