//! Terminal attention notifications: the two methods that turn
//! [`AppState::pending_attention`](crate::tui::state::AppState) into real
//! bytes on the real terminal. [`App::drain_attention_queue`] is the ONE
//! call site -- `app/run.rs`'s loop body, run after every `select!` arm
//! (see that call site's own comment) -- that ever calls
//! [`App::maybe_notify`]; nothing else should, or the "one notification per
//! event, not per redraw" guarantee the queue exists to provide would stop
//! being true.
//!
//! Split out of `app.rs` for the same reason `focus.rs`/`mention_scan.rs`
//! are: a small, independently testable seam with its own module doc,
//! rather than more inline code on the already-large `run` loop.

use super::App;
use crate::tui::config::AttentionEvent;

impl App {
    /// Drains every event [`AppState::pending_attention`](crate::tui::state::AppState)
    /// has queued since the last drain, applying [`Self::maybe_notify`] to
    /// each in order. Pops before processing (not left in place for a
    /// failed write to retry) -- a best-effort notification that failed to
    /// write is not worth re-attempting on the next event, the same
    /// best-effort posture `crate::tui::attention::AttentionWriter`'s own
    /// doc states for the write itself.
    pub(super) fn drain_attention_queue(&mut self) {
        while let Some(event) = self.state.pending_attention.pop_front() {
            self.maybe_notify(event);
        }
    }

