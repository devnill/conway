//! Mailboxes: bounded per-agent inbox, oldest-drop overflow policy, and the
//! primitives `AgentLoop::drain_inbox` (`agent_loop.rs`, WI-085) uses to give
//! steering its turn-boundary landing guarantee "by construction"
//! (architecture §6.2/§6.3).
//!
//! ## Why not `tokio::sync::mpsc`
//!
//! `mpsc::Sender::send` blocks on a full channel and `try_send` alone can't
//! give exact oldest-drop semantics without a side buffer anyway (§6.2: "a
//! stuck child must not deadlock its parent" -- the sender must never
//! block). The inbox is instead a plain bounded ring (`VecDeque`) behind a
//! `std::sync::Mutex`, held only for the pointer moves needed to push/evict
//! or drain -- never across an `.await`.
//!
//! ## `Mailbox::new` vs. `MailboxSender::with_events`
//!
//! The binding criterion is `Mailbox::new(capacity: usize) ->
//! (MailboxSender, MailboxReceiver)` -- deliberately just the bounded queue,
//! with no `EventBus`/`AgentId`/`CancellationToken` dependency. Runtime
//! wiring (`runtime.rs`, `subagent.rs`) attaches those separately via
//! [`MailboxSender::with_events`], which is what turns plain enqueue/evict
//! into the observable `Event::SteerQueued` / `Event::SteerDropped` /
//! `Event::MessageSent` stream and gives a hard `Cancel` a token to trip.
//! Tests that only care about the queue's own bound/eviction shape can use
//! a bare `Mailbox::new` pair with no events attached at all.
//!
//! ## Hard cancel is enqueue-time, not drain-time
//!
//! `AgentMessage::Cancel { hard: true, .. }` trips the attached
//! `CancellationToken` synchronously, inside [`MailboxSender::send`] --
//! before that call returns, not at the next [`MailboxReceiver::drain`].
//! Waiting for a drain would be too late by definition: the whole point of
//! a hard cancel is to interrupt a turn that is already in flight, which by
//! construction will not reach the top of the loop (where `drain` is
//! called) for an arbitrary amount of time. `Cancel { hard: false }` has no
//! such urgency and is classified at drain time like every other message
//! (see [`classify`]).
//!
//! ## `AgentMessage::Result`: a BLOCKING waiter resolves via `AgentTree`,
//! everyone else observes the persisted record
//!
//! Cycle-2 review (F-085 S2): an earlier revision of this module shipped a
//! `PendingSubagents` map (`AgentId` -> `oneshot::Sender<AgentResult>`) and
//! a `resolve_pending_subagent` helper, meant to resolve a parent's pending
//! `conway_fork`/`conway_spawn` tool call when a child's `Result` drained. Nothing in
//! production ever populated that map: `conway-tools`' subagent wait path
//! resolves exclusively through `SubagentHost::await_result` ->
//! `AgentTree::await_result` (WI-083, `tree.rs`), a `watch`-channel-backed
//! wait that is strictly more robust than a per-drain `oneshot` map entry
//! would have been -- it survives a panic or a deadline-driven synthesis
//! (`supervisor.rs`), and it does not require the waiter to have registered
//! before the drain that would resolve it races in. That machinery has been
//! removed rather than wired up to an invented consumer, and
//! `AgentTree::await_result` remains untouched by everything below -- a
//! caller that DID block on a specific child by id keeps resolving exactly
//! as before.
//!
//! What WAS missing (board item 01KZQHY6RTMYR4BRDTMQFP9J9R): a parent that
//! started several children and never blocked on any one of them by id had
//! no way to learn that any had finished -- the child's `AgentMessage::
//! Result` landed in the parent's mailbox, got classified, and was
//! discarded. `classify` now maps `AgentMessage::Result` to
//! [`DrainEffect::Persist`] carrying a fresh `LogRecord::
//! ChildResultRecord` -- the exact same path `AgentMessage::Steer` already
//! takes to become `LogRecord::ParentSteer` (see this module's own
//! `AgentMessage::Steer` arm, and `AgentLoop::drain_inbox`'s single
//! `DrainEffect::Persist` arm, which needed no change at all). The parent
//! observes the child's completion on its own very next turn's ordinary
//! `SessionStore::read` -- `context::builder`'s `own_segment` turns the
//! record into a `Role::System` segment tagged `Provenance::ChildResult {
//! from }`, never anything parent-authored (P-2). No new primitive, no
//! public signature change, and `await_result`'s blocking path is entirely
//! untouched -- this is purely an ADDITIONAL, non-blocking notification
//! path for the case `await_result` was never built to cover.
//!
//! ## Overflow policy: only an evicted `Steer` is `Event::SteerDropped`
//!
//! Cycle-2 review (F-085 S3): a full inbox's oldest entry is evicted
//! regardless of kind (see [`MailboxSender::send`]), but only when the
//! EVICTED message is itself a `Steer` does that eviction produce
//! `Event::SteerDropped` -- reporting a steer as dropped when what was
//! actually evicted was, say, a queued soft `Cancel` would be misleading to
//! any consumer (an IDE, an embedder) rendering that event. Eviction of any
//! other kind is instead logged via `tracing::warn` (naming the evicted
//! kind), not evented -- there is no `conway-core` event shaped for "a
//! non-steer message was silently dropped", and adding one is out of this
//! item's scope.
//!
//! Queued refinement question (not addressed here): a `Cancel` arguably
//! should be exempt from overflow eviction entirely, soft or hard -- losing
//! a caller's intent to stop an agent is a worse failure mode than losing a
//! steer, and unlike a steer, a dropped `Cancel` has no visible signal at
//! all once this module stops reporting eviction-by-kind. This would
//! require the ring to treat `Cancel` as un-evictable (or to evict some
//! other entry ahead of it), which is a real queue-policy redesign this
//! item does not attempt.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use conway_core::agent::{AgentMessage, MessageKind};
use conway_core::event::Event;
use conway_core::ids::{AgentId, LogSeq, SessionId};
use conway_core::log::LogRecord;
use conway_core::provenance::Provenance;
use tokio_util::sync::CancellationToken;

