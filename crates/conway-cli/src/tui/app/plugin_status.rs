//! The live poll half of board item `01M0Y3A8MYKKE0GMYKZE1K0QTD`: keeping
//! `AppState::plugin_status_contributions` current for the life of the
//! session, not just at `App::new`'s one-time copy. Extracted into its own
//! file the same way `focus.rs`'s `refresh_session_head` is -- a small,
//! directly-unit-testable method [`super::run`]'s own `select!` loop calls
//! on a bounded tick, kept out of that file so the loop itself stays a pure
//! dispatcher (module notes on `run.rs`: it composes pieces tested on their
//! own, rather than being tested through a real PTY).

use super::App;

impl App {
    /// Re-polls every installed plugin's CURRENT status contributions via
    /// [`conway::Conway::poll_plugin_status_contributions`] and writes the
    /// result into `AppState::plugin_status_contributions` **wholesale** --
    /// replacing whatever was there, never merging into it. Returns whether
    /// the value actually changed, so [`super::run`]'s caller only marks the
    /// frame dirty (forcing a redraw) when there is something new to show.
    ///
    /// **Synchronous and non-blocking**, deliberately not `async fn` despite
    /// every sibling `refresh_*` method on this type being one:
    /// `poll_plugin_status_contributions`'s own doc guarantees it never
    /// spawns, never awaits I/O, and never blocks on a plugin's own
    /// in-flight background work -- there is nothing here for `.await` to
    /// wait on, and giving this a signature that suggested otherwise would
    /// misstate the one property this whole item exists to preserve (a slow
    /// or wedged plugin must not stall a frame). Safe to call directly
    /// inside [`super::run`]'s own `tokio::select!`, on its own tick, with no
    /// risk of stalling the other arms.
    ///
    /// **This is also the acceptance-4 fix, for free, not a special case of
    /// it.** A plugin whose contribution disappears between one poll and the
    /// next (it stops reporting) is simply absent from THIS call's result;
    /// assigning that result wholesale is what removes the stale entry from
    /// `AppState` rather than leaving the last-seen value behind forever.
    /// There is no separate "has this gone stale" check to write, and this
    /// method does not attempt one -- see `Conway::
    /// poll_plugin_status_contributions`'s own doc for why that is a
    /// sufficient fix by construction, and why it is not the same thing as
    /// the still-undone per-key `ttl_ms` sweep
    /// (`crates/conway-plugin-subprocess/src/session.rs`,
    /// `docs/plugins/hooks.md` point 12).
    pub(super) fn refresh_plugin_status_contributions(&mut self) -> bool {
        let latest = self.conway.poll_plugin_status_contributions();
        if latest == self.state.plugin_status_contributions {
            return false;
        }
        self.state.plugin_status_contributions = latest;
        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    use conway::plugin::{
        Plugin, PluginDescription, PluginManifest, PluginStatusContribution, ResultStatus, Tool,
    };
    use conway_core::ids::BackendId;

    use super::super::fixtures::{
        base_config, conway_with_contributing_plugin_and_store, minimal_cli,
    };
    use super::App;

    /// A plugin whose `status_contributions()` answer changes on every call
    /// -- proving `Conway::poll_plugin_status_contributions` re-reads the
    /// live surface rather than replaying a value captured once at startup.
    /// Call 0 (the build-time snapshot `ConwayBuilder::build` itself takes)
    /// answers empty, matching the "typically empty at session start" case
    /// this item's own spec and `Conway::plugin_status_contributions`'s doc
    /// both describe; every call after that answers a distinct value.
    struct CountingPlugin {
        calls: AtomicUsize,
    }

    impl Plugin for CountingPlugin {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                id: "test.counting".to_string(),
                version: "0.0.0".to_string(),
                tools: vec![],
                required_host_caps: vec![],
                optional_host_caps: vec![],
                requires: vec![],
                optional: vec![],
            }
        }

        fn tools(&self) -> Vec<Arc<dyn Tool>> {
            vec![]
        }

