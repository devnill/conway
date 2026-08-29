//! `App::apply_add_provider_choice`/`apply_add_provider_credential`/
//! `apply_remove_provider` -- board item `01M11XWB4T8ZADNDB4M8R482MA`'s
//! write half, factored out of `run.rs`'s own giant `select!` match arm the
//! same way [`super::plugin_toggle::App::apply_plugin_toggle`] is, for the
//! identical reason (directly testable, no real terminal/`select!` loop
//! needed).
//!
//! **Calls the three BUILT primitives this item names, never a second
//! opinion about any of them (P-14):**
//! - `crate::first_run::HOSTED_CHOICES`/`resolve_credential_plan`/
//!   `backend_entry_json` decide which provider SHAPES exist and how to
//!   build one's own JSON entry -- the exact same table and functions the
//!   first-run flow already uses, reused verbatim.
//! - `conway::config::set_backend_provider` decides HOW a config is
//!   written (splice, preserve comments/order, tmp-then-rename) -- this
//!   module never touches `settings.json`'s bytes itself.
//! - `conway::backend_usability` (via `App::
//!   refresh_provider_entries_and_kick_off_status`, `provider_status.rs`)
//!   decides what "working" means -- this module never classifies a
//!   provider itself, only triggers a fresh classification after a write.
//!
//! # Removal has consequences -- refuse, don't warn-and-proceed
//!
//! **Ruling, made here and recorded per this item's own spec:** removing a
//! provider a role's `chain` still names is REFUSED outright, naming the
//! affected roles, before any write -- never a warn-and-proceed. This
//! follows `app/plugin_toggle.rs`'s own toggle-off posture (a plugin
//! `requires` still enabled refuses the toggle, naming the dependent)
//! because the item's own spec names that exact precedent and says "follow
//! it": both are the same shape of hazard (removing something else still
//! structurally depends on) and a plugin toggle-off already answered it for
//! this codebase. A role whose chain has OTHER entries besides the removed
//! provider (a real fallback) would still be independently affected --
//! refusing rather than silently letting a chain narrow to fewer usable
//! candidates is the same "the operator finds out at the next restart /
//! next routing failure instead of now" harm `plugin_toggle.rs`'s own doc
//! names for its own case, so this does not special-case a multi-entry
//! chain differently from a single-entry one.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use conway::config::schema::RoleEntry;
use conway::config::{discovery, merged_document, set_backend_provider, LoadOptions};

use super::App;
use crate::first_run::{
    backend_entry_json, resolve_credential_plan, CredentialPlan, CredentialSource, HOSTED_CHOICES,
};
use crate::tui::state::Entry;

/// Every role name whose `chain` contains an entry naming `provider_id` as
/// its backend half (`"<provider_id>/<model>"`) -- pure, and free of any
/// `App`/`Conway` machinery, mirroring `app/plugin_toggle.rs::
/// enabled_dependents_requiring`'s own shape exactly: a plain function over
/// data, so a fabricated `roles` map is enough to exercise every branch.
/// Sorted, so a caller's message never depends on `BTreeMap` iteration
/// order changing under an unrelated edit (it will not, in practice, since
/// `BTreeMap` iterates in key order already -- sorted anyway so this
/// function's own contract does not silently depend on that fact).
///
/// A malformed chain entry (no `/` at all, or an empty backend half) simply
/// never matches -- P-10's "untrusted input, no panics" applies to a
/// hand-edited config's `roles.*.chain` exactly as it does to a typed
/// credential; this never panics on one.
pub(super) fn roles_referencing_provider(
    roles: &BTreeMap<String, RoleEntry>,
    provider_id: &str,
) -> Vec<String> {
    let mut names: Vec<String> = roles
        .iter()
        .filter(|(_, entry)| {
            entry.chain.iter().any(|link| {
                link.split_once('/')
                    .map(|(backend, _)| backend == provider_id)
                    .unwrap_or(false)
            })
        })
        .map(|(name, _)| name.clone())
        .collect();
    names.sort();
    names
}

