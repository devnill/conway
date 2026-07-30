//! The event bus: the runtime's single fan-out point for the flat,
//! `agent`-tagged event stream (architecture §6.5, §8).
//!
//! One [`EventBus`] is shared by an entire agent tree — every session,
//! every agent. `seq` is monotonic per [`SessionId`], not per bus, so the
//! per-session counters live behind their own mutex, which is released
//! before the broadcast send. `emit` never awaits and never blocks: a
//! subscriber that falls behind the broadcast buffer sees a synthesized
//! [`Event::Lagged`] on its next poll rather than stalling any producer.
//!
//! ## What `seq` guarantees (restated from [`Envelope`]'s own doc)
//!
//! `seq` is strictly increasing per [`SessionId`], starting at 0, with no
//! gaps and no repeats, for as long as that id is live -- including across
//! a [`Runtime::resume_root`](crate::runtime::Runtime::resume_root), which
//! reuses the SAME `SessionId` (and `AgentId`) rather than minting fresh
//! ones. That is why `seqs` cannot simply be pruned whenever an agent
//! finishes: a finished, non-ephemeral session is exactly the case
//! `resume_root` exists to reactivate, and a pruned counter would restart
//! at 0 on that reactivation, colliding with `seq`s a subscriber already
//! saw before the resume -- silently breaking the guarantee this module
//! exists to uphold.
//!
//! ## Reclamation (WI item: `EventBus.seqs` grows unboundedly)
//!
//! One case IS safe to reclaim without weakening the guarantee above: a
//! session that finishes still flagged [`Event::AgentFinished::ephemeral`]
//! (i.e. an `/ask`-style child -- modal or tool-invoked, `conway_core::
//! log::AskOrigin` -- that ran to completion without ever being promoted).
//! `SessionMeta::ephemeral`'s own contract (`conway-core`) frames such a
//! session as a disposable scratchpad meant to be discarded, and the one
//! sanctioned way to keep one alive past its finish --
//! `Runtime::promote_agent` -- always flips the flag to `false` in the
//! live tree strictly before any subsequent finish can observe it, so an
//! `AgentFinished { ephemeral: true, .. }` can only ever be seen for a
//! child that was NEVER promoted. Nothing else in this codebase resumes
//! an ephemeral child (a `conway_ask` caller's kept `EphemeralSessionRef`
//! artifact is read straight off the session store, never through a live
//! resume) -- so `emit` reclaims that session's counter, in place, the
//! moment it observes that event. See `emit`'s own doc for why this adds
//! no new contention.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use conway_core::event::{Envelope, Event};
use conway_core::ids::{AgentId, SessionId};
use conway_core::ports::EventSink;
use futures::{Stream, StreamExt};
use tokio::sync::broadcast;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;

/// A boxed, owned stream of [`Envelope`]s — what [`EventBus::subscribe`]
/// returns.
pub type EventStream = Pin<Box<dyn Stream<Item = Envelope> + Send>>;

/// Broadcast buffer capacity used by callers with no more specific reason
/// to pick their own (architecture §8: broadcast, lossy-with-notice for
/// slow consumers).
pub const DEFAULT_CAPACITY: usize = 1024;

/// The runtime's single fan-out point for the event stream.
pub struct EventBus {
    tx: broadcast::Sender<Envelope>,
    seqs: Mutex<HashMap<SessionId, u64>>,
}

impl EventBus {
    /// Construct a bus with [`DEFAULT_CAPACITY`]. `Runtime::new` (WI-082)
    /// uses this unless configured otherwise.
    pub fn with_default_capacity() -> Arc<Self> {
        Self::new(DEFAULT_CAPACITY)
    }

    /// Construct a bus with the given broadcast buffer capacity.
    pub fn new(capacity: usize) -> Arc<Self> {
        let (tx, _rx) = broadcast::channel(capacity);
        Arc::new(Self {
            tx,
            seqs: Mutex::new(HashMap::new()),
        })
    }

