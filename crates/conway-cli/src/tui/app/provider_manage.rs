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
//! # A newly added provider is wired into the routing chain, not just saved
//!
//! Board item `01M1A54RS91QHHHTY7N1PV8X0H` (2026-08-30): before this item,
//! [`App::apply_add_provider_choice`]/[`App::apply_add_provider_credential`]
//! called only [`set_backend_provider`], exactly the same one-write defect
//! `first_run.rs::finish_setup` had before board item
//! `01M1A2HKMDGNK961ZFV1EGZDQ0` fixed it there -- `backends.<id>` was
//! written and nothing else, so `default_role` fell through to
//! `conway::config::merge`'s baked-in, empty-chain validation floor and a
//! prompt afterward died with `no candidate for role default (0
//! considered)`. Reachable from a real, supported path: decline first-run
//! (`Esc` leaves the app open, board item `01M163T1KGX3HTCC2YMDPT655J`),
//! then add a provider through `/settings` -- the operator's own report
//! walked exactly this door.
//!
//! **Fixed by reusing, never restating, the exact writers
//! `first_run::persist_chain` already established (P-14):**
//! [`conway::config::ensure_default_role`], [`conway::config::set_role_chain`],
//! and [`crate::first_run::chain_entry`] for the `"backend/model"` format.
//! No second opinion about chain-entry shape is written here -- a second
//! construction that could drift from the first is exactly how the
//! original bug hid (see `01M1A2HKMDGNK961ZFV1EGZDQ0`'s own record).
//!
//! **Decision: a newly added provider is APPENDED to the CURRENT
//! `default_role`'s chain, never left unwired.** [`load_default_role_lax`]
//! (`app/defaults.rs`, reused rather than re-read a second way) names which
//! role that is; its existing chain (empty, for an unconfigured floor role,
//! or real, for an operator who already has one working) is read via the
//! same [`load_roles_lax`] the removal guard below already uses, and the
//! new `"id/model"` entry is pushed onto the end. This is deliberately the
//! SAME "order added = chain order" rule `first_run.rs`'s own module doc
//! states for guided setup, extended to `/settings`' add flow rather than
//! invented a second time: an operator who has just added a provider
//! clearly wants it usable, and appending never narrows an existing
//! chain's set of candidates the way removing one would -- there is no
//! symmetric hazard here to refuse against. Concretely, this means the
//! SAME code path handles both cases the item named: an unconfigured floor
//! role (chain starts empty, gains exactly one entry -- Case 1, the
//! reachable-and-broken one) and an already-working chain (gains a second,
//! independent fallback -- Case 2, the mild one) with no branch between
//! them; the floor role's chain is already empty in exactly the shape
//! "append" handles for free, so treating the two cases identically is not
//! a shortcut, it is the actual absence of a meaningful difference once the
//! read is a real one instead of a hardcoded guess. **Rejected: leaving a
//! newly added provider unwired (inert) until the operator does something
//! else.** That is silence about a state acceptance -- the operator adds a
//! provider through the ONLY add-a-provider surface this app has and sees
//! it listed with a status, with no visible sign that a prompt would never
//! actually reach it; this item's own spec calls that exact silence "what
//! made this a finding" and requires either wiring it or saying so loudly,
//! and wiring it is strictly less work than building a second UI just to
//! say "this one does nothing yet." A write failure during this second step
//! (the backend entry is already saved) is reported by name rather than
//! rolled back, mirroring `first_run::finish_setup`'s own "it works, but
//! the routing config could not be saved" posture for the identical
//! partial-success shape.
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
//!
//! ## 2026-08-30: the guard's own premise moved -- narrowed to "would leave a role with an EMPTY chain"
//!
//! **The paragraph above was correct when it was written, against the
//! premise that held at the time: chains were sparse, so "any role still
//! references this provider" and "removing it leaves a role with fewer
//! candidates than it had" were close enough in practice to treat alike.**
//! Board item `01M1A2HKMDGNK961ZFV1EGZDQ0`'s guided-setup fix, and this same
//! item's own add-provider fix directly above, both changed that: every
//! provider a first run or a `/settings` add ever configures now lands in
//! `default_role`'s chain, unconditionally. The operator hit the
//! consequence within minutes of rebuilding: *"opening settings->providers
//! doesn't let me remove a model (cannot remove ollama_cloud -- role(s)
//! default still names in their chain...). This happens with both models
//! which are available."* Once every configured provider is guaranteed to
//! be in a chain, "any role still references this provider" is true of
//! EVERY provider, always -- the guard's real intent (never let a role drop
//! to zero routable candidates) had quietly become "never let anything be
//! removed at all."
//!
//! **Fixed: the guard now refuses ONLY when removal would leave a role with
//! an EMPTY chain** ([`roles_left_unroutable_by_removing`], replacing
//! `roles_referencing_provider` outright rather than adding a second
//! function beside it -- the old "any reference" question is no longer one
//! this module needs answered anywhere). Removing one of two (or more)
//! entries in a role's chain is safe -- the role still has a real,
//! independently declared fallback and still routes -- and refusing it
//! would be refusing on behalf of a hazard that entry's own removal does
//! not create. Removing a role's LAST routable entry is still refused,
//! preserving the guard's actual intent (a role that could route
//! yesterday must not silently stop being able to today) rather than the
//! broader, now-accidentally-absolute rule that used to stand in for it.
//!
//! **Two alternatives were considered and rejected:**
//! - **Warn and proceed anyway**, letting a role narrow to zero candidates
//!   with only a transcript notice. Rejected for the identical reason the
//!   original ruling above rejected it: this is `plugin_toggle.rs`'s own
//!   "found out at the next routing failure instead of now" harm, and nothing
//!   about the premise moving changes that a role with an empty chain is a
//!   worse failure than a refused removal.
//! - **Keep "any reference blocks removal" but add a chain-editing UI** so
//!   the refusal's own "update those roles first" advice would actually be
//!   actionable. Rejected as materially more work for a worse outcome: it
//!   would still refuse removing a provider from a role that ALREADY has a
//!   perfectly good fallback, which is not a bug this guard should be
//!   preventing in the first place -- narrowing the refusal condition
//!   costs nothing extra and removes the false refusals a chain editor
//!   would only have made bearable, not correct.
//!
//! **The refusal message itself is corrected to stop naming an action the
//! operator cannot take.** The old wording said "update those roles first"
//! -- this app has never had a way to hand-edit a role's `chain` from the
//! UI, so that was always a dead end dressed as advice. The new wording
//! names the two actions that actually exist: add another provider first
//! (which -- per the section above -- appends to the SAME `default_role`
//! chain this guard is protecting, giving the affected role a real
//! fallback) or leave the provider configured. **Known imprecision,
//! disclosed rather than silently accepted:** "add another provider first"
//! is exactly right when the affected role is `default_role` (the common
//! case, and the only one this app's own write paths ever populate), but
//! this app has no way to point a newly added provider at a DIFFERENT,
//! hand-authored role's chain -- an operator who has hand-edited a second
//! role into existence and hit this refusal there has no in-app remedy for
//! that specific role today. That gap is a real, disclosed limit of the
//! current `/settings` surface (it has never supported authoring a
//! non-default role's chain at all), not a defect this item introduces or
//! silently papers over.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use conway::config::schema::RoleEntry;
use conway::config::{
    discovery, ensure_default_role, merged_document, set_backend_provider, set_role_chain,
    LoadOptions,
};

