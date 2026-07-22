//! `EventStream`: the facade-native, session-scoped event stream (WI-101).
//!
//! This wraps `conway_runtime::events::EventStream` -- the runtime's
//! already lag-normalized, boxed broadcast stream (`EventBus::subscribe`
//! folds a lagging subscriber's `BroadcastStreamRecvError::Lagged` into a
//! synthesized `Event::Lagged` envelope before this type ever sees it) --
//! with a session/agent filter and an optional replay prefix.
//!
//! This is a facade-owned type, not a re-export of the runtime's own
//! `EventStream` alias: per the binding spec (WI-096's note on
//! `lib.rs`), the facade defines its own state machine here rather than
//! `pub use conway_runtime::EventStream`. `crate::event_stream` never
//! writes `pub use conway_runtime::`, so WI-096's grep-based
//! at-most-one-runtime-reexport test is unaffected by this file.

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use chrono::{DateTime, Utc};
use conway_core::event::{Envelope, Event};
use conway_core::ids::{AgentId, SessionId};
use futures_core::Stream;

/// A stream of [`Envelope`]s, filtered to one session (and optionally one
/// agent within it), implementing [`futures_core::Stream`].
///
/// [`Event::Lagged`] envelopes are always forwarded regardless of the
/// session/agent filter: `EventBus::subscribe` stamps a lag notice with a
/// freshly generated, unrelated session/agent id (it belongs to no
/// session's sequence), so filtering it out by session/agent would
/// silently swallow the one signal a slow consumer needs to see.
pub struct EventStream {
    session: SessionId,
    agent: Option<AgentId>,
    replay: VecDeque<Envelope>,
    /// `Some` only for a stream built by [`EventStream::replay_then_live`];
    /// renumbers every yielded envelope's `seq` sequentially from 0. See
    /// that constructor's doc for why this is necessary, not merely
    /// stylistic.
    next_seq: Option<u64>,
    /// `Some` only for a stream built by [`EventStream::replay_then_live`];
    /// the junction-dedup state that drops a live envelope that is a
    /// content-duplicate of one already yielded from the replay batch. See
    /// [`EventStream::replay_then_live`]'s doc for why this exists and what
    /// it does and does not guarantee.
    dedup: Option<Dedup>,
    live: conway_runtime::events::EventStream,
}

/// Junction-dedup state for one [`EventStream::replay_then_live`] stream.
///
/// `pending` holds one `(Event, DateTime<Utc>)` per replay envelope whose
/// `Event` variant has a live twin ([`has_live_twin`]) and whose `ts` falls
/// at or after `subscribed_at` (the moment `SessionHandle::events_from`
/// started listening live -- anything persisted strictly before that could
/// not have also been broadcast to *this* subscription). The `ts` kept per
/// entry is the *replay record's own* timestamp, not any live envelope's --
/// see [`is_live_duplicate`] for why that matters.
///
/// **Match-first, not ts-gated (critical -- see [`is_live_duplicate`]'s
/// doc):** every live envelope is checked for a content match against
/// `pending` *before* anything about its own `ts` is consulted. In
/// production the live twin of a persisted record is always emitted with a
/// *later* `ts` than the record itself (`agent_loop.rs`'s `finish`/turn-loop
/// stamp `record.ts = Utc::now()` and persist, then emit the live event
/// under a separately-stamped, later `Utc::now()`) -- so gating the match on
/// the live envelope's `ts` being within some window of the *record's* `ts`
/// would systematically miss the very duplicates this exists to catch. A
/// content match is dropped and its `pending` entry removed (so a third,
/// later, independently-duplicated event is not wrongly swallowed by an
/// already-spent dedup slot).
///
/// `ts` is used only to bound how long an *unspent* entry lingers: an entry
/// expires once the live stream's `ts` has moved a full [`DEDUP_TTL`] past
/// that entry's own `ts` -- not the whole window's max, so an
/// early-arriving unmatched entry cannot be kept alive indefinitely by a
/// later entry's activity. Expiring an unspent entry early can at worst
/// cause a **missed dedup** (a rare duplicate slips through) -- it can never
/// cause a **gap**, since expiry only ever removes a `pending` entry, never
/// vetoes a match that would otherwise fire.
struct Dedup {
    pending: Vec<(Event, DateTime<Utc>)>,
}

