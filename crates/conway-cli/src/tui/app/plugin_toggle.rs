//! `App::apply_plugin_toggle` -- the write half of `Action::TogglePlugin`
//! (board item `01M0KARX71A64NTSYTDBVANVPF`), extracted out of
//! `run.rs`'s own giant `select!` match arm into its own, directly
//! testable method -- mirroring [`super::plugin_cmd::App::
//! apply_plugin_command_done`]'s own "factored out so a test can call it
//! with no real terminal/`select!` loop" shape.
//!
//! **`env` is a caller-supplied parameter, never read from
//! `std::env::vars()` inside this method** -- the SAME hermetic-testing
//! idiom `conway::config::merge::LoadOptions::env`/`crates/conway/tests/
//! support/mod.rs::isolated_env` already establish workspace-wide, and for
//! the identical reason: `std::env::set_var` in an in-process unit test
//! would race every OTHER test thread reading process env in parallel
//! (`crates/conway/tests/config_isolation_guard.rs` exists because this
//! exact hazard already broke a test suite once). `run.rs`'s own call site
//! collects `std::env::vars()` at the point of use, the same way every
//! other `Action` arm needing `env` already does
//! (`Action::RevokePermissionPattern`'s own arm, one screen up).

use std::collections::HashMap;

use super::App;
use crate::tui::state::Entry;

impl App {
    /// Writes `installed` into `plugin_id`'s membership in
    /// `~/.conway/settings.json`'s `plugins.install` array (decision
    /// `01M0K8BAXJ6THVJAPK0JZ17VV6`'s resolved user layer,
    /// `CONWAY_CONFIG_DIR`-overridable via `env`), then reconciles
    /// `self.state.plugin_browser`'s display mirror and pushes a
    /// transcript entry -- success as a `Notice`, failure as a non-fatal
    /// `Error` (never silent, mirroring every other settings-menu action's
    /// own failure posture).
    ///
    /// **Never touches `self.conway`/the running session's own installed
    /// plugin set** -- restart-to-apply, exactly as `/settings`'s own
    /// footer states (acceptance criterion 4). The mirror flips ONLY on a
    /// successful write, so a failed write can never claim a state that
    /// disk does not actually hold.
    pub(super) fn apply_plugin_toggle(
        &mut self,
        plugin_id: String,
        installed: bool,
        env: &HashMap<String, String>,
        cwd: &std::path::Path,
    ) {
        let path = conway::config::discovery::user_config_path(env);
        let outcome = match &path {
            Some(path) => conway::config::set_plugin_installed(path, &plugin_id, installed),
            None => Err(conway::ConwayError::Config {
                path: None,
                message: "could not resolve a home directory to write settings.json into"
                    .to_string(),
            }),
        };
        match outcome {
            Ok(_) => {
                if let Some(entry) = self
                    .state
                    .plugin_browser
                    .iter_mut()
                    .find(|entry| entry.id == plugin_id)
                {
                    entry.installed = installed;
                }
                let verb = if installed { "on" } else { "off" };
                // "Applies on next restart" is a PREDICTION, so verify it
                // rather than assert it.
                //
                // This writer targets the user layer only, but
                // `plugins.install` is merged across five sources and an
                // array does not union -- a higher-precedence layer that
                // defines `plugins.install` at all replaces the user
                // layer's wholesale, for every plugin. So a project
                // `.conway/settings.json` or a `CONWAY_*` env override
                // silently decides the outcome, and the write genuinely
                // succeeded while changing nothing the next start will see.
                //
                // Re-running the real merge is the honest check: it asks
                // the same question the next start will ask, instead of
                // enumerating layers here and drifting from `merge.rs`'s
                // own precedence the first time that changes.
                let effective = conway::config::load(conway::config::LoadOptions {
                    env: env.clone(),
                    // Explicit, like `env`: the project layer is found by
                    // walking up from the working directory, so a default
                    // here would read the PROCESS cwd and make this check
                    // untestable (and a test that cannot observe the
                    // override passes without asserting anything).
                    cwd: cwd.to_path_buf(),
                    ..Default::default()
                })
                .ok()
                .map(|outcome| {
                    outcome
                        .config
                        .plugins
                        .install
                        .iter()
                        .any(|id| id == &plugin_id)
                });
                match effective {
                    Some(effective) if effective != installed => {
                        self.state.transcript.push(Entry::Error {
                            text: format!(
                                "{plugin_id}: wrote settings.json, but this will NOT take effect \
                                 -- a higher-precedence config layer (a project settings.json, or \
                                 a CONWAY_* environment override) defines plugins.install and \
                                 replaces the user layer's list wholesale. Edit that layer \
                                 instead."
                            ),
                            fatal: false,
                        });
                    }
                    _ => {
                        self.state.transcript.push(Entry::Notice {
                            text: format!("{plugin_id}: turned {verb} -- applies on next restart"),
                        });
                    }
                }
            }
            Err(e) => {
                self.state.transcript.push(Entry::Error {
                    text: format!("could not update settings.json for {plugin_id}: {e}"),
                    fatal: false,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::fixtures::{build_conway_with_echo_backend, minimal_cli};
    use super::App;
    use crate::tui::state::{Entry, PluginBrowserEntry};

    /// Every test in this module points `CONWAY_CONFIG_DIR` at a fresh
    /// temp directory of its own via the `env` PARAMETER
    /// `apply_plugin_toggle` takes -- never `std::env::set_var`, and
    /// never the real `~/.conway/` (see this module's own doc for why).
    /// A working directory guaranteed to hold no `.conway/settings.json`
    /// anywhere up its ancestry that the test cares about -- so the project
    /// layer contributes nothing and the user layer is the effective one.
    fn no_project_layer() -> tempfile::TempDir {
        tempfile::tempdir().expect("cwd tempdir")
    }

    fn isolated_env(dir: &std::path::Path) -> HashMap<String, String> {
        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            dir.to_string_lossy().into_owned(),
        );
        env
    }

    /// Turning a plugin ON writes `plugins.install`, flips the display
    /// mirror, and leaves a Notice naming the restart-to-apply contract --
    /// the acceptance criteria this whole item exists to satisfy, driven
    /// through a real `App` (fully in-memory `Conway`, no real network/
    /// disk beyond the isolated temp settings.json).
    #[tokio::test]
    async fn turning_a_plugin_on_writes_settings_json_and_flips_the_mirror() {
        let conway = build_conway_with_echo_backend();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");
        app.state.plugin_browser = vec![PluginBrowserEntry {
            id: "conway.memory".to_string(),
            version: "0.9.0".to_string(),
            installed: false,
            description: conway::plugin::PluginDescription::default(),
        }];

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());
        app.apply_plugin_toggle(
            "conway.memory".to_string(),
            true,
            &env,
            no_project_layer.path(),
        );

        let settings_path = dir.path().join("settings.json");
        let text = std::fs::read_to_string(&settings_path).expect("settings.json must exist");
        let value: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(
            value["plugins"]["install"],
            serde_json::json!(["conway.memory"])
        );

        assert!(
            app.state
                .plugin_browser
                .iter()
                .find(|e| e.id == "conway.memory")
                .expect("entry still present")
                .installed,
            "the display mirror must flip to installed on a successful write"
        );

        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text } if text.contains("conway.memory")
                    && text.contains("turned on")
                    && text.contains("next restart")
            )),
            "a successful toggle must be surfaced as a transcript notice naming the \
             restart-to-apply contract: {:?}",
            app.state.transcript
        );

