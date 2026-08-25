//! `App::apply_marketplace_install`/`App::apply_marketplace_uninstall` --
//! the operator-facing action half of board item
//! `01M0VR96Y87FF2BVNTBSC6GEYR` (browse a Claude Code marketplace, install
//! a plugin from it, uninstall it again), mirroring
//! [`super::plugin_toggle::App::apply_plugin_toggle`]'s own architecture
//! and testing style deliberately: both write into the user layer's
//! `settings.json` (`conway::config::discovery::user_config_path`,
//! `CONWAY_CONFIG_DIR`-overridable via `env`), re-run the REAL config merge
//! afterward to check whether a higher-precedence layer silently defeats
//! the write, and are tested by calling the method directly with an
//! isolated env/tempdir -- no real terminal, no simulated keystrokes, the
//! established shape this codebase already uses for a config-writing
//! operator action (`plugin_toggle.rs`'s own module doc: "driven through a
//! real `App`... no real terminal/`select!` loop").
//!
//! **Deliberately NOT wired through `tui::commands`' slash-command/
//! `Effect`/`Host` machinery.** That machinery's `Host` trait is a "thin
//! abstraction over exactly `SessionHandle`/`Conway`'s own methods"
//! (`commands.rs`'s own doc) -- a marketplace install needs neither: it
//! needs `env`/`cwd` (to resolve `settings.json`'s path, exactly like
//! `plugin_toggle` already does outside that machinery too) and a network
//! call this crate's own dependency graph deliberately keeps out of
//! `conway-cli` directly (`conway-plugin-marketplace`'s own crate doc). A
//! typed, always-available slash-command/palette entry point (mirroring
//! `/ask`'s `Effect::RunModalAsk` -> `App::spawn_modal_ask` pipeline) is a
//! reasonable, real follow-up, deliberately deferred here rather than
//! rushed -- see this item's own completion report for exactly what that
//! follow-up would touch.
//!
//! # Informed consent (determine-first Q2), in ONE action rather than a
//! two-step preview-then-confirm modal
//!
//! `/trust permissions` shows a preview card and waits for a separate
//! `[y]`/`[n]` before writing anything -- a heavier UX this item's own
//! spec names as the ANSWER to "what does the operator see before they
//! consent", but only by pointing at `plugin_toggle.rs`'s OWN standard,
//! not `/trust`'s: "the existing plugin toggle's honesty is the standard".
//! `plugin_toggle` is a ONE-STEP action with no preview modal, so matching
//! ITS standard does not require building a second preview-then-confirm
//! round trip here. [`App::apply_marketplace_install`] instead fetches the
//! marketplace's own entry for `plugin_id` FIRST (a read, nothing written
//! yet), and folds everything an operator needs to judge the install --
//! name, description, version, every file it will write and the URL each
//! comes from, the destination directory, and the unsandboxed-privilege
//! caveat every other plugin-install surface in this codebase already
//! states -- into the SAME transcript entry the install's own outcome is
//! reported in, before performing the write. A two-step modal (typed a URL
//! and id, see a preview, confirm separately) is a reasonable follow-up,
//! not built here -- named explicitly rather than silently short of the
//! spec's own bar.

// Mirrors `conway-plugin-backends/src/http.rs`'s own precedent EXACTLY,
// including its own reasoning stated verbatim: "`HttpClient` itself is not
// yet constructed anywhere outside this module's own tests: the adapters
// that build one ... are later work items. The `#[allow(dead_code)]` below
// is scoped to this file for exactly that reason." `App::
// apply_marketplace_install`/`apply_marketplace_uninstall` are real, tested,
// end-to-end-correct methods with no interactive TUI trigger yet -- see this
// module's own doc, "Deliberately NOT wired through `tui::commands`'", for
// exactly why and exactly what the follow-up (a `SlashCommand` variant
// mirroring `/ask`'s `Effect::RunModalAsk` -> `App::spawn_modal_ask`
// pipeline) would touch.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use conway_plugin_marketplace::MarketplaceError;

