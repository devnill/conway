//! Plugin-declared permission modes — a NAME plus a NARROWING, layered on
//! one of the three closed core [`PermissionMode`] variants. Board item
//! `01M0X4YDNVP7TZ0PVSRJ0388SS`; design
//! `docs/vision/DESIGN-permission-modes.md` §2c/§3b/§3d/§6b;
//! `docs/plugins/permission-modes.md` for the operator-facing framing.
//!
//! **The core enum stays closed, and this module never adds a fourth
//! `PermissionMode` variant.** `PermissionBroker::decide`
//! (`conway_runtime::permission`) is untouched by anything here — it still reads
//! exactly one [`PermissionMode`] (`PermissionBroker::mode`) to decide
//! plan-mode/`AutoAllow` gating, byte-for-byte as before this module
//! existed. A
//! declared mode is a LABEL over that same base, plus optional bookkeeping
//! for the status line and the mode cycle; it is never a second place
//! `decide()`'s own question ("what does this mode allow") gets answered —
//! one implementation, never a restatement at a second call site.
//!
//! ## Why this lives in `conway-core` rather than beside the broker
//!
//! It was written in `conway-runtime`, next to the `PermissionBroker`
//! that consumes it, and moved here when the facade needed to hand these
//! types to `conway-cli`. `crates/conway/tests/architecture_invariants.rs`
//! (T6) forbids the facade from publicly re-exporting a `conway-runtime`
//! type — that test caught the re-export, not a reviewer — and the honest
//! fix was to move the type rather than convert it at the boundary:
//! nothing here touches the runtime. It is `PermissionMode`,
//! `PluginDeclaredMode`, ordering, and collision handling, all of which
//! are `conway-core` vocabulary already.
//!
//! The one-implementation rule is unaffected by the move: this is still ONE
//! implementation of "which modes exist and in what order do they cycle."
//! It is simply in the layer every consumer can already see, instead of
//! one only the runtime can.
//!
//! ## Why widening is structurally impossible, not merely rejected
//!
//! [`crate::ports::PluginDeclaredMode`] carries exactly one
//! field bearing on enforcement: `base: PermissionMode`, one of the closed
//! three. There is no second field — no override list, no "extra allowed
//! categories," nothing — for a plugin to populate with anything wider
//! than `base` already permits, because [`ModeCycleEntry::base`] below is
//! the ONLY question this module (or the broker) ever asks a declared
//! mode: `decide()` never learns a mode's NAME, only its `base`. This is
//! the same shape [`crate::hook::HookOnFailure`] and
//! [`crate::ports::PluginPermissionVerdict`] use — a type
//! with no representable `Allow` — carried one level up: here, there is no
//! representable "allow more than `base`" at all, because there is no
//! field anywhere in the chain that could hold one.
//!
//! Any REAL narrowing a declared mode's plugin wants to add beyond its
//! base's own semantics is not this module's job to carry: it is
//! expressed through the SAME mechanisms every other plugin already
//! narrows with — [`crate::ports::Plugin::permission_rules`]
//! (`PluginPermissionVerdict`, no `Allow`) today, and a plugin's own
//! `pre_tool_use` hooks (`HookPermissionVerdict`/`HookOnFailure`, neither
//! with an `Allow`) once `Plugin::hooks()` lands (design §6c: a SEPARATE
//! item's job). Reusing those keeps this module from inventing a second
//! enforcement path for the identical question.
//!
//! ## What is, and is not, wired here
//!
//! This module is a pure, I/O-free projection: [`ModeCycle::build`] takes
//! whatever `(plugin_id, PluginDeclaredMode)` pairs a caller already
//! gathered (every installed plugin's `Plugin::manifest().id` paired with
//! its own `Plugin::permission_modes()`) and computes the cycle order, any
//! name collisions, and — via [`ModeCycle::reconcile_active`] —
//! what a currently-active declared mode resolves to after plugins change.
//! **Gathering that list from `ConwayBuilder`'s installed plugin set, and
//! driving Shift+Tab through it end-to-end, is facade/TUI wiring this item
//! leaves as a follow-up** (see this module's own test suite for exactly
//! what is pinned here versus what still needs that wiring).