/// How long an unmatched `pending` entry is kept alive past its own
/// timestamp before being dropped as stale. Generous relative to realistic
/// persist-then-emit latency (sub-millisecond in practice) while still
/// keeping the window bounded.
const DEDUP_TTL: chrono::Duration = chrono::Duration::seconds(30);

/// Whether `event`'s `LogRecord` origin (see
/// `session_handle::record_to_event`) has a live-side twin that could
/// collide with it at the replay/live junction. `AgentResultRecord` (->
/// `AgentFinished`), `Assistant` (-> `TurnFinished`), and `ToolResultRecord`
/// (-> `ToolCallFinished`) are each 1:1 mapped to a single live event with
/// the same payload: an agent finishes exactly once (a unique
/// `AgentResult`), a turn emits exactly one `TurnFinished`, and a tool call
/// has a unique `call_id` with `ToolResultRecord`'s persisted fields
/// (`call_id`, `is_error`, and `tool_result_preview`, which mirrors
/// `conway-runtime::tools::runner`'s live `preview_text` derivation
/// logic-for-logic) byte-identical to the live `ToolCallFinished` built from
/// the same `ToolOutcome`. Every other record kind
/// (`UserTurn`/`ForkDirective`/`ParentSteer`/`SystemNote`/
/// `ContextReportRecord` -> `AgentProgress`, and `ToolCallRecord` ->
/// `ToolCallProposed`, whose *persisted* side is dead -- `ToolCallRecord` is
/// never constructed in production, so no replay envelope of this kind can
/// arise to collide with the live `ToolCallProposed`, which is itself
/// emitted on every tool call) has no replayed counterpart that could ever
/// content-equal a live event, so there is nothing to dedup for those and
/// including them would only waste `pending` slots.
fn has_live_twin(event: &Event) -> bool {
    matches!(
        event,
        Event::AgentFinished { .. } | Event::TurnFinished { .. } | Event::ToolCallFinished { .. }
    )
}

impl EventStream {
    /// A live-only stream: no replay prefix, envelope `seq` values are
    /// passed through unchanged from the runtime's broadcast bus.
    pub(crate) fn live(
        session: SessionId,
        agent: Option<AgentId>,
        live: conway_runtime::events::EventStream,
    ) -> Self {
        Self {
            session,
            agent,
            replay: VecDeque::new(),
            next_seq: None,
            dedup: None,
            live,
        }
    }

