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
//!
//! ## Dependency enforcement (board item `01M0WWMQZN5WK1AADKW4WKTQQZ`)
//!
//! `docs/vision/DESIGN-plugin-dependencies.md` §3 names three enablement
//! points where ruling 3 ("a plugin cannot be enabled without its
//! dependencies enabled -- not degraded, not silently auto-installed --
//! refused") has to hold. This module is the SECOND -- the interactive
//! `/plugin` toggle -- and specifically its sharper, previously-unguarded
//! half: **turning a plugin OFF while an enabled plugin still `requires`
//! it** used to write straight to `settings.json` and print a cheerful
//! "turned off" notice, with the operator finding out the dependent broke
//! only at the next restart's `ConwayBuilder::build`. [`App::apply_plugin_toggle`]
//! now refuses that write, before it happens, naming the still-enabled
//! dependent (§3's own "before the write").
//!
//! Four checks, matching §4b's "Failure modes, matching ruling 3" list and
//! this item's own acceptance criteria:
//!
//! 1. Toggle **off** a plugin some enabled plugin `requires` -> refused,
//!    naming the dependent, before any write
//!    ([`enabled_dependents_requiring`]).
//! 2. Toggle **off** a plugin some enabled plugin merely `optional`s ->
//!    allowed, with a Notice naming what is lost
//!    ([`enabled_optional_dependents`]).
//! 3. Toggle **on** a plugin whose bundled `requires` is unmet -> NOT
//!    silently written; an offer notice names the missing, bundled
//!    dependency and how to proceed ([`missing_required_dependencies`]). An
//!    unmet dependency this binary does not even link at all is refused
//!    outright -- there is nothing bundled to offer.
//! 4. A plugin currently degraded (installed, with a missing `optional`
//!    dependency) says so **in the browser** -- [`refresh_degradation_annotation`]
//!    rewrites the affected row's own `description.you_lose` after every
//!    toggle, which `view/plugins.rs`'s detail panel already renders
//!    verbatim (no change needed there).
//!
//! **The three enablement points are distinct call sites, not one shared
//! choke point.** `ConwayBuilder::build`'s own hard-fail (landed earlier the
//! same day, `01M0WWJMYK0KDC2X7B7MR46FRR`) is the first and authoritative
//! one; this module is the second, interactive one; `/plugin install`'s
//! marketplace trigger is the third (see that module's own doc for why
//! auto-installing a NON-bundled dependency there is explicitly refused
//! rather than fetched -- `DESIGN-plugin-dependencies.md` §4b's own "trust
//! ruling the marketplace work just settled"). A rule enforced at only two
//! of the three is a hole, not a partial win, so each gets its own
//! dedicated coverage rather than one shared assumption that fixing the
//! build-time check was enough.
//!
//! **Auto-install is never in scope here, even for a bundled dependency.**
//! [`App::apply_plugin_toggle`] never writes a dependency's own toggle for the
//! operator -- accepting checks 1/2/3 above changes only what gets WRITTEN
//! for the plugin the operator actually asked to toggle (nothing, when
//! refused; that one plugin, when allowed), never a second plugin's own
//! `plugins.install` membership. "Offer" in check 3 is realized as an
//! actionable transcript message, not a second write -- the true
//! one-keystroke "accept and enable both" affordance §4b's own text
//! anticipates needs a NEW interactive surface (a confirm keybinding on the
//! `/plugin` browser's own row, `view/plugins.rs`/`input.rs`), which is
//! deliberately left as a disclosed follow-up: this item's own file
//! ownership fence grants this module and `view/settings.rs` (the
//! shortcut-only settings section, not the browser itself) alone.
//!
//! **Restart-to-apply stays untouched.** Every check above runs against
//! `self.state.plugin_browser`'s own display mirror (`installed` per row)
//! and the compiled-in bundle's manifests -- never against `self.conway`,
//! the running session's own installed plugin set, which this method's own
//! pre-existing doc already establishes it never touches. This is a
//! PRE-check ahead of the write, not live dependency reconciliation
//! mid-session (`DESIGN-plugin-dependencies.md` §3's own "Restart-to-apply
//! is convenient here").

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use conway::plugin::PluginManifest;

use super::App;
use crate::tui::state::{Entry, PluginBrowserEntry};