use crate::permission_mode::PermissionMode;
use crate::ports::PluginDeclaredMode;

/// One entry of the mode cycle Shift+Tab walks: one of the three closed
/// core modes, or a plugin's own name layered on one of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModeCycleEntry {
    /// One of [`PermissionMode`]'s own three variants, unlabeled.
    Core(PermissionMode),
    /// A plugin-declared name. `base` is the ONLY field this variant, or
    /// any consumer of it, ever consults for enforcement — see this
    /// module's own doc for why that makes widening unrepresentable.
    Declared {
        plugin_id: String,
        name: String,
        base: PermissionMode,
    },
}

impl ModeCycleEntry {
    /// The core mode this entry ultimately resolves to for enforcement.
    /// `PermissionBroker::decide` is the ONLY place that question is
    /// answered — this accessor exists so a caller
    /// selecting a `Declared` entry sets the broker's REAL mode with
    /// `PermissionBroker::set_mode(entry.base())`, never a second decision
    /// path.
    pub fn base(&self) -> PermissionMode {
        match self {
            ModeCycleEntry::Core(mode) => *mode,
            ModeCycleEntry::Declared { base, .. } => *base,
        }
    }

    /// A reference identifying this entry for
    /// `PermissionBroker::set_active_declared_mode`/reconciliation —
    /// `None` for a core entry (there is nothing to go dangling:
    /// `Prompt`/`Plan`/`AutoAllow` never depend on an installed plugin).
    pub fn declared_ref(&self) -> Option<DeclaredModeRef> {
        match self {
            ModeCycleEntry::Core(_) => None,
            ModeCycleEntry::Declared {
                plugin_id, name, ..
            } => Some(DeclaredModeRef {
                plugin_id: plugin_id.clone(),
                name: name.clone(),
            }),
        }
    }

    /// The status-line/TUI display label. **Never a replacement for the
    /// base mode's own [`PermissionMode::label`] — always alongside it.**
    /// This is what `docs/vision/DESIGN-permission-modes.md` §3b's
    /// operator ruling makes load-bearing: an `AutoAllow`-based declared
    /// mode's label still contains `PermissionMode::AutoAllow.label()`
    /// (`"AUTO-ALLOW"`) VERBATIM, so nothing here can soften it by
    /// omission the way an earlier draft of the design almost did. See
    /// this module's own test
    /// `declared_autoallow_mode_display_label_carries_the_unmodified_warning_verbatim`
    /// below.
    pub fn display_label(&self) -> String {
        match self {
            ModeCycleEntry::Core(mode) => mode.label().to_string(),
            ModeCycleEntry::Declared { name, base, .. } => {
                format!("{name} ({})", base.label())
            }
        }
    }
}

/// Identifies one declared mode for
/// `conway_runtime::permission::PermissionBroker`'s bookkeeping —
/// `(plugin_id, name)`, matching a [`ModeCycleEntry::Declared`]'s own two
/// identifying fields.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DeclaredModeRef {
    pub plugin_id: String,
    pub name: String,
}

/// Two or more plugins declared the identical mode `name` — handled
/// deterministically (see [`ModeCycle::build`]'s own doc): BOTH colliding
/// entries are excluded from the cycle (never silently pick one), and this
/// value names every plugin that collided so a caller can report it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclaredModeCollision {
    pub name: String,
    /// Every colliding plugin's id, sorted ascending — deterministic
    /// regardless of the order `declared` was supplied to
    /// [`ModeCycle::build`] in.
    pub plugin_ids: Vec<String>,
}

impl DeclaredModeCollision {
    /// The message acceptance 7 requires: "message naming both."
    pub fn describe(&self) -> String {
        format!(
            "permission mode name \"{}\" is declared by more than one plugin ({}); \
             none of them was added to the mode cycle",
            self.name,
            self.plugin_ids.join(", "),
        )
    }
}