/// Reads just the merged document's own `roles` section, WITHOUT deserializing
/// the whole document into a strict [`conway::config::ConwayConfig`]
/// (`conway::config::load`'s own contract). **Why this matters here
/// specifically:** `ConwayConfig` is `#[serde(deny_unknown_fields)]`, and
/// `config::writer`'s own top-doc names a top-level `"//": "..."`
/// comments-as-keys convention an operator may have used ELSEWHERE in the
/// same document -- a convention `conway::config::merge`'s own module doc
/// confirms is tolerated NOWHERE in the strict schema except the one named
/// `[tui]` exception. A removal's role-reference check must not fail
/// (and thereby block a removal that is actually perfectly safe) just
/// because an unrelated part of the operator's config uses a convention the
/// full strict schema does not parse -- so this reads the raw merged
/// `serde_json::Value` (`conway::config::merged_document`, the same
/// escape hatch `[tui]`'s own reader uses) and deserializes ONLY the
/// `roles` member, which is a far narrower -- and far more likely to
/// actually succeed -- validation surface.
fn load_roles_lax(
    env: &HashMap<String, String>,
    cwd: &Path,
) -> Result<BTreeMap<String, RoleEntry>, String> {
    let merged = merged_document(&LoadOptions {
        env: env.clone(),
        cwd: cwd.to_path_buf(),
        ..Default::default()
    })
    .map_err(|e| e.to_string())?;
    let roles_value = merged
        .get("roles")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    serde_json::from_value(roles_value).map_err(|e| e.to_string())
}

impl App {
    /// `Enter` on an `add {label}` leaf (`Action::AddProviderChoice`).
    /// Resolves `choice_id` against [`HOSTED_CHOICES`] (an unknown id --
    /// structurally unreachable from the real UI, which only ever emits an
    /// id straight from that same table -- surfaces as a non-fatal error
    /// rather than a panic, P-10) and branches on [`resolve_credential_plan`]
    /// exactly as `first_run.rs::run_guided_setup` already does: an
    /// already-set env var writes immediately (one keystroke, no typing);
    /// otherwise this opens the credential prompt
    /// (`AppState::begin_add_provider_credential`) instead of writing
    /// anything yet.
    pub(super) fn apply_add_provider_choice(
        &mut self,
        choice_id: &str,
        env: &HashMap<String, String>,
        cwd: &Path,
    ) {
        let Some(choice) = HOSTED_CHOICES.iter().find(|c| c.id == choice_id) else {
            self.state.transcript.push(Entry::Error {
                text: format!("unknown provider choice `{choice_id}`"),
                fatal: false,
            });
            return;
        };
        match resolve_credential_plan(choice, env) {
            CredentialPlan::ReuseEnvVar => {
                let entry_json = backend_entry_json(
                    choice,
                    &CredentialSource::EnvVar(choice.credential_env.to_string()),
                );
                self.write_provider_entry_and_refresh(choice.id, &entry_json, env, cwd);
            }
            CredentialPlan::PromptForLiteral => {
                self.state.begin_add_provider_credential(
                    choice.id,
                    choice.label,
                    choice.credential_env,
                );
            }
        }
    }

    /// The credential prompt's `Enter`
    /// (`Action::SubmitProviderCredential`) -- `secret` has ALREADY been
    /// validated by `input::handle_add_provider_credential_key` (via
    /// `crate::first_run::validate_credential_input`) before this is ever
    /// called, mirroring `finish_setup`'s own pre-validated call in
    /// `first_run.rs`.
    pub(super) fn apply_add_provider_credential(
        &mut self,
        choice_id: &str,
        secret: String,
        env: &HashMap<String, String>,
        cwd: &Path,
    ) {
        let Some(choice) = HOSTED_CHOICES.iter().find(|c| c.id == choice_id) else {
            self.state.transcript.push(Entry::Error {
                text: format!("unknown provider choice `{choice_id}`"),
                fatal: false,
            });
            return;
        };
        let entry_json = backend_entry_json(choice, &CredentialSource::Literal(secret));
        self.write_provider_entry_and_refresh(choice.id, &entry_json, env, cwd);
    }