    /// Builds a replay-then-live stream (`SessionHandle::events_from`):
    /// every envelope in `replay` is yielded first, in order, then this
    /// stream switches to `live` (already filtered to `session`/`agent` by
    /// [`Stream::poll_next`] below, same as [`EventStream::live`]).
    ///
    /// `subscribed_at` must be the timestamp `live`'s subscription was
    /// taken out at (`SessionHandle::events_from` captures this
    /// immediately after `Runtime::subscribe`, before its own persisted
    /// read) -- it bounds the junction-dedup window below.
    ///
    /// **Reconciliation (disclosed):** the binding spec describes this
    /// constructor as deduplicating live envelopes against the replay
    /// batch by comparing `seq` at the junction. That guarantee presumes
    /// replay and live envelopes are numbered from one shared counter.
    /// They are not, and no committed type reconciles them:
    /// `replay`'s envelopes (`session_handle.rs::record_to_event`) are
    /// synthesized from the session's persisted `LogRecord`s, numbered by
    /// `LogSeq` -- one counter per *persisted record*. `live`'s envelopes
    /// carry `conway_runtime::events::EventBus`'s own per-session `seq`
    /// -- one counter per *emitted event*, a strictly larger, faster-moving
    /// set (e.g. several `TextDelta`s and a `TurnFinished` are emitted live
    /// for the one `Assistant` record that gets persisted). Comparing the
    /// two numerically would not detect real duplicates and could drop
    /// unrelated live envelopes whose count happens to coincide.
    ///
    /// This constructor renumbers every envelope it yields -- replay and
    /// live alike -- with a fresh, local, strictly-increasing counter
    /// starting at 0. That makes "monotonically increasing `seq`, no gaps
    /// at the junction" true by construction (an [`EventStream::live`]
    /// stream never renumbers) -- but renumbering is an ordering fix only;
    /// it cannot by itself make "no duplicates" true, since it runs after
    /// the decision of *which* envelopes to yield, not before.
    ///
    /// **The actual duplicate this junction can produce, and how it's
    /// handled:** `SessionHandle::events_from` subscribes to `live` before
    /// reading the persisted store, deliberately -- reading first would
    /// leave a silent gap (anything persisted-and-broadcast between the
    /// read and the subscribe would be missed entirely, which is worse for
    /// an event stream than a duplicate). That ordering means a record
    /// persisted in the gap between subscribing and the store read lands in
    /// *both* the replay batch (as a synthesized envelope) and on the live
    /// bus (as the runtime's own envelope for the same occurrence). This
    /// constructor removes that duplicate by content: for every replay
    /// envelope whose event has a live twin ([`has_live_twin`]) and whose
    /// `ts` falls at or after `subscribed_at` (the only window in which a
    /// collision with `live` is structurally possible), it remembers the
    /// event's content. Every live envelope is matched against that content
    /// **first, before its own `ts` is consulted at all** -- see
    /// [`is_live_duplicate`]'s doc for why the match cannot be ts-gated: in
    /// production the live twin's `ts` is always later than the record's,
    /// so gating on it would silently defeat the dedup. A content match is
    /// dropped and the entry consumed; unmatched entries expire on their own
    /// individual age (see [`Dedup`], [`DEDUP_TTL`]) so this never dedups
    /// arbitrarily far into the stream's future, only within a bounded
    /// window past each candidate's own persist time.
    ///
    /// **Residual gap (disclosed):** this only catches collisions for the
    /// record kinds with an exact 1:1 live mapping ([`has_live_twin`]). It
    /// is not a fully general position/watermark reconciliation across the
    /// `LogSeq` and `EventBus` domains -- that would require a shared cursor
    /// type neither domain has today, out of this item's file scope
    /// (flagged to conway-runtime). Within its scope, the worst-case failure
    /// mode is a rare **missed dedup** (an unmatched entry ages out just
    /// before its true live twin arrives, so the twin passes through as an
    /// apparent duplicate) -- content-matching first, with `ts` used only to
    /// bound `pending`'s lifetime, means this can never produce a **gap**
    /// (a real, non-duplicate event wrongly dropped).
    pub(crate) fn replay_then_live(
        session: SessionId,
        agent: Option<AgentId>,
        replay: Vec<Envelope>,
        subscribed_at: DateTime<Utc>,
        live: conway_runtime::events::EventStream,
    ) -> Self {
        let pending: Vec<(Event, DateTime<Utc>)> = replay
            .iter()
            .filter(|e| e.ts >= subscribed_at && has_live_twin(&e.event))
            .map(|e| (e.event.clone(), e.ts))
            .collect();
        let dedup = if pending.is_empty() {
            None
        } else {
            Some(Dedup { pending })
        };
        Self {
            session,
            agent,
            replay: replay.into(),
            next_seq: Some(0),
            dedup,
            live,
        }
    }

    fn accept(&self, envelope: &Envelope) -> bool {
        if matches!(envelope.event, Event::Lagged { .. }) {
            return true;
        }
        if envelope.session != self.session {
            return false;
        }
        match self.agent {
            Some(agent) => envelope.agent == agent,
            None => true,
        }
    }