use super::defaults::load_default_role_lax;
use super::App;
use crate::first_run::{
    backend_entry_json, chain_entry, resolve_credential_plan, CredentialPlan, CredentialSource,
    HOSTED_CHOICES,
};
use crate::tui::state::Entry;

/// Every role name that would be left with a completely EMPTY `chain` if
/// every entry naming `provider_id` as its backend half
/// (`"<provider_id>/<model>"`) were removed from it -- pure, and free of
/// any `App`/`Conway` machinery, mirroring `app/plugin_toggle.rs::
/// enabled_dependents_requiring`'s own shape exactly: a plain function over
/// data, so a fabricated `roles` map is enough to exercise every branch.
/// Sorted, so a caller's message never depends on `BTreeMap` iteration
/// order changing under an unrelated edit (it will not, in practice, since
/// `BTreeMap` iterates in key order already -- sorted anyway so this
/// function's own contract does not silently depend on that fact).
///
/// **This module's own top doc, "2026-08-30: the guard's own premise
/// moved," has the full account of why this replaces the strictly broader
/// `roles_referencing_provider` (any reference at all, regardless of
/// whether the role had a fallback) rather than being added beside it.** A
/// role whose chain has at least one OTHER, independently declared entry
/// survives the removal with a real fallback and is deliberately NOT
/// returned; a role whose chain is ALREADY empty has nothing to lose and is
/// not returned either -- removal is refused only for a role this WOULD
/// newly strand.
///
/// A malformed chain entry (no `/` at all, or an empty backend half) simply
/// never matches -- P-10's "untrusted input, no panics" applies to a
/// hand-edited config's `roles.*.chain` exactly as it does to a typed
/// credential; this never panics on one. **Accepted limit, same scope its
/// predecessor always had:** this inspects the DECLARED chain array only,
/// never which entries would actually resolve to a live, registered
/// backend at runtime -- a chain mixing a malformed entry with a real
/// reference to `provider_id` is read as "has a fallback" (the malformed
/// entry keeps the chain non-empty after removal) even though that
/// malformed entry was never going to route anywhere either. Resolving
/// that would require this module to duplicate the router's own candidate
/// classification, a far larger surface than a removal guard should own.
pub(super) fn roles_left_unroutable_by_removing(
    roles: &BTreeMap<String, RoleEntry>,
    provider_id: &str,
) -> Vec<String> {
    let mut names: Vec<String> = roles
        .iter()
        .filter(|(_, entry)| {
            !entry.chain.is_empty()
                && entry.chain.iter().all(|link| {
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
///
/// `pub(super)`, not private: board item `01M18Q7P25DTSKQJDJJCC3E800`'s
/// `app/defaults.rs` reuses this exact function for its own
/// `default_role`-cycling refresh rather than re-reading `[roles]` a
/// second way (P-14) -- see that module's own doc.
pub(super) fn load_roles_lax(
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

/// Gives a just-saved `backends.<id>` entry a place to route from: reads
/// which role `default_role` currently names ([`load_default_role_lax`],
/// `app/defaults.rs`, reused rather than re-read a second way -- P-14) and
/// that role's current `chain` ([`load_roles_lax`], just above -- empty for
/// an unconfigured floor role, real for an operator who already has one),
/// appends `chain_entry(id, model)`, and persists both the role name (in
/// case `default_role` itself had never been written to the file) and the
/// full new chain via the SAME two writers `first_run::persist_chain`
/// already calls: [`ensure_default_role`] then [`set_role_chain`]. See this
/// module's own top doc, "A newly added provider is wired into the routing
/// chain, not just saved," for why appending (never leaving it unwired) is
/// the deliberate choice here, covering both the empty-floor-role case and
/// the already-working-chain case with the one code path. Returns the role
/// name actually wired into, for the caller's own success message.
///
/// Called only AFTER [`set_backend_provider`] has already succeeded (see
/// [`App::write_provider_entry_and_refresh`]) -- `path` is therefore
/// guaranteed to exist and be valid JSON by the time this runs, exactly the
/// precondition [`ensure_default_role`]/[`set_role_chain`] both document
/// refusing to invent on their own.
fn wire_provider_into_default_chain(
    path: &Path,
    id: &str,
    model: &str,
    env: &HashMap<String, String>,
    cwd: &Path,
) -> Result<String, String> {
    let role_name = load_default_role_lax(env, cwd)?.as_str().to_string();
    let roles = load_roles_lax(env, cwd)?;
    let mut chain: Vec<String> = roles
        .get(role_name.as_str())
        .map(|entry| entry.chain.clone())
        .unwrap_or_default();
    chain.push(chain_entry(id, model));
    ensure_default_role(path, &role_name).map_err(|e| e.to_string())?;
    set_role_chain(path, &role_name, &chain).map_err(|e| e.to_string())?;
    Ok(role_name)
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
                self.write_provider_entry_and_refresh(
                    choice.id,
                    &entry_json,
                    choice.default_model,
                    env,
                    cwd,
                );
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
        self.write_provider_entry_and_refresh(
            choice.id,
            &entry_json,
            choice.default_model,
            env,
            cwd,
        );
    }

    /// The shared write-then-refresh tail both add paths above end in:
    /// [`set_backend_provider`] (USER SCOPE, per that function's own doc),
    /// [`wire_provider_into_default_chain`] (this module's own top doc, "A
    /// newly added provider is wired into the routing chain, not just
    /// saved" -- board item `01M1A54RS91QHHHTY7N1PV8X0H`), a transcript
    /// notice/error, and -- on success -- [`Self::
    /// refresh_provider_entries_and_kick_off_status`] so the freshly added
    /// provider appears (and is classified) without a restart, acceptance
    /// 5's own requirement.
    fn write_provider_entry_and_refresh(
        &mut self,
        id: &str,
        entry_json: &str,
        model: &str,
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
                match wire_provider_into_default_chain(&path, id, model, env, cwd) {
                    Ok(role_name) => {
                        self.state.transcript.push(Entry::Notice {
                            text: format!(
                                "{id}: added to {} and wired into role \"{role_name}\"'s chain",
                                path.display()
                            ),
                        });
                    }
                    Err(e) => {
                        self.state.transcript.push(Entry::Error {
                            text: format!(
                                "{id}: added to {}, but its routing chain entry could not be \
                                 written ({e}) -- it will not answer any prompt until that is \
                                 fixed",
                                path.display()
                            ),
                            fatal: false,
                        });
                    }
                }
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
    /// affected role, before any write -- when
    /// [`roles_left_unroutable_by_removing`] finds one (a role this removal
    /// would leave with an EMPTY chain -- not merely a role that still
    /// mentions this provider somewhere in a chain that has other,
    /// independently usable entries too). See this module's own doc,
    /// "Removal has consequences" and its `2026-08-30` addendum, for why
    /// refusal (not warn-and-proceed) is this item's ruling and why the
    /// guard's own criterion narrowed to exactly this.
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

        let affected = roles_left_unroutable_by_removing(&roles, provider_id);
        if !affected.is_empty() {
            let (role_word, verb, possessive, pronoun) = if affected.len() == 1 {
                ("role", "names", "its", "it")
            } else {
                ("roles", "name", "their", "them")
            };
            self.state.transcript.push(Entry::Error {
                text: format!(
                    "cannot remove {provider_id} -- {role_word} {} {verb} it as {possessive} \
                     only routable entry; add another provider first to give {pronoun} a \
                     fallback, or leave {provider_id} configured",
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
    use super::{roles_left_unroutable_by_removing, App};
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
    // roles_left_unroutable_by_removing -- pure function coverage.
    // ---------------------------------------------------------------

    /// A role whose ONLY entry names the provider would be left with an
    /// empty chain -- blocked. A role naming a DIFFERENT provider is
    /// untouched either way.
    #[test]
    fn finds_only_roles_that_would_be_left_with_an_empty_chain() {
        let roles = std::collections::BTreeMap::from([
            ("coder".to_string(), role(&["kimi/k3"])),
            ("reviewer".to_string(), role(&["anthropic/claude"])),
        ]);
        assert_eq!(
            roles_left_unroutable_by_removing(&roles, "kimi"),
            vec!["coder".to_string()]
        );
        assert!(roles_left_unroutable_by_removing(&roles, "openai").is_empty());
    }

    /// 2026-08-30's own narrowing, pinned directly: a role with a REAL
    /// fallback (a second, different-backend entry) is NOT returned, even
    /// though it does still name the provider being removed somewhere in
    /// its chain -- the discriminating case this whole item exists to fix.
    /// Before this narrowing, this returned `["coder"]`.
    #[test]
    fn a_role_with_a_real_fallback_is_not_left_unroutable() {
        let roles = std::collections::BTreeMap::from([(
            "coder".to_string(),
            role(&["kimi/k3", "anthropic/claude"]),
        )]);
        assert!(
            roles_left_unroutable_by_removing(&roles, "kimi").is_empty(),
            "a role with another backend still in its chain must not block removal"
        );
    }

    /// A role whose chain is ALREADY empty has nothing to lose -- this
    /// removal did not cause that, so it must not be reported as
    /// newly-unroutable.
    #[test]
    fn a_role_with_an_already_empty_chain_is_not_reported() {
        let roles = std::collections::BTreeMap::from([("coder".to_string(), role(&[]))]);
        assert!(roles_left_unroutable_by_removing(&roles, "kimi").is_empty());
    }

    #[test]
    fn a_malformed_chain_entry_never_panics_and_never_matches() {
        let roles = std::collections::BTreeMap::from([(
            "coder".to_string(),
            role(&["not-a-model-ref", "/nobackend", ""]),
        )]);
        assert!(roles_left_unroutable_by_removing(&roles, "kimi").is_empty());
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

        // Board item `01M1A54RS91QHHHTY7N1PV8X0H`: adding a provider must
        // also wire it into `default_role`'s chain, not just save
        // `backends.anthropic` -- the fixture's `coder` role started with
        // an empty chain (Case 1's shape: nothing routable yet), and must
        // now name the freshly added provider.
        assert_eq!(
            value["roles"]["coder"]["chain"],
            serde_json::json!(["anthropic/claude-sonnet-4-6"]),
            "{text}"
        );

        // Acceptance 5's own words: it appears as working WITHOUT A
        // RESTART. `provider_entries` is a config snapshot re-read fresh
        // (never `Conway::config()`'s stale build-time one), and the
        // background classification is already under way.
        assert_eq!(app.state.provider_entries.len(), 1);
        assert!(app.state.provider_status_loading);
    }

    /// Case 2 (mild): a role that already has a real, working chain gains
    /// the new provider as an ADDITIONAL entry, appended after the
    /// existing one(s) -- "order added = chain order," the same rule
    /// `first_run.rs` documents for guided setup, extended here rather than
    /// re-decided. The existing entry must survive untouched.
    #[tokio::test]
    async fn adding_a_second_provider_appends_to_an_already_working_chain() {
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
        let mut env = isolated_env(dir.path());
        env.insert("ANTHROPIC_API_KEY".to_string(), "sk-real".to_string());
        let cwd = tempfile::tempdir().expect("cwd tempdir");

        app.apply_add_provider_choice("anthropic", &env, cwd.path());

        let text = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value["roles"]["coder"]["chain"],
            serde_json::json!(["kimi/k3", "anthropic/claude-sonnet-4-6"]),
            "the existing entry must survive, with the new one appended after it: {text}"
        );
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
    // Acceptance 4: removing a role's LAST routable entry is still
    // refused, BEFORE any write -- observed as no write happening, not
    // merely a message existing. Fixture shape matches acceptance 4's own
    // wording: exactly what a one-provider first run now produces
    // (`default_role: "default"`, one backend, a one-entry chain).
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn removing_the_only_provider_in_a_single_provider_config_is_refused_before_the_write() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::json!({
                "default_role": "default",
                "backends": {"kimi": {"kind": "openai-compat", "api_key": "sk-1"}},
                "roles": {"default": {"chain": ["kimi/k3"]}}
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
                Entry::Error { text, fatal: false } if text.contains("kimi") && text.contains("default")
            )),
            "the refusal must name both the provider and the affected role: {:?}",
            app.state.transcript
        );
        // Acceptance 5: the message must name only actions the operator
        // can actually take through this app -- never the old, dead-end
        // "update those roles first" (there has never been a chain editor
        // in this UI).
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Error { text, .. } if text.contains("add another provider")
            )),
            "the refusal must point at a real, reachable remedy: {:?}",
            app.state.transcript
        );
        assert!(
            !app
                .state
                .transcript
                .iter()
                .any(|e| matches!(e, Entry::Error { text, .. } if text.contains("update those roles"))),
            "the refusal must not name an action the operator has no UI for: {:?}",
            app.state.transcript
        );
    }

    /// Acceptance 3: a provider that is NOT the last routable entry for any
    /// role -- here, `kimi` has a real fallback (`anthropic`) in the same
    /// chain -- can be removed. Before the 2026-08-30 narrowing, this was
    /// refused solely because `kimi` was still MENTIONED in `coder`'s
    /// chain, even though `anthropic` would keep it routing perfectly well
    /// afterward.
    #[tokio::test]
    async fn removing_a_provider_that_still_leaves_a_role_with_a_fallback_succeeds() {
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
                "roles": {"coder": {"chain": ["kimi/k3", "anthropic/claude"]}}
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
            "kimi must actually be removed, since coder still routes via anthropic: {text}"
        );
        assert!(
            !app.state.transcript.iter().any(|e| matches!(e, Entry::Error { .. })),
            "a successful removal must not also carry a refusal: {:?}",
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

    // ---------------------------------------------------------------
    // Board item `01M1A54RS91QHHHTY7N1PV8X0H`, acceptance 1 -- THE
    // load-bearing test for this whole item. Asserting only that
    // `backends.<id>` was written is the test that was GREEN while the
    // product was broken (see this module's own top doc); this one builds
    // a SEPARATE `Conway` from the exact file the write path left behind
    // and completes a real turn against a real (mock) server, mirroring
    // `crates/conway-cli/tests/first_run.rs`'s own
    // `finish_setup_writes_a_config_that_routes_from_the_file_alone` --
    // the sibling test for the identical defect on the guided-setup side
    // of this same bug (board item `01M1A2HKMDGNK961ZFV1EGZDQ0`).
    // ---------------------------------------------------------------

    /// SSE body for one plain-text, no-tool-calls completion -- the same
    /// wire shape `crates/conway-plugin-backends/tests/openai_compat_stream.rs`'s
    /// own `sse_body` helper produces, reconstructed here rather than
    /// imported (that helper lives in a different crate's `tests/`
    /// directory, unreachable from this crate's own `#[cfg(test)]` code).
    fn sse_ok_body() -> String {
        format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            serde_json::json!({"choices": [{"delta": {"content": "ok"}, "finish_reason": null}]}),
            serde_json::json!({"choices": [{"delta": {}, "finish_reason": "stop"}]}),
        )
    }

    /// Builds a SEPARATE, real `Conway` straight from the file at `path`
    /// (never reusing the `App`/`echo_conway` fixture's own in-memory
    /// backend) and completes one real turn against it -- the same
    /// "prove the FILE routes, not a throwaway in-memory config" shape
    /// `crates/conway-cli/tests/first_run.rs::complete_a_turn_from_the_file_at`
    /// uses for the sibling item, reconstructed here for the identical
    /// reason that helper is unreachable from this crate's own `src/`
    /// test code (it lives under a different compilation root, `tests/`).
    async fn complete_a_turn_from_the_file_at(
        dir: &std::path::Path,
        path: &std::path::Path,
    ) -> conway::ResultStatus {
        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            dir.to_string_lossy().into_owned(),
        );
        let options = conway::config::LoadOptions {
            cwd: dir.to_path_buf(),
            explicit_path: Some(path.to_path_buf()),
            env,
            cli_overrides: Default::default(),
            model_metadata_refresh: false,
        };
        let conway = conway::ConwayBuilder::from_options_ignoring_user_config(options)
            .expect("the file the write path wrote must itself load")
            .with_backend_factory(std::sync::Arc::new(
                conway_plugin_backends::AnthropicBackendFactory,
            ))
            .with_backend_factory(std::sync::Arc::new(
                conway_plugin_backends::OpenAiCompatBackendFactory,
            ))
            .with_permission_gate(std::sync::Arc::new(conway::gates::DenyAllGate))
            .build()
            .expect("build must succeed against the file the write path wrote");
        let handle = conway
            .new_session(conway::SessionSpec::default())
            .await
            .expect("new_session must succeed against the file the write path wrote");
        let turn = handle
            .prompt("Reply with exactly one word: ok")
            .await
            .expect("prompt must succeed");
        turn.result().await.expect("result must resolve").status
    }

    #[tokio::test]
    async fn add_provider_via_settings_after_declining_first_run_completes_a_real_prompt() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_string(sse_ok_body())
                    .insert_header("content-type", "text/event-stream"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");

        // Declined first-run: no `settings.json` anywhere for this isolated
        // config dir -- the exact state `Esc` at first-run leaves behind
        // (board item `01M163T1KGX3HTCC2YMDPT655J`, "declining ... leaves
        // the app open, in every state").
        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());
        let cwd = tempfile::tempdir().expect("cwd tempdir");
        assert!(!dir.path().join("settings.json").exists());

        let entry_json = serde_json::json!({
            "kind": "openai-compat",
            "dialect": "openai",
            "base_url": server.uri(),
            "api_key": "any-key",
        })
        .to_string();

        // Drives the module's own shared write-then-wire tail directly,
        // exactly the function `apply_add_provider_choice`'s `ReuseEnvVar`
        // arm calls -- `HOSTED_CHOICES`'s three real entries all point at
        // real hosted URLs (Anthropic, OpenAI, Ollama Cloud) that a
        // hermetic test cannot reach, so this substitutes a caller-supplied
        // entry pointed at the local mock server above, the same
        // substitution `crates/conway-cli/tests/first_run.rs`'s own
        // acceptance-1 test makes (calling `finish_setup` directly rather
        // than the interactive `run_guided_setup`) for the analogous reason
        // (there, a pty; here, a fixed real base URL).
        app.write_provider_entry_and_refresh("mock", &entry_json, "mock-model", &env, cwd.path());

        let path = dir.path().join("settings.json");
        let text =
            std::fs::read_to_string(&path).expect("the write path must have created the file");
        let value: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(value["backends"]["mock"]["kind"], "openai-compat");
        assert_eq!(
            value["roles"]["default"]["chain"],
            serde_json::json!(["mock/mock-model"]),
            "{text}"
        );

        let status = complete_a_turn_from_the_file_at(dir.path(), &path).await;
        assert_eq!(
            status,
            conway::ResultStatus::Completed,
            "the file /settings' add-provider flow wrote must itself route and complete a turn, \
             not merely have the right JSON shape: {status:?}"
        );
    }
}