use crate::events::EventBus;

/// The inbox capacity every real agent in the runtime uses (WI-085
/// criterion: `Mailbox::new(capacity)` with runtime capacity 64).
pub const RUNTIME_CAPACITY: usize = 64;

/// The bounded, mutex-guarded ring shared by one `MailboxSender`/
/// `MailboxReceiver` pair.
struct Inbox {
    q: Mutex<VecDeque<AgentMessage>>,
    cap: usize,
}

/// The event-emitting side channel a [`MailboxSender`] may carry, attached
/// via [`MailboxSender::with_events`]. Absent for a bare `Mailbox::new`
/// pair (e.g. a unit test exercising only the queue's own bound).
#[derive(Clone)]
struct MailboxEvents {
    bus: Arc<EventBus>,
    session: SessionId,
    /// The mailbox's own owning agent -- the `target`/`to` stamped on every
    /// event this sender emits, regardless of who called `send`.
    target: AgentId,
    /// Tripped synchronously by a hard `Cancel` (see the module doc).
    cancel: CancellationToken,
}

/// Constructs one bounded mailbox pair. The only required input is the
/// capacity -- see the module doc for why `EventBus`/`AgentId`/
/// `CancellationToken` wiring is a separate, optional step
/// ([`MailboxSender::with_events`]).
pub struct Mailbox;

impl Mailbox {
    /// Criterion-pinned signature (WI-085): `Mailbox` itself is never
    /// constructed -- it exists only to namespace this constructor, so `new`
    /// returns the sender/receiver pair rather than `Self`.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(capacity: usize) -> (MailboxSender, MailboxReceiver) {
        let inbox = Arc::new(Inbox {
            q: Mutex::new(VecDeque::with_capacity(capacity.min(1024))),
            cap: capacity,
        });
        (
            MailboxSender {
                inbox: inbox.clone(),
                events: None,
            },
            MailboxReceiver { inbox },
        )
    }
}

/// The sending half of one agent's mailbox. Cheap to clone: every clone
/// enqueues into the same underlying bounded ring, so any number of callers
/// (an embedder, a `conway_fork`/`conway_spawn` tool, a sibling, a child steering its
/// own parent -- steer is bidirectional, Claude Code-style) can hold one
/// concurrently.
#[derive(Clone)]
pub struct MailboxSender {
    inbox: Arc<Inbox>,
    events: Option<MailboxEvents>,
}

