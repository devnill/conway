//! Board item `01M11XWB4T8ZADNDB4M8R482MA`: the settings providers
//! section's own live listing -- refreshing `AppState::provider_entries`
//! (the CURRENT merged config's `backends` map, re-read fresh rather than
//! taken from `Conway::config()`'s stale build-time snapshot -- see that
//! field's own doc for why acceptance 5 needs this) and kicking off a
//! background classification of every entry via `conway::
//! backend_usability::classify_fleet`, under `ProbePolicy::All` -- **never
//! `LocalOnly`**: this screen is the operator looking at the list with live
//! status implicitly requested by opening it, not the startup path
//! `LocalOnly` exists for (`backend_usability`'s own module doc).
//!
//! **The probe never blocks this loop's own `select!`.** `classify_fleet`
//! performs real network I/O (a TCP connect per declared-local-or-not
//! endpoint under `All`), so it is spawned off the render loop exactly the
//! way `app/plugin_cmd.rs::spawn_plugin_command` spawns a plugin command's
//! own `invoke` -- a `tokio::spawn`ed task reporting back over an
//! `mpsc::UnboundedSender`/`Receiver` pair (`App::provider_status_tx`/
//! `provider_status_rx`) that `App::run`'s own `select!` polls as one more
//! arm, mirroring `plugin_cmd_rx`'s own shape exactly. Reloading
//! `provider_entries` itself stays synchronous and inline here (a local
//! `settings.json`/project-config read via `conway::config::load`, never
//! network I/O -- the same "disk read, not spawned" treatment `app/
//! plugin_toggle.rs`'s own project-layer-override check already gives an
//! identical call).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use conway::backend_usability::{classify_fleet, ProbePolicy, Usability, DEFAULT_PROBE_TIMEOUT};
use conway::config::{load, LoadOptions};

use super::App;

/// One background classification's reply -- see this module's own doc for
/// the spawn/channel shape this rides on.
pub(super) struct ProviderStatusDone {
    pub(super) status: BTreeMap<String, Usability>,
}

impl App {
    /// Reloads `AppState::provider_entries` from the REAL merged config
    /// (never `self.conway.config()`'s stale snapshot -- see that field's
    /// own doc) and spawns the background probe that will eventually
    /// populate `AppState::provider_status` via [`Self::
    /// apply_provider_status_done`]. Called whenever the providers section's
    /// data could have changed: `/settings` opening (`App::submit`'s own
    /// `SlashCommand::Settings` refresh block) and after a successful
    /// add/remove (`app/provider_manage.rs`).
    ///
    /// A config that fails to load (a hand-edited `settings.json` that no
    /// longer parses, say) leaves `provider_entries`/`provider_status`
    /// exactly as they were and spawns nothing -- there is nothing new to
    /// show, and the section simply keeps displaying whatever it already
    /// had rather than silently going blank on a transient read error.
    pub(super) fn refresh_provider_entries_and_kick_off_status(
        &mut self,
        env: &HashMap<String, String>,
        cwd: &Path,
    ) {
        let outcome = load(LoadOptions {
            env: env.clone(),
            cwd: cwd.to_path_buf(),
            ..Default::default()
        });
        let Ok(outcome) = outcome else {
            return;
        };
        let config = outcome.config;
        self.state.provider_entries = config.backends.clone();
        self.state.provider_status_loading = true;

        let tx = self.provider_status_tx.clone();
        let env = env.clone();
        tokio::spawn(async move {
            let (status, _fleet) =
                classify_fleet(&config, &env, ProbePolicy::All, DEFAULT_PROBE_TIMEOUT).await;
            // The receiver only goes away once `App::run`'s loop has
            // already exited -- nothing left to notify, mirroring
            // `spawn_plugin_command`'s own identical send site.
            let _ = tx.send(ProviderStatusDone { status });
        });
    }

    /// Applies one [`ProviderStatusDone`] reply -- overwrites `AppState::
    /// provider_status` WHOLESALE (never merges), the same "replace, don't
    /// accumulate" contract `app/plugin_status.rs::
    /// refresh_plugin_status_contributions`'s own doc establishes for the
    /// identical reason: a provider whose entry was removed between the
    /// probe starting and finishing must not leave a stale row behind.
    pub(super) fn apply_provider_status_done(&mut self, done: ProviderStatusDone) {
        self.state.provider_status = done.status;
        self.state.provider_status_loading = false;
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{echo_conway, minimal_cli};
    use super::App;

    /// Acceptance 4, at the call this module makes: `ProbePolicy::All` is
    /// used, not `LocalOnly` -- proven by observing the OUTCOME (a
    /// non-local, credentialed backend classifies to something other than
    /// `Undetermined::NotProbed`, which is exactly what `LocalOnly` would
    /// produce for it -- `backend_usability`'s own
    /// `a_remote_endpoint_is_not_probed_under_the_startup_policy` test pins
    /// that), never by inspecting which branch ran. P-15: falsified by
    /// temporarily reverting the call to `ProbePolicy::LocalOnly` (see this
    /// crate's own settings integration test for the full breakage
    /// evidence recorded in the board report -- kept here as the narrowest,
    /// fastest-running proof of the same fact).
    #[tokio::test]
    async fn refresh_reaches_a_non_local_credentialed_backend_and_never_reports_not_probed() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::json!({
                "default_role": "coder",
                "backends": {
                    "acme": {
                        "kind": "openai-compat",
                        "base_url": "http://127.0.0.1:1/v1",
                        "api_key": "sk-test",
                        "local": false
                    }
                },
                "roles": {"coder": {"chain": []}}
            })
            .to_string(),
        )
        .expect("write fixture");
        let mut env = std::collections::HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            dir.path().to_string_lossy().into_owned(),
        );
        let cwd = tempfile::tempdir().expect("cwd tempdir");

        app.refresh_provider_entries_and_kick_off_status(&env, cwd.path());
        assert_eq!(app.state.provider_entries.len(), 1);
        assert!(app.state.provider_status_loading);

        let done = app
            .provider_status_rx
            .as_mut()
            .expect("provider_status_rx set by App::new")
            .recv()
            .await
            .expect("the spawned probe must reply");
        app.apply_provider_status_done(done);

        assert!(!app.state.provider_status_loading);
        let status = app
            .state
            .provider_status
            .get("acme")
            .expect("the acme entry must be classified");
        assert!(
            !matches!(
                status,
                conway::backend_usability::Usability::Undetermined(
                    conway::backend_usability::Undetermined::NotProbed
                )
            ),
            "ProbePolicy::All must reach a non-local backend rather than reporting \
             NotProbed, got: {status:?}"
        );
    }
}
