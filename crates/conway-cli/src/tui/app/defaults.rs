//! `App::refresh_default_entries`/`App::apply_cycle_default_role` -- board
//! item `01M18Q7P25DTSKQJDJJCC3E800`'s write half of `/settings`'
//! "defaults" section, factored out of `run.rs`'s own giant `select!`
//! match arm the same way [`super::provider_manage`]'s methods are, for
//! the identical reason (directly testable, no real terminal/`select!`
//! loop needed).
//!
//! **Calls the three BUILT primitives this item names, never a second
//! opinion about any of them (P-14):**
//! - `conway::config::schema::ConwayConfig::model_for` decides what "the
//!   default model" means -- the head of a role's `chain` -- the exact
//!   same function `ConwayConfig::default_model` itself calls; this module
//!   never restates that lookup.
//! - `conway::config::set_default_role` decides HOW `default_role` is
//!   written (splice, preserve comments/order, tmp-then-rename, refuse a
//!   missing key rather than invent one) -- this module never touches
//!   `settings.json`'s bytes itself.
//! - `conway::config::set_role_chain` decides HOW `roles.<role>.chain` is
//!   written -- the same writer `app/provider_manage.rs` already calls for
//!   its own chain edits; this module's own reorder (below) never touches
//!   `settings.json`'s bytes directly either.
//!
//! # Why `default_model` has no `apply_*` writer of its own -- but the
//! session DOES have one, as of board item `01M1AWGSTD7084VFVGN1GK9AS8`
//!
//! **Written 2026-08-29, corrected 2026-08-30 rather than left standing --
//! see the paragraph below for what changed.** `/settings`' "default
//! model" row is `MenuNode::Static` (`view/settings.rs`), not a leaf --
//! there is no `Action` that sets IT, and therefore no writer here for
//! THAT row, because `ConwayConfig::default_model`'s own doc records the
//! decision this item made: the default model is a DERIVED read over
//! `roles.<default_role>.chain`, not a second stored value.
//!
//! **What changed:** the observation above ("no write path here for it")
//! was true only because nothing in the app had a REASON to write a chain
//! reorder yet. Board item `01M1AWGSTD7084VFVGN1GK9AS8` found one: an
//! operator whose SESSION is running a different model than the
//! persistent default (via `/model`, session-scoped) had no in-app way to
//! promote that choice to the persistent default, and silently lost it on
//! restart. [`App::apply_promote_session_model_to_default`] is the writer
//! that gap needed -- it still never sets a `default_model` scalar (none
//! exists), and it still never mutates the STATIC row's own `Action` (it
//! has none, unchanged); it writes a REORDER of `roles.<default_role>.
//! chain` instead, driven by a NEW, separate leaf
//! (`view::settings::LEAF_PROMOTE_SESSION_MODEL`) that this module's own
//! doc, "The persistent-default gap," in `view/settings.rs` explains in
//! full, including the two rejected alternatives. Changing the derived
//! value therefore has THREE ways now, not two: changing `default_role`
//! (this module's own `apply_cycle_default_role`), promoting the running
//! session's model (this module's own `apply_promote_session_model_to_
//! default`), or hand-editing that role's `chain` in `settings.json` --
//! all three flow through the one existing source of truth
//! (`roles.<role>.chain`), so nothing here duplicates it.
//!
//! # Why a lax read, not the full `ConwayConfig`
//!
//! Same reasoning as `provider_manage.rs::load_roles_lax`'s own doc: an
//! operator's `settings.json` may carry a top-level `"//": "..."` comment
//! key that `ConwayConfig`'s `#[serde(deny_unknown_fields)]` schema does
//! not tolerate outside the one named `[tui]` exception. A live re-read
//! while a session is already running (this section's whole point -- see
//! `AppState::default_role_snapshot`'s own doc) must not fail just because
//! an unrelated part of the document uses that convention, so this reads
//! `default_role` the same lax way `load_roles_lax` already reads `roles`.

use std::collections::HashMap;
use std::path::Path;

use conway::config::schema::ConwayConfig;
use conway::config::{
    discovery, is_baked_in_role_floor, merged_document, set_default_role, set_role_chain,
    LoadOptions,
};
use conway::RoleAlias;

use super::provider_manage::load_roles_lax;
use super::App;
use crate::tui::state::Entry;

