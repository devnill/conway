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
///
/// [`Event::AgentSpawned`]/[`Event::AgentFinished`] are likewise always
/// forwarded regardless of the session/agent filter -- tree lifecycle is a
/// global concern, and a subagent's own lifecycle events are stamped with
/// its OWN, freshly-minted session/agent id, not its parent's. See
/// [`EventStream::accept`]'s doc for the full rationale and the
/// `TurnHandle`-safety note this depends on. [`Event::AgentPromoted`]
/// (B3) gets the same passthrough -- see `accept`.
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
    /// Agents whose in-flight turn's reply text was already yielded IN FULL
    /// from the replay batch (a replayed `Assistant`-derived
    /// `Event::TextDelta` -- see `record_to_event` -- with `ts >=
    /// subscribed_at`, i.e. persisted during the subscribe-then-read overlap
    /// window). This exists for exactly the case `pending`/[`has_live_twin`]
    /// cannot cover: a whole-text replay `TextDelta` can never
    /// content-match any single chunked LIVE `TextDelta` from the same
    /// turn, so there is nothing to dedup by content here -- the tail must
    /// instead be suppressed by TURN BOUNDARY. Each entry pairs the
    /// suppressed agent with the `ts` of the replay record that triggered
    /// it, for the same bounded-expiry discipline `pending` uses (see
    /// [`DEDUP_TTL`]). See [`EventStream::suppress_turn_tail`] for how an
    /// entry is cleared (on that SAME agent's next live turn-boundary
    /// marker) and why that can never suppress a later, genuinely new turn.
    suppress_turn_tail: Vec<(AgentId, DateTime<Utc>)>,
}

/// How long an unmatched `pending` entry is kept alive past its own
/// timestamp before being dropped as stale. Generous relative to realistic
/// persist-then-emit latency (sub-millisecond in practice) while still
/// keeping the window bounded.
const DEDUP_TTL: chrono::Duration = chrono::Duration::seconds(30);