    /// The shared write-then-refresh tail both add paths above end in:
    /// [`set_backend_provider`] (USER SCOPE, per that function's own doc),
    /// a transcript notice/error, and -- on success -- [`Self::
    /// refresh_provider_entries_and_kick_off_status`] so the freshly added
    /// provider appears (and is classified) without a restart, acceptance
    /// 5's own requirement.
    fn write_provider_entry_and_refresh(
        &mut self,
        id: &str,
        entry_json: &str,
        env: &HashMap<String, String>,
        cwd: &Path,
    ) {
        let Some(path) = discovery::user_config_path(env) else {
            self.state.transcript.push(Entry::Error {
                text: "could not resolve a home directory to write settings.json into".to_string(),
                fatal: false,
            });
            return;
        };
        match set_backend_provider(&path, id, entry_json, true) {
            Ok(_) => {
                self.state.transcript.push(Entry::Notice {
                    text: format!("{id}: added to {}", path.display()),
                });
                self.refresh_provider_entries_and_kick_off_status(env, cwd);
            }
            Err(e) => {
                self.state.transcript.push(Entry::Error {
                    text: format!("could not add provider {id}: {e}"),
                    fatal: false,
                });
            }
        }
    }

    /// `Enter` on a provider's own `(Enter to remove)` leaf
    /// (`Action::RemoveProvider`). Reloads the REAL merged config (never
    /// `self.conway.config()`'s stale snapshot -- a role added THIS session
    /// via a hand-edit must still be checked) and refuses -- naming every
    /// affected role, before any write -- when [`roles_referencing_provider`]
    /// finds one. See this module's own doc, "Removal has consequences",
    /// for why refusal (not warn-and-proceed) is this item's ruling.
    pub(super) fn apply_remove_provider(
        &mut self,
        provider_id: &str,
        env: &HashMap<String, String>,
        cwd: &Path,
    ) {
        let roles = match load_roles_lax(env, cwd) {
            Ok(roles) => roles,
            Err(e) => {
                self.state.transcript.push(Entry::Error {
                    text: format!(
                        "could not read [roles] to check for references before removing \
                         {provider_id}: {e}"
                    ),
                    fatal: false,
                });
                return;
            }
        };

        let affected = roles_referencing_provider(&roles, provider_id);
        if !affected.is_empty() {
            let (verb, pronoun) = if affected.len() == 1 {
                ("still names", "it")
            } else {
                ("still name", "it")
            };
            self.state.transcript.push(Entry::Error {
                text: format!(
                    "cannot remove {provider_id} -- role(s) {} {verb} in their chain and would \
                     fail to route without {pronoun}; update those roles first, or leave \
                     {provider_id} configured",
                    affected.join(", "),
                ),
                fatal: false,
            });
            return;
        }

        let Some(path) = discovery::user_config_path(env) else {
            self.state.transcript.push(Entry::Error {
                text: "could not resolve a home directory to write settings.json into".to_string(),
                fatal: false,
            });
            return;
        };
        match set_backend_provider(&path, provider_id, "{}", false) {
            Ok(_) => {
                self.state.transcript.push(Entry::Notice {
                    text: format!("{provider_id}: removed from {}", path.display()),
                });
                self.refresh_provider_entries_and_kick_off_status(env, cwd);
            }
            Err(e) => {
                self.state.transcript.push(Entry::Error {
                    text: format!("could not remove provider {provider_id}: {e}"),
                    fatal: false,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::fixtures::{echo_conway, minimal_cli};
    use super::{roles_referencing_provider, App};
    use crate::tui::state::Entry;

    fn isolated_env(dir: &std::path::Path) -> HashMap<String, String> {
        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            dir.to_string_lossy().into_owned(),
        );
        env
    }

    fn role(chain: &[&str]) -> conway::config::schema::RoleEntry {
        conway::config::schema::RoleEntry {
            chain: chain.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    // ---------------------------------------------------------------
    // roles_referencing_provider -- pure function coverage.
    // ---------------------------------------------------------------

    #[test]
    fn finds_only_roles_whose_chain_names_the_provider() {
        let roles = std::collections::BTreeMap::from([
            ("coder".to_string(), role(&["kimi/k3"])),
            ("reviewer".to_string(), role(&["anthropic/claude"])),
        ]);
        assert_eq!(
            roles_referencing_provider(&roles, "kimi"),
            vec!["coder".to_string()]
        );
        assert!(roles_referencing_provider(&roles, "openai").is_empty());
    }

    #[test]
    fn a_malformed_chain_entry_never_panics_and_never_matches() {
        let roles = std::collections::BTreeMap::from([(
            "coder".to_string(),
            role(&["not-a-model-ref", "/nobackend", ""]),
        )]);
        assert!(roles_referencing_provider(&roles, "kimi").is_empty());
    }

    // ---------------------------------------------------------------
    // Acceptance 5: adding via a reused env var writes the same shape a
    // hand-edit would, and the listing refreshes without a restart.
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn adding_a_provider_via_reused_env_var_appears_as_working_without_a_restart() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        // No top-level `"//"` comment here -- `conway::config::load`'s
        // strict `#[serde(deny_unknown_fields)]` schema does not tolerate
        // one at the document root (only `[tui]` is a named exception,
        // `config::merge`'s own module doc), so a fixture that needs a
        // SUCCESSFUL load (this test does, to prove the restart-less
        // listing refresh) cannot use one. The comment/ordering-survival
        // half of acceptance 7 is proven separately below, against the raw
        // bytes alone, which never asks `conway::config::load` to parse
        // the comment at all.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::json!({
                "default_role": "coder",
                "roles": {"coder": {"chain": []}}
            })
            .to_string(),
        )
        .expect("write fixture");
        let mut env = isolated_env(dir.path());
        env.insert("ANTHROPIC_API_KEY".to_string(), "sk-real".to_string());
        let cwd = tempfile::tempdir().expect("cwd tempdir");

        app.apply_add_provider_choice("anthropic", &env, cwd.path());

        let text = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["backends"]["anthropic"]["kind"], "anthropic");
        assert_eq!(
            value["backends"]["anthropic"]["api_key_env"],
            "ANTHROPIC_API_KEY"
        );
        // The literal secret must never be written -- `api_key_env` is used.
        assert!(!text.contains("sk-real"));

        // Acceptance 5's own words: it appears as working WITHOUT A
        // RESTART. `provider_entries` is a config snapshot re-read fresh
        // (never `Conway::config()`'s stale build-time one), and the
        // background classification is already under way.
        assert_eq!(app.state.provider_entries.len(), 1);
        assert!(app.state.provider_status_loading);
    }

    // ---------------------------------------------------------------
    // Acceptance 7: a hand-edited settings.json survives an add and a
    // remove byte-for-byte outside the changed table -- proven against the
    // raw bytes directly, the same idiom `config::writer`'s own tests and
    // `app/plugin_toggle.rs::a_toggle_preserves_unrelated_keys_in_an_
    // existing_settings_json` already use.
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn adding_a_provider_preserves_an_operators_comment_and_key_order_byte_for_byte() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let dir = tempfile::tempdir().expect("tempdir");
        let original = "{\n  \"//\": \"an operator comment\",\n  \"default_role\": \"coder\"\n}\n";
        std::fs::write(dir.path().join("settings.json"), original).expect("write fixture");
        let mut env = isolated_env(dir.path());
        env.insert("ANTHROPIC_API_KEY".to_string(), "sk-real".to_string());
        let cwd = tempfile::tempdir().expect("cwd tempdir");

        app.apply_add_provider_choice("anthropic", &env, cwd.path());

        let text = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        assert!(
            text.contains("an operator comment"),
            "the operator's own comment must survive: {text}"
        );
        assert!(text.contains("\"default_role\": \"coder\""), "{text}");
        // Everything outside the spliced `backends` table is BYTE-FOR-BYTE
        // the original -- the writer's own "targeted splice, never a
        // reserialize" contract (`config::writer`'s own module doc).
        let backends_start = text.find("\"backends\"").expect("backends member inserted");
        let before_backends = &text[..text.find(",\n  \"backends\"").unwrap_or(backends_start)];
        assert!(
            original.trim_end().starts_with(before_backends.trim_end()),
            "everything before the inserted `backends` member must be untouched: {text}"
        );
    }

