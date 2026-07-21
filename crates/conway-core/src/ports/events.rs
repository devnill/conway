//! The `EventSink` port: the runtime's single fan-out point for the event
//! stream (architecture §8).

use std::sync::Arc;

use crate::event::Event;

/// Receives every event a running agent tree emits.
///
/// `emit` is synchronous and non-blocking **by contract**: implementations
/// back it with something like a broadcast channel and must never block the
/// caller. A slow consumer must be dropped from delivery and see
/// `Event::Lagged { skipped }` on its next successful receive rather than
/// stalling the runtime (architecture §8).
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, event: Event);
}

/// A shared handle to an `EventSink`, threaded through `ToolCtx` and the
/// runtime.
pub type EventSinkHandle = Arc<dyn EventSink>;