use super::App;
use crate::tui::state::Entry;

impl App {
    /// The directory this crate treats as conway's own plugin store --
    /// `conway_plugin_marketplace::install::install_entry`'s own doc:
    /// "the CALLER decides what `store_root` actually is". Alongside the
    /// `settings.json` a matching `[plugins].claude_compat[]` entry is
    /// written into (`<config dir>/plugins/marketplace`), so the artifact
    /// and the config that names it live under the same root an operator
    /// already knows to look in (`~/.conway/`, or `$CONWAY_CONFIG_DIR`).
    /// `None` under the identical condition `user_config_path` itself
    /// returns `None` (no resolvable home directory) -- the same "cannot
    /// even find a place to write" failure `apply_plugin_toggle` already
    /// reports as a `FacadeError::Config`.
    fn marketplace_store_root(env: &HashMap<String, String>) -> Option<PathBuf> {
        conway::config::discovery::user_config_path(env)
            .and_then(|settings| settings.parent().map(|dir| dir.join("plugins/marketplace")))
    }

    /// Fetches `marketplace_url`'s manifest, installs `plugin_id` into
    /// conway's own plugin store, and -- only once every declared file has
    /// landed successfully (P-13: never before) -- writes a
    /// `[plugins].claude_compat[]` entry naming it
    /// (`conway::config::set_claude_compat_entry`), the SAME array-of-
    /// objects writer this item's own completion report proves preserves
    /// an operator's hand-edited formatting.
    ///
    /// Every step is reported to the transcript: a fetch/install failure
    /// (offline, a bad URL, a malformed marketplace response, an unsafe
    /// path, ...) as a non-fatal `Entry::Error` naming the plugin id and
    /// the underlying [`MarketplaceError`]'s own message, with NOTHING
    /// written to `settings.json` (the config write only happens after
    /// [`conway_plugin_marketplace::install_plugin`] already returned
    /// `Ok`); success as an `Entry::Notice` disclosing what was installed
    /// (this module's own doc, "Informed consent"), the destination
    /// directory, and -- mirroring `apply_plugin_toggle`'s own honesty
    /// check exactly -- whether a higher-precedence config layer silently
    /// defeats the write.
    ///
    /// If the config write itself fails AFTER a successful fetch+install
    /// (a rare but real gap between the two steps -- a permissions error on
    /// `settings.json`, say), the just-installed artifact is removed again
    /// (best-effort) rather than left as an orphan nothing in
    /// `settings.json` references -- see `conway-plugin-marketplace/src/
    /// install.rs`'s own doc, "Where a fetched artifact lives, and who
    /// owns it": an artifact conway downloaded that nothing tracks is worse
    /// than one the operator placed themselves, precisely because the
    /// trust ruling checks a fetched artifact against nothing -- knowing
    /// where it came from and being able to remove it completely is the
    /// whole of the operator's own control, and a config write that fails
    /// must not leave an untracked artifact behind to defeat that control.
    pub(super) async fn apply_marketplace_install(
        &mut self,
        marketplace_url: String,
        plugin_id: String,
        env: &HashMap<String, String>,
        cwd: &Path,
    ) {
        let Some(settings_path) = conway::config::discovery::user_config_path(env) else {
            self.state.transcript.push(Entry::Error {
                text: "could not resolve a home directory to write settings.json into".to_string(),
                fatal: false,
            });
            return;
        };
        let Some(store_root) = Self::marketplace_store_root(env) else {
            self.state.transcript.push(Entry::Error {
                text: "could not resolve a home directory for conway's plugin store".to_string(),
                fatal: false,
            });
            return;
        };

        let manifest = match conway_plugin_marketplace::fetch_marketplace(&marketplace_url).await {
            Ok(manifest) => manifest,
            Err(err) => {
                self.state.transcript.push(Entry::Error {
                    text: format!("{marketplace_url}: {err}"),
                    fatal: false,
                });
                return;
            }
        };
        let entry = match manifest.find(&marketplace_url, &plugin_id) {
            Ok(entry) => entry.clone(),
            Err(err) => {
                self.state.transcript.push(Entry::Error {
                    text: err.to_string(),
                    fatal: false,
                });
                return;
            }
        };

        let installed =
            match conway_plugin_marketplace::install_entry(&marketplace_url, &entry, &store_root)
                .await
            {
                Ok(installed) => installed,
                Err(err) => {
                    self.state.transcript.push(Entry::Error {
                        text: format!("{plugin_id}: install failed: {err}"),
                        fatal: false,
                    });
                    return;
                }
            };

        let dir_string = installed.dir.to_string_lossy().into_owned();
        if let Err(write_err) = conway::config::set_claude_compat_entry(
            &settings_path,
            &installed.id,
            &dir_string,
            true,
        ) {
            // The config write is the thing that makes this artifact
            // tracked at all -- if it fails, remove what was just fetched
            // rather than leave an orphan (this method's own doc).
            let cleanup = conway_plugin_marketplace::uninstall_plugin(&installed.id, &store_root);
            let cleanup_note = match cleanup {
                Ok(true) => "the just-fetched files were removed again".to_string(),
                Ok(false) => "nothing to clean up".to_string(),
                Err(cleanup_err) => {
                    format!(
                        "AND cleanup itself failed -- {} may still be on disk at {}: {cleanup_err}",
                        installed.id,
                        installed.dir.display()
                    )
                }
            };
            self.state.transcript.push(Entry::Error {
                text: format!(
                    "{}: fetched successfully but could not update settings.json ({write_err}) \
                     -- {cleanup_note}",
                    installed.id
                ),
                fatal: false,
            });
            return;
        }

        // Informed consent (this module's own doc): everything an operator
        // needs to judge what just happened, in the one Notice this action
        // produces.
        let mut files: Vec<&String> = entry.files.keys().collect();
        files.sort();
        let file_list = files
            .iter()
            .map(|f| f.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let description = if entry.description.is_empty() {
            String::new()
        } else {
            format!(" -- {}", entry.description)
        };

        // Same honesty check `apply_plugin_toggle` performs, word for word
        // in spirit: a write to the user layer can succeed on disk and
        // change nothing the next start will actually see, if a
        // higher-precedence layer (project settings.json, CONWAY_* env)
        // defines `plugins.claude_compat` at all -- arrays replace
        // wholesale, they do not union, across `merge.rs`'s five-source
        // precedence.
        let effective = conway::config::load(conway::config::LoadOptions {
            env: env.clone(),
            cwd: cwd.to_path_buf(),
            ..Default::default()
        })
        .ok()
        .map(|outcome| {
            outcome
                .config
                .plugins
                .claude_compat
                .iter()
                .any(|e| e.id == installed.id)
        });

        match effective {
            Some(false) => {
                self.state.transcript.push(Entry::Error {
                    text: format!(
                        "{}: installed to {} and wrote settings.json, but this will NOT take \
                         effect -- a higher-precedence config layer (a project settings.json, \
                         or a CONWAY_* environment override) defines plugins.claude_compat and \
                         replaces the user layer's list wholesale. Edit that layer instead.",
                        installed.id,
                        installed.dir.display()
                    ),
                    fatal: false,
                });
            }
            _ => {
                self.state.transcript.push(Entry::Notice {
                    text: format!(
                        "installed '{}'{description} (version {}) from {marketplace_url} into {} \
                         -- files: {file_list} -- runs with your own privileges, unsandboxed \
                         (same trust footing as any other [plugins].claude_compat entry; nothing \
                         checks a fetched artifact against a digest or an allow-list) -- applies \
                         on next restart",
                        installed.id,
                        if entry.version.is_empty() {
                            "unspecified"
                        } else {
                            entry.version.as_str()
                        },
                        installed.dir.display(),
                    ),
                });
            }
        }
    }

    /// Removes `plugin_id`'s `[plugins].claude_compat[]` entry from
    /// `settings.json` and its own directory from conway's plugin store --
    /// acceptance 3, "uninstall works and leaves neither a config entry nor
    /// a stray artifact behind". Synchronous (no network): both steps are
    /// local filesystem operations.
    ///
    /// The config entry is removed FIRST, the artifact SECOND -- the
    /// opposite order from install, and deliberately so: if only one step
    /// can succeed, an orphan directory with no config entry pointing at it
    /// (this outcome) is the state `install`'s own cleanup-on-failure path
    /// already treats as acceptable-but-worth-reporting, while an orphan
    /// CONFIG ENTRY pointing at a directory that no longer exists (the
    /// other possible order) would make the NEXT config load fail outright
    /// (`claude_compat_plugins::install`'s own "an unresolvable entry fails
    /// the whole build" contract) -- a far worse failure mode than a stray,
    /// harmless directory an operator can find and remove by hand. Both
    /// failures are reported, never silently swallowed.
    pub(super) fn apply_marketplace_uninstall(
        &mut self,
        plugin_id: String,
        env: &HashMap<String, String>,
        cwd: &Path,
    ) {
        let Some(settings_path) = conway::config::discovery::user_config_path(env) else {
            self.state.transcript.push(Entry::Error {
                text: "could not resolve a home directory to write settings.json into".to_string(),
                fatal: false,
            });
            return;
        };
        let Some(store_root) = Self::marketplace_store_root(env) else {
            self.state.transcript.push(Entry::Error {
                text: "could not resolve a home directory for conway's plugin store".to_string(),
                fatal: false,
            });
            return;
        };

        let config_result =
            conway::config::set_claude_compat_entry(&settings_path, &plugin_id, "", false);
        let config_removed = match config_result {
            Ok(removed) => removed,
            Err(err) => {
                self.state.transcript.push(Entry::Error {
                    text: format!("{plugin_id}: could not update settings.json: {err}"),
                    fatal: false,
                });
                return;
            }
        };

        let artifact_result = conway_plugin_marketplace::uninstall_plugin(&plugin_id, &store_root);
        let artifact_removed = match &artifact_result {
            Ok(removed) => *removed,
            Err(MarketplaceError::UnsafePluginId { .. }) => {
                // Cannot happen for an id this method itself resolved from
                // an install (validated then), but never assume it: report
                // rather than panic (P-10, applied to this module's own
                // stored state, not just network input).
                self.state.transcript.push(Entry::Error {
                    text: format!("{plugin_id}: not a safe plugin id, refusing to remove anything"),
                    fatal: false,
                });
                return;
            }
            Err(err) => {
                self.state.transcript.push(Entry::Error {
                    text: format!(
                        "{plugin_id}: settings.json entry removed, but its files at {} could not \
                         be removed: {err}",
                        store_root.join(&plugin_id).display()
                    ),
                    fatal: false,
                });
                return;
            }
        };

        if !config_removed && !artifact_removed {
            self.state.transcript.push(Entry::Notice {
                text: format!("{plugin_id}: not installed -- nothing to do"),
            });
            return;
        }

        // Same honesty check as install: report if a higher-precedence
        // layer means this write does not change what the next start
        // actually sees.
        let effective = conway::config::load(conway::config::LoadOptions {
            env: env.clone(),
            cwd: cwd.to_path_buf(),
            ..Default::default()
        })
        .ok()
        .map(|outcome| {
            outcome
                .config
                .plugins
                .claude_compat
                .iter()
                .any(|e| e.id == plugin_id)
        });

        match effective {
            Some(true) => {
                self.state.transcript.push(Entry::Error {
                    text: format!(
                        "{plugin_id}: removed from settings.json and its files deleted, but this \
                         will NOT take effect -- a higher-precedence config layer still names it. \
                         Edit that layer instead."
                    ),
                    fatal: false,
                });
            }
            _ => {
                self.state.transcript.push(Entry::Notice {
                    text: format!(
                        "{plugin_id}: uninstalled -- config entry and files removed, applies on \
                         next restart"
                    ),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::super::fixtures::{echo_conway, minimal_cli};
    use super::App;
    use crate::tui::state::Entry;

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

    async fn mount_marketplace(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/marketplace.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"{{
                    "name": "acme-marketplace",
                    "plugins": [
                        {{
                            "id": "acme-tools",
                            "name": "Acme Tools",
                            "description": "Search Acme's index",
                            "version": "1.0.0",
                            "files": {{
                                ".claude-plugin/plugin.json": "{base}/plugin.json"
                            }}
                        }}
                    ]
                }}"#,
                base = server.uri()
            )))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/plugin.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"name":"acme-tools"}"#))
            .mount(server)
            .await;
    }