/// The scalar-valued sibling of `load_roles_lax`: reads the merged
/// document's own top-level `"default_role"` member, laxly -- see this
/// module's own doc, "Why a lax read, not the full `ConwayConfig`".
///
/// `pub(super)`, not private: board item `01M1A54RS91QHHHTY7N1PV8X0H`'s
/// `app/provider_manage.rs` reuses this exact function to find which role
/// a newly-added provider should be wired into (P-14 -- "which role is the
/// current default" is read exactly once across this crate, never
/// restated at a second callsite that could drift from this one when the
/// merge's own baked-in-floor handling changes).
pub(super) fn load_default_role_lax(
    env: &HashMap<String, String>,
    cwd: &Path,
) -> Result<RoleAlias, String> {
    let merged = merged_document(&LoadOptions {
        env: env.clone(),
        cwd: cwd.to_path_buf(),
        ..Default::default()
    })
    .map_err(|e| e.to_string())?;
    let value = merged
        .get("default_role")
        .cloned()
        .ok_or_else(|| "\"default_role\" is missing from the merged config".to_string())?;
    serde_json::from_value(value).map_err(|e| e.to_string())
}

impl App {
    /// Refreshes `AppState::default_role_snapshot`/`default_model_snapshot`/
    /// `known_role_names` from the REAL merged config -- never
    /// `self.conway.config()`'s stale build-time snapshot, mirroring
    /// `App::refresh_provider_entries_and_kick_off_status`'s own reasoning
    /// (see that function's own doc). Called on the same "`/settings` is
    /// about to open" seam as that function, and again after
    /// [`Self::apply_cycle_default_role`] writes, so a change this section
    /// just made is visible without a restart.
    ///
    /// A read failure (a hand-edited `settings.json` that no longer parses
    /// at all, or has dropped `default_role`) reports a named error and
    /// leaves the three snapshot fields at whatever they held before --
    /// never silently blanked, since a stale-but-honest value is a smaller
    /// harm than a menu that suddenly claims "no roles configured" for a
    /// config that is merely between edits.
    pub(super) fn refresh_default_entries(&mut self, env: &HashMap<String, String>, cwd: &Path) {
        let role = match load_default_role_lax(env, cwd) {
            Ok(role) => role,
            Err(e) => {
                self.state.transcript.push(Entry::Error {
                    text: format!("could not read default_role: {e}"),
                    fatal: false,
                });
                return;
            }
        };
        let roles = match load_roles_lax(env, cwd) {
            Ok(roles) => roles,
            Err(e) => {
                self.state.transcript.push(Entry::Error {
                    text: format!("could not read [roles]: {e}"),
                    fatal: false,
                });
                return;
            }
        };
        self.state.default_model_snapshot =
            ConwayConfig::model_for(&roles, role.as_str()).map(|s| s.to_string());
        self.state.default_role_snapshot = role.as_str().to_string();
        // `roles` came from the full five-source merge (`load_roles_lax`),
        // whose lowest layer always bakes in an untouched `"default"` role
        // floor (`conway::config::merge::default_document`'s own doc) so a
        // config with no `[roles]`/`default_role` of its own still
        // validates. That floor is a validation safety net, never a role
        // an operator declared, so it must not appear in the list a human
        // cycles through here -- see `is_baked_in_role_floor`'s own doc.
        self.state.known_role_names = roles
            .iter()
            .filter_map(|(name, entry)| {
                if is_baked_in_role_floor(name, entry) {
                    None
                } else {
                    Some(name.clone())
                }
            })
            .collect();
        // Board item `01M1A35S609TZ613GAECPEHX8D`: `/model` bare's own
        // listing -- every `"backend/model"` pair an OPERATOR-configured
        // role's `chain` names, sorted and deduped (a `BTreeSet` gives both
        // for free), from the SAME `roles` this function already loaded --
        // no second config read. Excludes the identical baked-in `"default"`
        // floor `known_role_names` just above excludes, for the identical
        // reason (see that field's own doc): a floor role's chain is a
        // validation safety net, never something to offer switching to.
        self.state.configured_models = roles
            .iter()
            .filter(|(name, entry)| !is_baked_in_role_floor(name, entry))
            .flat_map(|(_, entry)| entry.chain.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
    }

    /// `Enter` on the "defaults" section's `default role` leaf
    /// (`Action::CycleDefaultRole`): advances
    /// `state.default_role_snapshot` to the next name in
    /// `state.known_role_names` (sorted, wrapping past the last back to
    /// the first), and persists it via [`set_default_role`] -- see that
    /// function's own doc for the splice-only write. Refuses -- a named
    /// error, no write -- when no role is configured to cycle through at
    /// all.
    pub(super) fn apply_cycle_default_role(&mut self, env: &HashMap<String, String>, cwd: &Path) {
        if self.state.known_role_names.is_empty() {
            self.state.transcript.push(Entry::Error {
                text: "no roles are configured to cycle the default role through".to_string(),
                fatal: false,
            });
            return;
        }
        let current = &self.state.default_role_snapshot;
        let next_index = self
            .state
            .known_role_names
            .iter()
            .position(|r| r == current)
            .map(|i| (i + 1) % self.state.known_role_names.len())
            .unwrap_or(0);
        let next = self.state.known_role_names[next_index].clone();

        let Some(path) = discovery::user_config_path(env) else {
            self.state.transcript.push(Entry::Error {
                text: "could not resolve a home directory to write settings.json into".to_string(),
                fatal: false,
            });
            return;
        };
        match set_default_role(&path, &next) {
            Ok(_) => {
                self.state.transcript.push(Entry::Notice {
                    text: format!("default role: {next} (written to {})", path.display()),
                });
                self.refresh_default_entries(env, cwd);
            }
            Err(e) => {
                self.state.transcript.push(Entry::Error {
                    text: format!("could not set default role to {next}: {e}"),
                    fatal: false,
                });
            }
        }
    }

    /// `Enter` on the "defaults" section's `LEAF_PROMOTE_SESSION_MODEL`
    /// leaf (`Action::PromoteSessionModelToDefault`, board item
    /// `01M1AWGSTD7084VFVGN1GK9AS8`): writes `state.focused_model` -- the
    /// model THIS session is actually running, set by a real
    /// `Event::ModelDecision` -- to the head of the default role's own
    /// `chain`, persistently. See `view/settings.rs`'s own doc, "The
    /// persistent-default gap: promoting the session's model", for why
    /// this is a REORDER via [`set_role_chain`], never a new scalar.
    ///
    /// Reads the default role's CURRENT chain via
    /// `super::provider_manage::load_roles_lax` (reused, never a second
    /// opinion -- P-14, same reuse `refresh_default_entries` above already
    /// makes), removes any existing occurrence of `state.focused_model`
    /// (a model already present elsewhere in the chain must not appear
    /// TWICE once promoted), and inserts it at index 0 -- every other
    /// configured fallback keeps its own relative order, never truncated
    /// away: a chain's whole point is fallback candidates, and promoting
    /// one to head is not a reason to discard the rest.
    ///
    /// Refuses -- a named error, no write -- when `state.focused_model` is
    /// `None` (nothing to promote; `view/settings.rs`'s own `build_tree`
    /// should never even render this leaf in that state, but this guards
    /// the write path independently rather than trusting the render side
    /// alone) or when it already matches `state.default_model_snapshot`
    /// (nothing WOULD change; the render side already hides the leaf in
    /// this case too, for the identical reason).
    pub(super) fn apply_promote_session_model_to_default(
        &mut self,
        env: &HashMap<String, String>,
        cwd: &Path,
    ) {
        let Some(model) = self.state.focused_model.clone() else {
            self.state.transcript.push(Entry::Error {
                text: "no session model to promote yet -- this session has not received a \
                       model decision"
                    .to_string(),
                fatal: false,
            });
            return;
        };
        if self.state.default_model_snapshot.as_deref() == Some(model.as_str()) {
            self.state.transcript.push(Entry::Error {
                text: format!("{model} is already the persistent default"),
                fatal: false,
            });
            return;
        }

        let role = self.state.default_role_snapshot.clone();
        let roles = match load_roles_lax(env, cwd) {
            Ok(roles) => roles,
            Err(e) => {
                self.state.transcript.push(Entry::Error {
                    text: format!("could not read [roles] to promote {model}: {e}"),
                    fatal: false,
                });
                return;
            }
        };
        let mut chain: Vec<String> = roles
            .get(role.as_str())
            .map(|entry| entry.chain.clone())
            .unwrap_or_default();
        chain.retain(|m| m != &model);
        chain.insert(0, model.clone());

        let Some(path) = discovery::user_config_path(env) else {
            self.state.transcript.push(Entry::Error {
                text: "could not resolve a home directory to write settings.json into".to_string(),
                fatal: false,
            });
            return;
        };
        match set_role_chain(&path, &role, &chain) {
            Ok(_) => {
                self.state.transcript.push(Entry::Notice {
                    text: format!(
                        "default model: {model} (written to {}, head of role \"{role}\"'s \
                         chain)",
                        path.display()
                    ),
                });
                self.refresh_default_entries(env, cwd);
            }
            Err(e) => {
                self.state.transcript.push(Entry::Error {
                    text: format!("could not promote {model} to the persistent default: {e}"),
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
    use super::App;

    fn isolated_env(dir: &std::path::Path) -> HashMap<String, String> {
        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            dir.to_string_lossy().into_owned(),
        );
        env
    }

    /// ACCEPTANCE 2/3: opening the section reads the live `default_role`
    /// and the derived `default_model` (the head of that role's chain),
    /// never `Conway::config()`'s stale build-time snapshot.
    #[tokio::test]
    async fn refresh_reads_the_live_default_role_and_derives_the_default_model() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::json!({
                "default_role": "coder",
                "roles": {
                    "coder": {"chain": ["anthropic/claude-sonnet-4-6"]},
                    "reviewer": {"chain": ["kimi/k3"]}
                }
            })
            .to_string(),
        )
        .expect("write fixture");
        let env = isolated_env(dir.path());
        let cwd = tempfile::tempdir().expect("cwd tempdir");

        app.refresh_default_entries(&env, cwd.path());

        assert_eq!(app.state.default_role_snapshot, "coder");
        assert_eq!(
            app.state.default_model_snapshot.as_deref(),
            Some("anthropic/claude-sonnet-4-6")
        );
        assert_eq!(
            app.state.known_role_names,
            vec!["coder".to_string(), "reviewer".to_string()]
        );
    }

    /// A default role whose chain is empty derives `None`, not a
    /// synthesized model name.
    #[tokio::test]
    async fn refresh_derives_no_default_model_when_the_default_roles_chain_is_empty() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

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
        let env = isolated_env(dir.path());
        let cwd = tempfile::tempdir().expect("cwd tempdir");

        app.refresh_default_entries(&env, cwd.path());

        assert_eq!(app.state.default_model_snapshot, None);
    }

    /// ACCEPTANCE 3 ("setting either persists and is read back"), driven
    /// through `App`: cycling the default role writes the NEW role to
    /// `settings.json` (persists) and the snapshot fields reflect it
    /// immediately after (read back), including the derived default
    /// model following it to the new role's own chain.
    #[tokio::test]
    async fn cycling_the_default_role_persists_and_is_read_back() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::json!({
                "default_role": "coder",
                "roles": {
                    "coder": {"chain": ["anthropic/claude-sonnet-4-6"]},
                    "reviewer": {"chain": ["kimi/k3"]}
                }
            })
            .to_string(),
        )
        .expect("write fixture");
        let env = isolated_env(dir.path());
        let cwd = tempfile::tempdir().expect("cwd tempdir");

        app.refresh_default_entries(&env, cwd.path());
        assert_eq!(app.state.default_role_snapshot, "coder");

        app.apply_cycle_default_role(&env, cwd.path());

        // Persists: the file on disk now names the new role.
        let text = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["default_role"], "reviewer", "{text}");