impl MailboxSender {
    /// Attaches event-emission and hard-cancel wiring. Runtime callers
    /// (`runtime.rs`, `subagent.rs`) always call this immediately after
    /// [`Mailbox::new`]; it exists as a separate step only so the
    /// criterion-pinned `Mailbox::new(capacity)` signature stays exactly
    /// that.
    pub fn with_events(
        mut self,
        bus: Arc<EventBus>,
        session: SessionId,
        target: AgentId,
        cancel: CancellationToken,
    ) -> Self {
        self.events = Some(MailboxEvents {
            bus,
            session,
            target,
            cancel,
        });
        self
    }

    /// Enqueues `msg`. Never blocks: a full inbox evicts the oldest entry
    /// first -- reported as `Event::SteerDropped` only when the EVICTED
    /// entry was itself a `Steer` (any other evicted kind is logged via
    /// `tracing::warn` instead; see the module doc's "Overflow policy"
    /// section for why) -- see §6.2, "a stuck child must not deadlock its
    /// parent, or its parent it". Every accepted message is reported as
    /// `Event::MessageSent`; an accepted `Steer` is additionally reported
    /// as `Event::SteerQueued` at this exact enqueue instant (§6.3: the
    /// queued-since timestamp is what lets a consumer render "steer
    /// pending"). A hard `Cancel` trips the attached `CancellationToken`
    /// synchronously, before the message is even pushed -- see the module
    /// doc.
    pub fn send(&self, msg: AgentMessage) {
        if let AgentMessage::Cancel { hard: true, .. } = &msg {
            if let Some(events) = &self.events {
                events.cancel.cancel();
            }
        }

        let is_steer = matches!(msg, AgentMessage::Steer { .. });
        let kind = MessageKind::from(&msg);

        let evicted = {
            let mut q = self.inbox.q.lock().expect("mailbox queue poisoned");
            let evicted = if q.len() >= self.inbox.cap {
                q.pop_front()
            } else {
                None
            };
            q.push_back(msg);
            evicted
        };

        if let Some(evicted_msg) = &evicted {
            if matches!(evicted_msg, AgentMessage::Steer { .. }) {
                if let Some(events) = &self.events {
                    events.bus.emit(
                        events.session,
                        events.target,
                        Event::SteerDropped {
                            target: events.target,
                            reason: "mailbox full: oldest message dropped".to_string(),
                        },
                    );
                }
            } else {
                tracing::warn!(
                    evicted_kind = ?MessageKind::from(evicted_msg),
                    "mailbox full: evicted a non-Steer message (not reported as \
                     Event::SteerDropped -- see mailbox.rs's module doc)"
                );
            }
        }

        if let Some(events) = &self.events {
            events.bus.emit(
                events.session,
                events.target,
                Event::MessageSent {
                    to: events.target,
                    kind,
                },
            );
            if is_steer {
                events.bus.emit(
                    events.session,
                    events.target,
                    Event::SteerQueued {
                        target: events.target,
                        queued_since: Utc::now(),
                    },
                );
            }
        }
    }
}

/// The receiving half of one agent's mailbox. Owned by that agent's
/// `AgentLoop` alone -- draining is never shared (mirroring
/// `tokio::mpsc::Receiver`'s single-consumer shape), even though the
/// underlying ring is a plain `Arc` under the hood.
pub struct MailboxReceiver {
    inbox: Arc<Inbox>,
}

impl MailboxReceiver {
    /// Takes every message currently queued, in FIFO order, without
    /// waiting for more to arrive -- this crate's turn-boundary drain is a
    /// point-in-time snapshot, not a subscription (`AgentLoop::drain_inbox`
    /// calls this once per turn boundary; an empty inbox yields an empty
    /// `Vec`, never a block).
    pub fn drain(&mut self) -> Vec<AgentMessage> {
        let mut q = self.inbox.q.lock().expect("mailbox queue poisoned");
        std::mem::take(&mut *q).into_iter().collect()
    }
}