/// Whether `event`'s `LogRecord` origin (see
/// `session_handle::record_to_event`) has a live-side twin that could
/// collide with it at the replay/live junction. `AgentResultRecord` (->
/// `AgentFinished`) and `ToolResultRecord` (-> `ToolCallFinished`) are each
/// 1:1 mapped to a single live event with the same payload: an agent
/// finishes exactly once (a unique `AgentResult`), and a tool call has a
/// unique `call_id` with `ToolResultRecord`'s persisted fields (`call_id`,
/// `is_error`, and `tool_result_preview`, which mirrors
/// `conway-runtime::tools::runner`'s live `preview_text` derivation
/// logic-for-logic) byte-identical to the live `ToolCallFinished` built from
/// the same `ToolOutcome`.
///
/// **`UserTurn` joined this list (this item):** `record_to_event`'s
/// `LogRecord::UserTurn` arm now maps to `Event::UserTurn{text, prov}`
/// byte-for-byte from the persisted record, and `conway-runtime` (`Runtime::
/// prompt`/`start_root`, `subagent.rs`'s `start` for a `Spawn` with a
/// non-empty prompt) emits the SAME `Event::UserTurn{text, prov}` live, at
/// the same occurrence the record persists — a genuine 1:1 twin, exactly
/// like `AgentFinished`/`ToolCallFinished` above. Without this, the
/// subscribe-before-read race `events_from`/`agent_events` already accept
/// for those two kinds (see `replay_then_live`'s own doc) could duplicate a
/// prompt: a `UserTurn` persisted (and broadcast live) in the gap between
/// this stream's live subscribe and its persisted-store read would
/// otherwise land in *both* the replay batch and on `live`.
///
/// Every remaining record kind (`ForkDirective`/`ParentSteer`/`SystemNote`/
/// `ContextReportRecord` -> `AgentProgress`) has no replayed counterpart that
/// could ever content-equal a live event, so there is nothing to dedup for
/// those and including them would only waste `pending` slots. The live
/// `Event::ToolCallProposed` (emitted on every tool call) has no replayed
/// counterpart at all -- `record_to_event` has no arm that produces it, since
/// no `LogRecord` variant persists a standalone tool-call proposal (a
/// model-proposed tool call is recorded as a `ContentBlock::ToolUse` inside
/// the preceding `Assistant` record instead) -- so it cannot collide with a
/// replay envelope either.
///
/// **`Assistant` (-> `Event::TextDelta`, WI-140 review fix) is deliberately
/// NOT included here, and `Event::TurnFinished` no longer needs to be
/// either:** `record_to_event`'s `Assistant` arm used to map to
/// `Event::TurnFinished{usage, stop}`, a genuine 1:1 live twin, which is why
/// `TurnFinished` was matched below. It now maps to one `Event::TextDelta`
/// carrying the record's FULL concatenated reply text -- which is never
/// byte-identical to any single LIVE `TextDelta` (the live side is chunked
/// into many small deltas per turn, per this module's own doc above on
/// mismatched replay/live cardinality), so it could never validly match a
/// live envelope here even if included -- content matching is structurally
/// the wrong tool for this pair.
///
/// **This does NOT mean the resulting duplicate goes unhandled (cycle-3
/// review finding, fixed):** a replayed `Assistant` record and its live
/// turn's still-queued `TextDelta` chunks CAN coexist on the same stream, in
/// the same subscribe-before-read race window `events_from`/`agent_events`
/// already accept for the other mapped kinds -- and left alone, that
/// duplicates the reply's tail in the rendered transcript (`AppState::
/// append_assistant_text` appends the live chunks onto the already-replayed
/// full-text bubble). That case is instead handled by a SEPARATE,
/// turn-boundary-scoped mechanism -- [`Dedup::suppress_turn_tail`] /
/// [`EventStream::suppress_turn_tail`] -- entirely outside `has_live_twin`'s
/// content-match scope: see that field's doc for why boundary suppression,
/// not content matching, is the correct tool for a cardinality-mismatched
/// pair like this one.
fn has_live_twin(event: &Event) -> bool {
    matches!(
        event,
        Event::AgentFinished { .. } | Event::ToolCallFinished { .. } | Event::UserTurn { .. }
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
    ///
    /// **The `Assistant`/`TextDelta` duplicate (cycle-3 review finding,
    /// fixed separately from `pending` above):** `has_live_twin` deliberately
    /// excludes `TextDelta` -- a replayed `Assistant` record's full-text
    /// `TextDelta` can never content-match a chunked live `TextDelta`, so
    /// `pending`'s content-match mechanism cannot catch this pair. Left
    /// unhandled, the SAME subscribe-before-read race this constructor's doc
    /// above describes would duplicate an in-flight turn's reply tail (the
    /// replayed full text, immediately followed by that same turn's still-
    /// queued live chunks). This constructor separately seeds
    /// [`Dedup::suppress_turn_tail`] with every replay envelope whose event
    /// is a `TextDelta` (i.e. an `Assistant`-derived one -- `record_to_event`
    /// maps no other kind to `TextDelta`) and whose `ts` falls at or after
    /// `subscribed_at`, exactly the same overlap-window test `pending` uses.
    /// See [`EventStream::suppress_turn_tail`] for how it is drained.
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
        let suppress_turn_tail: Vec<(AgentId, DateTime<Utc>)> = replay
            .iter()
            .filter(|e| e.ts >= subscribed_at && matches!(e.event, Event::TextDelta { .. }))
            .map(|e| (e.agent, e.ts))
            .collect();
        let dedup = if pending.is_empty() && suppress_turn_tail.is_empty() {
            None
        } else {
            Some(Dedup {
                pending,
                suppress_turn_tail,
            })
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
        // Tree-lifecycle events are a session-agnostic, global concern, not
        // this-session turn text: a subagent is spawned and finishes on its
        // OWN, freshly-minted session (`tree.rs::attach`'s `self.bus.emit(
        // node.session, node.id, event)`, and `supervisor.rs`'s matching
        // `AgentFinished` emit are both stamped with the CHILD's own
        // session/agent id, by design -- see `events_from`'s doc above on
        // `SessionId` keying "one session per agent"). A subscriber scoped
        // to any single session -- the TUI's root `handle.events()`, or a
        // per-turn `TurnHandle`'s internal stream -- would otherwise never
        // observe another agent's spawn/finish at all: exactly the bug this
        // passthrough fixes (the `/agents` panel and inline `Entry::Agent`
        // activity staying empty when a subagent is spawned). Bypass BOTH
        // the session and the agent filter for these two variants,
        // unconditionally, exactly like `Event::Lagged` above.
        //
        // This means an agent-scoped stream (`self.agent: Some(_)`, i.e. a
        // `TurnHandle`) can now observe an `AgentFinished` for an agent
        // other than its own. `TurnHandle::text`/`TurnHandle::result`
        // (`session_handle.rs`) are written to tolerate exactly that: both
        // check the finished `AgentResult`'s own `agent_id` before treating
        // an `AgentFinished` as THEIR turn's terminal event, rather than
        // assuming (as they could when this stream was fully session/agent
        // scoped) that any `AgentFinished` reaching them must be their own.
        // This filter deliberately does not attempt that narrower scoping
        // itself -- it has no notion of "this turn's own agent" to check
        // against, only "this subscription's declared session/agent filter".
        // `AgentPromoted` (B3) rides the same passthrough: it is stamped
        // with the PROMOTED child's own session/agent id
        // (`Runtime::promote_agent` emits under the node's own session),
        // exactly like its spawn/finish — a parent-scoped subscriber (the
        // TUI's root `handle.events()`) must still observe it, or the
        // `/agents` panel's cached `ephemeral` flag would never clear when
        // the user promotes a child from anywhere but the child's own
        // focused view. It carries no turn content, so the `TurnHandle`
        // safety argument above applies unchanged (`text`/`result` ignore
        // it via their wildcard arms).
        if matches!(
            envelope.event,
            Event::AgentSpawned { .. } | Event::AgentFinished { .. } | Event::AgentPromoted { .. }
        ) {
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
        self.clear_dedup_if_spent();
        matched
    }

    /// `true` if `envelope` is a live `TextDelta` whose tail belongs to a
    /// turn already replayed in full ([`Dedup::suppress_turn_tail`]) and
    /// must therefore be dropped. This is a SEPARATE mechanism from
    /// [`EventStream::is_live_duplicate`], not an extension of it: that
    /// method dedups by exact content match, which is structurally
    /// impossible here (a whole-text replay `TextDelta` never byte-equals
    /// any one chunked live `TextDelta` -- see [`has_live_twin`]'s doc). This
    /// suppresses by TURN BOUNDARY instead.
    ///
    /// **Boundary detection, and why it cannot over-suppress a later turn:**
    /// a live `Event::TurnFinished` or `Event::AgentFinished` is never itself
    /// suppressed by this method -- it is read purely as a MARKER (never a
    /// content-match target) that the in-flight turn responsible for a
    /// `suppress_turn_tail` entry has truly ended, and it clears ONLY the
    /// entry for the SAME agent (`envelope.agent`, matched against the
    /// entry's own agent). Three things make this safe:
    /// - `agent_loop.rs::finish` persists the `Assistant` record (which is
    ///   what seeds a `suppress_turn_tail` entry, in `replay_then_live`) and
    ///   then emits the matching live `TurnFinished` `Event` for the SAME
    ///   agent's SAME turn, persist strictly before emit -- so in the
    ///   overwhelmingly common case a boundary for the suppressed turn is
    ///   already on its way once an entry exists. This is not exceptionless:
    ///   `TurnFinished` is only reached after a SECOND store append (the
    ///   context-report persist) succeeds; if that append errors,
    ///   `run_inner` returns early and unwinds to `finish`/`finish_error`
    ///   instead, whose `AgentFinished` emission is itself gated on winning
    ///   `AgentTree::publish_result`'s CAS (the supervisor may already have
    ///   published first, e.g. on a grace-timeout) -- so neither boundary
    ///   variant is unconditionally guaranteed. This is harmless here: in
    ///   every such case the agent has necessarily terminated, so there is
    ///   no further live `TextDelta` traffic for it to wrongly suppress --
    ///   the orphaned entry just ages out via [`DEDUP_TTL`] (or, per the
    ///   `Event::Lagged` handling below, clears immediately if a lag
    ///   happens to intervene first). Suppression therefore never depends on
    ///   a boundary that is truly guaranteed, only on one that is either
    ///   overwhelmingly likely to arrive promptly, or -- when it doesn't --
    ///   provably harmless to wait out.
    /// - Scoping the clear to `envelope.agent` matters because
    ///   `Event::AgentFinished` bypasses this stream's own session/agent
    ///   filter entirely (`accept`, above) -- a SIBLING agent's finish can
    ///   reach this method. Matching on the specific agent id (not "any
    ///   AgentFinished") is what stops a sibling's boundary from wrongly
    ///   clearing (and thus prematurely un-suppressing) this agent's own
    ///   still-in-flight tail.
    /// - Once cleared, an entry cannot be reinstated except by a FRESH call
    ///   to `replay_then_live` (a fresh subscribe), so a later, genuinely new
    ///   live turn for that same agent is never itself suppressed -- only
    ///   the SPECIFIC already-replayed turn's tail was ever in scope, and
    ///   the boundary marker that ends it also ends the suppression.
    ///
    /// Also expires stale entries on the same [`DEDUP_TTL`] discipline
    /// [`EventStream::is_live_duplicate`] uses for `pending` -- a defensive
    /// bound only; in production the boundary above always arrives first.
    ///
    /// **`Event::Lagged` fails this mechanism OPEN, not closed (cycle-4
    /// review finding, fixed):** `EventBus` is one process-wide
    /// `broadcast::channel`, shared by every agent in the tree; when this
    /// subscription lags, the whole missed range collapses into a single
    /// `Event::Lagged` with no per-agent/session information at all (see
    /// this module's own top-level doc). If the specific boundary marker
    /// that would have cleared an agent's entry fell inside that dropped
    /// range, the entry would otherwise linger -- and since it is scoped
    /// only by `agent`, not by the specific turn that seeded it, it would
    /// then wrongly suppress every subsequent `TextDelta` for that agent,
    /// including a genuinely NEW turn's real reply, until TTL expiry. That
    /// is a **dropped real event**, the opposite failure mode from
    /// `pending`'s (whose worst case, documented above, is only ever a
    /// missed dedup -- a rare surviving duplicate, never a gap). A
    /// transcript silently missing a reply is strictly worse than an
    /// occasional duplicated one, especially since a `Lagged` envelope
    /// itself already tells the caller "N events were missed" -- so on
    /// `Event::Lagged`, EVERY entry is cleared unconditionally (not just the
    /// one for a matching agent -- `Lagged` carries no agent to match
    /// against, and after a lag any agent's boundary could be the one that
    /// was dropped). The `Lagged` envelope itself is never suppressed by
    /// this method (matching [`EventStream::accept`]'s own unconditional
    /// passthrough for it).
    fn suppress_turn_tail(&mut self, envelope: &Envelope) -> bool {
        let Some(dedup) = &mut self.dedup else {
            return false;
        };
        if matches!(envelope.event, Event::Lagged { .. }) {
            dedup.suppress_turn_tail.clear();
            self.clear_dedup_if_spent();
            return false;
        }
        if matches!(
            envelope.event,
            Event::TurnFinished { .. } | Event::AgentFinished { .. }
        ) {
            dedup
                .suppress_turn_tail
                .retain(|(agent, _)| *agent != envelope.agent);
            self.clear_dedup_if_spent();
            return false;
        }
        let suppress = matches!(envelope.event, Event::TextDelta { .. })
            && dedup
                .suppress_turn_tail
                .iter()
                .any(|(agent, _)| *agent == envelope.agent);
        dedup
            .suppress_turn_tail
            .retain(|(_, ts)| envelope.ts - *ts <= DEDUP_TTL);
        self.clear_dedup_if_spent();
        suppress
    }

    /// Drops `self.dedup` once both its `pending` content-match slots and
    /// its `suppress_turn_tail` entries are empty -- shared by
    /// [`EventStream::is_live_duplicate`] and
    /// [`EventStream::suppress_turn_tail`], which each mutate one of the two
    /// vecs and must not tear down the other's still-live state.
    fn clear_dedup_if_spent(&mut self) {
        if let Some(dedup) = &self.dedup {
            if dedup.pending.is_empty() && dedup.suppress_turn_tail.is_empty() {
                self.dedup = None;
            }
        }
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
                        // Both checks always run -- not short-circuited via
                        // `||` -- because `suppress_turn_tail` must still
                        // observe a `TurnFinished`/`AgentFinished` boundary
                        // marker even when `is_live_duplicate` has already
                        // decided to drop that SAME envelope as a content
                        // duplicate (e.g. a tool-call-free final turn whose
                        // `AgentResultRecord` also landed in the replay
                        // overlap window): the boundary must still clear
                        // this agent's suppressed tail regardless of whether
                        // the envelope carrying it is itself forwarded.
                        let is_dup = this.is_live_duplicate(&envelope);
                        let is_suppressed = this.suppress_turn_tail(&envelope);
                        if is_dup || is_suppressed {
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

    /// The bug this item fixes: a subagent's `AgentSpawned`/`AgentFinished`
    /// is emitted on the CHILD's own session (`tree.rs::attach`,
    /// `supervisor.rs::finish`), not the parent's -- so a subscriber
    /// filtered to the parent's session must still observe it. Also asserts
    /// the mirror-image half of the fix: a NON-lifecycle event on the other
    /// session (e.g. `AgentProgress`) must stay dropped -- this passthrough
    /// is scoped to tree lifecycle only, not a blanket session bypass.
    #[tokio::test]
    async fn lifecycle_events_bypass_session_filter_but_other_events_stay_scoped() {
        let bus = EventBus::new(64);
        let session_a = SessionId::new();
        let session_b = SessionId::new();
        let agent_a = AgentId::new();
        let agent_b = AgentId::new();

        let mut stream = EventStream::live(session_a, None, bus.subscribe());

        // A non-lifecycle event on the other session: must NOT reach a
        // session-A subscriber.
        bus.emit(
            session_b,
            agent_b,
            Event::AgentProgress {
                note: "unrelated".into(),
            },
        );
        // The child's own spawn/finish, on the child's own session: MUST
        // reach a session-A subscriber despite `session_b != session_a`.
        bus.emit(
            session_b,
            agent_b,
            Event::AgentSpawned {
                kind: conway_core::agent::SubagentMode::Spawn,
                parent: Some(agent_a),
                agent_def: Some("reviewer".into()),
                inherited_upto: None,
                ephemeral: false,
            },
        );
        bus.emit(
            session_b,
            agent_b,
            fixture_agent_finished(session_b, agent_b),
        );
        // A genuinely session-A event must still arrive too, after the
        // cross-session lifecycle pair.
        bus.emit(
            session_a,
            agent_a,
            Event::AgentProgress { note: "own".into() },
        );

        let spawned = next(&mut stream).await.expect("stream ended early");
        assert_eq!(
            spawned.session, session_b,
            "the envelope's own session must be preserved (it identifies which agent spawned)"
        );
        assert!(
            matches!(spawned.event, Event::AgentSpawned { .. }),
            "the unrelated AgentProgress must have been dropped, not this; got {:?}",
            spawned.event
        );

        let finished = next(&mut stream).await.expect("stream ended early");
        assert_eq!(finished.session, session_b);
        assert!(matches!(finished.event, Event::AgentFinished { .. }));

        let own = next(&mut stream).await.expect("stream ended early");
        assert_eq!(own.session, session_a);
        assert!(matches!(&own.event, Event::AgentProgress { note } if note == "own"));
    }

    /// The agent filter (used by `TurnHandle`'s internal, per-turn stream)
    /// is bypassed for lifecycle events exactly like the session filter --
    /// a stream scoped to one agent must still observe another agent's
    /// (e.g. a spawned child's) spawn/finish, while a non-lifecycle event
    /// for that other agent stays dropped.
    #[tokio::test]
    async fn lifecycle_events_bypass_agent_filter_too() {
        let bus = EventBus::new(64);
        let session = SessionId::new();
        let agent = AgentId::new();
        let other_agent = AgentId::new();

        let mut stream = EventStream::live(session, Some(agent), bus.subscribe());

        bus.emit(
            session,
            other_agent,
            Event::AgentProgress {
                note: "unrelated".into(),
            },
        );
        bus.emit(
            session,
            other_agent,
            Event::AgentSpawned {
                kind: conway_core::agent::SubagentMode::Spawn,
                parent: Some(agent),
                agent_def: None,
                inherited_upto: None,
                ephemeral: false,
            },
        );

        let envelope = next(&mut stream).await.expect("stream ended early");
        assert!(
            matches!(envelope.event, Event::AgentSpawned { .. }),
            "the unrelated AgentProgress for `other_agent` must have been dropped, not this; got {:?}",
            envelope.event
        );
    }

    /// `AgentPromoted` (B3) rides the same lifecycle passthrough as
    /// `AgentSpawned`/`AgentFinished`: it is stamped under the PROMOTED
    /// child's own session (`Runtime::promote_agent`), so a parent-scoped
    /// subscriber must still observe it, while a non-lifecycle event on
    /// that same foreign session stays dropped.
    #[tokio::test]
    async fn promoted_events_bypass_session_and_agent_filters() {
        let bus = EventBus::new(64);
        let parent_session = SessionId::new();
        let child_session = SessionId::new();
        let parent = AgentId::new();
        let child = AgentId::new();

        // Session-scoped (no agent filter): the passthrough half.
        let mut stream = EventStream::live(parent_session, None, bus.subscribe());
        bus.emit(
            child_session,
            child,
            Event::AgentProgress {
                note: "unrelated".into(),
            },
        );
        bus.emit(child_session, child, Event::AgentPromoted {});
        let envelope = next(&mut stream).await.expect("stream ended early");
        assert!(
            matches!(envelope.event, Event::AgentPromoted { .. }),
            "the unrelated AgentProgress must have been dropped, not this; got {:?}",
            envelope.event
        );
        assert_eq!(envelope.session, child_session);
        assert_eq!(envelope.agent, child);

        // Agent-scoped (a TurnHandle-shaped filter): same passthrough.
        let mut stream = EventStream::live(parent_session, Some(parent), bus.subscribe());
        bus.emit(child_session, child, Event::AgentPromoted {});
        let envelope = next(&mut stream).await.expect("stream ended early");
        assert!(
            matches!(envelope.event, Event::AgentPromoted { .. }),
            "AgentPromoted must bypass the agent filter too; got {:?}",
            envelope.event
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
            ephemeral: false,
        }
    }

    fn fixture_tool_call_finished() -> Event {
        Event::ToolCallFinished {
            call_id: "tc_race".into(),
            is_error: false,
            preview: "ok".into(),
        }
    }

    fn fixture_user_turn() -> Event {
        Event::UserTurn {
            text: "race prompt".into(),
            prov: conway_core::provenance::Provenance::UserPrompt,
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

    /// The `UserTurn` twin (this item): `Runtime::prompt` persists the
    /// `LogRecord::UserTurn` and then emits the content-identical live
    /// `Event::UserTurn` for the same occurrence, exactly like
    /// `AgentFinished` above -- so the same subscribe-before-read race can
    /// duplicate a prompt across the replay/live junction unless
    /// `has_live_twin` catches it. This is the acceptance test for "a prompt
    /// appears exactly once" at the `EventStream` layer (see also
    /// `conway`'s `crates/conway/tests/session_handle.rs` for the full,
    /// real-runtime version of this same property).
    #[tokio::test]
    async fn replay_then_live_dedups_user_turn_race_duplicate_exactly_once() {
        let bus = EventBus::new(64);
        let session = SessionId::new();
        let agent = AgentId::new();

        let subscribed_at = chrono::Utc::now();
        let raced_event = fixture_user_turn();

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

        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        bus.emit(session, agent, raced_event.clone());
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
            "the live duplicate UserTurn must be dropped, not this later distinct event; got {:?}",
            second.event
        );
        assert_eq!(
            second.seq, 1,
            "seq must stay monotonic and gap-free across the dropped duplicate"
        );
    }

    /// The other half of the `UserTurn` dedup contract, and the one that
    /// actually constrains the implementation: dedup must suppress a
    /// duplicate, never a REPETITION.
    ///
    /// Sending the same text twice in a row is ordinary user behavior
    /// ("retry", "again"), and two `Event::UserTurn`s with identical `text`
    /// and `prov` are byte-identical -- there is no id or nonce
    /// distinguishing a genuine second prompt from the live echo of the
    /// first. So the ONLY thing keeping the second one alive is that
    /// `is_live_duplicate` consumes its match (`Vec::position` +
    /// `Vec::remove`) rather than merely testing membership. A `contains`-
    /// style check would pass the sibling test above and silently swallow
    /// every repeat of an already-replayed prompt -- a user's turn vanishing
    /// from their own transcript.
    ///
    /// One replayed prompt + two identical live ones => exactly two
    /// occurrences survive: the replay copy, and the second live one.
    #[tokio::test]
    async fn replay_then_live_suppresses_the_echo_but_never_a_genuine_repeat() {
        let bus = EventBus::new(64);
        let session = SessionId::new();
        let agent = AgentId::new();

        let subscribed_at = chrono::Utc::now();
        let prompt = fixture_user_turn();

        // One occurrence already persisted and replayed.
        let replay = vec![Envelope {
            seq: 999,
            ts: chrono::Utc::now(),
            session,
            agent,
            event: prompt.clone(),
        }];

        let mut stream =
            EventStream::replay_then_live(session, None, replay, subscribed_at, bus.subscribe());

        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        // The first live copy is the race echo of the replayed one; the
        // second is the user genuinely sending the same text again.
        bus.emit(session, agent, prompt.clone());
        bus.emit(session, agent, prompt.clone());
        bus.emit(
            session,
            agent,
            Event::AgentProgress {
                note: "after".into(),
            },
        );

        let first = next(&mut stream).await.expect("replay envelope");
        assert_eq!(first.event, prompt, "replay copy must be yielded");

        let second = next(&mut stream).await.expect("stream ended early");
        assert_eq!(
            second.event, prompt,
            "the SECOND live UserTurn is a genuine repeat, not an echo -- \
             dedup consumed its one pending entry on the first and must not \
             suppress this one; got {:?}",
            second.event
        );

        let third = next(&mut stream).await.expect("stream ended early");
        assert!(
            matches!(&third.event, Event::AgentProgress { note } if note == "after"),
            "exactly one live UserTurn should have been dropped; got {:?}",
            third.event
        );
        assert_eq!(
            third.seq, 2,
            "seq stays monotonic and gap-free across the single dropped echo"
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

    fn fixture_turn_finished() -> Event {
        Event::TurnFinished {
            usage: Default::default(),
            stop: conway_core::content::StopReason::EndTurn,
        }
    }

    /// The bug this item fixes (cycle-3 review finding, "focus-switch can
    /// duplicate an in-flight turn's assistant text at the replay/live
    /// junction"): `record_to_event` maps a persisted `Assistant` record to
    /// ONE `Event::TextDelta` carrying the record's FULL reply text
    /// (`has_live_twin` deliberately excludes `TextDelta`, so
    /// `is_live_duplicate`'s content-match dedup structurally cannot catch
    /// this pair -- see that function's doc). Left unhandled, a replay batch
    /// containing that whole-text delta, immediately followed by the SAME
    /// turn's still-queued LIVE chunked `TextDelta`s, would render the
    /// reply's tail twice. This constructs exactly that junction
    /// deterministically (a fixed `replay: Vec<Envelope>`, a pinned
    /// `subscribed_at`, and a `FixedStream` standing in for the live bus --
    /// no timing race) and asserts:
    /// - the replayed full text is yielded exactly once;
    /// - the live tail chunks of the SAME already-replayed turn are
    ///   suppressed entirely (never reach the caller);
    /// - the turn-boundary marker (`TurnFinished`) that ends the suppression
    ///   is itself still delivered, unsuppressed;
    /// - critically, a NEW live turn's `TextDelta`s that arrive AFTER that
    ///   boundary are delivered in full, unsuppressed -- proving this cannot
    ///   over-suppress a later, genuinely new turn.
    #[tokio::test]
    async fn replay_then_live_suppresses_duplicated_assistant_tail_then_resumes_after_boundary() {
        let session = SessionId::new();
        let agent = AgentId::new();

        let subscribed_at = chrono::Utc::now();

        // The persisted side of the race: `record_to_event` synthesized this
        // from the `Assistant` record `store.read()` picked up because it
        // landed after `subscribed_at` -- exactly the overlap window
        // `replay_then_live`'s doc describes. Its `ts` is stamped before any
        // live envelope below, mirroring `agent_loop.rs::finish`'s real
        // ordering (persist, then emit).
        let record_ts = subscribed_at + chrono::Duration::milliseconds(1);
        let replay = vec![Envelope {
            seq: 999,
            ts: record_ts,
            session,
            agent,
            event: Event::TextDelta {
                text: "Hello world".into(),
            },
        }];

        let live = fixed_live(vec![
            // The SAME turn's still-queued live chunks -- must be
            // suppressed, not delivered.
            Envelope {
                seq: 0,
                ts: record_ts + chrono::Duration::milliseconds(1),
                session,
                agent,
                event: Event::TextDelta {
                    text: "Hello".into(),
                },
            },
            Envelope {
                seq: 1,
                ts: record_ts + chrono::Duration::milliseconds(2),
                session,
                agent,
                event: Event::TextDelta {
                    text: " world".into(),
                },
            },
            // The turn's real boundary marker: clears the suppression for
            // this agent. Must itself be delivered, unsuppressed.
            Envelope {
                seq: 2,
                ts: record_ts + chrono::Duration::milliseconds(3),
                session,
                agent,
                event: fixture_turn_finished(),
            },
            // A genuinely NEW turn's live text, arriving after the
            // boundary -- must be delivered in full, proving suppression
            // does not leak into the next turn.
            Envelope {
                seq: 3,
                ts: record_ts + chrono::Duration::milliseconds(4),
                session,
                agent,
                event: Event::TextDelta {
                    text: "Second turn reply".into(),
                },
            },
        ]);

        let mut stream = EventStream::replay_then_live(session, None, replay, subscribed_at, live);

        let first = next(&mut stream).await.expect("replay envelope");
        assert!(
            matches!(&first.event, Event::TextDelta { text } if text == "Hello world"),
            "the replayed full-text delta must be yielded first; got {:?}",
            first.event
        );
        assert_eq!(first.seq, 0);

        let second = next(&mut stream).await.expect("stream ended early");
        assert!(
            matches!(second.event, Event::TurnFinished { .. }),
            "the two duplicated live tail chunks must be suppressed -- the turn boundary must \
             be the very next envelope; got {:?}",
            second.event
        );
        assert_eq!(
            second.seq, 1,
            "seq must stay monotonic and gap-free across the suppressed tail"
        );

        let third = next(&mut stream).await.expect("stream ended early");
        assert!(
            matches!(&third.event, Event::TextDelta { text } if text == "Second turn reply"),
            "a NEW turn's live text arriving after the boundary must be delivered in full, not \
             suppressed; got {:?}",
            third.event
        );
        assert_eq!(third.seq, 2);
    }

    /// The agent-scoping half of the same fix: `Event::AgentFinished`
    /// bypasses this stream's own session/agent filter (`accept`, tree
    /// lifecycle is a global concern), so a SIBLING agent's `AgentFinished`
    /// can reach `suppress_turn_tail` while this agent's own tail is still
    /// suppressed. That must NOT clear this agent's suppression -- only a
    /// boundary marker for THIS agent may. This also exercises
    /// `AgentFinished` (rather than `TurnFinished`) as the clearing marker.
    #[tokio::test]
    async fn replay_then_live_turn_tail_suppression_ignores_a_siblings_boundary_marker() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let sibling = AgentId::new();

        let subscribed_at = chrono::Utc::now();
        let record_ts = subscribed_at + chrono::Duration::milliseconds(1);

        let replay = vec![Envelope {
            seq: 999,
            ts: record_ts,
            session,
            agent,
            event: Event::TextDelta {
                text: "Full reply".into(),
            },
        }];

        let live = fixed_live(vec![
            // A sibling's own AgentFinished, unrelated to `agent`'s
            // suppressed turn -- must NOT clear it.
            Envelope {
                seq: 0,
                ts: record_ts + chrono::Duration::milliseconds(1),
                session,
                agent: sibling,
                event: fixture_agent_finished(session, sibling),
            },
            // The duplicated live tail chunk -- must still be suppressed,
            // since the sibling's finish above must not have cleared it.
            Envelope {
                seq: 1,
                ts: record_ts + chrono::Duration::milliseconds(2),
                session,
                agent,
                event: Event::TextDelta {
                    text: "tail".into(),
                },
            },
            // THIS agent's own boundary marker: now it clears.
            Envelope {
                seq: 2,
                ts: record_ts + chrono::Duration::milliseconds(3),
                session,
                agent,
                event: fixture_agent_finished(session, agent),
            },
        ]);

        let mut stream = EventStream::replay_then_live(session, None, replay, subscribed_at, live);

        let first = next(&mut stream).await.expect("replay envelope");
        assert!(matches!(&first.event, Event::TextDelta { text } if text == "Full reply"));

        let second = next(&mut stream).await.expect("stream ended early");
        assert!(
            matches!(second.event, Event::AgentFinished { .. }),
            "the sibling's own AgentFinished must pass through (lifecycle events bypass the \
             session/agent filter); the suppressed tail chunk must still be dropped after it; \
             got {:?}",
            second.event
        );
        if let Event::AgentFinished { result, .. } = &second.event {
            assert_eq!(
                result.agent_id, sibling,
                "this must be the sibling's own AgentFinished, not agent's"
            );
        }

        let third = next(&mut stream).await.expect("stream ended early");
        assert!(
            matches!(third.event, Event::AgentFinished { .. }),
            "after the sibling's finish (which must not have cleared this agent's \
             suppression) and the still-suppressed tail chunk, this agent's OWN AgentFinished \
             must be next; got {:?}",
            third.event
        );
        if let Event::AgentFinished { result, .. } = &third.event {
            assert_eq!(
                result.agent_id, agent,
                "this must be this agent's own AgentFinished"
            );
        }
    }

    /// Cycle-4 review finding: `EventBus` is one process-wide broadcast
    /// channel shared by every agent in the tree, and a lagging subscriber's
    /// missed range collapses into a single `Event::Lagged` with no
    /// per-agent/session information at all. If the boundary marker that
    /// would have cleared a `suppress_turn_tail` entry fell inside that
    /// dropped range, the entry -- scoped only by `agent`, not by the
    /// specific turn that seeded it -- would otherwise linger and wrongly
    /// suppress a later, genuinely NEW turn's real `TextDelta`s: a dropped
    /// real event, the opposite of this module's documented
    /// never-drop-a-real-event invariant. This asserts `Event::Lagged`
    /// clears the suppression outright (fails OPEN) rather than leaving it
    /// to linger: seed suppression for `agent` via a replayed
    /// `Assistant`-derived `TextDelta`, then deliver a `Lagged` on the live
    /// side WITHOUT `agent`'s own `TurnFinished`/`AgentFinished`, then a new
    /// turn's `TextDelta` for the SAME agent -- the new turn's text must be
    /// delivered, not silently eaten.
    #[tokio::test]
    async fn replay_then_live_lagged_clears_turn_tail_suppression() {
        let session = SessionId::new();
        let agent = AgentId::new();

        let subscribed_at = chrono::Utc::now();
        let record_ts = subscribed_at + chrono::Duration::milliseconds(1);

        let replay = vec![Envelope {
            seq: 999,
            ts: record_ts,
            session,
            agent,
            event: Event::TextDelta {
                text: "First turn reply".into(),
            },
        }];

        let live = fixed_live(vec![
            // A lag notice -- NOT `agent`'s own `TurnFinished`/
            // `AgentFinished` -- must clear the suppression outright rather
            // than leaving it to linger until TTL expiry.
            Envelope {
                seq: 0,
                ts: record_ts + chrono::Duration::milliseconds(1),
                session: SessionId::new(), // Lagged carries an unrelated, freshly-minted id
                agent: AgentId::new(),
                event: Event::Lagged { skipped: 3 },
            },
            // A genuinely NEW turn's live text for the SAME agent, arriving
            // after the lag -- must be delivered, not suppressed.
            Envelope {
                seq: 1,
                ts: record_ts + chrono::Duration::milliseconds(2),
                session,
                agent,
                event: Event::TextDelta {
                    text: "Second turn reply".into(),
                },
            },
        ]);

        let mut stream = EventStream::replay_then_live(session, None, replay, subscribed_at, live);

        let first = next(&mut stream).await.expect("replay envelope");
        assert!(matches!(&first.event, Event::TextDelta { text } if text == "First turn reply"));

        let second = next(&mut stream).await.expect("stream ended early");
        assert!(
            matches!(second.event, Event::Lagged { .. }),
            "the Lagged envelope must be forwarded, never suppressed; got {:?}",
            second.event
        );

        let third = next(&mut stream).await.expect("stream ended early");
        assert!(
            matches!(&third.event, Event::TextDelta { text } if text == "Second turn reply"),
            "the Lagged notice must have cleared the suppression, so the new turn's real text \
             must be delivered, not silently dropped; got {:?}",
            third.event
        );
    }

    /// Regression guard for `poll_next`'s deliberate non-short-circuit
    /// evaluation of `is_live_duplicate` and `suppress_turn_tail` (see the
    /// inline comment at the `is_dup`/`is_suppressed` call site): a single
    /// live envelope can simultaneously be a `pending` content-duplicate
    /// AND the boundary marker that clears `suppress_turn_tail` for its
    /// agent. If a future edit reintroduced `||` short-circuiting
    /// (`is_live_duplicate(&envelope) || suppress_turn_tail(&envelope)`),
    /// the second call would never run once the first returned `true` --
    /// silently breaking the boundary clear and reintroducing
    /// over-suppression of the agent's NEXT, genuinely new turn, with
    /// nothing to catch it.
    ///
    /// Constructs exactly that overlap: the replay batch, in the overlap
    /// window, contains for the SAME agent BOTH an `Assistant`-derived
    /// `TextDelta` (seeds `suppress_turn_tail`) and an `AgentFinished`
    /// (seeds `pending` via `has_live_twin`) -- a turn that both replied
    /// and completed the agent's run (no tool calls), so both a text
    /// duplicate and a result duplicate are in flight for the same
    /// occurrence. On the live side: a duplicate tail chunk of the
    /// replayed turn (must be suppressed), then the live `AgentFinished`
    /// twin -- BOTH the `pending` content-duplicate to dedup AND the
    /// boundary marker that must clear this agent's suppression -- then a
    /// NEW turn's `TextDelta` (must be delivered, proving the
    /// boundary-clear fired even though the very same envelope was also
    /// content-deduped).
    #[tokio::test]
    async fn replay_then_live_boundary_marker_both_dedups_and_clears_suppression() {
        let session = SessionId::new();
        let agent = AgentId::new();

        let subscribed_at = chrono::Utc::now();
        let record_ts = subscribed_at + chrono::Duration::milliseconds(1);
        let finished_event = fixture_agent_finished(session, agent);

        let replay = vec![
            Envelope {
                seq: 999,
                ts: record_ts,
                session,
                agent,
                event: Event::TextDelta {
                    text: "Full reply".into(),
                },
            },
            Envelope {
                seq: 1000,
                ts: record_ts + chrono::Duration::milliseconds(1),
                session,
                agent,
                event: finished_event.clone(),
            },
        ];

        let live = fixed_live(vec![
            // The SAME turn's still-queued live tail chunk -- must be
            // suppressed.
            Envelope {
                seq: 0,
                ts: record_ts + chrono::Duration::milliseconds(2),
                session,
                agent,
                event: Event::TextDelta {
                    text: "tail".into(),
                },
            },
            // The live twin of the replayed AgentFinished: simultaneously a
            // `pending` content-duplicate AND the boundary marker.
            Envelope {
                seq: 1,
                ts: record_ts + chrono::Duration::milliseconds(3),
                session,
                agent,
                event: finished_event.clone(),
            },
            // A genuinely NEW turn's live text -- must be delivered.
            Envelope {
                seq: 2,
                ts: record_ts + chrono::Duration::milliseconds(4),
                session,
                agent,
                event: Event::TextDelta {
                    text: "Second turn reply".into(),
                },
            },
        ]);

        let mut stream = EventStream::replay_then_live(session, None, replay, subscribed_at, live);

        let first = next(&mut stream).await.expect("replay envelope");
        assert!(matches!(&first.event, Event::TextDelta { text } if text == "Full reply"));

        let second = next(&mut stream).await.expect("replay envelope");
        assert_eq!(
            second.event, finished_event,
            "the replayed copy of AgentFinished must be yielded"
        );

        let third = next(&mut stream).await.expect("stream ended early");
        assert!(
            matches!(&third.event, Event::TextDelta { text } if text == "Second turn reply"),
            "the duplicate tail chunk must be suppressed and the live AgentFinished twin must \
             be deduped by content -- both dropped -- so the boundary-clear it also carries \
             must still have fired, making the NEW turn's text the very next envelope; got {:?}",
            third.event
        );
    }

    /// Completeness check: the boundary branch of `suppress_turn_tail` must
    /// not depend on having first observed a suppressed live `TextDelta` --
    /// it must clear correctly even when the boundary marker is the very
    /// first live envelope after the replay batch (an empty live tail, e.g.
    /// the subscribe-then-read race window closed with nothing further
    /// queued before the turn's boundary was broadcast).
    #[tokio::test]
    async fn replay_then_live_boundary_clears_suppression_with_no_intervening_tail() {
        let session = SessionId::new();
        let agent = AgentId::new();

        let subscribed_at = chrono::Utc::now();
        let record_ts = subscribed_at + chrono::Duration::milliseconds(1);

        let replay = vec![Envelope {
            seq: 999,
            ts: record_ts,
            session,
            agent,
            event: Event::TextDelta {
                text: "Full reply".into(),
            },
        }];

        let live = fixed_live(vec![
            // The boundary itself, with NO suppressed live TextDelta chunk
            // preceding it.
            Envelope {
                seq: 0,
                ts: record_ts + chrono::Duration::milliseconds(1),
                session,
                agent,
                event: fixture_turn_finished(),
            },
            // A new turn's text -- must be delivered.
            Envelope {
                seq: 1,
                ts: record_ts + chrono::Duration::milliseconds(2),
                session,
                agent,
                event: Event::TextDelta {
                    text: "Second turn reply".into(),
                },
            },
        ]);

        let mut stream = EventStream::replay_then_live(session, None, replay, subscribed_at, live);

        let first = next(&mut stream).await.expect("replay envelope");
        assert!(matches!(&first.event, Event::TextDelta { text } if text == "Full reply"));

        let second = next(&mut stream).await.expect("stream ended early");
        assert!(
            matches!(second.event, Event::TurnFinished { .. }),
            "the boundary marker itself must be forwarded; got {:?}",
            second.event
        );

        let third = next(&mut stream).await.expect("stream ended early");
        assert!(
            matches!(&third.event, Event::TextDelta { text } if text == "Second turn reply"),
            "the boundary must clear suppression even with zero intervening suppressed deltas; \
             got {:?}",
            third.event
        );
    }
}