    /// `true` if `envelope` is a live-side duplicate of a still-pending
    /// replay envelope and should be dropped -- consumes the matched
    /// `pending` entry. Always `false` for a stream with no dedup state (an
    /// [`EventStream::live`] stream, or a [`EventStream::replay_then_live`]
    /// stream with nothing in its overlap window).
    ///
    /// **Content match runs first, unconditionally on `envelope`'s `ts`.**
    /// This is not merely simpler than a ts-gated check -- a ts gate would
    /// be *wrong*: in production, `agent_loop.rs` always stamps a persisted
    /// record's `ts` (via `Utc::now()`, before `store.append`) strictly
    /// *before* the live twin's own `ts` (a separate, later `Utc::now()`
    /// stamped by `EventBus::emit`). So the live duplicate's `ts` is always
    /// later than the `pending` entry's `ts` it matches -- an "only accept
    /// if `envelope.ts` is within some bound of the entry's `ts`" check
    /// would need to look *forward* in time from the entry, not bound it
    /// from above, and any bail-early-on-ts-order check (as this method
    /// previously had) discards the match before it can even run. Content
    /// equality alone is a reliable duplicate signal here (see
    /// [`has_live_twin`]'s doc on why each mapped kind is effectively
    /// unique), so matching first and using `ts` only for expiry (below) is
    /// both simpler and correct where the ts-gated version was not.
    ///
    /// After checking for a match, every *unspent* `pending` entry whose own
    /// `ts` is more than [`DEDUP_TTL`] behind `envelope.ts` is dropped as
    /// stale -- bounding how long a never-matched entry can linger without
    /// depending on any other entry's activity.
    fn is_live_duplicate(&mut self, envelope: &Envelope) -> bool {
        let Some(dedup) = &mut self.dedup else {
            return false;
        };
        let matched = if let Some(pos) = dedup
            .pending
            .iter()
            .position(|(event, _)| event == &envelope.event)
        {
            dedup.pending.remove(pos);
            true
        } else {
            false
        };
        dedup
            .pending
            .retain(|(_, ts)| envelope.ts - *ts <= DEDUP_TTL);
        if dedup.pending.is_empty() {
            self.dedup = None;
        }
        matched
    }

    fn renumber(&mut self, mut envelope: Envelope) -> Envelope {
        if let Some(seq) = &mut self.next_seq {
            envelope.seq = *seq;
            *seq += 1;
        }
        envelope
    }
}