/// One drained message's effect on the loop, once classified
/// (`AgentLoop::drain_inbox`, architecture §6.2). Kept here, not inlined in
/// `agent_loop.rs`, so the exact same classification backs both the real
/// loop and `tests/steering.rs`'s direct, `AgentLoop`-free coverage of it.
// `Persist(LogRecord)` is ~288 bytes against a 24-byte second-largest variant,
// which trips `clippy::large_enum_variant`. Boxing it is NOT the right answer
// here, and this is a deliberate ruling rather than a suppression of
// convenience.
//
// The lint is about performance, and the cost it names is not on a hot path.
// `AgentLoop::drain_inbox` runs once per turn, and `classify` once per queued
// message inside it; a message is a steer, a cancel or a progress note from a
// parent, arriving at a turn boundary. That is zero or one construction per
// turn in ordinary use, of a value that lives for microseconds on the stack.
// Boxing would trade that for a heap allocation per steer.
//
// This project's rule is to measure a baseline before an efficiency change and
// gate the change on it demonstrating value, and to treat "cannot be measured"
// as an argument against shipping rather than a reason to skip the step. There
// is no benchmark here and no measurement showing the stack size matters, so
// boxing would be exactly the unmeasured optimization that rule forbids.
//
// REOPEN THIS if `DrainEffect` acquires a construction site inside a hot loop
// -- per-token, per-tool-call, or per-agent in a wide fan-out -- or if a
// measurement shows the stack cost is real. Board item
// 01KZW92NWQNSR4SYZ2RXQ18XKG records the reasoning and what would change it.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum DrainEffect {
    /// Persist as `LogRecord::ParentSteer` -- appended by the caller before
    /// continuing, never used to build a context segment directly. This is
    /// what "no code path injects into a context outside `drain_inbox`"
    /// means structurally: the *only* way a steer becomes visible is by
    /// first becoming a stored record, read back like any other record on
    /// the next `SessionStore::read` (see `agent_loop.rs`'s own structural
    /// test for this).
    Persist(LogRecord),
    /// Consumed by the loop's own top-of-turn cancel check; takes effect at
    /// the next turn boundary once the in-flight tool batch (if any)
    /// completes.
    SoftCancel { reason: String },
    /// Already handled at enqueue time (`MailboxSender::send` trips the
    /// token directly, see the module doc) -- nothing left to do at drain.
    HardCancelAcknowledged,
    /// Never becomes a record or a context segment (§6.2: unsolicited
    /// child chatter in a parent's context is the "context clash" failure
    /// mode) -- the caller emits `Event::AgentProgress` and moves on.
    Progress { note: String },
    /// A future `AgentMessage` variant this crate doesn't yet recognize
    /// (`AgentMessage` is `#[non_exhaustive]` in `conway-core`). Treated as
    /// inert -- mirrors `tree.rs`'s `status_for` convention of mapping an
    /// unrecognized future variant to a safe default rather than failing to
    /// compile.
    Unknown,
}

