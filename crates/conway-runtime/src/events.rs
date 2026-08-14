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
//! gaps and no repeats, for as long as that id's counter is live in THIS
//! process. That is the whole of the guarantee: `seqs` is an in-memory
//! `HashMap` that nothing reseeds, so it was never actually gap-free or
//! non-repeating ACROSS a resume in a fresh process even before this
//! module's reclamation existed -- no `(session, seq)` dedup set, no
//! persisted cursor, and no consumer anywhere in this workspace reads
//! `Envelope::seq` for anything other than display (a read-only sweep
//! establishing this is this module's own reclamation item's record). An
//! earlier revision of this doc claimed no-repeats held "including across a
//! `resume_root`" and used that claim as the reason `seqs` could not be
//! pruned; that claim was aspirational, not delivered, and is retracted
//! here rather than repeated.
//!
//! ## Reclamation (`EventBus.seqs` still leaks for spawned and
//! forked agents)
//!
//! Two cases are safe to reclaim -- neither weakens the guarantee above,
//! since nothing depends on a pruned counter's identity surviving past its
//! own agent's finish:
//! - A session that finishes still flagged
//!   [`Event::AgentFinished::ephemeral`] (i.e. an `/ask`-style child --
//!   modal or tool-invoked, `conway_core::log::AskOrigin` -- that ran to
//!   completion without ever being promoted). `SessionMeta::ephemeral`'s
//!   own contract (`conway-core`) frames such a session as a disposable
//!   scratchpad meant to be discarded, and the one sanctioned way to keep
//!   one alive past its finish -- `Runtime::promote_agent` -- always flips
//!   the flag to `false` in the live tree strictly before any subsequent
//!   finish can observe it, so an `AgentFinished { ephemeral: true, .. }`
//!   can only ever be seen for a child that was NEVER promoted. `emit`
//!   reclaims that session's counter, in place, the moment it observes
//!   that event.
//! - A spawn/fork child (`AgentTree::is_prunable_on_finish`, `tree.rs`)
//!   that was never ephemeral in the first place -- an ordinary
//!   `conway_fork`/`conway_spawn` -- reclaimed by its caller passing
//!   `prune: true` to [`EventBus::emit_pruning`] at the same emission site.
//!   A PROMOTED child is deliberately excluded from this case (it stays
//!   governed by the first case above, i.e. not reclaimed): see
//!   `is_prunable_on_finish`'s own doc for why the promotion "keep" fate
//!   means it should behave like any other lastingly-referenced session,
//!   not a disposable one.
//!
//! A ROOT session's counter is never reclaimed -- one entry per process is
//! not a leak, and is out of this item's scope. See `emit_pruning`'s own
//! doc for why this adds no new contention.

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
    /// Construct a bus with [`DEFAULT_CAPACITY`]. `Runtime::new`
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
    /// return the assigned `seq`. Equivalent to
    /// `self.emit_pruning(session, agent, event, false)` -- see that
    /// method's doc for the full contract, including the ephemeral-finish
    /// reclamation this method still performs unconditionally.
    pub fn emit(&self, session: SessionId, agent: AgentId, event: Event) -> u64 {
        self.emit_pruning(session, agent, event, false)
    }

    /// [`Self::emit`], with one addition: if `prune` is `true`, `session`'s
    /// `seqs` entry is reclaimed regardless of `event`'s own `ephemeral`
    /// flag (or lack of one).
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
    /// Reclamation (see this module's doc): `session`'s entry in `seqs` is
    /// removed, before the lock is released, whenever EITHER `event` is
    /// itself an `AgentFinished { ephemeral: true, .. }` OR the caller
    /// passes `prune: true` (the latter is how `AgentLoop::finish` and
    /// `supervisor.rs`'s synthesized finish reclaim an ordinary,
    /// never-ephemeral spawn/fork child's counter --
    /// `AgentTree::is_prunable_on_finish` computes that bool for them,
    /// BEFORE this call, outside any lock this method holds). Either way
    /// this is a single `O(1)` `HashMap::remove` on the exact same map the
    /// `entry`/`insert` above already touched under this same,
    /// already-held lock -- it neither adds a new critical section nor
    /// changes this method's existing "never awaits, never blocks"
    /// contract. Any later `emit`/`emit_pruning` for that same `session`
    /// (which, per the module doc, should not happen for a reclaimed
    /// session) would simply re-seed its counter at 0 via the `entry` call
    /// above rather than panic -- a benign, typed degradation, not a
    /// new failure mode.
    pub fn emit_pruning(
        &self,
        session: SessionId,
        agent: AgentId,
        event: Event,
        prune: bool,
    ) -> u64 {
        let mut seqs = self.seqs.lock().expect("event bus seq mutex poisoned");
        let counter = seqs.entry(session).or_insert(0);
        let seq = *counter;
        *counter += 1;
        let ephemeral_finish = matches!(
            &event,
            Event::AgentFinished {
                ephemeral: true,
                ..
            }
        );
        if ephemeral_finish || prune {
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

    /// Renamed and narrowed from this test's pre-item name
    /// (`non_ephemeral_agent_finished_keeps_its_session_counter`): a
    /// non-ephemeral session's counter is NO LONGER universally kept after
    /// this item -- an ordinary spawn/fork child's is now reclaimed too (see
    /// `spawn_or_fork_child_finished_reclaims_its_session_counter_via_prune`
    /// below). What survives is specifically the case where nothing ever
    /// computes a `true` prune signal for this session -- a root's own
    /// finish always calls plain `emit` (equivalent to
    /// `emit_pruning(.., false)`), since `AgentTree::is_prunable_on_finish`
    /// is `false` for a root (`kind.is_none()`) by construction.
    #[tokio::test]
    async fn root_agent_finished_keeps_its_session_counter_when_not_pruned() {
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
            "a session finished via plain `emit` (a root's own path) must keep its counter"
        );

        // Further emits under the same session stay monotonic -- nothing
        // pruned it, so there is nothing to restart from 0.
        let seq = bus.emit(
            session,
            agent,
            Event::AgentProgress {
                note: "later".into(),
            },
        );
        assert_eq!(
            seq, 4,
            "seq must stay monotonic when its session was never pruned"
        );
    }

    /// Acceptance: a spawn/fork child that was never ephemeral (an ordinary
    /// `conway_fork`/`conway_spawn` -- see `AgentTree::is_prunable_on_finish`,
    /// `tree.rs`) has its `seqs` entry reclaimed too, via `emit_pruning`'s
    /// explicit `prune` flag -- the item's own motivating case (
    /// long-lived embedder driving many such children, with no way to
    /// reclaim this before this item).
    #[tokio::test]
    async fn spawn_or_fork_child_finished_reclaims_its_session_counter_via_prune() {
        let bus = EventBus::new(64);
        let session = SessionId::new();
        let agent = AgentId::new();

        for _ in 0..3 {
            bus.emit(session, agent, Event::AgentProgress { note: "x".into() });
        }
        assert!(
            bus.seqs.lock().unwrap().contains_key(&session),
            "counter should exist while the child is live"
        );

        bus.emit_pruning(session, agent, finished(session, agent, false), true);

        assert!(
            !bus.seqs.lock().unwrap().contains_key(&session),
            "a spawn/fork child's counter must be reclaimed when its caller passes prune: true"
        );
    }

    /// An ephemeral session promoted to persistent (`Runtime::promote_agent`
    /// flips the tree's flag to `false` before any subsequent finish can
    /// observe it) reaches `AgentFinished` with `ephemeral: false`. Unlike an
    /// ordinary spawn/fork child (see the prune test above), a promoted
    /// child's caller computes `AgentTree::is_prunable_on_finish == false`
    /// for it (that method's own doc explains why: the promotion "keep"
    /// fate excludes it), so it is emitted via plain `emit`
    /// (`prune: false`) and survives -- this test exercises exactly that
    /// call shape, not `emit_pruning(.., true)`.
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