    /// Assign the next `seq` for `session`, publish the envelope, and
    /// return the assigned `seq`.
    ///
    /// Per-session counters start at 0 and are independent across
    /// sessions. The counter mutex is deliberately held ACROSS the
    /// `tx.send` call: assigning `seq` and publishing must be one atomic
    /// step, or two racing emitters could deliver `seq = N + 1` to
    /// subscribers before `seq = N`, breaking the architecture §8
    /// "monotonic per session" delivery guarantee (cycle-1 incremental
    /// review, Critical). `broadcast::Sender::send` is synchronous and
    /// non-blocking (drop-oldest on a full buffer), so no await ever
    /// happens under the lock. `send`'s `Err` (no subscribers) is
    /// ignored: having no subscribers is normal.
    ///
    /// Reclamation (see this module's doc): once an ephemeral session's
    /// terminal `Event::AgentFinished` is observed, its entry in `seqs` is
    /// removed before the lock is released. This is a single `O(1)`
    /// `HashMap::remove` on the exact same map the `entry`/`insert` above
    /// already touched under this same, already-held lock -- it neither
    /// adds a new critical section nor changes this method's existing
    /// "never awaits, never blocks" contract. Any later `emit` for that
    /// same `session` (which, per the module doc, should not happen for a
    /// non-promoted ephemeral child) would simply re-seed its counter at
    /// 0 via the `entry` call above rather than panic -- a benign, typed
    /// degradation (P-10), not a new failure mode.
    pub fn emit(&self, session: SessionId, agent: AgentId, event: Event) -> u64 {
        let mut seqs = self.seqs.lock().expect("event bus seq mutex poisoned");
        let counter = seqs.entry(session).or_insert(0);
        let seq = *counter;
        *counter += 1;
        if let Event::AgentFinished { ephemeral: true, .. } = &event {
            seqs.remove(&session);
        }
        let envelope = Envelope {
            seq,
            ts: Utc::now(),
            session,
            agent,
            event,
        };
        let _ = self.tx.send(envelope);
        seq
    }

    /// Subscribe to every envelope emitted after this call.
    ///
    /// A lagging subscriber's missed range is reported as one synthesized
    /// envelope carrying [`Event::Lagged`]; its `seq` is the sentinel
    /// `u64::MAX` since the notice is out-of-band and belongs to no
    /// session's sequence.
    pub fn subscribe(&self) -> EventStream {
        let stream = BroadcastStream::new(self.tx.subscribe()).map(|item| match item {
            Ok(envelope) => envelope,
            Err(BroadcastStreamRecvError::Lagged(skipped)) => Envelope {
                seq: u64::MAX,
                ts: Utc::now(),
                session: SessionId::new(),
                agent: AgentId::new(),
                event: Event::Lagged { skipped },
            },
        });
        Box::pin(stream)
    }
}

/// Binds an [`EventBus`] to one `(session, agent)` pair so tools and other
/// components can implement [`EventSink`] — and thus report progress —
/// without ever seeing the bus itself.
pub struct BusSink {
    bus: Arc<EventBus>,
    session: SessionId,
    agent: AgentId,
}

impl BusSink {
    pub fn new(bus: Arc<EventBus>, session: SessionId, agent: AgentId) -> Self {
        Self {
            bus,
            session,
            agent,
        }
    }
}