impl Stream for EventStream {
    type Item = Envelope;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Envelope>> {
        // Every field is `Unpin` (a `VecDeque`, an `Option<u64>`, and a
        // `Pin<Box<dyn Stream + Send>>`, which is always `Unpin` regardless
        // of the pointee), so `EventStream` is `Unpin` and this is safe.
        let this = self.get_mut();

        if let Some(envelope) = this.replay.pop_front() {
            return Poll::Ready(Some(this.renumber(envelope)));
        }

        loop {
            match this.live.as_mut().poll_next(cx) {
                Poll::Ready(Some(envelope)) => {
                    if this.accept(&envelope) {
                        if this.is_live_duplicate(&envelope) {
                            continue;
                        }
                        return Poll::Ready(Some(this.renumber(envelope)));
                    }
                    continue;
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

fn assert_stream_send_unpin<T: Stream + Send + Unpin>() {}

#[allow(dead_code)]
fn _event_stream_is_stream_send_unpin() {
    assert_stream_send_unpin::<EventStream>();
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway_runtime::events::EventBus;

    async fn next(stream: &mut EventStream) -> Option<Envelope> {
        std::future::poll_fn(|cx| Pin::new(&mut *stream).poll_next(cx)).await
    }

    #[tokio::test]
    async fn live_stream_filters_to_one_session() {
        let bus = EventBus::new(64);
        let session_a = SessionId::new();
        let session_b = SessionId::new();
        let agent = AgentId::new();

        let mut stream = EventStream::live(session_a, None, bus.subscribe());

        bus.emit(session_b, agent, Event::AgentProgress { note: "b".into() });
        bus.emit(session_a, agent, Event::AgentProgress { note: "a".into() });

        let envelope = next(&mut stream).await.expect("stream ended early");
        assert_eq!(envelope.session, session_a);
        assert!(matches!(envelope.event, Event::AgentProgress { ref note } if note == "a"));
    }

    #[tokio::test]
    async fn live_stream_forwards_lagged_regardless_of_session_filter() {
        let bus = EventBus::new(2);
        let session = SessionId::new();
        let other_session = SessionId::new();
        let agent = AgentId::new();

        let mut stream = EventStream::live(session, None, bus.subscribe());

        // Flood past the tiny buffer with envelopes for an unrelated
        // session so the subscriber lags.
        for i in 0..10 {
            bus.emit(
                other_session,
                agent,
                Event::AgentProgress {
                    note: format!("flood-{i}"),
                },
            );
        }

        let envelope = next(&mut stream).await.expect("stream ended early");
        assert!(
            matches!(envelope.event, Event::Lagged { .. }),
            "expected a Lagged notice to be forwarded despite the session filter, got {envelope:?}"
        );

        // Subsequent envelopes for the subscribed session must keep
        // arriving -- a lag notice must never leave the stream stalled.
        // With such a small buffer, emitting into an already-full,
        // not-yet-drained backlog can itself provoke another `Lagged`
        // report (a real tokio broadcast-channel race, not a bug in this
        // stream), so this drains past any further `Lagged` notices rather
        // than asserting the very next envelope is the one just emitted.
        bus.emit(
            session,
            agent,
            Event::AgentProgress {
                note: "after-lag".into(),
            },
        );
        let mut saw_after_lag = false;
        for _ in 0..10 {
            let envelope = next(&mut stream).await.expect("stream ended early");
            if matches!(&envelope.event, Event::AgentProgress { note } if note == "after-lag") {
                saw_after_lag = true;
                break;
            }
            assert!(
                matches!(envelope.event, Event::Lagged { .. }),
                "expected either the post-lag envelope or another Lagged notice, got {envelope:?}"
            );
        }
        assert!(
            saw_after_lag,
            "events must keep arriving after a Lagged notice"
        );
    }

    #[tokio::test]
    async fn replay_then_live_yields_replay_then_live_with_monotonic_local_seq() {
        let bus = EventBus::new(64);
        let session = SessionId::new();
        let agent = AgentId::new();

        let replay: Vec<Envelope> = (0..10)
            .map(|i| Envelope {
                seq: 999, // deliberately wrong/unrelated; must be renumbered
                ts: chrono::Utc::now(),
                session,
                agent,
                event: Event::AgentProgress {
                    note: format!("replay-{i}"),
                },
            })
            .collect();

        let mut stream = EventStream::replay_then_live(
            session,
            None,
            replay,
            chrono::Utc::now(),
            bus.subscribe(),
        );

        for i in 0..5 {
            bus.emit(
                session,
                agent,
                Event::AgentProgress {
                    note: format!("live-{i}"),
                },
            );
        }

        let mut seen = Vec::new();
        for _ in 0..15 {
            seen.push(next(&mut stream).await.expect("stream ended early"));
        }

        assert_eq!(seen.len(), 15);
        for (i, envelope) in seen.iter().enumerate() {
            assert_eq!(
                envelope.seq, i as u64,
                "seq must be locally renumbered from 0"
            );
        }
        for (i, envelope) in seen.iter().take(10).enumerate() {
            assert!(
                matches!(&envelope.event, Event::AgentProgress { note } if note == &format!("replay-{i}"))
            );
        }
        for (i, envelope) in seen.iter().skip(10).enumerate() {
            assert!(
                matches!(&envelope.event, Event::AgentProgress { note } if note == &format!("live-{i}"))
            );
        }
    }

    fn fixture_agent_finished(session: SessionId, agent: AgentId) -> Event {
        Event::AgentFinished {
            result: conway_core::agent::AgentResult {
                agent_id: agent,
                status: conway_core::agent::ResultStatus::Completed,
                summary: "race-fixture".into(),
                facts: vec![],
                artifacts: vec![],
                structured: None,
                transcript_ref: session,
                usage: Default::default(),
                steps_taken: 1,
            },
        }
    }

    fn fixture_tool_call_finished() -> Event {
        Event::ToolCallFinished {
            call_id: "tc_race".into(),
            is_error: false,
            preview: "ok".into(),
        }
    }

    /// A fixed, pre-built sequence of envelopes played back as a
    /// `conway_runtime::events::EventStream` -- lets tests pin exact `ts`
    /// values on "live" envelopes (`EventBus::emit` always stamps its own
    /// `Utc::now()`, which a test cannot control), so the `Dedup` TTL-expiry
    /// boundary can be exercised deterministically instead of via a real
    /// multi-second sleep.
    struct FixedStream(VecDeque<Envelope>);

    impl Stream for FixedStream {
        type Item = Envelope;

        fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Envelope>> {
            Poll::Ready(self.get_mut().0.pop_front())
        }
    }

    fn fixed_live(envelopes: Vec<Envelope>) -> conway_runtime::events::EventStream {
        Box::pin(FixedStream(envelopes.into()))
    }

    /// The C1 regression: `SessionHandle::events_from` subscribes to the
    /// live bus *before* reading the persisted store, so a record persisted
    /// (and broadcast live) in that gap is captured by both the replay
    /// batch and the live subscription. This constructs exactly that
    /// situation with PRODUCTION-REALISTIC ordering -- the replay record's
    /// `ts` is stamped, and only *afterward* (mirroring
    /// `agent_loop.rs::finish`: `record.ts = Utc::now()` and persist, then a
    /// separately, later-stamped `Event::AgentFinished` broadcast) does the
    /// content-identical live twin arrive with a strictly later `ts`. A
    /// dedup mechanism that gates the content match on the live envelope's
    /// `ts` being *before* the record's `ts` (the bug this test used to
    /// mask by inverting that ordering) would never fire here -- this
    /// asserts the occurrence is yielded exactly once, with a monotonic,
    /// gap-free `seq` across the drop.
    #[tokio::test]
    async fn replay_then_live_dedups_the_race_duplicate_exactly_once() {
        let bus = EventBus::new(64);
        let session = SessionId::new();
        let agent = AgentId::new();

        let subscribed_at = chrono::Utc::now();
        let raced_event = fixture_agent_finished(session, agent);

        // The persisted side of the race: `record_to_event` synthesized
        // this from the `AgentResultRecord` that `store.read()` picked up
        // because it landed after `subscribed_at`. Its `ts` is stamped now,
        // BEFORE the live twin below -- the real ordering.
        let record_ts = chrono::Utc::now();
        let replay = vec![Envelope {
            seq: 999,
            ts: record_ts,
            session,
            agent,
            event: raced_event.clone(),
        }];

        let mut stream =
            EventStream::replay_then_live(session, None, replay, subscribed_at, bus.subscribe());

        // The live side of the same race, arriving strictly LATER (the
        // reviewer's 5ms-sleep repro): the runtime's own broadcast of the
        // identical occurrence, whose envelope `ts` is stamped by
        // `EventBus::emit` after this sleep -- always after `record_ts`.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        assert!(
            chrono::Utc::now() > record_ts,
            "test precondition: real time must have advanced past record_ts"
        );
        bus.emit(session, agent, raced_event.clone());
        // A genuinely distinct, later event must still come through
        // untouched -- dedup must not swallow anything beyond the raced
        // duplicate.
        bus.emit(
            session,
            agent,
            Event::AgentProgress {
                note: "after".into(),
            },
        );

        let first = next(&mut stream).await.expect("replay envelope");
        assert_eq!(first.event, raced_event, "replay copy must be yielded");
        assert_eq!(first.seq, 0);

        let second = next(&mut stream).await.expect("stream ended early");
        assert!(
            matches!(&second.event, Event::AgentProgress { note } if note == "after"),
            "the live duplicate must be dropped, not this later distinct event; got {:?}",
            second.event
        );
        assert_eq!(
            second.seq, 1,
            "seq must stay monotonic and gap-free across the dropped duplicate"
        );
    }

    /// The `ToolCallFinished` twin: unlike `AgentFinished`/`TurnFinished`,
    /// `agent_loop.rs`'s tool-result handling emits the live
    /// `Event::ToolCallFinished` DURING `ToolRunner::run_batch`, and only
    /// stamps/persists the matching `ToolResultRecord` afterward (its `ts`
    /// is `Utc::now()`'d after `run_batch` returns) -- so for this record
    /// kind the live twin arrives BEFORE the replay record's `ts`, the
    /// opposite ordering from the `AgentFinished` case above. A dedup that
    /// depends on either ts direction would only ever catch one of the two
    /// kinds; match-first must catch both.
    #[tokio::test]
    async fn replay_then_live_dedups_tool_call_finished_with_live_emitted_first() {
        let bus = EventBus::new(64);
        let session = SessionId::new();
        let agent = AgentId::new();

        let subscribed_at = chrono::Utc::now();
        let raced_event = fixture_tool_call_finished();

        // Subscribe (as `events_from` does) before the live twin is
        // broadcast, so this subscription actually observes it.
        let live = bus.subscribe();

        // The live side of the race fires FIRST, mid-`run_batch`.
        bus.emit(session, agent, raced_event.clone());
        bus.emit(
            session,
            agent,
            Event::AgentProgress {
                note: "after".into(),
            },
        );

        // Only afterward is `ToolResultRecord.ts` stamped and the record
        // persisted -- what `store.read()` later picks up into `replay`.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let record_ts = chrono::Utc::now();
        assert!(record_ts > subscribed_at);
        let replay = vec![Envelope {
            seq: 999,
            ts: record_ts,
            session,
            agent,
            event: raced_event.clone(),
        }];

        let mut stream = EventStream::replay_then_live(session, None, replay, subscribed_at, live);

        let first = next(&mut stream).await.expect("replay envelope");
        assert_eq!(first.event, raced_event, "replay copy must be yielded");

        let second = next(&mut stream).await.expect("stream ended early");
        assert!(
            matches!(&second.event, Event::AgentProgress { note } if note == "after"),
            "the live ToolCallFinished duplicate must be dropped, not this later distinct event; got {:?}",
            second.event
        );
    }

    /// Dedup must not run forever: once an unmatched `pending` entry has
    /// aged out (its own `ts` is more than `DEDUP_TTL` behind the live
    /// stream's current `ts`), a later live event that happens to
    /// content-equal it must NOT be dropped -- that would be exactly the
    /// "wrongly drop a legitimately-repeated later event" failure mode the
    /// bounded expiry exists to avoid. Uses `FixedStream` to pin exact `ts`
    /// values on either side of `DEDUP_TTL` deterministically.
    #[tokio::test]
    async fn replay_then_live_stops_deduping_once_past_the_watermark() {
        let session = SessionId::new();
        let agent = AgentId::new();

        let subscribed_at = chrono::Utc::now();
        let never_raced_event = fixture_agent_finished(session, agent);

        // This replay entry is in the dedup window but never gets a live
        // twin -- e.g. the runtime never emitted one for some other
        // reason. It must expire rather than linger indefinitely.
        let entry_ts = subscribed_at;
        let replay = vec![Envelope {
            seq: 999,
            ts: entry_ts,
            session,
            agent,
            event: never_raced_event.clone(),
        }];

        let live = fixed_live(vec![
            // Moves the live stream's clock past `entry_ts + DEDUP_TTL`
            // first, aging the unmatched entry out.
            Envelope {
                seq: 0,
                ts: entry_ts + DEDUP_TTL + chrono::Duration::milliseconds(1),
                session,
                agent,
                event: Event::AgentProgress {
                    note: "far-future".into(),
                },
            },
            // Only now does a content-identical, but legitimately new and
            // independent, occurrence arrive.
            Envelope {
                seq: 1,
                ts: entry_ts + DEDUP_TTL + chrono::Duration::milliseconds(2),
                session,
                agent,
                event: never_raced_event.clone(),
            },
        ]);

        let mut stream = EventStream::replay_then_live(session, None, replay, subscribed_at, live);

        let first = next(&mut stream).await.expect("replay envelope");
        assert_eq!(first.event, never_raced_event);

        let second = next(&mut stream).await.expect("stream ended early");
        assert!(matches!(&second.event, Event::AgentProgress { note } if note == "far-future"));

        let third = next(&mut stream).await.expect("stream ended early");
        assert_eq!(
            third.event, never_raced_event,
            "a content-equal event arriving after the watermark has passed must NOT be dropped"
        );
    }
}