        // Never touches the running session's own facade -- restart-to-
        // apply is a real absence, not a claim: `Conway::config()` still
        // reports whatever it was built with, untouched by this write.
        assert!(!app
            .conway
            .config()
            .plugins
            .install
            .contains(&"conway.memory".to_string()));
    }

    /// Turning a plugin OFF removes it from `plugins.install` and flips
    /// the mirror the other way.
    #[tokio::test]
    async fn turning_a_plugin_off_removes_it_from_settings_json() {
        let conway = build_conway_with_echo_backend();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");
        app.state.plugin_browser = vec![PluginBrowserEntry {
            id: "conway.skills".to_string(),
            version: "0.9.0".to_string(),
            installed: true,
            description: conway::plugin::PluginDescription::default(),
        }];

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());
        // First turn it on (so there is something real to remove), then
        // off -- proving the round trip through the real writer, not just
        // a single-direction happy path.
        app.apply_plugin_toggle(
            "conway.skills".to_string(),
            true,
            &env,
            no_project_layer.path(),
        );
        app.apply_plugin_toggle(
            "conway.skills".to_string(),
            false,
            &env,
            no_project_layer.path(),
        );

        let settings_path = dir.path().join("settings.json");
        let text = std::fs::read_to_string(&settings_path).expect("settings.json must exist");
        let value: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(value["plugins"]["install"], serde_json::json!([]));
        assert!(
            !app.state
                .plugin_browser
                .iter()
                .find(|e| e.id == "conway.skills")
                .expect("entry still present")
                .installed
        );
    }

    /// A hand-edited settings.json with unrelated keys survives a toggle
    /// intact -- the SAME round-trip property `config::writer`'s own
    /// tests prove at the writer layer, checked again here at the `App`
    /// method layer so the wiring between the two is not merely assumed.
    #[tokio::test]
    async fn a_toggle_preserves_unrelated_keys_in_an_existing_settings_json() {
        let conway = build_conway_with_echo_backend();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");
        app.state.plugin_browser = vec![PluginBrowserEntry {
            id: "conway.path".to_string(),
            version: "0.9.0".to_string(),
            installed: false,
            description: conway::plugin::PluginDescription::default(),
        }];

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"default_role": "coder", "plugins": {"install": ["conway.memory"]}}"#,
        )
        .expect("write fixture");
        let env = isolated_env(dir.path());
        app.apply_plugin_toggle(
            "conway.path".to_string(),
            true,
            &env,
            no_project_layer.path(),
        );

        let text = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["default_role"], "coder");
        assert_eq!(
            value["plugins"]["install"],
            serde_json::json!(["conway.memory", "conway.path"])
        );
    }

    /// A write failure (here: an unresolvable path, simulated by pointing
    /// at a location that cannot be created) is surfaced as a non-fatal
    /// transcript Error, and the mirror is left UNCHANGED -- never a
    /// silent no-op, and never a claim the write does not back.
    #[tokio::test]
    async fn a_failed_write_surfaces_as_a_transcript_error_and_leaves_the_mirror_unchanged() {
        let conway = build_conway_with_echo_backend();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");
        app.state.plugin_browser = vec![PluginBrowserEntry {
            id: "conway.memory".to_string(),
            version: "0.9.0".to_string(),
            installed: false,
            description: conway::plugin::PluginDescription::default(),
        }];

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        // Not valid JSON -- `config::writer::set_plugin_installed` refuses
        // to rewrite it blindly (see that module's own "Safety posture").
        std::fs::write(dir.path().join("settings.json"), "{ not json").expect("write fixture");
        let env = isolated_env(dir.path());
        app.apply_plugin_toggle(
            "conway.memory".to_string(),
            true,
            &env,
            no_project_layer.path(),
        );

        assert!(
            !app.state
                .plugin_browser
                .iter()
                .find(|e| e.id == "conway.memory")
                .expect("entry still present")
                .installed,
            "a failed write must never flip the display mirror"
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Error { text, fatal: false } if text.contains("conway.memory")
            )),
            "a failed write must surface as a non-fatal transcript error: {:?}",
            app.state.transcript
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("settings.json")).unwrap(),
            "{ not json",
            "an invalid file must be left byte-for-byte untouched"
        );
    }

    /// A project layer that defines `plugins.install` replaces the user
    /// layer's array wholesale, so a toggle written to the user layer can
    /// succeed on disk and change nothing the next start will see.
    ///
    /// Regression for a review finding: the toggle previously reported
    /// "turned on -- applies on next restart" unconditionally. Writing a
    /// file and predicting an effect it will not have is worse than
    /// failing, because the operator has no reason to look further.
    ///
    /// This asserts UNCONDITIONALLY. An earlier draft guarded the
    /// assertions behind "if the override actually took", which passed
    /// without testing anything when the fixture failed to install the
    /// project layer at all -- the same shape of worthless test this
    /// codebase has been bitten by before.
    #[tokio::test]
    async fn a_project_layer_override_is_reported_not_claimed_as_success() {
        let conway = build_conway_with_echo_backend();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");
        app.state.plugin_browser = vec![PluginBrowserEntry {
            id: "conway.memory".to_string(),
            version: "0.9.0".to_string(),
            installed: false,
            description: conway::plugin::PluginDescription::default(),
        }];

        // A project layer pinning `plugins.install` to a list that does
        // NOT name the plugin being toggled on.
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::create_dir_all(project.path().join(".conway")).expect("mkdir .conway");
        std::fs::write(
            project.path().join(".conway/settings.json"),
            r#"{"plugins": {"install": []}}"#,
        )
        .expect("write project settings");

        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());
        app.apply_plugin_toggle("conway.memory".to_string(), true, &env, project.path());

        // The user-layer write still happens, and that is correct.
        let text = std::fs::read_to_string(dir.path().join("settings.json"))
            .expect("settings.json must still be written");
        assert!(
            text.contains("conway.memory"),
            "the user-layer write must still happen: {text}"
        );

        // The fixture must genuinely defeat the toggle -- assert that
        // first, so this test can never pass by failing to set itself up.
        let effective = conway::config::load(conway::config::LoadOptions {
            env: env.clone(),
            cwd: project.path().to_path_buf(),
            ..Default::default()
        })
        .expect("config loads")
        .config
        .plugins
        .install
        .iter()
        .any(|id| id == "conway.memory");
        assert!(
            !effective,
            "fixture precondition: the project layer must actually override the user \
             layer, otherwise this test proves nothing"
        );

        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Error { text, .. } if text.contains("conway.memory")
                    && text.contains("NOT take effect")
            )),
            "a toggle defeated by a higher-precedence layer must be reported, not \
             claimed as success: {:?}",
            app.state.transcript
        );
        assert!(
            !app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text } if text.contains("applies on next restart")
            )),
            "a defeated toggle must not also claim it applies on next restart: {:?}",
            app.state.transcript
        );
    }
}