/// Every currently-ENABLED plugin (by `enabled`, the display-mirror id set)
/// other than `plugin_id` itself whose own [`PluginManifest::requires`]
/// names `plugin_id` -- the exact set that would break, silently, if
/// `plugin_id` were turned off (`DESIGN-plugin-dependencies.md` §3's
/// headline defect). Sorted, so a caller's message and a test's assertion
/// never depend on `manifests`' own iteration order.
///
/// Pure and free-standing (no `App`/`Conway`/TUI machinery), the SAME shape
/// `crates/conway/src/builder.rs`'s `missing_required_dependency`/
/// `missing_optional_dependencies` take, so a fabricated `PluginManifest`
/// fixture is enough to exercise every branch -- no real first-party plugin
/// declares a `requires`/`optional` edge yet (the mechanism landed the same
/// day this item's own bundled dependent might not exist), so a test that
/// could only drive the REAL compiled-in bundle could never observe a
/// refusal at all.
fn enabled_dependents_requiring(
    manifests: &[PluginManifest],
    enabled: &HashSet<String>,
    plugin_id: &str,
) -> Vec<String> {
    let mut dependents: Vec<String> = manifests
        .iter()
        .filter(|m| m.id != plugin_id && enabled.contains(&m.id))
        .filter(|m| m.requires.iter().any(|dep| dep == plugin_id))
        .map(|m| m.id.clone())
        .collect();
    dependents.sort();
    dependents
}

/// The `optional` counterpart of [`enabled_dependents_requiring`]: every
/// currently-enabled plugin (other than `plugin_id`) whose
/// [`PluginManifest::optional`] names `plugin_id` -- never a reason to
/// refuse (§4a's "absence degrades, and is announced"), but exactly the set
/// a toggle-off notice must name so the operator knows what is about to
/// lose only a presentation/convenience, not its core function.
fn enabled_optional_dependents(
    manifests: &[PluginManifest],
    enabled: &HashSet<String>,
    plugin_id: &str,
) -> Vec<String> {
    let mut dependents: Vec<String> = manifests
        .iter()
        .filter(|m| m.id != plugin_id && enabled.contains(&m.id))
        .filter(|m| m.optional.iter().any(|dep| dep == plugin_id))
        .map(|m| m.id.clone())
        .collect();
    dependents.sort();
    dependents
}

/// `plugin_id`'s own [`PluginManifest::requires`] entries NOT already in
/// `enabled` -- what turning `plugin_id` ON, right now, would leave
/// unsatisfied. Returns an empty `Vec` when `plugin_id` is unknown to
/// `manifests` (nothing this call can reason about) or declares no unmet
/// requirement.
fn missing_required_dependencies(
    manifests: &[PluginManifest],
    enabled: &HashSet<String>,
    plugin_id: &str,
) -> Vec<String> {
    let Some(manifest) = manifests.iter().find(|m| m.id == plugin_id) else {
        return Vec::new();
    };
    let mut missing: Vec<String> = manifest
        .requires
        .iter()
        .filter(|dep| !enabled.contains(dep.as_str()))
        .cloned()
        .collect();
    missing.sort();
    missing
}

/// The fixed marker [`refresh_degradation_annotation`] appends to (and, on
/// a later call, strips back out of) a row's own
/// `PluginDescription::you_lose` -- distinct enough from ordinary curated
/// prose that a plugin author's own text is exceedingly unlikely to collide
/// with it by accident, and never repeated: every call first truncates
/// `you_lose` back to whatever precedes this marker, so re-annotating after
/// a later toggle can never accumulate duplicate notes.
const DEGRADATION_MARKER: &str = " [DEGRADED: optionally uses ";

