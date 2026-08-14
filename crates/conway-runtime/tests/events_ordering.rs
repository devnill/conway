//! Event bus ordering and back-pressure guarantees (criteria).

use std::time::Duration;

use conway_core::event::Event;
use conway_core::ids::{AgentId, SessionId};
use conway_runtime::events::EventBus;
use futures::StreamExt;

/// Cycle-1 Critical regression: delivery order (not just the seq value
/// set) must be monotonic per session as RECEIVED by a subscriber, under
/// heavy concurrent same-session emission. No sorting before asserting.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn delivery_order_is_monotonic_per_session_under_concurrency() {
    use conway_core::event::Event;
    use futures::StreamExt;

    let bus = EventBus::new(65_536);
    let session = SessionId::new();
    let agent = AgentId::new();
    let mut stream = bus.subscribe();

    let emitters: Vec<_> = (0..8)
        .map(|_| {
            let bus = std::sync::Arc::clone(&bus);
            tokio::spawn(async move {
                for turn in 0..2_000u32 {
                    bus.emit(session, agent, Event::TurnStarted { turn });
                }
            })
        })
        .collect();
    for task in emitters {
        task.await.unwrap();
    }

    let mut last: Option<u64> = None;
    for _ in 0..16_000 {
        let envelope = stream.next().await.expect("stream open");
        assert!(
            !matches!(envelope.event, Event::Lagged { .. }),
            "buffer sized to hold all events; Lagged means the test is wrong"
        );
        if let Some(prev) = last {
            assert!(
                envelope.seq == prev + 1,
                "delivery order violated: received seq {} after {}",
                envelope.seq,
                prev
            );
        } else {
            assert_eq!(envelope.seq, 0);
        }
        last = Some(envelope.seq);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn eight_tasks_x_500_events_yield_exact_gapless_seq_range() {
    let bus = EventBus::new(8192);
    let session = SessionId::new();
    let mut stream = bus.subscribe();

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let bus = bus.clone();
            tokio::spawn(async move {
                let agent = AgentId::new();
                for i in 0..500u32 {
                    bus.emit(
                        session,
                        agent,
                        Event::AgentProgress {
                            note: i.to_string(),
                        },
                    );
                }
            })
        })
        .collect();

    for handle in handles {
        handle.await.expect("producer task panicked");
    }

    let mut seqs = Vec::with_capacity(4000);
    while seqs.len() < 4000 {
        let envelope = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("timed out waiting for an envelope")
            .expect("event stream ended early");
        if envelope.session == session {
            seqs.push(envelope.seq);
        }
    }

    seqs.sort_unstable();
    let expected: Vec<u64> = (0..4000).collect();
    assert_eq!(seqs, expected, "seqs must be exactly 0..4000, no dups/gaps");
}

#[tokio::test]
async fn two_sessions_have_independent_monotonic_counters_from_zero() {
    let bus = EventBus::new(64);
    let session_a = SessionId::new();
    let session_b = SessionId::new();
    let agent = AgentId::new();

    for expected in 0..10u64 {
        let seq_a = bus.emit(session_a, agent, Event::AgentProgress { note: "a".into() });
        let seq_b = bus.emit(session_b, agent, Event::AgentProgress { note: "b".into() });
        assert_eq!(seq_a, expected);
        assert_eq!(seq_b, expected);
    }
}

#[tokio::test]
async fn slow_subscriber_gets_lagged_and_emit_never_blocks_or_errors() {
    let bus = EventBus::new(4);
    let mut stream = bus.subscribe();
    let session = SessionId::new();
    let agent = AgentId::new();

    // Far more than the buffer capacity, with nobody polling `stream` yet.
    // If `emit` ever blocked on a full broadcast buffer this would hang and
    // the surrounding timeout would trip.
    tokio::time::timeout(Duration::from_millis(500), async {
        for i in 0..50u32 {
            bus.emit(
                session,
                agent,
                Event::AgentProgress {
                    note: i.to_string(),
                },
            );
        }
    })
    .await
    .expect("emit blocked on a full broadcast buffer");

    let mut saw_lagged = false;
    while let Ok(Some(envelope)) =
        tokio::time::timeout(Duration::from_millis(500), stream.next()).await
    {
        if let Event::Lagged { skipped } = envelope.event {
            assert!(skipped > 0, "Lagged must report skipped > 0");
            saw_lagged = true;
            break;
        }
    }
    assert!(saw_lagged, "slow subscriber should observe Event::Lagged");
}