/// The three closed core modes' own fixed cycle order — unrelated to any
/// plugin, so it is a `const` every [`ModeCycle`] starts from, never a
/// value a plugin could influence. Matches the order
/// `crates/conway-cli/src/tui/app/run.rs`'s existing `CyclePermissionMode`
/// handler already cycles today (`Prompt -> Plan -> AutoAllow -> ...`).
const CORE_ORDER: [PermissionMode; 3] = [
    PermissionMode::Prompt,
    PermissionMode::Plan,
    PermissionMode::AutoAllow,
];

/// The mode cycle Shift+Tab walks, plus any name collisions found while
/// building it. Pure and I/O-free — see this module's own doc for what
/// gathers the input and what still needs to drive Shift+Tab through it.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ModeCycle {
    entries: Vec<ModeCycleEntry>,
    collisions: Vec<DeclaredModeCollision>,
}

impl ModeCycle {
    /// Builds the cycle from every currently-installed plugin's own
    /// `(plugin_id, PluginDeclaredMode)` pairs — the caller (facade
    /// wiring, not this module) is responsible for gathering that list
    /// from `Plugin::manifest().id` + `Plugin::permission_modes()` across
    /// every installed plugin; this function performs no I/O and has no
    /// knowledge of `ConwayBuilder`.
    ///
    /// **Order is deterministic and independent of installation order:**
    /// the three core modes always come first, in `CORE_ORDER`;
    /// declared modes follow, sorted by name ascending — so a declared
    /// mode's position in the cycle depends only on its own name, never on
    /// which plugin happened to load first, and never on the order
    /// `declared` was supplied in.
    ///
    /// **Collisions are excluded, not resolved by picking one.** Two
    /// plugins declaring the identical `name` is ambiguous — there is no
    /// principled way to prefer one plugin's mode over the other's — so
    /// BOTH are omitted from [`Self::entries`] and reported in
    /// [`Self::collisions`] instead (acceptance 7: "handled
    /// deterministically, message naming both"). A collision never panics
    /// and never silently picks a winner.
    pub fn build(declared: &[(String, PluginDeclaredMode)]) -> Self {
        use std::collections::BTreeMap;

        let mut by_name: BTreeMap<String, Vec<(String, PermissionMode)>> = BTreeMap::new();
        for (plugin_id, mode) in declared {
            let owner: String = plugin_id.clone();
            by_name
                .entry(mode.name.clone())
                .or_default()
                .push((owner, mode.base));
        }

        let mut entries: Vec<ModeCycleEntry> = CORE_ORDER
            .iter()
            .copied()
            .map(ModeCycleEntry::Core)
            .collect();
        let mut collisions = Vec::new();

        // `by_name` is already ordered by `name` ascending (a `BTreeMap`),
        // so no further sort is needed for the declared entries this loop
        // appends.
        for (name, mut owners) in by_name {
            owners.sort_by(|a, b| a.0.cmp(&b.0));
            if owners.len() > 1 {
                let plugin_ids: Vec<String> = owners.into_iter().map(|(id, _)| id).collect();
                collisions.push(DeclaredModeCollision { name, plugin_ids });
                continue;
            }
            let (plugin_id, base) = owners.into_iter().next().expect("len == 1 checked above");
            entries.push(ModeCycleEntry::Declared {
                plugin_id,
                name,
                base,
            });
        }

        Self {
            entries,
            collisions,
        }
    }

    /// Every entry in cycle order — always starts with the three core
    /// modes (`CORE_ORDER`), so this is never empty.
    pub fn entries(&self) -> &[ModeCycleEntry] {
        &self.entries
    }

    /// Every name collision found while building this cycle, for a caller
    /// to surface (e.g. a `tracing::warn!` or a `ConfigWarning`, mirroring
    /// `PluginManifest::optional_host_caps`'s own "never silent" posture).
    pub fn collisions(&self) -> &[DeclaredModeCollision] {
        &self.collisions
    }