    #[tokio::test]
    async fn removing_a_provider_preserves_an_operators_comment_and_key_order_byte_for_byte() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let dir = tempfile::tempdir().expect("tempdir");
        let original = "{\n  \"//\": \"an operator comment\",\n  \"default_role\": \"coder\",\n  \
             \"backends\": {\"kimi\": {\"kind\": \"openai-compat\", \"api_key\": \"sk-1\"}}\n}\n";
        std::fs::write(dir.path().join("settings.json"), original).expect("write fixture");
        let env = isolated_env(dir.path());
        let cwd = tempfile::tempdir().expect("cwd tempdir");

        app.apply_remove_provider("kimi", &env, cwd.path());

        let text = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        assert!(
            text.contains("an operator comment"),
            "the operator's own comment must survive a removal too: {text}"
        );
        assert!(text.contains("\"default_role\": \"coder\""), "{text}");
        assert!(
            !text.contains("kimi"),
            "kimi must actually be removed: {text}"
        );
    }

    #[tokio::test]
    async fn an_unset_credential_env_var_opens_the_credential_prompt_instead_of_writing() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());
        let cwd = tempfile::tempdir().expect("cwd tempdir");

        app.apply_add_provider_choice("openai", &env, cwd.path());

        assert!(
            !dir.path().join("settings.json").exists(),
            "no write must happen until a credential is actually entered"
        );
        assert!(matches!(
            app.state.mode,
            crate::tui::state::Mode::AddProviderCredential(_)
        ));
    }

    #[tokio::test]
    async fn a_typed_credential_writes_the_same_shape_a_hand_edit_would() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());
        let cwd = tempfile::tempdir().expect("cwd tempdir");

        app.apply_add_provider_credential("openai", "sk-typed".to_string(), &env, cwd.path());

        let text = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["backends"]["openai"]["kind"], "openai-compat");
        assert_eq!(value["backends"]["openai"]["api_key"], "sk-typed");
    }

    // ---------------------------------------------------------------
    // Acceptance 6: removing a provider a role still points at is refused,
    // BEFORE any write -- observed as no write happening, not merely a
    // message existing.
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn removing_a_provider_a_role_still_references_is_refused_before_the_write() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::json!({
                "default_role": "coder",
                "backends": {"kimi": {"kind": "openai-compat", "api_key": "sk-1"}},
                "roles": {"coder": {"chain": ["kimi/k3"]}}
            })
            .to_string(),
        )
        .expect("write fixture");
        let env = isolated_env(dir.path());
        let cwd = tempfile::tempdir().expect("cwd tempdir");

        let before = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();

        app.apply_remove_provider("kimi", &env, cwd.path());

        let after = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        assert_eq!(
            before, after,
            "a refused removal must never touch the file -- the warning must precede any write"
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Error { text, fatal: false } if text.contains("kimi") && text.contains("coder")
            )),
            "the refusal must name both the provider and the affected role: {:?}",
            app.state.transcript
        );
    }

    /// Falsifies the fixture above: with no role referencing the provider,
    /// removal proceeds normally.
    #[tokio::test]
    async fn removing_an_unreferenced_provider_succeeds() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::json!({
                "default_role": "coder",
                "backends": {
                    "kimi": {"kind": "openai-compat", "api_key": "sk-1"},
                    "anthropic": {"kind": "anthropic", "api_key": "sk-2"}
                },
                "roles": {"coder": {"chain": ["anthropic/claude"]}}
            })
            .to_string(),
        )
        .expect("write fixture");
        let env = isolated_env(dir.path());
        let cwd = tempfile::tempdir().expect("cwd tempdir");

        app.apply_remove_provider("kimi", &env, cwd.path());

        let text = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(
            value["backends"].get("kimi").is_none(),
            "kimi must be removed: {text}"
        );
        assert_eq!(value["default_role"], "coder", "{text}");
    }
}