/// Classifies one drained [`AgentMessage`] (architecture §6.2's per-kind
/// handling table).
pub fn classify(msg: AgentMessage) -> DrainEffect {
    match msg {
        AgentMessage::Steer {
            from,
            text,
            at_parent_seq,
        } => DrainEffect::Persist(LogRecord::ParentSteer {
            // `SessionStore::append`'s `assign_seq` always overwrites this
            // placeholder with the store's own next value (see
            // `runtime.rs`/`subagent.rs`'s identical `LogSeq::ZERO` head
            // records) -- the store, not the caller, is the seq authority.
            seq: LogSeq::ZERO,
            ts: Utc::now(),
            text,
            from,
            parent_seq: at_parent_seq,
            prov: Provenance::ParentSteer {
                from,
                parent_seq: at_parent_seq,
            },
        }),
        AgentMessage::Cancel {
            hard: false,
            reason,
            ..
        } => DrainEffect::SoftCancel { reason },
        AgentMessage::Cancel { hard: true, .. } => DrainEffect::HardCancelAcknowledged,
        AgentMessage::Progress { note, .. } => DrainEffect::Progress { note },
        // Board item 01KZQHY6RTMYR4BRDTMQFP9J9R: a drained
        // `AgentMessage::Result` now persists into THIS agent's (the
        // parent's) own log, the same `DrainEffect::Persist` path
        // `AgentMessage::Steer` already takes -- see the module doc. `from`
        // and `result.agent_id` are always equal (the child's own
        // `AgentLoop::finish` sets both from `self.agent_id`); `from` is
        // carried at the top level too, mirroring `ParentSteer`, so a
        // reader can identify the originating child without reaching into
        // `result`.
        AgentMessage::Result { from, result } => {
            DrainEffect::Persist(LogRecord::ChildResultRecord {
                // `SessionStore::append`'s `assign_seq` always overwrites this
                // placeholder -- see the identical comment on the `Steer` arm
                // above.
                seq: LogSeq::ZERO,
                ts: Utc::now(),
                result,
                prov: Provenance::ChildResult { from },
            })
        }
        _ => DrainEffect::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway_core::agent::{AgentResult, ResultStatus};

    fn steer(from: AgentId, text: &str) -> AgentMessage {
        AgentMessage::Steer {
            from,
            text: text.to_string(),
            at_parent_seq: LogSeq::ZERO,
        }
    }

    #[test]
    fn new_pair_has_the_requested_capacity_and_never_blocks_on_overflow() {
        let (tx, mut rx) = Mailbox::new(4);
        for i in 0..10 {
            tx.send(steer(AgentId::new(), &format!("msg {i}")));
        }
        let drained = rx.drain();
        assert_eq!(drained.len(), 4, "oldest entries must have been evicted");
    }

    #[test]
    fn drain_is_a_point_in_time_snapshot() {
        let (tx, mut rx) = Mailbox::new(RUNTIME_CAPACITY);
        assert!(rx.drain().is_empty());
        tx.send(steer(AgentId::new(), "a"));
        tx.send(steer(AgentId::new(), "b"));
        assert_eq!(rx.drain().len(), 2);
        assert!(rx.drain().is_empty());
    }

    #[test]
    fn hard_cancel_trips_the_token_synchronously_without_a_drain() {
        let (tx, _rx) = Mailbox::new(RUNTIME_CAPACITY);
        let cancel = CancellationToken::new();
        let bus = EventBus::new(16);
        let target = AgentId::new();
        let tx = tx.with_events(bus, SessionId::new(), target, cancel.clone());

        assert!(!cancel.is_cancelled());
        tx.send(AgentMessage::Cancel {
            from: AgentId::new(),
            reason: "urgent".to_string(),
            hard: true,
        });
        assert!(cancel.is_cancelled());
    }

    #[test]
    fn soft_cancel_does_not_trip_the_token() {
        let (tx, _rx) = Mailbox::new(RUNTIME_CAPACITY);
        let cancel = CancellationToken::new();
        let bus = EventBus::new(16);
        let target = AgentId::new();
        let tx = tx.with_events(bus, SessionId::new(), target, cancel.clone());

        tx.send(AgentMessage::Cancel {
            from: AgentId::new(),
            reason: "please stop soon".to_string(),
            hard: false,
        });
        assert!(!cancel.is_cancelled());
    }

    #[test]
    fn classify_maps_every_message_kind() {
        let from = AgentId::new();
        assert!(matches!(
            classify(steer(from, "hi")),
            DrainEffect::Persist(LogRecord::ParentSteer { .. })
        ));
        assert!(matches!(
            classify(AgentMessage::Cancel {
                from,
                reason: "r".into(),
                hard: false
            }),
            DrainEffect::SoftCancel { .. }
        ));
        assert!(matches!(
            classify(AgentMessage::Cancel {
                from,
                reason: "r".into(),
                hard: true
            }),
            DrainEffect::HardCancelAcknowledged
        ));
        assert!(matches!(
            classify(AgentMessage::Progress {
                from,
                note: "n".into()
            }),
            DrainEffect::Progress { .. }
        ));
        let result = AgentResult::new(from, SessionId::new(), ResultStatus::Completed, "done");
        assert!(matches!(
            classify(AgentMessage::Result {
                from,
                result: result.clone()
            }),
            DrainEffect::Persist(LogRecord::ChildResultRecord { .. })
        ));
    }

    /// Board item 01KZQHY6RTMYR4BRDTMQFP9J9R: the persisted record carries
    /// the originating child's id (both at the top level and inside
    /// `result`) and its provenance is `ChildResult`, never anything that
    /// would misattribute the child's output as parent-authored (P-2).
    #[test]
    fn result_classifies_to_a_child_result_record_naming_the_child() {
        let child = AgentId::new();
        let result = AgentResult::new(
            child,
            SessionId::new(),
            ResultStatus::Completed,
            "child done",
        );
        match classify(AgentMessage::Result {
            from: child,
            result: result.clone(),
        }) {
            DrainEffect::Persist(LogRecord::ChildResultRecord {
                result: r, prov, ..
            }) => {
                assert_eq!(r.agent_id, child);
                assert_eq!(r, result);
                match prov {
                    Provenance::ChildResult { from } => assert_eq!(from, child),
                    other => panic!("expected Provenance::ChildResult, got {other:?}"),
                }
            }
            other => panic!("expected DrainEffect::Persist(ChildResultRecord), got {other:?}"),
        }
    }
}