    /// The entry immediately after `current` in cycle order, wrapping —
    /// `Action::CyclePermissionMode`'s own resolution once wired to a real
    /// plugin set. Never panics: [`Self::entries`] always holds at least
    /// the three core modes.
    ///
    /// **Doubles as acceptance 6's uninstall-safety net.** If `current`
    /// names a declared mode this cycle no longer contains (its plugin was
    /// uninstalled, or its name just collided and was excluded), `current`
    /// is not found in [`Self::entries`] at all, and this method returns
    /// the FIRST entry (`PermissionMode::Prompt`, `CORE_ORDER`'s own
    /// first element, matching `PermissionMode::default`) rather than
    /// panicking or silently repeating a dangling entry — the same
    /// "land on a sane core mode, not a dangling name" answer
    /// [`Self::reconcile_active`] gives via a different question. This is
    /// deliberately ONE cycle-position algorithm answering both the
    /// ordinary Shift+Tab case and the uninstall-recovery case, not two.
    pub fn next(&self, current: &ModeCycleEntry) -> ModeCycleEntry {
        debug_assert!(
            !self.entries.is_empty(),
            "CORE_ORDER always seeds at least the three core entries"
        );
        match self.entries.iter().position(|e| e == current) {
            Some(idx) => self.entries[(idx + 1) % self.entries.len()].clone(),
            None => self.entries[0].clone(),
        }
    }