        fn status_contributions(&self) -> Vec<PluginStatusContribution> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                vec![]
            } else {
                vec![PluginStatusContribution {
                    key: "counting".to_string(),
                    status: ResultStatus::Completed,
                    value: format!("call-{n}"),
                }]
            }
        }
    }

    /// Acceptance criterion 1, at the `Conway` layer
    /// `refresh_plugin_status_contributions` sits on top of: a contribution
    /// that did not exist at `build()` time is visible through
    /// `poll_plugin_status_contributions` afterward, and a second poll sees
    /// a DIFFERENT value still -- proving this is a live re-read, not a
    /// value captured once and replayed.
    #[tokio::test]
    async fn poll_reflects_a_plugin_whose_answer_changes_after_build() {
        let conway = conway::test_support::test_builder(base_config())
            .with_backend(Arc::new(conway_testkit::FakeBackend::echo(BackendId::new(
                "fake",
            ))))
            .with_plugin(Arc::new(CountingPlugin {
                calls: AtomicUsize::new(0),
            }))
            .build()
            .expect("build should succeed with one status-contributing plugin installed");

        // The build-time snapshot (`Conway::plugin_status_contributions`)
        // is still taken from the plugin's FIRST call, exactly as before
        // this item -- unaffected by the new live poll surface.
        assert!(
            conway.plugin_status_contributions().is_empty(),
            "the build-time snapshot must still see the plugin's first (empty) answer"
        );

        let first_poll = conway.poll_plugin_status_contributions();
        assert_eq!(first_poll.len(), 1);
        assert_eq!(first_poll[0].value, "call-1");

        let second_poll = conway.poll_plugin_status_contributions();
        assert_eq!(
            second_poll[0].value, "call-2",
            "a second poll must re-invoke the plugin, not replay the first poll's answer"
        );
    }

    /// A plugin whose contribution can be toggled off, from outside --
    /// acceptance criterion 4's fixture: a plugin that STOPS reporting.
    struct FlakyPlugin {
        reporting: AtomicBool,
    }

    impl Plugin for FlakyPlugin {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                id: "test.flaky".to_string(),
                version: "0.0.0".to_string(),
                tools: vec![],
                required_host_caps: vec![],
                optional_host_caps: vec![],
                requires: vec![],
                optional: vec![],
            }
        }

        fn tools(&self) -> Vec<Arc<dyn Tool>> {
            vec![]
        }

        fn description(&self) -> PluginDescription {
            PluginDescription {
                summary: "test".to_string(),
                you_get: "test".to_string(),
                you_lose: "test".to_string(),
                costs: "test".to_string(),
            }
        }

        fn status_contributions(&self) -> Vec<PluginStatusContribution> {
            if self.reporting.load(Ordering::SeqCst) {
                vec![PluginStatusContribution {
                    key: "flaky".to_string(),
                    status: ResultStatus::Completed,
                    value: "up".to_string(),
                }]
            } else {
                vec![]
            }
        }
    }

    /// Acceptance criterion 4, at the `Conway` layer: a plugin whose
    /// contribution disappears between one poll and the next is absent
    /// from the very next poll -- no stale entry survives past one poll
    /// cycle, and no separate expiry/TTL step is needed to make that true.
    #[tokio::test]
    async fn poll_drops_a_contribution_the_instant_the_plugin_stops_reporting_it() {
        let plugin = Arc::new(FlakyPlugin {
            reporting: AtomicBool::new(true),
        });
        let conway = conway::test_support::test_builder(base_config())
            .with_backend(Arc::new(conway_testkit::FakeBackend::echo(BackendId::new(
                "fake",
            ))))
            .with_plugin(plugin.clone())
            .build()
            .expect("build should succeed with one status-contributing plugin installed");

        assert_eq!(conway.poll_plugin_status_contributions().len(), 1);

        plugin.reporting.store(false, Ordering::SeqCst);
        assert!(
            conway.poll_plugin_status_contributions().is_empty(),
            "a plugin that stops reporting must not leave a stale contribution behind on the \
             very next poll"
        );
    }

    /// [`super::App::refresh_plugin_status_contributions`] itself: proves
    /// the dirty-tracking contract this method's doc promises --
    /// `AppState::plugin_status_contributions` is only overwritten, and
    /// `true` only returned, when the live poll actually differs from what
    /// was already there. Drives the real `App` (via
    /// `super::super::fixtures::conway_with_contributing_plugin_and_store`
    /// -- a plugin installed through the real `ConwayBuilder::with_plugin`,
    /// never `AppState` set by hand), so this is the same "real plugin,
    /// real `Conway`" idiom `startup.rs`'s own
    /// `app_new_populates_plugin_status_contributions_from_a_real_plugin`
    /// test uses, one layer further into the session's life.
    #[tokio::test]
    async fn refresh_returns_false_and_leaves_state_alone_when_the_poll_is_unchanged() {
        let (conway, _store) = conway_with_contributing_plugin_and_store();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        // `App::new` already copied the build-time snapshot
        // (`ContributingPlugin` always answers the same one contribution),
        // so the live poll agrees with what is already there -- no change,
        // no dirty.
        assert!(!app.refresh_plugin_status_contributions());
        assert_eq!(app.state.plugin_status_contributions.len(), 1);
    }

    /// Acceptance criterion 1, end to end and at the layer the criterion
    /// itself names: "a contribution produced by a plugin AFTER `build()`
    /// returns reaches the rendered status line." `CountingPlugin`'s first
    /// call (folded into `ConwayBuilder::build`'s own snapshot collection,
    /// then again into `App::new`'s copy of it) answers empty -- so nothing
    /// is on screen immediately after startup, exactly like a real
    /// `conway.statusline` command that has not finished its first refresh
    /// yet. Calling `refresh_plugin_status_contributions` (standing in for
    /// this loop's own `plugin_status_ticker` tick, `run.rs`) is what makes
    /// the SAME contribution `startup.rs`'s own
    /// `app_new_populates_plugin_status_contributions_from_a_real_plugin`
    /// test proves reaches the screen at startup ALSO reach it when it only
    /// becomes available afterward -- the exact case this item's own spec
    /// says "has never worked" before this item.
    #[tokio::test]
    async fn a_contribution_that_appears_after_build_reaches_the_rendered_status_line() {
        let conway = conway::test_support::test_builder(base_config())
            .with_backend(Arc::new(conway_testkit::FakeBackend::echo(BackendId::new(
                "fake",
            ))))
            .with_plugin(Arc::new(CountingPlugin {
                calls: AtomicUsize::new(0),
            }))
            .build()
            .expect("build should succeed with one status-contributing plugin installed");

        let mut cli = minimal_cli();
        let tui_config_dir = tempfile::tempdir().expect("tempdir");
        let tui_config_path = tui_config_dir.path().join("settings.json");
        std::fs::write(
            &tui_config_path,
            serde_json::json!({"tui": {"status_line": {"fields": ["plugins"]}}}).to_string(),
        )
        .expect("write settings.json carrying [tui.status_line.fields]");
        cli.config = Some(tui_config_path);

        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        // Nothing on screen yet -- the plugin's first-ever call (folded
        // into the build-time snapshot) answered empty.
        assert!(app.state.plugin_status_contributions.is_empty());
        let before = crate::tui::test_support::render_text(&app.state, 120, 40);
        assert!(!before.contains("counting:"));

        // The live poll's turn: this is what `run.rs`'s
        // `plugin_status_ticker` arm calls on its own bounded tick.
        assert!(
            app.refresh_plugin_status_contributions(),
            "a poll that finds a NEW contribution must report a change"
        );
        assert_eq!(app.state.plugin_status_contributions.len(), 1);

        let after = crate::tui::test_support::render_text(&app.state, 120, 40);
        assert!(
            after.contains("counting: call-1"),
            "a contribution that only became available AFTER build() must reach the rendered \
             status line once polled: {after}"
        );
    }

    /// Acceptance criterion 5: a live poll makes contributions appear and
    /// DISAPPEAR mid-session, which is a new way to reach the boundary
    /// `view/status.rs`'s `plugin_contributions_never_displace_the_forced_
    /// in_mode_field` already pins -- that test sets `AppState::
    /// plugin_status_contributions` directly and is agnostic to how the
    /// field got populated, so it already covers a mid-session-CHANGING
    /// value by construction (it makes no claim the value was ever fixed).
    /// This test confirms the same guarantee holds when the field is
    /// actually driven through THIS item's own new path -- `Conway::
    /// poll_plugin_status_contributions` by way of `App::
    /// refresh_plugin_status_contributions` -- rather than merely inferring
    /// it from the pre-existing, more direct test.
    #[tokio::test]
    async fn a_mid_session_poll_still_never_displaces_auto_allow() {
        let conway = conway::test_support::test_builder(base_config())
            .with_backend(Arc::new(conway_testkit::FakeBackend::echo(BackendId::new(
                "fake",
            ))))
            .with_plugin(Arc::new(CountingPlugin {
                calls: AtomicUsize::new(0),
            }))
            .build()
            .expect("build should succeed with one status-contributing plugin installed");

        let mut cli = minimal_cli();
        let tui_config_dir = tempfile::tempdir().expect("tempdir");
        let tui_config_path = tui_config_dir.path().join("settings.json");
        std::fs::write(
            &tui_config_path,
            serde_json::json!({"tui": {"status_line": {"fields": ["session", "hint"]}}})
                .to_string(),
        )
        .expect("write settings.json naming neither `mode` nor `plugins` explicitly");
        cli.config = Some(tui_config_path);

        let mut app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        app.state.permission_mode = conway::PermissionMode::AutoAllow;

        // The live poll's turn -- the SAME call `run.rs`'s
        // `plugin_status_ticker` arm makes, now populating `plugins` for
        // the first time mid-session, under `AUTO-ALLOW` contention.
        assert!(app.refresh_plugin_status_contributions());

        // A narrow width, deliberately: `mode`/`plugins` are both
        // force-inserted (neither was named above), so this is the
        // contended case `view/status.rs`'s own test exercises. Asserted
        // the same way `view/mod.rs`'s own narrow-terminal precedent
        // (`status_row_keeps_a_hint_pointer_at_a_narrow_forty_column_
        // terminal`) checks the real rendered buffer -- a plain substring
        // search over the whole screen, not an isolated single line.
        let text = crate::tui::test_support::render_text(&app.state, 20, 40);
        assert!(
            text.contains("AUTO-ALLOW"),
            "AUTO-ALLOW must survive -- never silently displaced by a plugin contribution that \
             only arrived via the live poll: {text:?}"
        );
    }
}