    /// Applies `crate::tui::attention::should_notify` (the configured
    /// method/`when`/`events` gate, against `self.state.terminal_focused`)
    /// and, when it passes, builds and writes the bytes via
    /// `crate::tui::attention::emit_bytes` -- sanitizing/truncating
    /// `event`'s own fixed phrase (`crate::tui::attention::
    /// notification_text`) and wrapping for tmux passthrough when `$TMUX`
    /// is set in `self.env` (the SAME captured environment snapshot
    /// `history_path`'s own doc already uses for ambient reads, not a
    /// fresh `std::env::var` call -- keeps this reachable by a test that
    /// constructs `self.env` directly, the same isolation
    /// `apply_marketplace_install`'s own doc already relies on).
    pub(super) fn maybe_notify(&mut self, event: AttentionEvent) {
        if !crate::tui::attention::should_notify(
            &self.state.attention,
            event,
            self.state.terminal_focused,
        ) {
            return;
        }
        let inside_tmux = self.env.contains_key("TMUX");
        let text = crate::tui::attention::notification_text(event);
        if let Some(bytes) =
            crate::tui::attention::emit_bytes(self.state.attention.method, text, inside_tmux)
        {
            self.attention_writer.write_attention(&bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{echo_conway, minimal_cli};
    use super::App;
    use crate::tui::config::{AttentionConfig, AttentionEvent, AttentionMethod, AttentionWhen};

    /// A `Vec<Vec<u8>>`-backed sink, shared via `Arc<Mutex<_>>` (NOT
    /// `Rc<RefCell<_>>` -- `AttentionWriter: Send`, and `Rc` is not `Send`,
    /// so a `Rc`-backed sink would fail that bound) so the test can both
    /// hand ownership of one half to `App` (behind the `Box<dyn
    /// AttentionWriter>` field) and keep reading the other -- one entry per
    /// [`crate::tui::attention::AttentionWriter::write_attention`] call, so
    /// a test can assert BOTH the exact bytes AND the call count (the
    /// debounce requirement: one notification per event, not per redraw).
    struct FakeWriter(std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>);

    impl crate::tui::attention::AttentionWriter for FakeWriter {
        fn write_attention(&mut self, bytes: &[u8]) {
            self.0
                .lock()
                .expect("the sink's mutex is never poisoned in these tests")
                .push(bytes.to_vec());
        }
    }

    /// Swaps `app`'s real `attention_writer` for a [`FakeWriter`] and
    /// returns the shared sink this test reads back through --
    /// private-field access already used throughout this crate's own test
    /// modules (e.g. `app.state`, `app.env` -- `App`'s own field doc on
    /// `env`/`cwd`).
    fn with_fake_writer(app: &mut App) -> std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>> {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        app.attention_writer = Box::new(FakeWriter(captured.clone()));
        captured
    }

    async fn app_with_fake_writer() -> (App, std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>) {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        let captured = with_fake_writer(&mut app);
        (app, captured)
    }

    /// Acceptance: `turn_finished`, unfocused (the default `when`), emits
    /// the exact default bell byte.
    #[tokio::test]
    async fn turn_finished_while_unfocused_emits_the_default_bell() {
        let (mut app, captured) = app_with_fake_writer().await;
        app.state.terminal_focused = Some(false);
        app.state.pending_attention.push_back(AttentionEvent::TurnFinished);

        app.drain_attention_queue();

        assert_eq!(
            *captured.lock().unwrap(),
            vec![vec![0x07u8]],
            "the default config must emit exactly one bare BEL"
        );
    }

    /// Acceptance: the SAME event, while focused, emits nothing -- the
    /// default `when = "unfocused"` gate.
    #[tokio::test]
    async fn turn_finished_while_focused_emits_nothing() {
        let (mut app, captured) = app_with_fake_writer().await;
        app.state.terminal_focused = Some(true);
        app.state.pending_attention.push_back(AttentionEvent::TurnFinished);

        app.drain_attention_queue();

        assert!(
            captured.lock().unwrap().is_empty(),
            "a focused terminal must not be notified under the default config"
        );
    }

    /// Acceptance: `method = "off"` emits nothing regardless of focus or
    /// event.
    #[tokio::test]
    async fn method_off_emits_nothing() {
        let (mut app, captured) = app_with_fake_writer().await;
        app.state.attention.method = AttentionMethod::Off;
        app.state.terminal_focused = Some(false);
        app.state.pending_attention.push_back(AttentionEvent::TurnFinished);
        app.state
            .pending_attention
            .push_back(AttentionEvent::PermissionPending);

        app.drain_attention_queue();

        assert!(captured.lock().unwrap().is_empty());
    }

    /// Acceptance: `osc777` emits the documented sequence carrying the
    /// event's own (sanitized) text.
    #[tokio::test]
    async fn osc777_emits_the_documented_sequence_with_event_text() {
        let (mut app, captured) = app_with_fake_writer().await;
        app.state.attention = AttentionConfig {
            method: AttentionMethod::Osc777,
            when: AttentionWhen::Always,
            events: vec![AttentionEvent::PermissionPending],
        };
        app.state.terminal_focused = Some(true);
        app.state
            .pending_attention
            .push_back(AttentionEvent::PermissionPending);

        app.drain_attention_queue();

        let calls = captured.lock().unwrap();
        assert_eq!(calls.len(), 1, "{calls:?}");
        let text = String::from_utf8(calls[0].clone()).expect("ASCII payload");
        assert_eq!(text, "\x1b]777;notify;conway;conway: permission requested\x07");
    }

    /// Debounce: one notification per event, not per redraw. Draining the
    /// queue repeatedly with nothing newly pushed writes nothing further --
    /// simulating the 16ms redraw tick running many times between real
    /// events.
    #[tokio::test]
    async fn draining_an_empty_queue_repeatedly_writes_nothing_further() {
        let (mut app, captured) = app_with_fake_writer().await;
        app.state.terminal_focused = Some(false);
        app.state.pending_attention.push_back(AttentionEvent::TurnFinished);

        app.drain_attention_queue();
        assert_eq!(captured.lock().unwrap().len(), 1);

        // Mirrors many more `REDRAW_TICK`s firing with no new event queued.
        for _ in 0..50 {
            app.drain_attention_queue();
        }
        assert_eq!(
            captured.lock().unwrap().len(),
            1,
            "redraws with nothing newly queued must never write again"
        );
    }

    /// An event not in the configured `events` list is silently skipped --
    /// `permission_pending` is pushed, but the config only names
    /// `turn_finished`.
    #[tokio::test]
    async fn an_unconfigured_event_is_skipped() {
        let (mut app, captured) = app_with_fake_writer().await;
        app.state.attention.events = vec![AttentionEvent::TurnFinished];
        app.state.terminal_focused = Some(false);
        app.state
            .pending_attention
            .push_back(AttentionEvent::PermissionPending);

        app.drain_attention_queue();

        assert!(captured.lock().unwrap().is_empty());
    }
}
