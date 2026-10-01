//! Board item `01M1YVF4X864GKSGZM4PSCTMEH`, round-1 review fix (CRITICAL):
//! the `@`-mention/plain-`Tab` path completion's own background walk.
//!
//! `AppState::sync_mention`/`request_path_scan_warm_up` run on `input::
//! handle_key`'s own call stack, synchronous inside `App::run`'s
//! `tokio::select!` loop -- they decide a walk is NEEDED (a cache miss) but
//! never perform it themselves, leaving an `AppState::
//! take_pending_mention_scan_request` for this module's [`App::
//! spawn_mention_scan`] to act on. This mirrors `provider_status.rs`'s own
//! "spawn off-loop, reply over a dedicated `mpsc` channel" shape exactly
//! (see that module's own doc) -- the SAME reasoning `app/startup.rs::
//! read_git_branch` already gives for its own single `git` call:
//! `tokio::task::spawn_blocking` is what keeps a slow/contended filesystem
//! (or, here, a slow/contended `git` subprocess) from ever stalling the
//! render loop.

use std::path::PathBuf;
use std::time::Duration;

use crate::tui::mentions::{self, PathScan};
use crate::tui::state::MentionScanRequest;

use super::App;

/// One background walk's reply -- see this module's own doc for the
/// spawn/channel shape this rides on.
pub(super) struct MentionScanDone {
    anchor: usize,
    root: PathBuf,
    scan: PathScan,
}

/// A further, belt-and-suspenders bound on top of [`mentions::scan_paths`]'s
/// own internal caps (`GIT_SUBPROCESS_TIMEOUT`/`MANUAL_WALK_TIME_BUDGET`):
/// this is what guarantees `App::run`'s loop gets SOME reply within a
/// bounded time even if `scan_paths` itself were ever changed in a way that
/// stopped honoring its own caps -- a deliberate second layer, not a
/// reason to trust the inner ones less.
const MENTION_SCAN_HARD_TIMEOUT: Duration = Duration::from_secs(2);

impl App {
    /// Drains whatever `AppState::take_pending_mention_scan_request`
    /// returns (a no-op when `None`) and, if a request was queued, spawns
    /// it immediately. `Self::run`'s loop calls this unconditionally after
    /// EVERY key and paste it dispatches -- cheap (an `Option::take` on a
    /// miss), and the single call site both `input::handle_key` and
    /// `input::handle_paste`'s own post-dispatch code funnel through, so a
    /// future third input-mutating entry point cannot forget to wire this
    /// up the way `AppState::sync_palette_stem` is already the ONE funnel
    /// for re-anchoring the mention itself.
    pub(super) fn kick_off_pending_mention_scan(&mut self) {
        if let Some(request) = self.state.take_pending_mention_scan_request() {
            self.spawn_mention_scan(request);
        }
    }

    /// Spawns `request`'s walk off this loop and arranges for its result to
    /// arrive over `self.mention_scan_tx` -- never awaited here, so this
    /// method itself returns immediately regardless of how long the walk
    /// takes. Called from `Self::run`'s own key/paste-dispatch sites,
    /// right after draining `AppState::take_pending_mention_scan_request`.
    pub(super) fn spawn_mention_scan(&self, request: MentionScanRequest) {
        let tx = self.mention_scan_tx.clone();
        let root = request.root.clone();
        let anchor = request.anchor;
        tokio::spawn(async move {
            let walk_root = root.clone();
            let walk = tokio::task::spawn_blocking(move || {
                mentions::scan_paths(&walk_root, mentions::DEFAULT_SCAN_CAP)
            });
            let scan = match tokio::time::timeout(MENTION_SCAN_HARD_TIMEOUT, walk).await {
                Ok(Ok(scan)) => scan,
                // Either the hard timeout fired (the blocking task is still
                // running on its own thread in that case -- `scan_paths`'s
                // own internal caps should make this unreachable in
                // practice, but this is the belt-and-suspenders half of
                // that promise) or the blocking task itself panicked.
                // Either way, the operator gets an honestly-`capped`,
                // empty-for-now answer rather than the overlay hanging on
                // "scanning..." forever.
                _ => PathScan {
                    paths: Vec::new(),
                    capped: true,
                },
            };
            // The receiver only goes away once `App::run`'s loop has
            // already exited -- nothing left to notify, mirroring
            // `provider_status.rs`'s own identical send site.
            let _ = tx.send(MentionScanDone { anchor, root, scan });
        });
    }

    /// Applies one [`MentionScanDone`] reply -- delegates straight to
    /// `AppState::apply_mention_scan_result`, which owns the actual
    /// cache-update/still-open-mention-update split (this method is just
    /// the channel's own landing site, mirroring `App::
    /// apply_provider_status_done`'s identical shape).
    pub(super) fn apply_mention_scan_done(&mut self, done: MentionScanDone) {
        self.state
            .apply_mention_scan_result(done.anchor, done.root, done.scan);
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{echo_conway, minimal_cli};
    use super::App;

    /// End-to-end proof that a queued request really does come back over
    /// the channel and land via `apply_mention_scan_done` -- the SAME round
    /// trip `App::run`'s own `select!` arm performs, just driven by hand
    /// here (no real terminal/event loop needed) the way `provider_status.
    /// rs`'s own tests drive `refresh_reaches_a_non_local_credentialed_
    /// backend_and_never_reports_not_probed`.
    #[tokio::test]
    async fn a_spawned_scan_reports_back_over_the_channel_and_populates_the_mention() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let root =
            std::env::temp_dir().join(format!("conway-mention-scan-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("tempdir must be creatable");
        std::fs::write(root.join("main.rs"), "fn main() {}").expect("fixture file");
        app.state.mention_scan_root = root.clone();

        app.state.input = "@mai".to_string();
        app.state.cursor = app.state.input.chars().count();
        app.state.sync_mention(false);
        let request = app
            .state
            .take_pending_mention_scan_request()
            .expect("a cache-miss mention must queue a scan request");

        app.spawn_mention_scan(request);

        let mut rx = app.mention_scan_rx.take().expect("set by App::new");
        let done = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("the scan must report back well within 5s")
            .expect("the channel must not close while the sender is alive");
        app.apply_mention_scan_done(done);

        assert_eq!(app.state.mention_matches(), vec!["main.rs"]);

        let _ = std::fs::remove_dir_all(&root);
    }
}