/// Acceptance criterion 4: **a degraded plugin says so in the browser**,
/// not only in a one-shot transcript notice at the moment it degraded.
/// Rewrites `entry.description.you_lose` in place -- first stripping any
/// PRIOR [`DEGRADATION_MARKER`] annotation (idempotent: calling this twice
/// in a row, or against an unchanged world, leaves the text unchanged),
/// then appending a fresh one naming every currently-missing `optional`
/// dependency, but ONLY when `entry.installed` (an OFF row has nothing to
/// degrade -- it contributes nothing at all right now, which is a
/// different, already-honest story `view/plugins.rs`'s own `active` field
/// tells).
///
/// `manifest: None` (the row's own id is not in the bundle this call was
/// given -- should not happen for a real compiled-in row, but a test
/// fixture may deliberately omit one) still strips a stale marker, so a
/// plugin that drops out of the bundle entirely never keeps showing a
/// degradation note nothing can explain any more.
///
/// `view/plugins.rs`'s `draw_plugin_detail` already renders `you_lose`
/// verbatim in its "you lose" line -- this needs no renderer change at all,
/// only an honest value to render.
fn refresh_degradation_annotation(
    entry: &mut PluginBrowserEntry,
    manifest: Option<&PluginManifest>,
    enabled: &HashSet<String>,
) {
    if let Some(marker_at) = entry.description.you_lose.find(DEGRADATION_MARKER) {
        entry.description.you_lose.truncate(marker_at);
    }
    let Some(manifest) = manifest else {
        return;
    };
    if !entry.installed {
        return;
    }
    let mut missing: Vec<&str> = manifest
        .optional
        .iter()
        .filter(|dep| !enabled.contains(dep.as_str()))
        .map(|dep| dep.as_str())
        .collect();
    if missing.is_empty() {
        return;
    }
    missing.sort_unstable();
    entry.description.you_lose.push_str(DEGRADATION_MARKER);
    entry.description.you_lose.push_str(&missing.join(", "));
    entry
        .description
        .you_lose
        .push_str("; currently off -- presentation/convenience only, core function unaffected]");
}

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
    /// successful write, so a failed write -- INCLUDING a dependency
    /// refusal, this module's own doc -- can never claim a state that disk
    /// does not actually hold.
    ///
    /// Resolves the compiled-in bundle's own manifests (this binary's real
    /// `requires`/`optional` graph, via `first_party_plugins::
    /// all_bundle_plugins`, the SAME read `app/startup.rs` uses to build
    /// `plugin_browser` in the first place) and hands them to
    /// [`Self::apply_plugin_toggle_against`], which carries every actual
    /// decision. Split in two so a test can drive the decision logic
    /// against a FABRICATED manifest graph (this binary's real bundle
    /// declares no `requires`/`optional` edge yet, so a test that could
    /// only exercise the real bundle could never observe a refusal --
    /// see [`enabled_dependents_requiring`]'s own doc).
    pub(super) fn apply_plugin_toggle(
        &mut self,
        plugin_id: String,
        installed: bool,
        env: &HashMap<String, String>,
        cwd: &std::path::Path,
    ) {
        let bundle_cwd = self.conway.config().cwd.clone();
        // A throwaway, never-opened store -- this call only ever reads
        // `.manifest()` off each candidate, never a method that touches a
        // `MemoryStore`, mirroring `app/startup.rs`'s own identical
        // `browse_memory_store` (see that call site's own comment for why
        // opening the REAL durable store again here would violate
        // `first_party_plugins::resolve_memory_store`'s single-open-site
        // invariant for no benefit).
        let browse_memory_store: Arc<dyn conway::plugin::MemoryStore> =
            Arc::new(conway_plugin_memory::InMemoryMemoryStore::new());
        let manifests: Vec<PluginManifest> =
            crate::first_party_plugins::all_bundle_plugins(&bundle_cwd, browse_memory_store, env)
                .iter()
                .map(|p| p.manifest())
                .collect();
        self.apply_plugin_toggle_against(plugin_id, installed, env, cwd, &manifests);
    }

    /// The decision-and-write core [`Self::apply_plugin_toggle`] delegates
    /// to, parametrized by `manifests` (the bundle's own dependency graph)
    /// rather than resolving it itself -- see that method's own doc for
    /// why. `pub(super)` (not private) so this module's own test suite can
    /// drive it directly against a fabricated graph; `run.rs`'s one
    /// production call site still only ever calls [`Self::
    /// apply_plugin_toggle`], unchanged.
    pub(super) fn apply_plugin_toggle_against(
        &mut self,
        plugin_id: String,
        installed: bool,
        env: &HashMap<String, String>,
        cwd: &std::path::Path,
        manifests: &[PluginManifest],
    ) {
        let enabled_before: HashSet<String> = self
            .state
            .plugin_browser
            .iter()
            .filter(|entry| entry.installed)
            .map(|entry| entry.id.clone())
            .collect();

        // ---- Check 1: toggle OFF, refuse if a still-enabled plugin
        // `requires` this one -- BEFORE any write, naming the dependent
        // (`DESIGN-plugin-dependencies.md` §4b's own failure-mode list).
        if !installed {
            let dependents = enabled_dependents_requiring(manifests, &enabled_before, &plugin_id);
            if !dependents.is_empty() {
                let (verb, pronoun) = if dependents.len() == 1 {
                    ("requires", "it")
                } else {
                    ("require", "them")
                };
                self.state.transcript.push(Entry::Error {
                    text: format!(
                        "cannot turn {plugin_id} off -- {} still {verb} it and cannot function \
                         without it; turn {pronoun} off first, or leave {plugin_id} on",
                        dependents.join(", "),
                    ),
                    fatal: false,
                });
                return;
            }
        } else {
            // ---- Check 3: toggle ON, offer rather than silently write
            // when `plugin_id`'s own `requires` is unmet.
            let missing = missing_required_dependencies(manifests, &enabled_before, &plugin_id);
            if !missing.is_empty() {
                let known_ids: HashSet<&str> = manifests.iter().map(|m| m.id.as_str()).collect();
                let (bundled, unbundled): (Vec<String>, Vec<String>) = missing
                    .into_iter()
                    .partition(|dep| known_ids.contains(dep.as_str()));
                if !unbundled.is_empty() {
                    // Scope fence: a dependency this binary does not even
                    // link cannot be offered -- there is nothing bundled to
                    // enable, and auto-fetching a non-bundled dependency is
                    // explicitly out of scope here (that is the marketplace
                    // trigger's own refusal, a different enablement point).
                    let (verb, subj) = if unbundled.len() == 1 {
                        ("is", "it")
                    } else {
                        ("are", "them")
                    };
                    self.state.transcript.push(Entry::Error {
                        text: format!(
                            "cannot turn {plugin_id} on -- it requires {}, which {verb} not \
                             linked into this binary at all; nothing here can enable {subj}",
                            unbundled.join(", "),
                        ),
                        fatal: false,
                    });
                    return;
                }
                // Every missing dependency is bundled -- offer, don't
                // silently enable either plugin (this method's own doc,
                // "Auto-install is never in scope here").
                let (verb, subj) = if bundled.len() == 1 {
                    ("is", "it")
                } else {
                    ("are", "them")
                };
                self.state.transcript.push(Entry::Notice {
                    text: format!(
                        "{plugin_id} requires {} to function, and {verb} currently off -- \
                         {plugin_id} was NOT turned on. {} bundled with this binary (no \
                         download, no trust decision) -- turn {subj} on first, then {plugin_id}.",
                        bundled.join(", "),
                        if bundled.len() == 1 {
                            "It is"
                        } else {
                            "They are"
                        },
                    ),
                });
                return;
            }
        }

        let path = conway::config::discovery::user_config_path(env);
        let outcome = match &path {
            Some(path) => conway::config::set_plugin_installed(path, &plugin_id, installed),
            None => Err(conway::FacadeError::Config {
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

                // ---- Check 2: toggle OFF a merely-`optional` dependency
                // is allowed (already past check 1's refusal above) --
                // announce what is lost, once, right here, in addition to
                // the persistent browser annotation check 4 below leaves
                // behind.
                if !installed {
                    let optional_dependents =
                        enabled_optional_dependents(manifests, &enabled_before, &plugin_id);
                    if !optional_dependents.is_empty() {
                        let verb = if optional_dependents.len() == 1 {
                            "optionally uses"
                        } else {
                            "optionally use"
                        };
                        self.state.transcript.push(Entry::Notice {
                            text: format!(
                                "note: {} {verb} {plugin_id} -- turning it off costs only \
                                 presentation/convenience for {}; their core function is \
                                 unaffected",
                                optional_dependents.join(", "),
                                if optional_dependents.len() == 1 {
                                    "it"
                                } else {
                                    "them"
                                },
                            ),
                        });
                    }
                }

                // ---- Check 4: refresh every row's own degradation
                // annotation against the POST-toggle enabled set -- not
                // only the rows this specific toggle touched, since
                // toggling `plugin_id` on/off can change whether OTHER
                // rows are degraded too.
                let enabled_after: HashSet<String> = self
                    .state
                    .plugin_browser
                    .iter()
                    .filter(|entry| entry.installed)
                    .map(|entry| entry.id.clone())
                    .collect();
                for entry in self.state.plugin_browser.iter_mut() {
                    let manifest = manifests.iter().find(|m| m.id == entry.id);
                    refresh_degradation_annotation(entry, manifest, &enabled_after);
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

    use super::super::fixtures::{echo_conway, minimal_cli};
    use super::{
        enabled_dependents_requiring, enabled_optional_dependents, missing_required_dependencies,
        refresh_degradation_annotation, App, PluginManifest,
    };
    use crate::tui::state::{Entry, PluginBrowserEntry};

    /// Every test in this module points `CONWAY_CONFIG_DIR` at a fresh
    /// temp directory of its own via the `env` PARAMETER
    /// `apply_plugin_toggle`/`apply_plugin_toggle_against` take -- never
    /// `std::env::set_var`, and never the real `~/.conway/` (see this
    /// module's own doc for why). A working directory guaranteed to hold
    /// no `.conway/settings.json` anywhere up its ancestry that the test
    /// cares about -- so the project layer contributes nothing and the
    /// user layer is the effective one.
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

    /// A minimal fabricated `PluginManifest` -- this binary's REAL bundle
    /// declares no `requires`/`optional` edge yet (this module's own doc),
    /// so every dependency-enforcement test in this file drives a
    /// fabricated graph instead, the same shape `crates/conway/src/
    /// builder.rs`'s own `plugin_dependency_resolution_tests::manifest`
    /// helper uses.
    fn manifest(id: &str, requires: &[&str], optional: &[&str]) -> PluginManifest {
        PluginManifest {
            id: id.to_string(),
            version: "0.0.0".to_string(),
            tools: vec![],
            required_host_caps: vec![],
            requires: requires.iter().map(|s| s.to_string()).collect(),
            optional: optional.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn browser_entry(id: &str, installed: bool) -> PluginBrowserEntry {
        PluginBrowserEntry {
            id: id.to_string(),
            version: "0.0.0".to_string(),
            installed,
            description: conway::plugin::PluginDescription::default(),
        }
    }

    // ---------------------------------------------------------------
    // Pure graph-function unit coverage -- no `App`/TUI machinery at all,
    // mirroring `crates/conway/src/builder.rs`'s own
    // `plugin_dependency_resolution_tests` module one crate over.
    // ---------------------------------------------------------------

    #[test]
    fn enabled_dependents_requiring_finds_only_enabled_requirers() {
        let manifests = vec![
            manifest("conway.ui", &[], &[]),
            manifest("conway.permissions", &["conway.ui"], &[]),
            manifest("conway.other", &["conway.ui"], &[]),
        ];
        let enabled: std::collections::HashSet<String> = ["conway.ui", "conway.permissions"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let dependents = enabled_dependents_requiring(&manifests, &enabled, "conway.ui");
        assert_eq!(dependents, vec!["conway.permissions".to_string()]);
    }

    #[test]
    fn enabled_dependents_requiring_is_empty_with_no_enabled_requirer() {
        let manifests = vec![
            manifest("conway.ui", &[], &[]),
            manifest("conway.permissions", &["conway.ui"], &[]),
        ];
        let enabled: std::collections::HashSet<String> =
            ["conway.ui"].iter().map(|s| s.to_string()).collect();
        assert!(
            enabled_dependents_requiring(&manifests, &enabled, "conway.ui").is_empty(),
            "conway.permissions is off, so it cannot be broken by conway.ui turning off"
        );
    }

    #[test]
    fn enabled_optional_dependents_finds_optional_users_only() {
        let manifests = vec![
            manifest("conway.ui", &[], &[]),
            manifest("conway.permissions", &[], &["conway.ui"]),
        ];
        let enabled: std::collections::HashSet<String> = ["conway.ui", "conway.permissions"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let dependents = enabled_optional_dependents(&manifests, &enabled, "conway.ui");
        assert_eq!(dependents, vec!["conway.permissions".to_string()]);
        // The required-dependents function must NOT also report an
        // optional user -- the two tiers must never cross-fire.
        assert!(enabled_dependents_requiring(&manifests, &enabled, "conway.ui").is_empty());
    }

    #[test]
    fn missing_required_dependencies_reports_only_unmet_ones() {
        let manifests = vec![
            manifest("conway.ui", &[], &[]),
            manifest("conway.permissions", &["conway.ui", "conway.memory"], &[]),
        ];
        let enabled: std::collections::HashSet<String> =
            ["conway.ui"].iter().map(|s| s.to_string()).collect();
        let missing = missing_required_dependencies(&manifests, &enabled, "conway.permissions");
        assert_eq!(missing, vec!["conway.memory".to_string()]);
    }

    #[test]
    fn missing_required_dependencies_is_empty_for_an_unknown_id() {
        let manifests = vec![manifest("conway.ui", &[], &[])];
        let enabled = std::collections::HashSet::new();
        assert!(
            missing_required_dependencies(&manifests, &enabled, "acme.unknown").is_empty(),
            "nothing this call can reason about must not be treated as a violation"
        );
    }

    #[test]
    fn refresh_degradation_annotation_appends_and_is_idempotent() {
        let manifest = manifest("conway.permissions", &[], &["conway.ui"]);
        let mut entry = browser_entry("conway.permissions", true);
        entry.description.you_lose = "nothing without conway.ui".to_string();
        let enabled_without_ui = std::collections::HashSet::new();

        refresh_degradation_annotation(&mut entry, Some(&manifest), &enabled_without_ui);
        assert!(
            entry.description.you_lose.contains("DEGRADED"),
            "{}",
            entry.description.you_lose
        );
        assert!(entry.description.you_lose.contains("conway.ui"));
        let after_first = entry.description.you_lose.clone();

        // Calling again against the SAME world must not duplicate the note.
        refresh_degradation_annotation(&mut entry, Some(&manifest), &enabled_without_ui);
        assert_eq!(
            entry.description.you_lose, after_first,
            "re-annotating an unchanged world must be idempotent, not append again"
        );

        // Once conway.ui is enabled, the annotation must clear.
        let enabled_with_ui: std::collections::HashSet<String> =
            ["conway.ui"].iter().map(|s| s.to_string()).collect();
        refresh_degradation_annotation(&mut entry, Some(&manifest), &enabled_with_ui);
        assert_eq!(entry.description.you_lose, "nothing without conway.ui");
    }

    #[test]
    fn refresh_degradation_annotation_never_fires_for_an_off_row() {
        let manifest = manifest("conway.permissions", &[], &["conway.ui"]);
        let mut entry = browser_entry("conway.permissions", false);
        let enabled = std::collections::HashSet::new();
        refresh_degradation_annotation(&mut entry, Some(&manifest), &enabled);
        assert!(
            !entry.description.you_lose.contains("DEGRADED"),
            "an OFF plugin contributes nothing at all right now -- 'active' already says so \
             honestly; annotating it as degraded on top would be a second, redundant claim: {}",
            entry.description.you_lose
        );
    }

    // ---------------------------------------------------------------
    // `App::apply_plugin_toggle_against` -- the three enablement-point
    // behaviours, driven end to end (real `App`, real transcript, real
    // settings.json write path, fabricated manifest graph).
    // ---------------------------------------------------------------

    /// Enablement point 2/3 (browser toggle): turning a plugin ON writes
    /// `plugins.install`, flips the display mirror, and leaves a Notice
    /// naming the restart-to-apply contract -- unchanged by this item for
    /// the no-dependency case.
    #[tokio::test]
    async fn turning_a_plugin_on_writes_settings_json_and_flips_the_mirror() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");
        app.state.plugin_browser = vec![browser_entry("conway.memory", false)];

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());
        app.apply_plugin_toggle_against(
            "conway.memory".to_string(),
            true,
            &env,
            no_project_layer.path(),
            &[manifest("conway.memory", &[], &[])],
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
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");
        app.state.plugin_browser = vec![browser_entry("conway.skills", true)];

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());
        let manifests = [manifest("conway.skills", &[], &[])];
        // First turn it on (so there is something real to remove), then
        // off -- proving the round trip through the real writer, not just
        // a single-direction happy path.
        app.apply_plugin_toggle_against(
            "conway.skills".to_string(),
            true,
            &env,
            no_project_layer.path(),
            &manifests,
        );
        app.apply_plugin_toggle_against(
            "conway.skills".to_string(),
            false,
            &env,
            no_project_layer.path(),
            &manifests,
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
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");
        app.state.plugin_browser = vec![browser_entry("conway.path", false)];

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"default_role": "coder", "plugins": {"install": ["conway.memory"]}}"#,
        )
        .expect("write fixture");
        let env = isolated_env(dir.path());
        app.apply_plugin_toggle_against(
            "conway.path".to_string(),
            true,
            &env,
            no_project_layer.path(),
            &[manifest("conway.path", &[], &[])],
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
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");
        app.state.plugin_browser = vec![browser_entry("conway.memory", false)];

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        // Not valid JSON -- `config::writer::set_plugin_installed` refuses
        // to rewrite it blindly (see that module's own "Safety posture").
        std::fs::write(dir.path().join("settings.json"), "{ not json").expect("write fixture");
        let env = isolated_env(dir.path());
        app.apply_plugin_toggle_against(
            "conway.memory".to_string(),
            true,
            &env,
            no_project_layer.path(),
            &[manifest("conway.memory", &[], &[])],
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
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");
        app.state.plugin_browser = vec![browser_entry("conway.memory", false)];

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
        app.apply_plugin_toggle_against(
            "conway.memory".to_string(),
            true,
            &env,
            project.path(),
            &[manifest("conway.memory", &[], &[])],
        );

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

    // ---------------------------------------------------------------
    // Enablement point 2 (browser toggle) -- the four acceptance criteria
    // this item exists to satisfy.
    // ---------------------------------------------------------------

    /// Criterion 1, and the item's own headline: toggling OFF a plugin a
    /// still-enabled plugin `requires` is refused BEFORE the write --
    /// checked here against `settings.json` itself, not merely that an
    /// error was shown (this method's own doc: "a refusal must preserve
    /// that [mirror-follows-disk] property").
    #[tokio::test]
    async fn toggling_off_a_required_dependency_of_an_enabled_plugin_is_refused_before_the_write() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");
        app.state.plugin_browser = vec![
            browser_entry("conway.ui", true),
            browser_entry("conway.permissions", true),
        ];

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());
        let manifests = [
            manifest("conway.ui", &[], &[]),
            manifest("conway.permissions", &["conway.ui"], &[]),
        ];

        app.apply_plugin_toggle_against(
            "conway.ui".to_string(),
            false,
            &env,
            no_project_layer.path(),
            &manifests,
        );

        // No write happened at all -- settings.json must not even exist.
        assert!(
            !dir.path().join("settings.json").exists(),
            "a refused toggle must never touch disk"
        );

        // The mirror-follows-disk property: since disk holds nothing, the
        // mirror must still say conway.ui is ON.
        assert!(
            app.state
                .plugin_browser
                .iter()
                .find(|e| e.id == "conway.ui")
                .expect("entry still present")
                .installed,
            "a refusal must never flip the mirror -- disk was never written"
        );

        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Error { text, fatal: false } if text.contains("conway.ui")
                    && text.contains("conway.permissions")
            )),
            "the refusal must name both the plugin being refused and the dependent that \
             still requires it: {:?}",
            app.state.transcript
        );
    }

    /// Falsifies the fixture used above: with NO enabled dependent,
    /// toggling the same plugin off succeeds normally -- proving the
    /// refusal above is genuinely conditioned on an enabled `requires`
    /// edge, not on the plugin's own id.
    #[tokio::test]
    async fn toggling_off_a_required_dependency_succeeds_once_its_dependent_is_also_off() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");
        app.state.plugin_browser = vec![
            browser_entry("conway.ui", true),
            browser_entry("conway.permissions", false),
        ];

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());
        let manifests = [
            manifest("conway.ui", &[], &[]),
            manifest("conway.permissions", &["conway.ui"], &[]),
        ];

        // `config::writer::set_plugin_installed` is a no-op (never creates
        // a file) when asked to turn something OFF against a settings.json
        // that does not exist yet -- there is nothing to remove. Turn
        // conway.ui ON first (unrefused: conway.permissions is off, so
        // nothing requires it yet) so there is something real for the OFF
        // toggle under test to remove.
        app.apply_plugin_toggle_against(
            "conway.ui".to_string(),
            true,
            &env,
            no_project_layer.path(),
            &manifests,
        );
        app.apply_plugin_toggle_against(
            "conway.ui".to_string(),
            false,
            &env,
            no_project_layer.path(),
            &manifests,
        );

        let text = std::fs::read_to_string(dir.path().join("settings.json"))
            .expect("settings.json must exist -- this toggle must not be refused");
        let value: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(value["plugins"]["install"], serde_json::json!([]));
        assert!(
            !app.state
                .plugin_browser
                .iter()
                .find(|e| e.id == "conway.ui")
                .expect("entry still present")
                .installed
        );
    }

    /// Criterion 2: toggling off a merely-`optional` dependency is
    /// ALLOWED -- the write happens -- with an additional Notice naming
    /// what the operator is giving up. The two tiers must not conflate: an
    /// `optional` edge must never produce the criterion-1 refusal.
    #[tokio::test]
    async fn toggling_off_an_optional_dependency_is_allowed_and_announces_the_loss() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");
        app.state.plugin_browser = vec![
            browser_entry("conway.ui", true),
            browser_entry("conway.permissions", true),
        ];

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());
        let manifests = [
            manifest("conway.ui", &[], &[]),
            manifest("conway.permissions", &[], &["conway.ui"]),
        ];

        // Same reason as the required-dependency test above: an OFF toggle
        // against a settings.json that does not exist yet is a genuine
        // no-op write. Turn conway.ui ON first (unrefused) so the OFF
        // toggle under test has something real to remove.
        app.apply_plugin_toggle_against(
            "conway.ui".to_string(),
            true,
            &env,
            no_project_layer.path(),
            &manifests,
        );
        app.apply_plugin_toggle_against(
            "conway.ui".to_string(),
            false,
            &env,
            no_project_layer.path(),
            &manifests,
        );

        let text = std::fs::read_to_string(dir.path().join("settings.json"))
            .expect("an optional dependent must never block the write");
        let value: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(value["plugins"]["install"], serde_json::json!([]));
        assert!(
            !app.state
                .plugin_browser
                .iter()
                .find(|e| e.id == "conway.ui")
                .expect("entry still present")
                .installed,
            "the mirror must flip -- this toggle succeeded"
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text } if text.contains("conway.permissions")
                    && text.contains("conway.ui")
                    && text.contains("presentation/convenience")
            )),
            "an optional-dependency toggle-off must announce what is lost: {:?}",
            app.state.transcript
        );
        assert!(
            !app.state
                .transcript
                .iter()
                .any(|e| matches!(e, Entry::Error { .. })),
            "an optional dependency must never be refused: {:?}",
            app.state.transcript
        );
    }

    /// Criterion 3: toggling ON a plugin whose bundled `requires` is
    /// unmet does NOT silently enable the dependency -- neither plugin's
    /// membership is written -- and offers, by name, what would satisfy
    /// it.
    #[tokio::test]
    async fn toggling_on_a_plugin_with_an_unmet_bundled_requirement_offers_rather_than_writes() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");
        app.state.plugin_browser = vec![
            browser_entry("conway.ui", false),
            browser_entry("conway.permissions", false),
        ];

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());
        let manifests = [
            manifest("conway.ui", &[], &[]),
            manifest("conway.permissions", &["conway.ui"], &[]),
        ];

        app.apply_plugin_toggle_against(
            "conway.permissions".to_string(),
            true,
            &env,
            no_project_layer.path(),
            &manifests,
        );

        assert!(
            !dir.path().join("settings.json").exists(),
            "an offer must never write to disk -- neither plugin is silently enabled"
        );
        assert!(
            !app.state
                .plugin_browser
                .iter()
                .find(|e| e.id == "conway.permissions")
                .expect("entry still present")
                .installed,
            "the requested plugin itself must not be silently enabled either"
        );
        assert!(
            !app.state
                .plugin_browser
                .iter()
                .find(|e| e.id == "conway.ui")
                .expect("entry still present")
                .installed,
            "the dependency must not be silently enabled"
        );
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Notice { text } if text.contains("conway.permissions")
                    && text.contains("conway.ui")
                    && text.contains("bundled")
            )),
            "the offer must name both plugins and that the dependency is bundled: {:?}",
            app.state.transcript
        );
    }

    /// A dependency this binary does not link at all cannot be offered --
    /// refused outright, distinctly from the bundled-offer Notice above
    /// (this is an `Error`, not a `Notice`).
    #[tokio::test]
    async fn toggling_on_a_plugin_with_an_unlinked_requirement_is_refused_not_offered() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");
        app.state.plugin_browser = vec![browser_entry("conway.permissions", false)];

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());
        // conway.ui is NOT present in the manifest set at all -- unlinked.
        let manifests = [manifest("conway.permissions", &["conway.ui"], &[])];

        app.apply_plugin_toggle_against(
            "conway.permissions".to_string(),
            true,
            &env,
            no_project_layer.path(),
            &manifests,
        );

        assert!(!dir.path().join("settings.json").exists());
        assert!(
            app.state.transcript.iter().any(|e| matches!(
                e,
                Entry::Error { text, fatal: false } if text.contains("conway.permissions")
                    && text.contains("conway.ui")
            )),
            "an unlinked requirement must be refused as a non-fatal Error: {:?}",
            app.state.transcript
        );
    }

    /// Criterion 3's happy path: once the bundled dependency is already
    /// enabled, toggling the dependent ON proceeds normally -- the offer
    /// only fires while the dependency is genuinely missing.
    #[tokio::test]
    async fn toggling_on_a_plugin_whose_requirement_is_already_enabled_writes_normally() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");
        app.state.plugin_browser = vec![
            browser_entry("conway.ui", true),
            browser_entry("conway.permissions", false),
        ];

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());
        let manifests = [
            manifest("conway.ui", &[], &[]),
            manifest("conway.permissions", &["conway.ui"], &[]),
        ];

        app.apply_plugin_toggle_against(
            "conway.permissions".to_string(),
            true,
            &env,
            no_project_layer.path(),
            &manifests,
        );

        let text = std::fs::read_to_string(dir.path().join("settings.json"))
            .expect("the write must happen -- the requirement is already satisfied");
        assert!(text.contains("conway.permissions"), "{text}");
        assert!(
            app.state
                .plugin_browser
                .iter()
                .find(|e| e.id == "conway.permissions")
                .expect("entry still present")
                .installed
        );
    }

    /// Criterion 4: a degraded plugin says so in the browser -- driven end
    /// to end through `apply_plugin_toggle_against` (not
    /// `refresh_degradation_annotation` directly), proving the wiring, not
    /// only the pure function.
    #[tokio::test]
    async fn a_toggle_that_leaves_a_plugin_degraded_annotates_it_in_the_browser() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");
        app.state.plugin_browser = vec![
            browser_entry("conway.ui", true),
            browser_entry("conway.permissions", true),
        ];

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());
        let manifests = [
            manifest("conway.ui", &[], &[]),
            manifest("conway.permissions", &[], &["conway.ui"]),
        ];

        // conway.permissions is not yet degraded -- conway.ui is on.
        assert!(!app
            .state
            .plugin_browser
            .iter()
            .find(|e| e.id == "conway.permissions")
            .unwrap()
            .description
            .you_lose
            .contains("DEGRADED"));

        app.apply_plugin_toggle_against(
            "conway.ui".to_string(),
            false,
            &env,
            no_project_layer.path(),
            &manifests,
        );

        let permissions_entry = app
            .state
            .plugin_browser
            .iter()
            .find(|e| e.id == "conway.permissions")
            .expect("entry still present");
        assert!(
            permissions_entry.description.you_lose.contains("DEGRADED"),
            "the browser row for conway.permissions must now say it is degraded: {}",
            permissions_entry.description.you_lose
        );
        assert!(permissions_entry.description.you_lose.contains("conway.ui"));

        // Turning conway.ui back on must clear the annotation again.
        app.apply_plugin_toggle_against(
            "conway.ui".to_string(),
            true,
            &env,
            no_project_layer.path(),
            &manifests,
        );
        let permissions_entry = app
            .state
            .plugin_browser
            .iter()
            .find(|e| e.id == "conway.permissions")
            .expect("entry still present");
        assert!(
            !permissions_entry.description.you_lose.contains("DEGRADED"),
            "re-enabling the optional dependency must clear the degradation note: {}",
            permissions_entry.description.you_lose
        );
    }

    /// `apply_plugin_toggle` (the public entry point `run.rs` actually
    /// calls) resolves the REAL compiled-in bundle rather than a
    /// fabricated one -- a thin end-to-end proof that the split introduced
    /// by this item did not break the production wiring. Since no real
    /// first-party plugin declares a `requires`/`optional` edge yet (this
    /// module's own doc), this can only prove the ordinary no-dependency
    /// path still works -- the dependency-enforcement behaviour itself is
    /// covered exhaustively above, against
    /// `apply_plugin_toggle_against` directly.
    #[tokio::test]
    async fn apply_plugin_toggle_resolves_the_real_bundle_and_still_writes() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let mut app = App::new(&cli, &conway, &[]).await.expect("App::new");
        app.state.plugin_browser = vec![browser_entry("conway.memory", false)];

        let no_project_layer = no_project_layer();
        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(dir.path());
        app.apply_plugin_toggle(
            "conway.memory".to_string(),
            true,
            &env,
            no_project_layer.path(),
        );

        let text = std::fs::read_to_string(dir.path().join("settings.json"))
            .expect("settings.json must exist");
        assert!(text.contains("conway.memory"), "{text}");
        assert!(
            app.state
                .plugin_browser
                .iter()
                .find(|e| e.id == "conway.memory")
                .expect("entry still present")
                .installed
        );
    }
}