    /// **Acceptance 6.** If `active` names a declared mode no longer
    /// present in this cycle (its plugin was uninstalled, or its name
    /// collided and was excluded), returns `None` — the sane fallback: the
    /// broker's own `PermissionMode` field is untouched by this call (it
    /// was never anything but one of the closed three to begin with), so
    /// the session is ALREADY resting on a real core mode; this only stops
    /// the STATUS LINE from continuing to show a name nothing backs.
    /// `active == None` (already a plain core mode) round-trips to `None`
    /// unchanged.
    pub fn reconcile_active(&self, active: Option<DeclaredModeRef>) -> Option<DeclaredModeRef> {
        match active {
            Some(want)
                if self
                    .entries
                    .iter()
                    .any(|e| e.declared_ref().as_ref() == Some(&want)) =>
            {
                Some(want)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(plugin_id: &str, name: &str, base: PermissionMode) -> (String, PluginDeclaredMode) {
        (
            plugin_id.to_string(),
            PluginDeclaredMode {
                name: name.to_string(),
                base,
            },
        )
    }

    // ---- Acceptance 1: zero plugins is byte-identical to today ----

    #[test]
    fn no_declared_modes_cycles_only_the_three_core_modes_in_order() {
        let cycle = ModeCycle::build(&[]);
        assert_eq!(
            cycle.entries(),
            &[
                ModeCycleEntry::Core(PermissionMode::Prompt),
                ModeCycleEntry::Core(PermissionMode::Plan),
                ModeCycleEntry::Core(PermissionMode::AutoAllow),
            ]
        );
        assert!(cycle.collisions().is_empty());
    }

    // ---- Acceptance 2: widening is unrepresentable ----

    /// `ModeCycleEntry::base` is the ONLY question `decide()`-adjacent code
    /// ever asks a declared entry — pinning that a `Plan`-based declared
    /// mode's `base()` is exactly `Plan`, never anything wider, proves
    /// there is no second field this type could have used to answer
    /// differently. See this module's own doc for the full structural
    /// argument.
    #[test]
    fn a_declared_modes_base_is_exactly_what_the_plugin_named_never_wider() {
        let cycle =
            ModeCycle::build(&[declared("acme.strict", "strict-plan", PermissionMode::Plan)]);
        let declared_entry = cycle
            .entries()
            .iter()
            .find(|e| matches!(e, ModeCycleEntry::Declared { .. }))
            .expect("one declared entry");
        assert_eq!(declared_entry.base(), PermissionMode::Plan);
    }

    // ---- Acceptance 4: deterministic ordering ----

    #[test]
    fn declared_modes_are_ordered_by_name_regardless_of_input_order() {
        let cycle = ModeCycle::build(&[
            declared("acme.z", "zzz-mode", PermissionMode::AutoAllow),
            declared("acme.a", "aaa-mode", PermissionMode::Plan),
        ]);
        let names: Vec<&str> = cycle
            .entries()
            .iter()
            .filter_map(|e| match e {
                ModeCycleEntry::Declared { name, .. } => Some(name.as_str()),
                ModeCycleEntry::Core(_) => None,
            })
            .collect();
        assert_eq!(
            names,
            vec!["aaa-mode", "zzz-mode"],
            "declared entries must sort by name, independent of the order they were supplied in"
        );
    }

    #[test]
    fn core_modes_always_come_first_ahead_of_any_declared_mode() {
        let cycle = ModeCycle::build(&[declared(
            "acme.permissions",
            "aaa-comes-before-every-core-name-alphabetically",
            PermissionMode::AutoAllow,
        )]);
        assert_eq!(
            &cycle.entries()[..3],
            &[
                ModeCycleEntry::Core(PermissionMode::Prompt),
                ModeCycleEntry::Core(PermissionMode::Plan),
                ModeCycleEntry::Core(PermissionMode::AutoAllow),
            ],
            "the three core modes must lead the cycle regardless of a declared name's own \
             alphabetical position"
        );
    }

    #[test]
    fn next_wraps_around_the_whole_cycle_core_then_declared_then_back_to_core() {
        let cycle = ModeCycle::build(&[declared(
            "acme.permissions",
            "auto-gated",
            PermissionMode::AutoAllow,
        )]);
        let entries = cycle.entries().to_vec();
        assert_eq!(entries.len(), 4, "3 core + 1 declared");

        let mut current = entries[0].clone();
        for expected in &entries[1..] {
            current = cycle.next(&current);
            assert_eq!(&current, expected);
        }
        // One more step wraps back to the first entry.
        current = cycle.next(&current);
        assert_eq!(current, entries[0]);
    }

    // ---- Acceptance 5, second half: the AUTO-ALLOW warning is never
    // softened by a declared mode ----

    /// **The most important test in this item** (per the board spec's own
    /// framing): an `AutoAllow`-based declared mode's display label still
    /// carries `PermissionMode::AutoAllow.label()` — `"AUTO-ALLOW"` — byte
    /// for byte. Gated auto is still auto; nothing in this module composes
    /// a softer replacement string, only appends the plugin's own name
    /// alongside the unmodified warning.
    #[test]
    fn declared_autoallow_mode_display_label_carries_the_unmodified_warning_verbatim() {
        let entry = ModeCycleEntry::Declared {
            plugin_id: "conway.permissions".to_string(),
            name: "auto-gated".to_string(),
            base: PermissionMode::AutoAllow,
        };
        let label = entry.display_label();
        assert!(
            label.contains(PermissionMode::AutoAllow.label()),
            "a declared mode's display label must contain the base mode's own unmodified \
             label -- {label:?} does not contain {:?}",
            PermissionMode::AutoAllow.label()
        );
        assert_eq!(
            label, "auto-gated (AUTO-ALLOW)",
            "the emphatic warning must survive verbatim, not be paraphrased or omitted"
        );
    }

    /// The inverse check for the two SAFE core modes: a declared mode based
    /// on `Prompt` or `Plan` must not gain an unwarranted `AUTO-ALLOW`-style
    /// shout — `display_label` only ever echoes the REAL base's own label,
    /// never a fixed severity independent of which base was named.
    #[test]
    fn declared_prompt_mode_display_label_does_not_invent_an_auto_allow_warning() {
        let entry = ModeCycleEntry::Declared {
            plugin_id: "acme.audit".to_string(),
            name: "confirm-everything".to_string(),
            base: PermissionMode::Prompt,
        };
        assert_eq!(entry.display_label(), "confirm-everything (prompt)");
    }

    // ---- Acceptance 6: uninstalling the declaring plugin ----

    /// **The failure most likely to be missed**, per the board item's own
    /// guard rail. A session sitting in a declared mode whose plugin is
    /// uninstalled must reconcile back to `None` (a plain core mode), not
    /// keep pointing at a name nothing backs.
    #[test]
    fn uninstalling_the_declaring_plugin_reconciles_the_active_declared_mode_to_none() {
        let with_plugin_installed = ModeCycle::build(&[declared(
            "conway.permissions",
            "auto-gated",
            PermissionMode::AutoAllow,
        )]);
        let active = with_plugin_installed
            .entries()
            .iter()
            .find_map(|e| e.declared_ref())
            .expect("one declared mode is active");
        assert_eq!(
            with_plugin_installed.reconcile_active(Some(active.clone())),
            Some(active.clone()),
            "while the plugin is still installed, the active declared mode round-trips \
             unchanged"
        );

        // The plugin is uninstalled: rebuild the cycle with an empty
        // declared list, exactly as a caller would after
        // `ConwayBuilder`/plugin-registry state drops the plugin.
        let after_uninstall = ModeCycle::build(&[]);
        assert_eq!(
            after_uninstall.reconcile_active(Some(active)),
            None,
            "a declared mode whose plugin was uninstalled must reconcile to `None` -- a \
             dangling name must never survive an uninstall"
        );
    }

    /// `next()` gives the identical uninstall-safety answer via the
    /// Shift+Tab path: pressing the key while sitting on a now-dangling
    /// declared entry lands on the first core entry, never a panic and
    /// never a repeat of the dangling name.
    #[test]
    fn cycling_from_a_now_dangling_declared_entry_lands_on_the_first_core_mode() {
        let dangling = ModeCycleEntry::Declared {
            plugin_id: "conway.permissions".to_string(),
            name: "auto-gated".to_string(),
            base: PermissionMode::AutoAllow,
        };
        let after_uninstall = ModeCycle::build(&[]);
        assert_eq!(
            after_uninstall.next(&dangling),
            ModeCycleEntry::Core(PermissionMode::Prompt),
            "cycling from an entry the current cycle no longer contains must land on the \
             first (safest) core mode, not panic and not repeat the dangling entry"
        );
    }

    #[test]
    fn reconcile_active_of_none_stays_none() {
        let cycle = ModeCycle::build(&[declared(
            "conway.permissions",
            "auto-gated",
            PermissionMode::AutoAllow,
        )]);
        assert_eq!(cycle.reconcile_active(None), None);
    }

    // ---- Acceptance 7: name collision, handled deterministically ----

    #[test]
    fn two_plugins_declaring_the_same_name_collide_and_neither_enters_the_cycle() {
        let cycle = ModeCycle::build(&[
            declared("acme.one", "auto-gated", PermissionMode::AutoAllow),
            declared("acme.two", "auto-gated", PermissionMode::AutoAllow),
        ]);

        assert!(
            cycle
                .entries()
                .iter()
                .all(|e| !matches!(e, ModeCycleEntry::Declared { .. })),
            "a colliding name must not enter the cycle from either plugin: {:?}",
            cycle.entries()
        );

        assert_eq!(cycle.collisions().len(), 1);
        let collision = &cycle.collisions()[0];
        assert_eq!(collision.name, "auto-gated");
        assert_eq!(
            collision.plugin_ids,
            vec!["acme.one".to_string(), "acme.two".to_string()],
            "both colliding plugins must be named, in deterministic (sorted) order"
        );
        let message = collision.describe();
        assert!(message.contains("acme.one") && message.contains("acme.two"));
    }

    #[test]
    fn collision_detection_is_independent_of_input_order() {
        let forward = ModeCycle::build(&[
            declared("acme.one", "auto-gated", PermissionMode::AutoAllow),
            declared("acme.two", "auto-gated", PermissionMode::AutoAllow),
        ]);
        let reversed = ModeCycle::build(&[
            declared("acme.two", "auto-gated", PermissionMode::AutoAllow),
            declared("acme.one", "auto-gated", PermissionMode::AutoAllow),
        ]);
        assert_eq!(forward.collisions(), reversed.collisions());
        assert_eq!(forward.entries(), reversed.entries());
    }

    /// A non-colliding name from a THIRD plugin is unaffected by an
    /// unrelated collision between the other two.
    #[test]
    fn a_non_colliding_declared_mode_survives_alongside_an_unrelated_collision() {
        let cycle = ModeCycle::build(&[
            declared("acme.one", "auto-gated", PermissionMode::AutoAllow),
            declared("acme.two", "auto-gated", PermissionMode::AutoAllow),
            declared("acme.three", "careful-plan", PermissionMode::Plan),
        ]);
        assert_eq!(cycle.collisions().len(), 1);
        assert!(cycle.entries().iter().any(|e| matches!(
            e,
            ModeCycleEntry::Declared { name, .. } if name == "careful-plan"
        )));
    }
}