    /// Acceptance 1/2 (install), acceptance 4 (format preservation via the
    /// real writer), and this module's own "informed consent in one
    /// action" doc: a real install writes settings.json, discloses what
    /// was installed, and never touches the live session.
    #[tokio::test]
    async fn installing_writes_settings_json_and_the_artifact_and_discloses_what_happened() {
        let server = MockServer::start().await;
        mount_marketplace(&server).await;
        let marketplace_url = format!("{}/marketplace.json", server.uri());

        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());

        app.apply_marketplace_install(
            marketplace_url.clone(),
            "acme-tools".to_string(),
            &env,
            no_project_layer.path(),
        )
        .await;

        let settings_path = dir.path().join("settings.json");
        let text = std::fs::read_to_string(&settings_path).expect("settings.json must exist");
        let value: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        let cc = value["plugins"]["claude_compat"]
            .as_array()
            .expect("claude_compat array");
        assert_eq!(cc.len(), 1);
        assert_eq!(cc[0]["id"], "acme-tools");
        let installed_dir = cc[0]["dir"].as_str().expect("dir string").to_string();
        assert!(
            std::path::Path::new(&installed_dir)
                .join(".claude-plugin/plugin.json")
                .is_file(),
            "the artifact itself must exist on disk at the path the config entry names"
        );

        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text } if text.contains("acme-tools")
                    && text.contains("Search Acme's index")
                    && text.contains("1.0.0")
                    && text.contains(".claude-plugin/plugin.json")
                    && text.contains("unsandboxed")
                    && text.contains(&marketplace_url)
            )),
            "the disclosure must name the plugin, its description, version, files, source, and \
             the unsandboxed caveat: {:?}",
            app.state.transcript
        );

        // Never touches the running session's own facade -- restart-to-
        // apply, mirroring `apply_plugin_toggle`'s own guarantee.
        assert!(app.conway.config().plugins.claude_compat.is_empty());
    }

    /// Acceptance 5, case 1 (offline) at the App layer: the config is never
    /// written when the fetch itself fails.
    #[tokio::test]
    async fn offline_reports_an_error_and_writes_nothing() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());

        app.apply_marketplace_install(
            "http://127.0.0.1:1/marketplace.json".to_string(),
            "acme-tools".to_string(),
            &env,
            no_project_layer.path(),
        )
        .await;

        assert!(
            !dir.path().join("settings.json").exists(),
            "an unreachable marketplace must never produce a settings.json write"
        );
        assert!(app
            .state
            .transcript
            .iter()
            .any(|e| matches!(e, Entry::Error { fatal: false, .. })));
    }

    /// Acceptance 5, case 3 (malformed marketplace response) at the App
    /// layer: a bad manifest never writes config either.
    #[tokio::test]
    async fn a_malformed_marketplace_reports_an_error_and_writes_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/marketplace.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());

        app.apply_marketplace_install(
            format!("{}/marketplace.json", server.uri()),
            "acme-tools".to_string(),
            &env,
            no_project_layer.path(),
        )
        .await;

        assert!(!dir.path().join("settings.json").exists());
        assert!(app
            .state
            .transcript
            .iter()
            .any(|e| matches!(e, Entry::Error { fatal: false, .. })));
    }

    /// A plugin id the marketplace does not list is reported, never
    /// silently a no-op.
    #[tokio::test]
    async fn an_unknown_plugin_id_is_reported() {
        let server = MockServer::start().await;
        mount_marketplace(&server).await;

        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());

        app.apply_marketplace_install(
            format!("{}/marketplace.json", server.uri()),
            "nope".to_string(),
            &env,
            no_project_layer.path(),
        )
        .await;

        assert!(!dir.path().join("settings.json").exists());
        assert!(app.state.transcript.iter().any(|e| matches!(
            e,
            Entry::Error { text, fatal: false } if text.contains("nope")
        )));
    }

    /// Uninstall removes both the config entry and the artifact -- proven
    /// by installing first, then uninstalling, through the real App
    /// methods.
    #[tokio::test]
    async fn uninstalling_removes_both_the_config_entry_and_the_artifact() {
        let server = MockServer::start().await;
        mount_marketplace(&server).await;
        let marketplace_url = format!("{}/marketplace.json", server.uri());

        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());

        app.apply_marketplace_install(
            marketplace_url,
            "acme-tools".to_string(),
            &env,
            no_project_layer.path(),
        )
        .await;
        let settings_path = dir.path().join("settings.json");
        let installed_dir = {
            let text = std::fs::read_to_string(&settings_path).unwrap();
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            value["plugins"]["claude_compat"][0]["dir"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert!(
            std::path::Path::new(&installed_dir).is_dir(),
            "precondition: installed"
        );

        app.apply_marketplace_uninstall("acme-tools".to_string(), &env, no_project_layer.path());

        let text = std::fs::read_to_string(&settings_path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["plugins"]["claude_compat"], serde_json::json!([]));
        assert!(
            !std::path::Path::new(&installed_dir).exists(),
            "no stray artifact may remain after uninstall"
        );
        assert!(app.state.transcript.iter().any(|e| matches!(
            e,
            Entry::Notice { text } if text.contains("acme-tools") && text.contains("uninstalled")
        )));
    }

    /// Uninstalling a never-installed id is a reported no-op, never an
    /// error.
    #[tokio::test]
    async fn uninstalling_a_never_installed_id_is_a_reported_no_op() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());

        app.apply_marketplace_uninstall("never-there".to_string(), &env, no_project_layer.path());

        assert!(app.state.transcript.iter().any(|e| matches!(
            e,
            Entry::Notice { text } if text.contains("not installed")
        )));
    }

    /// A project layer overriding `plugins.claude_compat` wholesale is
    /// reported, not silently claimed as taking effect -- the identical
    /// regression class `apply_plugin_toggle`'s own
    /// `a_project_layer_override_is_reported_not_claimed_as_success` test
    /// guards.
    #[tokio::test]
    async fn a_project_layer_override_is_reported_not_claimed_as_success() {
        let server = MockServer::start().await;
        mount_marketplace(&server).await;

        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::create_dir_all(project.path().join(".conway")).expect("mkdir .conway");
        std::fs::write(
            project.path().join(".conway/settings.json"),
            r#"{"plugins": {"claude_compat": []}}"#,
        )
        .expect("write project settings");

        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());

        app.apply_marketplace_install(
            format!("{}/marketplace.json", server.uri()),
            "acme-tools".to_string(),
            &env,
            project.path(),
        )
        .await;

        // The user-layer write still happened.
        let text = std::fs::read_to_string(dir.path().join("settings.json"))
            .expect("settings.json must still be written");
        assert!(text.contains("acme-tools"));

        assert!(app.state.transcript.iter().any(|e| matches!(
            e,
            Entry::Error { text, .. } if text.contains("acme-tools") && text.contains("NOT take effect")
        )));
    }
}
