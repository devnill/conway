//! The event bus: the runtime's single fan-out point for the flat,
//! `agent`-tagged event stream (architecture §6.5, §8).
//!
//! One [`EventBus`] is shared by an entire agent tree — every session,
//! every agent. `seq` is monotonic per [`SessionId`], not per bus, so the
//! per-session counters live behind their own mutex, which is released
//! before the broadcast send. `emit` never awaits and never blocks: a
//! subscriber that falls behind the broadcast buffer sees a synthesized
//! [`Event::Lagged`] on its next poll rather than stalling any producer.

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
    pub fn emit(&self, session: SessionId, agent: AgentId, event: Event) -> u64 {
        let mut seqs = self.seqs.lock().expect("event bus seq mutex poisoned");
        let counter = seqs.entry(session).or_insert(0);
        let seq = *counter;
        *counter += 1;
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
}