        // Read back: the in-memory snapshot -- refreshed by
        // `apply_cycle_default_role` itself -- already reflects it, and the
        // derived default model follows the new role's own chain.
        assert_eq!(app.state.default_role_snapshot, "reviewer");
        assert_eq!(app.state.default_model_snapshot.as_deref(), Some("kimi/k3"));

        // Cycling again wraps back around to "coder" (sorted: coder,
        // reviewer).
        app.apply_cycle_default_role(&env, cwd.path());
        assert_eq!(app.state.default_role_snapshot, "coder");
    }

    /// No roles configured at all: refuses rather than panicking on an
    /// empty `known_role_names`, and never writes.
    #[tokio::test]
    async fn cycling_with_no_roles_configured_refuses_without_writing() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let dir = tempfile::tempdir().expect("tempdir");
        let original = serde_json::json!({"default_role": "coder", "roles": {}}).to_string();
        std::fs::write(dir.path().join("settings.json"), &original).expect("write fixture");
        let env = isolated_env(dir.path());
        let cwd = tempfile::tempdir().expect("cwd tempdir");

        app.refresh_default_entries(&env, cwd.path());
        assert!(app.state.known_role_names.is_empty());

        app.apply_cycle_default_role(&env, cwd.path());

        let text = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        assert_eq!(text, original, "a refusal must never write the file");
    }

    // ---------------------------------------------------------------
    // Board item `01M1AWGSTD7084VFVGN1GK9AS8`:
    // apply_promote_session_model_to_default.
    // ---------------------------------------------------------------

    /// ACCEPTANCE 1/2: a session running a DIFFERENT model than the
    /// persistent default writes that model to the head of the default
    /// role's chain -- a REORDER (the pre-existing fallback survives,
    /// after the promoted entry, in its original relative order), never a
    /// truncation to one entry and never a new scalar.
    #[tokio::test]
    async fn promoting_the_session_model_reorders_the_chain_and_persists() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::json!({
                "default_role": "coder",
                "roles": {
                    "coder": {"chain": ["local/qwen3.8:27b-mlx", "ollama_cloud/glm-5.2"]}
                }
            })
            .to_string(),
        )
        .expect("write fixture");
        let env = isolated_env(dir.path());
        let cwd = tempfile::tempdir().expect("cwd tempdir");

        app.refresh_default_entries(&env, cwd.path());
        assert_eq!(
            app.state.default_model_snapshot.as_deref(),
            Some("local/qwen3.8:27b-mlx"),
            "the chain head starts as the slow model, matching the operator's own report"
        );
        app.state.focused_model = Some("ollama_cloud/glm-5.2".to_string());

        app.apply_promote_session_model_to_default(&env, cwd.path());

        // Persists: the chain on disk now has the session's model FIRST,
        // with the previous head surviving right after it -- not dropped.
        let text = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value["roles"]["coder"]["chain"],
            serde_json::json!(["ollama_cloud/glm-5.2", "local/qwen3.8:27b-mlx"]),
            "{text}"
        );

        // Read back: the derived default model now follows the session's
        // own choice, without a restart.
        assert_eq!(
            app.state.default_model_snapshot.as_deref(),
            Some("ollama_cloud/glm-5.2")
        );
    }

    /// A session running a model that is not in the default role's chain
    /// AT ALL is still promotable -- inserted at the head, not refused.
    #[tokio::test]
    async fn promoting_a_model_absent_from_the_chain_inserts_it_at_the_head() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::json!({
                "default_role": "coder",
                "roles": {"coder": {"chain": ["anthropic/claude-sonnet-4-6"]}}
            })
            .to_string(),
        )
        .expect("write fixture");
        let env = isolated_env(dir.path());
        let cwd = tempfile::tempdir().expect("cwd tempdir");

        app.refresh_default_entries(&env, cwd.path());
        app.state.focused_model = Some("kimi/k3".to_string());

        app.apply_promote_session_model_to_default(&env, cwd.path());

        let text = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value["roles"]["coder"]["chain"],
            serde_json::json!(["kimi/k3", "anthropic/claude-sonnet-4-6"]),
            "{text}"
        );
    }

    /// No `focused_model` at all (no `ModelDecision` has arrived this
    /// session yet): refuses without writing, rather than promoting `None`
    /// or panicking on an absent value.
    #[tokio::test]
    async fn promoting_with_no_focused_model_refuses_without_writing() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let dir = tempfile::tempdir().expect("tempdir");
        let original = serde_json::json!({
            "default_role": "coder",
            "roles": {"coder": {"chain": ["anthropic/claude-sonnet-4-6"]}}
        })
        .to_string();
        std::fs::write(dir.path().join("settings.json"), &original).expect("write fixture");
        let env = isolated_env(dir.path());
        let cwd = tempfile::tempdir().expect("cwd tempdir");

        app.refresh_default_entries(&env, cwd.path());
        assert!(app.state.focused_model.is_none());

        app.apply_promote_session_model_to_default(&env, cwd.path());

        let text = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        assert_eq!(text, original, "a refusal must never write the file");
        assert!(
            app.state
                .transcript
                .iter()
                .any(|e| matches!(e, crate::tui::state::Entry::Error { text, .. }
                    if text.contains("no session model to promote"))),
            "a named refusal must reach the transcript"
        );
    }

    /// The session's model already IS the persistent default: refuses
    /// without writing -- there is nothing to promote, and a write here
    /// would be a no-op action pretending to be a decision.
    #[tokio::test]
    async fn promoting_a_model_that_already_matches_the_default_refuses_without_writing() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let dir = tempfile::tempdir().expect("tempdir");
        let original = serde_json::json!({
            "default_role": "coder",
            "roles": {"coder": {"chain": ["anthropic/claude-sonnet-4-6"]}}
        })
        .to_string();
        std::fs::write(dir.path().join("settings.json"), &original).expect("write fixture");
        let env = isolated_env(dir.path());
        let cwd = tempfile::tempdir().expect("cwd tempdir");

        app.refresh_default_entries(&env, cwd.path());
        app.state.focused_model = Some("anthropic/claude-sonnet-4-6".to_string());

        app.apply_promote_session_model_to_default(&env, cwd.path());

        let text = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        assert_eq!(text, original, "a refusal must never write the file");
    }
}