impl EventSink for BusSink {
    fn emit(&self, event: Event) {
        self.bus.emit(self.session, self.agent, event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway_core::agent::{AgentResult, ResultStatus};
    use conway_core::log::SubagentMode;

    fn finished(session: SessionId, agent: AgentId, ephemeral: bool) -> Event {
        Event::AgentFinished {
            result: AgentResult::new(agent, session, ResultStatus::Completed, "done"),
            ephemeral,
        }
    }

    #[tokio::test]
    async fn seq_starts_at_zero_and_increments_per_session() {
        let bus = EventBus::new(64);
        let session = SessionId::new();
        let agent = AgentId::new();
        for expected in 0..5u64 {
            let seq = bus.emit(session, agent, Event::AgentProgress { note: "x".into() });
            assert_eq!(seq, expected);
        }
    }

    #[tokio::test]
    async fn bus_sink_stamps_bound_session_and_agent() {
        let bus = EventBus::new(16);
        let mut stream = bus.subscribe();
        let session = SessionId::new();
        let agent = AgentId::new();
        let sink = BusSink::new(bus, session, agent);

        sink.emit(Event::AgentProgress { note: "hi".into() });

        let envelope = stream.next().await.expect("stream ended early");
        assert_eq!(envelope.session, session);
        assert_eq!(envelope.agent, agent);
        assert!(matches!(envelope.event, Event::AgentProgress { .. }));
    }

    /// Acceptance: an ephemeral session's terminal `AgentFinished` reclaims
    /// its `seqs` entry -- the ephemeral case the item requires proof of.
    #[tokio::test]
    async fn ephemeral_agent_finished_reclaims_its_session_counter() {
        let bus = EventBus::new(64);
        let session = SessionId::new();
        let agent = AgentId::new();

        for _ in 0..3 {
            bus.emit(session, agent, Event::AgentProgress { note: "x".into() });
        }
        assert!(
            bus.seqs.lock().unwrap().contains_key(&session),
            "counter should exist while the session is live"
        );

        bus.emit(session, agent, finished(session, agent, true));

        assert!(
            !bus.seqs.lock().unwrap().contains_key(&session),
            "an ephemeral session's counter must be reclaimed on AgentFinished"
        );
    }

    /// A NON-ephemeral session's counter must survive its own
    /// `AgentFinished`: `Runtime::resume_root` reuses the same `SessionId`,
    /// and reclaiming here would restart `seq` at 0 on resume, breaking the
    /// "monotonic per session" guarantee this module's doc restates from
    /// `Envelope`.
    #[tokio::test]
    async fn non_ephemeral_agent_finished_keeps_its_session_counter() {
        let bus = EventBus::new(64);
        let session = SessionId::new();
        let agent = AgentId::new();

        for expected in 0..3u64 {
            let seq = bus.emit(session, agent, Event::AgentProgress { note: "x".into() });
            assert_eq!(seq, expected);
        }

        let seq = bus.emit(session, agent, finished(session, agent, false));
        assert_eq!(seq, 3);
        assert!(
            bus.seqs.lock().unwrap().contains_key(&session),
            "a non-ephemeral session's counter must survive its own AgentFinished"
        );

        // Simulate `resume_root` reusing the same `(session, agent)` pair:
        // `seq` must continue from where it left off, not restart at 0.
        let seq = bus.emit(session, agent, Event::AgentProgress { note: "resumed".into() });
        assert_eq!(seq, 4, "seq must stay monotonic across a resume");
    }

    /// An ephemeral session promoted to persistent (`Runtime::promote_agent`
    /// flips the tree's flag to `false` before any subsequent finish can
    /// observe it) reaches `AgentFinished` with `ephemeral: false`, so it is
    /// governed by the non-ephemeral case above, not reclaimed.
    #[tokio::test]
    async fn promoted_then_finished_session_is_not_ephemeral_at_finish_and_survives() {
        let bus = EventBus::new(64);
        let session = SessionId::new();
        let agent = AgentId::new();

        bus.emit(
            session,
            agent,
            Event::AgentSpawned {
                kind: SubagentMode::Fork,
                parent: None,
                agent_def: None,
                inherited_upto: None,
                ephemeral: true,
            },
        );
        bus.emit(session, agent, Event::AgentPromoted {});
        // By the time a promoted session finishes, `AgentTree::ephemeral_of`
        // already reads `false` (the promote flipped it strictly before
        // this), so the caller stamps `AgentFinished { ephemeral: false }`.
        bus.emit(session, agent, finished(session, agent, false));

        assert!(
            bus.seqs.lock().unwrap().contains_key(&session),
            "a promoted session's counter must survive, exactly like any non-ephemeral one"
        );
    }
}
