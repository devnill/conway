//! The `/settings` menu (V4; its plugins section, added by board item
//! `01M0KARX71A64NTSYTDBVANVPF` and restructured by
//! `01M0RW3CPE8SG3PZ2J8RTK9Y9N`, was REPLACED by a single shortcut row by
//! board item `01M0VR5RCCB8NDGG2JEQW8X7XR` -- see "Plugins: one home, not
//! two" below): a mostly session-only display-preferences tree drawn on
//! V1's shared [`super::modal`] + [`super::menu`] primitives -- the first
//! real caller of [`super::menu`], which existed only as an
//! exercised-by-its-own-tests primitive before V4 (see that module's own
//! doc).
//!
//! ## Session-only, not `settings.json`
//!
//! Conway's config load (`conway::config::merge::load`) is a five-source
//! layered read; every setting this menu shows directly (display/tool-
//! output/permissions) stays session-only, changing `AppState` at runtime
//! only, the way the two slash commands display toggling replaced
//! (`/thinking`, `/timestamps` -- both REMOVED, not aliased, see
//! `commands.rs`'s parser) already did. [`SESSION_NOTE`] says so, on the
//! footer, on every render; the one leaf with a real backing config key
//! (`tool_preview_lines`) names it inline (see [`build_tree`]'s own doc) --
//! the other two display toggles have no config-key equivalent to point to
//! at all today, so they carry no such annotation.
//!
//! **The plugins shortcut row is the one exception, by proxy.** This menu
//! itself writes nothing -- `/plugin` (`view/plugins.rs`) owns the real
//! persistence story (`conway::config::writer::set_plugin_installed`, the
//! user layer, `CONWAY_CONFIG_DIR`-overridable, decision
//! `01M0K8BAXJ6THVJAPK0JZ17VV6`) -- so nothing here needs its own
//! disclosure footer line for it beyond pointing at the surface that does.
//!
//! ## Content: a survey, not "every bool on `AppState`"
//!
//! Three settings, deliberately -- everything else on `AppState` is either
//! internal bookkeeping (scroll offsets, in-flight flags, palette state,
//! ...) or already has its own dedicated, better-fitting UI (`v` cycles
//! `/agents`' visibility filter in place; `Ctrl-E` expands/collapses tool
//! output in place). A setting earns a row here only if a user would
//! deliberately reach for it as a persistent-for-the-session DISPLAY
//! preference: `show_reasoning` (was `/thinking`), `show_timestamps` (was
//! `/timestamps`), and `tool_preview_lines` (previously config-only,
//! `[tui.tool_preview_lines]` -- this menu is the first place it becomes
//! reachable at runtime at all).
//!
//! ## Grouping
//!
//! **Restated in full here, not appended to, because it had already drifted
//! once before this item touched it: this doc used to say "four top-level
//! groups" naming only display/tool-output/permissions/plugins, while
//! "providers" (board item `01M11XWB4T8ZADNDB4M8R482MA`) had already
//! shipped a fifth without this paragraph being updated for it. Corrected
//! here alongside this item's own sixth, "defaults", per GP-14 -- a
//! declaration site is one artifact, not an append log.**
//!
//! Six top-level [`MenuNode::Group`]s -- "defaults" (the default role, a
//! cyclable leaf; the derived default model, a static row -- see
//! "Defaults: role settable, model derived" below), "display" (the two
//! booleans), "tool output" (the one numeric setting), "permissions" (the
//! mode, plus allow/deny/prompt/hooks rule review as FOUR SUB-groups -- see
//! [`build_tree`]'s own doc for why they are separate sections, why
//! deny/prompt rows are read-only [`MenuNode::Static`] rows, and why hooks
//! get a fourth section rather than folding into allow), "providers" (see
//! "Providers: add/remove owned here" below), and "plugins" (see
//! "Plugins: one home, not two" below) -- rather than one flat group or
//! several separate ones: this is genuinely the shape a further settings
//! category (say, session history) would extend later, not artificial
//! nesting invented only to exercise the primitive.
//!
//! ## Defaults: role settable, model derived
//!
//! Board item `01M18Q7P25DTSKQJDJJCC3E800`, closing
//! `docs/vision/DESIGN-surface-coherence.md`'s corrected rule 1: `/model`
//! and `/role` stay top-level commands (session-scoped state), and the
//! *default* model and *default* role live here instead, each labelled as
//! a default. `default role` is an ordinary settable leaf -- `Enter`
//! advances `AppState::default_role_snapshot` through every configured
//! `[roles]` entry, wrapping, and persists via `conway::config::
//! set_default_role` (`app/defaults.rs`).
//!
//! `default model` is deliberately NOT a leaf -- it renders as
//! [`MenuNode::Static`], with no `Enter` behavior at all. This is the
//! decision `conway::config::schema::ConwayConfig::default_model`'s own
//! doc records in full (rejected alternative and its cost included): the
//! default model is a DERIVED READ over `roles.<default_role>.chain`, not
//! a second, independently-settable value, because model selection already
//! has exactly one source of truth in this schema and a parallel
//! `default_model` field would be a second one (P-14). Changing it means
//! changing the default role (the leaf above) or that role's own `chain`
//! in `settings.json` by hand -- there is no third way, and this row does
//! not pretend there is one.
//!
//! ## The persistent-default gap: promoting the session's model
//!
//! Board item `01M1AWGSTD7084VFVGN1GK9AS8`. An operator who runs `/model
//! <pair>` (session-scoped, forks -- INTENT.md §5c) has no in-app way to
//! make that choice stick past a restart: before this item, the derived
//! "default model" row above was READ-ONLY end to end, so the only path
//! from "I like what this session is running" to "conway starts on it next
//! time" was hand-editing `roles.<default_role>.chain` in `settings.json`.
//! The operator report this item closes hit exactly that gap -- they chose
//! a fast model with `/model`, worked on it, restarted, and landed back on
//! the chain head (a slow default) with no warning that the switch had
//! been session-only, corrupting a timing comparison they did not know
//! they were making incorrectly.
//!
//! **Decided: [`LEAF_PROMOTE_SESSION_MODEL`], a SIBLING leaf, not the
//! static row turned settable.** Three options were on the table (this
//! item's own spec names all three): (a) make the static row itself
//! actionable; (b) `/model --default <pair>`, an argument-modified
//! `/model`; (c) documentation only. (b) is rejected against
//! `docs/vision/DESIGN-surface-coherence.md`'s rule 1: the PERSISTENT
//! default's home is `/settings` -- `/model` itself stays the
//! session-scoped half of the split and must not grow a second,
//! persistent-writing mode, which is exactly the "same capability, two
//! claimed homes" shape rule 2 forbids. (c) is rejected as leaving the
//! daily-driver friction the report exists to fix. (a) -- turning the
//! static row itself into a leaf -- is rejected too, for a narrower
//! reason: that row's `Enter` would have to mean something conditional on
//! state that is not implied by the row's own label (cycle through the
//! chain? promote the session's model? both, depending on what's
//! running?), and `default_model_row_is_static_never_a_settable_leaf`
//! exists precisely to catch that row becoming a leaf at all -- reversing
//! it would need the test rewritten to describe a DIFFERENT invariant,
//! for no benefit over a second, clearly-labelled row. A sibling leaf
//! keeps the derived row's own contract ("this is a read, not a control")
//! intact, keeps that test passing UNCHANGED, and names its own action in
//! its own label instead of overloading the derived row's.
//!
//! **Rendering.** The new leaf appears in the "defaults" group, directly
//! after the "default model" row, ONLY when `state.focused_model` is
//! `Some` and differs from `state.default_model_snapshot` -- see
//! [`build_tree`]. This is deliberately the SAME legibility test the
//! report's own failure turned on: when the two already agree there is
//! nothing to promote and no session/persistent distinction to surface;
//! when they differ, the row states BOTH values by name (`"this session is
//! running {session model} -- Enter to make it the persistent default"`),
//! so an operator can never mistake a session choice for a persistent one
//! or vice versa (acceptance 4).
//!
//! **The write.** `App::apply_promote_session_model_to_default`
//! (`app/defaults.rs`) reads the default role's CURRENT chain (`app/
//! provider_manage.rs::load_roles_lax`, reused rather than re-read a
//! second way -- P-14), removes any existing occurrence of the session's
//! model, and inserts it at index 0 -- a REORDER, preserving every other
//! configured fallback and their relative order, never a truncation to a
//! single entry (a chain's whole point is fallback candidates; promoting
//! one to head must not silently discard the rest). Persisted via
//! `conway::config::set_role_chain` -- the exact writer `/settings`'
//! provider-add flow already calls for the identical "id/model" shape, no
//! second opinion about chain-entry format (P-14, this item's own spec
//! names this reuse explicitly).
//!
//! ## Plugins: one home, not two
//!
//! Board item `01M0VR5RCCB8NDGG2JEQW8X7XR` gave conway a `/plugin` command
//! (`view/plugins.rs`) that lists EVERY kind of plugin conway can run
//! (compiled-in, subprocess, MCP) -- a strict superset of what this
//! section could ever show on its own (`AppState::plugin_browser` alone;
//! an operator with a configured MCP server had no listing anywhere). Two
//! surfaces reading the identical `plugins.install` array, with the
//! identical restart-to-apply semantics and the identical silent-
//! higher-layer-override hazard (`app/plugin_toggle.rs:66-95`), is real
//! drift risk for no benefit -- so this section is no longer a second,
//! independent plugin browser. It is now a single [`MenuNode::Leaf`] shortcut
//! row (`LEAF_OPEN_PLUGINS`) naming the live counts across ALL THREE kinds
//! and opening `/plugin` (`AppState::open_plugins`) on `Enter`. Everything
//! this section used to own directly -- the per-plugin toggle leaf, the
//! `[x]`/`[ ]` checkbox idiom, the "you get"/"you lose"/"costs" detail
//! panel -- MOVED to `view/plugins.rs` unchanged (see that module's own
//! doc for the full "one home, not two" argument and exactly what carried
//! over verbatim vs. what widened to cover subprocess/MCP). This also
//! retires the one collapse-state hazard the section used to warn about
//! here: with no more per-plugin [`MenuNode::Group`] EVER having existed in
//! this file (the toggle leaves were always flat siblings, not
//! subgroups), there is no per-plugin entry in `AppState::
//! settings_collapsed_groups` to key correctly in the first place.
//!
//! ## Providers: add/remove owned here (board item `01M11XWB4T8ZADNDB4M8R482MA`)
//!
//! Unlike plugins, providers get NO separate `/provider` command to
//! shortcut into -- this section IS the one implementation of provider
//! management, and it is the surface named in "whichever surface does not
//! own provider management delegates to the one that does" (this item's own
//! acceptance 8). There is no drift risk analogous to plugins' pre-existing
//! `/plugin` browser to duplicate: no other surface in this crate lists,
//! adds, or removes a `backends.<id>` entry today. **The precedent this
//! choice sets, flagged for the surface-coherence session this item's own
//! spec names as not-yet-held:** plugins concluded "one home, not two" by
//! MOVING ownership out of `/settings` into a dedicated `/plugin` view;
//! this item concludes the opposite -- `/settings` IS the dedicated view,
//! with no `/provider` sibling at all. Both are defensible under P-14 (one
//! implementation, wherever it lives); which one is the house style for a
//! THIRD future settings category is exactly the question that session
//! should rule on, not something this item decides for it.
//!
//! Every provider currently in [`AppState::provider_entries`] (a config
//! snapshot refreshed on open and after every add/remove -- see that
//! field's own doc for why it is NOT `Conway::config()`, the stale
//! build-time snapshot every other section reads) renders as its own
//! selectable, revocable leaf -- `{id} ({kind}) -- {status} (Enter to
//! remove)`, the SAME "selectable because a real per-row action exists"
//! shape the allow-grant/hook rows already established, mirrored by
//! [`provider_status_label`] for the one part that differs: `{status}`
//! reads [`AppState::provider_status`] (a LIVE classification, probed under
//! `ProbePolicy::All` -- see that field's own doc for why this screen
//! passes a different policy than startup) rather than anything
//! `build_tree`'s other rows read, and renders one of three, never
//! collapsed to two: `working`, `not working: <the Unusable Display,
//! verbatim -- never reworded>`, or `undetermined: <the Undetermined
//! Display, verbatim>` -- visibly distinct wording is this item's own
//! acceptance 3, and reusing `Display` verbatim rather than inventing new
//! phrasing is P-14 applied to `conway::backend_usability` specifically
//! (that module's own doc calls out "a classification vocabulary restated
//! at a second call site" as the exact hazard). A row absent from
//! `provider_status` entirely (not yet classified) reads `checking...` --
//! a FOURTH, honest state distinct from all three of `Usability`'s own
//! variants, for the window between opening this section and the
//! background probe's reply arriving.
//!
//! One leaf per `crate::first_run::HOSTED_CHOICES` entry follows --
//! `add {label} (Enter)` -- reusing that table verbatim rather than
//! restating which provider shapes exist (this item's own acceptance 8/
//! P-14 again: `first_run.rs`'s own module doc names exactly this reuse as
//! the intended one). Local-server auto-detection (the third option
//! `first_run.rs`'s interactive flow offers before falling back to
//! `HOSTED_CHOICES`) is NOT reproduced here -- a disclosed scope
//! narrowing, not an oversight: detecting a local Ollama server is itself
//! an async network probe, and offering it here would need the same
//! spawn-and-poll machinery `App::refresh_provider_entries_and_kick_off_status`
//! already uses for the LISTING half, a second time, for a convenience an
//! operator can still reach by hand-editing `settings.json` exactly as
//! `first_run.rs::non_interactive_guidance` already documents.
//!
//! ## Interaction
//!
//! `Up`/`Down` navigate, `Enter` toggles a boolean leaf or expands/collapses
//! a group (mirrors the `/agents` panel's own Enter-to-activate shape --
//! `menu::MenuState::selected_leaf_id`'s own doc). The one NON-boolean leaf
//! (`tool_preview_lines`) is a `Left`/`Right` STEPPER instead of a third
//! `Enter` behavior -- see [`crate::tui::state::AppState::
//! adjust_tool_preview_lines`]'s own doc for why a continuous +/-1 step
//! fits a small integer range better than cycling a fixed preset list
//! (there is no natural "meaningfully different" preset set here the way,
//! say, a theme picker would have -- every value in `1..=200` is an equally
//! plausible answer to "how many lines before folding", so a stepper lets
//! the user land on exactly the number they want instead of hunting through
//! presets that may straddle it). `Esc` closes.
//!
//! ## Fresh tree every call, not a long-lived stored `MenuState`
//!
//! [`build_tree`] is called fresh by both [`draw`] (below) and
//! `input::handle_settings_key` on every render/keypress, baking the
//! CURRENT `AppState` values into each leaf's label text (`"show reasoning
//! traces -- on"`). A long-lived, mutated-in-place `MenuState` would go
//! stale the instant a toggle changed the very value its own label
//! displays, and `menu::MenuState` deliberately exposes no "relabel this
//! one leaf" mutator (it doesn't know what a leaf's opaque `id` means).
//! Only [`crate::tui::state::AppState::settings_selected`] (the cursor) and
//! [`crate::tui::state::AppState::settings_collapsed_groups`] (which groups
//! are collapsed, keyed by label) persist across calls -- `MenuState::
//! set_selected` (V4 addition to `menu.rs`) restores the cursor onto the
//! freshly built tree.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use conway::backend_usability::Usability;

use super::menu::{self, MenuNode, MenuState};
use super::modal;
use super::theme::Theme;
use crate::tui::state::AppState;

/// [`MenuNode::Leaf`] ids -- opaque to `menu.rs`, interpreted only here and
/// by `input::handle_settings_key` (the only two places that need to agree
/// on their meaning). `pub(crate)` so `input.rs` can match on them without
/// this module needing to expose its whole tree-building internals.
/// Board item `01M18Q7P25DTSKQJDJJCC3E800`: `Enter` cycles the default role
/// through every configured `[roles]` entry, wrapping -- see
/// `input::Action::CycleDefaultRole`'s own doc. There is no equivalent leaf
/// id for "default model": that row is `MenuNode::Static`, a DERIVED
/// display, not something `Enter` sets -- see this module's own doc,
/// "Defaults: role settable, model derived".
pub(crate) const LEAF_DEFAULT_ROLE: &str = "default_role";
/// Board item `01M1AWGSTD7084VFVGN1GK9AS8`: `Enter` writes the running
/// session's model (`AppState::focused_model`) to the head of the default
/// role's own `chain` via `conway::config::set_role_chain` -- a REORDER,
/// not a second stored value, and a SIBLING of the "default model" row
/// above, not that row turned settable (see this module's own doc,
/// "Defaults: role settable, model derived", and "The persistent-default
/// gap: promoting the session's model" below). `build_tree` renders this
/// leaf only when `state.focused_model` is `Some` and differs from
/// `state.default_model_snapshot` -- when the two already match there is
/// nothing to promote, and the row would be a no-op action pretending to
/// be a decision.
pub(crate) const LEAF_PROMOTE_SESSION_MODEL: &str = "promote_session_model";
pub(crate) const LEAF_SHOW_REASONING: &str = "show_reasoning";
pub(crate) const LEAF_SHOW_TIMESTAMPS: &str = "show_timestamps";
pub(crate) const LEAF_TOOL_PREVIEW_LINES: &str = "tool_preview_lines";
/// V2b: cycles `prompt` -> `plan` -> `AUTO-ALLOW` -> `prompt`.
pub(crate) const LEAF_PERMISSION_MODE: &str = "permission_mode";
/// V2b: drops every pattern grant and cached allow-always.
pub(crate) const LEAF_REVOKE_GRANTS: &str = "revoke_grants";
/// Prefix for one grant row's own
/// leaf id, `"{LEAF_REVOKE_GRANT_PREFIX}{index}"` where `index` is the
/// row's position in `state.permission_grants` at the moment this tree was
/// built. `input::activate_settings_selection` resolves that index back
/// into `state.permission_grants` in the SAME call that built this tree, so
/// there is no window in which the index could point at a different grant
/// than the one rendered — see that function's own doc.
pub(crate) const LEAF_REVOKE_GRANT_PREFIX: &str = "revoke_grant:";
/// A2: prefix for one STRUCTURED allow rule's own leaf id,
/// `"{LEAF_REVOKE_STRUCTURED_ALLOW_PREFIX}{index}"` where `index` is the
/// row's position in `state.structured_allow_rules` at the moment this tree
/// was built -- a DISTINCT id space from [`LEAF_REVOKE_GRANT_PREFIX`] so a
/// flat revocation and a structured revocation can never resolve against
/// each other's mirror (the two lists are indexed independently, and
/// `input::activate_settings_selection` resolves each prefix against its
/// OWN mirror in the same call that built this tree, exactly as the flat
/// path does -- see that function's own doc).
pub(crate) const LEAF_REVOKE_STRUCTURED_ALLOW_PREFIX: &str = "revoke_structured_allow:";
/// Prefix for one hook-backed rule's
/// own leaf id, `"{LEAF_REVOKE_HOOK_PREFIX}{index}"` where `index` is the
/// row's position in `state.hook_rules` at the moment this tree was built --
/// a DISTINCT id space from the two prefixes above, resolved against its own
/// mirror the same way `input::activate_settings_selection` already resolves
/// the other two.
pub(crate) const LEAF_REVOKE_HOOK_PREFIX: &str = "revoke_hook:";
/// Board item `01M0VR5RCCB8NDGG2JEQW8X7XR`: the plugins section's own
/// single shortcut leaf -- `Enter` opens `/plugin` (`AppState::
/// open_plugins`, `view/plugins.rs`) rather than toggling anything
/// in-place. See this module's own doc, "Plugins: one home, not two".
pub(crate) const LEAF_OPEN_PLUGINS: &str = "open_plugins";
/// Board item `01M11XWB4T8ZADNDB4M8R482MA`: prefix for one configured
/// provider's own leaf id, `"{LEAF_REMOVE_PROVIDER_PREFIX}{id}"` where `id`
/// is the LITERAL `backends.<id>` map key (never an index -- `AppState::
/// provider_entries` is a `BTreeMap`, so there is nothing to drift the way a
/// `Vec`-indexed row could; matches `set_backend_provider`'s own "matching
/// an existing provider is by id alone" contract). See [`Action::
/// RemoveProvider`]'s own doc.
pub(crate) const LEAF_REMOVE_PROVIDER_PREFIX: &str = "remove_provider:";
/// Prefix for one add-a-provider leaf, one per
/// `crate::first_run::HOSTED_CHOICES` entry -- `"{LEAF_ADD_PROVIDER_PREFIX}{choice.id}"`.
/// The same shapes the first-run flow offers, reused verbatim (never a
/// second, independent list of "which provider kinds conway supports" --
/// P-14; see this module's own doc, "Providers: add/remove owned here").
pub(crate) const LEAF_ADD_PROVIDER_PREFIX: &str = "add_provider:";

/// The top-level group labels (see this module's own doc, "Grouping").
/// `pub(crate)` for the same reason the leaf ids are -- `input.rs` and this
/// module must agree on the SAME strings, since [`crate::tui::state::
/// AppState::settings_collapsed_groups`] is keyed by them.
///
/// Board item `01M18Q7P25DTSKQJDJJCC3E800`: the "defaults" section's own
/// top-level group label -- default role (a leaf, cyclable) and the
/// derived default model (a static row) -- see this module's own doc,
/// "Defaults: role settable, model derived".
const DEFAULTS_GROUP: &str = "defaults";
const DISPLAY_GROUP: &str = "display";
const TOOL_OUTPUT_GROUP: &str = "tool output";
const PERMISSIONS_GROUP: &str = "permissions";
/// The permissions group's three sub-section labels. Allow, deny, and
/// prompt compose by DIFFERENT rules (deny beats everything except root
/// containment; prompt narrows below deny; allow grants), so one
/// undifferentiated list would misrepresent the model -- each gets its own
/// collapsible section.
const ALLOW_GROUP: &str = "allow";
const DENY_GROUP: &str = "deny";
const PROMPT_GROUP: &str = "prompt";
/// The fourth review-list sub-section:
/// every hook-backed rule that can currently deny a call (`pre_tool_use`,
/// `prompt_submitted`) -- its own section, not merged into `ALLOW_GROUP`/
/// `DENY_GROUP`/`PROMPT_GROUP`, because a hook rule composes by a DIFFERENT
/// mechanism than a pattern rule (it runs an operator-configured command and
/// answers `Deny`/`no_opinion`, not a `When`/`Then` match) and revoking one
/// only ever drops it for the rest of THIS session (`Conway::
/// revoke_hook_rule`'s own doc) -- unlike a flat/structured allow grant,
/// which can also persist to a file. See [`build_tree`]'s own doc for why
/// it appears here rather than being folded into an existing section.
const HOOKS_GROUP: &str = "hooks";
/// The plugins section's own top-level group label. Deliberately just
/// `"plugins"` -- never carries the live counts the way the section's own
/// header row does, because `group_node`/`AppState::
/// settings_collapsed_groups` key a group's collapsed state by its exact
/// label text; a label that changed text every time the counts changed
/// would silently forget whether the operator had this section collapsed.
/// The counts render instead as the section's own first, non-selectable
/// row -- see [`build_tree`]'s own doc.
const PLUGINS_GROUP: &str = "plugins";
/// Board item `01M11XWB4T8ZADNDB4M8R482MA`: the providers section's own
/// top-level group label. Unlike `PLUGINS_GROUP` this section owns its data
/// directly rather than shortcutting into a second surface -- see this
/// module's own doc, "Providers: add/remove owned here" -- so, unlike the
/// plugins header row, nothing here needs to avoid encoding live counts in
/// the label text itself; the label stays a fixed string for the identical
/// `settings_collapsed_groups`-keying reason `PLUGINS_GROUP`'s own doc
/// gives.
const PROVIDERS_GROUP: &str = "providers";

/// The footer's session-only disclosure for the display/tool-output/
/// permissions sections. Shown on every render, regardless of which row is
/// selected.
const SESSION_NOTE: &str = "display/permission changes apply to this session only";

/// Builds the settings tree from the CURRENT `state` (see this module's own
/// doc, "Fresh tree every call") and restores the persisted cursor
/// (`state.settings_selected`) onto it. `pub(crate)` so both [`draw`] and
/// `input::handle_settings_key` build from the exact same function -- the
/// tree the user sees and the tree navigation resolves against can never
/// drift apart.
pub(crate) fn build_tree(state: &AppState) -> MenuState {
    let roots = vec![
        // Board item `01M18Q7P25DTSKQJDJJCC3E800`: see this module's own
        // doc, "Defaults: role settable, model derived", for why the first
        // row is a settable leaf (`Enter` cycles) and the second is
        // `MenuNode::Static` (a derived display, never a leaf). Board item
        // `01M1AWGSTD7084VFVGN1GK9AS8` adds a THIRD, conditional row -- see
        // this module's own doc, "The persistent-default gap: promoting
        // the session's model".
        group_node(DEFAULTS_GROUP, state, {
            let mut rows = vec![
                MenuNode::leaf(
                    format!(
                        "default role -- {} (default) (Enter to cycle)",
                        state.default_role_snapshot
                    ),
                    LEAF_DEFAULT_ROLE,
                ),
                MenuNode::static_row(format!(
                    "default model -- {} (default; the head of the default \
                     role's routing chain -- change the default role above, \
                     or that role's own chain, to change this)",
                    state
                        .default_model_snapshot
                        .as_deref()
                        .unwrap_or("not configured")
                )),
            ];
            // Shown only when THIS SESSION is running a model that
            // differs from the persistent default -- when the two agree
            // there is nothing to promote (see `LEAF_PROMOTE_SESSION_
            // MODEL`'s own doc). Named by BOTH values, never just one, so
            // the session choice and the persistent default can never be
            // mistaken for each other (acceptance 4).
            if let Some(session_model) = state.focused_model.as_deref() {
                if Some(session_model) != state.default_model_snapshot.as_deref() {
                    rows.push(MenuNode::leaf(
                        format!(
                            "this session is running {session_model} -- Enter to make it \
                             the persistent default"
                        ),
                        LEAF_PROMOTE_SESSION_MODEL,
                    ));
                }
            }
            rows
        }),
        group_node(
            DISPLAY_GROUP,
            state,
            vec![
                MenuNode::leaf(
                    bool_label("show reasoning traces", state.show_reasoning),
                    LEAF_SHOW_REASONING,
                ),
                MenuNode::leaf(
                    bool_label("show timestamps", state.show_timestamps),
                    LEAF_SHOW_TIMESTAMPS,
                ),
            ],
        ),
        group_node(
            TOOL_OUTPUT_GROUP,
            state,
            vec![MenuNode::leaf(
                format!(
                    "tool preview lines -- {} (Left/Right to adjust; \
                     persists via [tui.tool_preview_lines])",
                    state.tool_preview_lines
                ),
                LEAF_TOOL_PREVIEW_LINES,
            )],
        ),
        // each active grant is now a
        // real, selectable leaf row with a destructive action (revoke)
        // rather than inert label text -- the reasoning that made a
        // selectable-but-inert row "a worse lie than an obviously static
        // one" held only until per-rule revocation existed to back it; it
        // now does (`Conway::revoke_permission_pattern`). The row's label
        // is unchanged (`[origin] description`, `describe()`) plus an
        // explicit "(Enter to revoke)" hint, so the destructive action is
        // named before it happens, not just implied by the row becoming
        // selectable.
        //
        // Deny and prompt rules get their own sections BELOW the allow
        // section, and their rows are `MenuNode::Static` -- read-only, so
        // the cursor can never land on one and nothing highlights or
        // answers `Enter`. That is the same "worse lie" reasoning run in
        // the OTHER direction: deny/prompt are deliberately NOT revocable
        // from this menu (a safety rule offering one-keystroke removal is
        // the wrong shape -- `Conway::revoke_permission_pattern`'s own
        // doc), so their rows must not LOOK actionable the way a grant row
        // does. They are shown at all because deny/prompt install from ANY
        // permissions file, trusted or not -- an untrusted checkout can
        // ship one -- and a rule set nobody can inspect is a trap. Flat
        // and structured (F12) rules render alike, `[origin] description`;
        // the structured ones simply come from `Rule::describe()` instead
        // of `PatternRule::describe()`.
        group_node(PERMISSIONS_GROUP, state, {
            let rows = vec![
                MenuNode::leaf(
                    format!("mode -- {} (Enter to cycle)", state.permission_mode.label()),
                    LEAF_PERMISSION_MODE,
                ),
                group_node(ALLOW_GROUP, state, {
                    let mut allow = Vec::new();
                    if state.permission_grants.is_empty() && state.structured_allow_rules.is_empty()
                    {
                        allow.push(MenuNode::static_row("no active grants"));
                    } else {
                        for (i, (rule, origin)) in state.permission_grants.iter().enumerate() {
                            allow.push(MenuNode::leaf(
                                format!(
                                    "granted: [{}] {} (Enter to revoke)",
                                    origin.describe(),
                                    rule.describe()
                                ),
                                format!("{LEAF_REVOKE_GRANT_PREFIX}{i}"),
                            ));
                        }
                        // A2: the structured allow rules the flat
                        // form cannot express render in the SAME allow
                        // section, as the same selectable-leaf shape -- they
                        // are revocable grants exactly like the flat rows,
                        // just addressed through the Rule-identity revoke
                        // (`input::activate_settings_selection`'s
                        // `LEAF_REVOKE_STRUCTURED_ALLOW_PREFIX` arm) because
                        // the flat `(PatternRule, origin)` key cannot name
                        // one. The scope is annotated only when it is NOT
                        // the whole session: every rule the TUI itself
                        // installs is session-scoped, so a constant
                        // "session" annotation would be noise on every row,
                        // while an Agent/AgentSubtree grant (an embedder's)
                        // covers LESS than the row otherwise implies and
                        // must say so.
                        for (i, (rule, origin, scope)) in
                            state.structured_allow_rules.iter().enumerate()
                        {
                            let scope_note = match scope {
                                conway::GrantScope::Session => String::new(),
                                other => format!(" (scope: {})", other.describe()),
                            };
                            allow.push(MenuNode::leaf(
                                format!(
                                    "granted: [{}] {}{scope_note} (Enter to revoke)",
                                    origin.describe(),
                                    rule.describe()
                                ),
                                format!("{LEAF_REVOKE_STRUCTURED_ALLOW_PREFIX}{i}"),
                            ));
                        }
                        allow.push(MenuNode::leaf(
                            "revoke all grants (Enter)".to_string(),
                            LEAF_REVOKE_GRANTS,
                        ));
                    }
                    allow
                }),
                group_node(DENY_GROUP, state, {
                    let mut deny: Vec<MenuNode> = state
                        .permission_denies
                        .iter()
                        .map(|(rule, origin)| {
                            MenuNode::static_row(format!(
                                "[{}] {}",
                                origin.describe(),
                                rule.describe()
                            ))
                        })
                        .collect();
                    deny.extend(state.structured_deny_rules.iter().map(|(rule, origin)| {
                        MenuNode::static_row(format!("[{}] {}", origin.describe(), rule.describe()))
                    }));
                    if deny.is_empty() {
                        deny.push(MenuNode::static_row("no active deny rules"));
                    }
                    deny
                }),
                group_node(PROMPT_GROUP, state, {
                    let mut prompt: Vec<MenuNode> = state
                        .permission_prompts
                        .iter()
                        .map(|(rule, origin)| {
                            MenuNode::static_row(format!(
                                "[{}] {}",
                                origin.describe(),
                                rule.describe()
                            ))
                        })
                        .collect();
                    prompt.extend(state.structured_prompt_rules.iter().map(|(rule, origin)| {
                        MenuNode::static_row(format!("[{}] {}", origin.describe(), rule.describe()))
                    }));
                    if prompt.is_empty() {
                        prompt.push(MenuNode::static_row("no active prompt rules"));
                    }
                    prompt
                }),
                // the fourth
                // review list -- every hook-backed rule that can currently
                // DENY a call, selectable and revocable exactly like an
                // allow-grant row (unlike deny/prompt's read-only rows: a
                // hook rule an operator wants OFF should be one keystroke
                // away, same as an allow grant, not permanently locked the
                // way a safety-rule deny pattern deliberately is -- see
                // `Conway::revoke_hook_rule`'s own doc for why this is
                // always session-scoped, never a file rewrite). Each row
                // names its id, event, matcher, and origin so an operator
                // can tell WHICH rule they are about to turn off.
                group_node(HOOKS_GROUP, state, {
                    if state.hook_rules.is_empty() {
                        vec![MenuNode::static_row("no active hook rules")]
                    } else {
                        state
                            .hook_rules
                            .iter()
                            .enumerate()
                            .map(|(i, rule)| {
                                let matcher = match &rule.match_tool {
                                    Some(pattern) => format!("matching `{pattern}`"),
                                    None => "every call".to_string(),
                                };
                                MenuNode::leaf(
                                    format!(
                                        "hook: [{}] `{}` on `{}`, {matcher} (Enter to revoke)",
                                        rule.origin, rule.id, rule.event
                                    ),
                                    format!("{LEAF_REVOKE_HOOK_PREFIX}{i}"),
                                )
                            })
                            .collect()
                    }
                }),
            ];
            rows
        }),
        // Board item `01M11XWB4T8ZADNDB4M8R482MA`: the providers section --
        // see this module's own doc, "Providers: add/remove owned here",
        // for why this OWNS its data directly rather than shortcutting into
        // a second surface the way the plugins section below does.
        group_node(PROVIDERS_GROUP, state, {
            let mut rows: Vec<MenuNode> = Vec::new();
            if state.provider_entries.is_empty() {
                rows.push(MenuNode::static_row("no providers configured"));
            } else {
                for (id, entry) in &state.provider_entries {
                    let status = provider_status_label(
                        state.provider_status.get(id),
                        state.provider_status_loading,
                    );
                    rows.push(MenuNode::leaf(
                        format!("{id} ({}) -- {status} (Enter to remove)", entry.kind),
                        format!("{LEAF_REMOVE_PROVIDER_PREFIX}{id}"),
                    ));
                }
            }
            for choice in crate::first_run::HOSTED_CHOICES {
                rows.push(MenuNode::leaf(
                    format!("add {} (Enter)", choice.label),
                    format!("{LEAF_ADD_PROVIDER_PREFIX}{}", choice.id),
                ));
            }
            rows
        }),
        // Board item `01M0VR5RCCB8NDGG2JEQW8X7XR`: the plugins section is
        // now a single shortcut into `/plugin` (`view/plugins.rs`), not a
        // second listing -- see this module's own doc, "Plugins: one home,
        // not two". The header row names the live counts across ALL THREE
        // kinds conway can run (compiled-in, subprocess, MCP), not only
        // the compiled-in subset this section used to browse directly --
        // an operator glancing at `/settings` alone should not be told
        // "0 available" when a subprocess/MCP entry is in fact configured
        // and running.
        group_node(PLUGINS_GROUP, state, {
            let installed_count = state.plugin_browser.iter().filter(|p| p.installed).count();
            let available_count = state.plugin_browser.len() - installed_count;
            vec![
                MenuNode::static_row(format!(
                    "{installed_count} compiled-in installed \u{b7} {available_count} \
                     compiled-in available \u{b7} {} subprocess \u{b7} {} mcp",
                    state.subprocess_plugins.len(),
                    state.mcp_plugins.len(),
                )),
                MenuNode::leaf(
                    "open the full plugin listing -- /plugin (Enter)".to_string(),
                    LEAF_OPEN_PLUGINS,
                ),
            ]
        }),
    ];
    let mut menu = MenuState::new(roots);
    menu.set_selected(state.settings_selected);
    menu
}

/// Builds one top-level group node via [`MenuNode::group`] (expanded by
/// default -- the primitive's own convenience constructor) and then patches
/// `expanded` to `false` only when `label` is in `state.
/// settings_collapsed_groups` -- collapsed is the exception, so the
/// convenience constructor's own default covers the common case directly
/// rather than every call site re-deriving `!collapsed.contains(label)`
/// inline.
fn group_node(label: &str, state: &AppState, children: Vec<MenuNode>) -> MenuNode {
    let mut node = MenuNode::group(label, children);
    if state.settings_collapsed_groups.contains(label) {
        if let MenuNode::Group { expanded, .. } = &mut node {
            *expanded = false;
        }
    }
    node
}

/// Leads with the SAME `[x]`/`[ ]` bracket marker `view/plugins.rs`'s own
/// toggle rows use -- one idiom applied to every boolean leaf in this menu,
/// not invented per-section. The trailing `-- on`/`-- off` text is kept
/// alongside the box rather than replaced by it, so a value is never
/// conveyed by the box glyph alone.
/// One provider row's own status fragment -- acceptance 2's own "names the
/// variable" and acceptance 3's own "visibly distinct wording" both live
/// here. Renders the underlying `Usability::Unusable`/`Undetermined`
/// payload's own `Display` VERBATIM (never a re-phrasing -- see this
/// module's own doc, "Providers: add/remove owned here"): `Display` already
/// names the actionable cause (`CredentialVariableUnset` carries the
/// variable, `EndpointRefused` carries the URL), and re-wording it here is
/// exactly the drift P-14 exists to prevent.
///
/// `status: None` (an id in [`AppState::provider_entries`] with no entry yet
/// in [`AppState::provider_status`]) is `"checking..."` -- a FOURTH state,
/// distinct from all three of [`Usability`]'s own variants, for the window
/// before the background probe's first reply arrives. `loading` is
/// currently unused by the rendered text itself (every not-yet-classified
/// row already says "checking..." on its own merits) but is threaded
/// through so a future need to distinguish "never classified" from
/// "re-classifying after a stale value" has somewhere to read from without
/// a signature change.
fn provider_status_label(status: Option<&Usability>, _loading: bool) -> String {
    match status {
        None => "checking...".to_string(),
        Some(Usability::Usable) => "working".to_string(),
        Some(Usability::Unusable(reason)) => format!("not working: {reason}"),
        Some(Usability::Undetermined(reason)) => format!("undetermined: {reason}"),
    }
}

fn bool_label(name: &str, value: bool) -> String {
    let box_glyph = if value { "x" } else { " " };
    format!(
        "[{box_glyph}] {name} -- {}",
        if value { "on" } else { "off" }
    )
}

/// Rows the settings modal's footer ALWAYS reserves: the key hint and the
/// session-only disclosure -- mirroring every other ported surface's
/// "footer rows are fixed, never squeezed by body growth" invariant
/// (`view/modal.rs`'s own doc).
const FOOTER_ROWS: u16 = 2;

/// The settings menu's own cap denominator -- `2`: at most half the
/// transcript pane.
///
/// This was `1` (claim the whole pane, the generous cap `/help` still
/// uses), on the reasoning that an INFORMATIONAL surface the user opened
/// on purpose may take more of the screen than a decision-owed one. That
/// reasoning was sound while the menu drew OVER an already-rendered
/// transcript: claiming the whole pane cost nothing that the `Clear` was
/// not covering anyway.
///
/// The reservation this item introduced (see [`modal_rect`]) inverts that.
/// `layout` now shrinks the transcript by exactly the menu's height, so
/// a denominator of `1` is self-defeating by construction: the menu claims
/// the full pane, the transcript is shrunk to nothing, and an error raised
/// while `/settings` is open is once again unreadable -- the very defect
/// the reservation exists to fix. A cap and a reservation are the same
/// mechanism seen from two ends; only the cap decides what the reservation
/// leaves behind.
///
/// Capping is safe here because [`menu::draw`] renders the tree as a
/// stateful `List` whose `ListState` carries the selection: ratatui scrolls
/// it to keep the selected row visible, so rows past the cap stay reachable
/// with `Up`/`Down` rather than being truncated away.
///
/// `/help` keeps `1` and stays coherent: it takes no reservation, so
/// nothing there contradicts.
const CAP_DENOMINATOR: u16 = 2;

/// The `/settings` menu's own bottom-anchored, content-sized `Rect`,
/// computed against `transcript_area` -- exactly what [`draw`] itself now
/// asks for via this same function, never a second, independent
/// computation (steering P-14).
///
/// **Board item `01M1A9M2EVJNR0HBN86A8E40EA`.** Factored out so
/// `view::mod::layout` can learn how tall the menu will render BEFORE
/// `transcript::draw` runs, and shrink the transcript pane by exactly that
/// height -- pushing its content up rather than letting the menu draw over
/// already-rendered lines (an error appended while `/settings` is open used
/// to render behind the menu, invisible until the menu closed). `draw`
/// passes the SAME `transcript_area` it was itself given straight through
/// here, so the reservation `layout` computed and the `Rect` `draw` actually
/// paints into can never disagree.
pub(crate) fn modal_rect(state: &AppState, transcript_area: Rect) -> Rect {
    let tree = build_tree(state);
    let content_rows = tree.rows().len().min(u16::MAX as usize) as u16;
    modal::modal_area(transcript_area, content_rows, FOOTER_ROWS, CAP_DENOMINATOR)
}

/// Draws the `/settings` menu over `transcript_area` via the shared
/// [`modal`] primitive (bottom-anchored, content-sized, capped) with
/// [`menu::draw`] rendering the tree itself into the modal's own
/// `body_area`. Reuses `theme.help_border` for the border rather than
/// adding a new theme slot -- like `/help`, this is an informational
/// overlay, and the two are mutually exclusive (`AppState::open_settings`/
/// `open_help` each close the other), so no two things can ever be on
/// screen at once needing visually distinct borders.
pub fn draw(frame: &mut Frame, transcript_area: Rect, state: &AppState, theme: &Theme) {
    let tree = build_tree(state);
    let area = modal_rect(state, transcript_area);

    let frame_areas =
        modal::draw_modal_frame_in(frame, area, FOOTER_ROWS, " SETTINGS ", theme.help_border);

    menu::draw(frame, frame_areas.body_area, &tree, theme);

    let footer_lines = vec![
        Line::from("[Up/Down] move  [Enter] toggle/expand  [Left/Right] adjust  [Esc] close"),
        Line::from(Span::styled(SESSION_NOTE, theme.dim)),
    ];
    let footer = Paragraph::new(footer_lines).wrap(Wrap { trim: true });
    frame.render_widget(footer, frame_areas.footer_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway::AgentId;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(state: &AppState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(f, f.area(), state, &Theme::default()))
            .expect("draw");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    // ---- build_tree: labels reflect the CURRENT AppState values ----

    #[test]
    fn build_tree_labels_reflect_current_boolean_values() {
        let mut state = AppState::new(AgentId::new());
        state.show_reasoning = true;
        state.show_timestamps = false;

        let rows = build_tree(&state).rows();
        let labels: Vec<String> = rows.iter().map(|r| r.label.clone()).collect();

        assert!(
            labels
                .iter()
                .any(|l| l.contains("show reasoning traces") && l.ends_with("on")),
            "{labels:?}"
        );
        assert!(
            labels
                .iter()
                .any(|l| l.contains("show timestamps") && l.ends_with("off")),
            "{labels:?}"
        );

        state.show_reasoning = false;
        let rows_after = build_tree(&state).rows();
        let labels_after: Vec<String> = rows_after.iter().map(|r| r.label.clone()).collect();
        assert!(
            labels_after
                .iter()
                .any(|l| l.contains("show reasoning traces") && l.ends_with("off")),
            "a freshly built tree must reflect the CURRENT value, not a stale one: {labels_after:?}"
        );
    }

    #[test]
    fn build_tree_labels_the_numeric_leaf_with_its_current_value_and_config_key() {
        let mut state = AppState::new(AgentId::new());
        state.tool_preview_lines = 7;

        let rows = build_tree(&state).rows();
        let numeric_label = rows
            .iter()
            .find(|r| r.label.contains("tool preview lines"))
            .expect("the numeric leaf must exist")
            .label
            .clone();

        assert!(numeric_label.contains('7'), "{numeric_label}");
        assert!(
            numeric_label.contains("[tui.tool_preview_lines]"),
            "the one leaf with a real config key must name it: {numeric_label}"
        );
    }

    #[test]
    fn boolean_leaves_do_not_claim_a_config_key_that_does_not_exist() {
        let state = AppState::new(AgentId::new());
        let rows = build_tree(&state).rows();
        for row in &rows {
            if row.label.contains("show reasoning") || row.label.contains("show timestamps") {
                assert!(
                    !row.label.contains("[tui."),
                    "no `[tui.*]` config key backs these two -- the label must not \
                     imply one exists: {}",
                    row.label
                );
            }
        }
    }

    #[test]
    fn build_tree_restores_the_persisted_cursor() {
        let mut state = AppState::new(AgentId::new());
        // Any already-selectable row proves the restore -- `menu.rs`'s own
        // `set_selected_is_clamped_on_read_not_on_write` already covers the
        // "restored index now lands on a `Static` row" resolution
        // separately. Row 2 used to be "show timestamps" (a leaf), but
        // board item `01M18Q7P25DTSKQJDJJCC3E800` inserted the "defaults"
        // group ahead of "display", so row 2 is now "default model" --
        // `MenuNode::Static`, not selectable -- which would exercise that
        // OTHER resolution path instead of the plain restore this test
        // means to check. Row 4 ("show reasoning traces") is selectable in
        // the new layout and keeps this test on its own, single behavior.
        state.settings_selected = 4;

        let tree = build_tree(&state);
        assert_eq!(tree.selected_index(), 4);
    }

    // ---- draw: bottom-anchored, content-sized (V1's shape) ----

    #[test]
    fn draw_renders_bottom_anchored_and_content_sized() {
        let state = AppState::new(AgentId::new());
        let text = render(&state, 80, 24);
        assert!(text.contains("SETTINGS"), "{text}");
        assert!(text.contains("display"), "{text}");
        assert!(text.contains("tool output"), "{text}");
        assert!(text.contains(SESSION_NOTE), "{text}");
    }

    #[test]
    fn draw_never_panics_on_a_tiny_terminal() {
        let state = AppState::new(AgentId::new());
        for (w, h) in [(80u16, 1u16), (80, 2), (1, 24), (0, 0)] {
            let backend = TestBackend::new(w.max(1), h.max(1));
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| draw(f, f.area(), &state, &Theme::default()))
                .unwrap_or_else(|e| panic!("panicked/errored at {w}x{h}: {e}"));
        }
    }
    /// V2b: the permissions group exposes the mode and, once grants exist,
    /// a review list plus revoke-all.
    #[test]
    fn the_permissions_group_shows_mode_and_grants() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();

        let text = plain_rows(&state);
        assert!(text.contains("permissions"), "{text}");
        assert!(text.contains("mode -- prompt"), "{text}");
        assert!(
            text.contains("no active grants"),
            "an empty grant list says so rather than rendering nothing: {text}"
        );

        state.permission_grants = vec![(
            conway::PatternRule::parse("bash:git status").expect("valid rule"),
            conway::PatternOrigin::Interactive,
        )];
        let text = plain_rows(&state);
        assert!(
            text.contains("granted: [interactive] `bash` commands starting with `git status` (Enter to revoke)"),
            "{text}"
        );
        assert!(
            text.contains("revoke all grants"),
            "revoke-all appears only once there is something to revoke: {text}"
        );
    }

    /// Each grant row is now a real
    /// selectable leaf, addressed by its position in `state.
    /// permission_grants` at build time -- not the inert `""` id the
    /// pre-revocation shape used.
    #[test]
    fn each_grant_row_is_a_selectable_leaf_addressed_by_its_index() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.permission_grants = vec![
            (
                conway::PatternRule::parse("bash:git status").expect("valid rule"),
                conway::PatternOrigin::Interactive,
            ),
            (
                conway::PatternRule::parse("read:*").expect("valid rule"),
                conway::PatternOrigin::File(std::path::PathBuf::from(
                    "/repo/.conway/permissions.json",
                )),
            ),
        ];

        let rows = build_tree(&state).rows();
        let grant_rows: Vec<_> = rows
            .iter()
            .filter(|r| r.label.starts_with("granted:"))
            .collect();
        assert_eq!(grant_rows.len(), 2);
        assert_eq!(
            grant_rows[0].kind,
            menu::MenuRowKind::Leaf {
                id: format!("{LEAF_REVOKE_GRANT_PREFIX}0")
            }
        );
        assert_eq!(
            grant_rows[1].kind,
            menu::MenuRowKind::Leaf {
                id: format!("{LEAF_REVOKE_GRANT_PREFIX}1")
            }
        );
        assert!(grant_rows[1]
            .label
            .contains("/repo/.conway/permissions.json"));
    }

    /// The mode label in the menu tracks `AppState`, so it cannot show a
    /// stale value after a cycle.
    #[test]
    fn the_mode_row_reflects_the_current_permission_mode() {
        let mut state = AppState::new(AgentId::new());
        state.permission_mode = conway::PermissionMode::AutoAllow;
        assert!(plain_rows(&state).contains("mode -- AUTO-ALLOW"));

        state.permission_mode = conway::PermissionMode::Plan;
        assert!(plain_rows(&state).contains("mode -- plan"));
    }

    /// Helper: the menu's visible row labels as one string.
    fn plain_rows(state: &AppState) -> String {
        build_tree(state)
            .rows()
            .iter()
            .map(|r| r.label.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ---- Deny/prompt review sections: visible, read-only, with origin ----

    /// One flat deny + one structured deny + one flat prompt + one
    /// structured prompt, all from the same project file.
    fn populate_deny_and_prompt(state: &mut AppState) {
        let file = || {
            conway::PatternOrigin::File(std::path::PathBuf::from("/repo/.conway/permissions.json"))
        };
        state.permission_denies = vec![(
            conway::PatternRule::parse("bash:curl").expect("valid rule"),
            file(),
        )];
        state.structured_deny_rules = vec![(
            conway::Rule {
                select: conway::Select::Tools(vec!["bash".to_string(), "read".to_string()]),
                when: conway::When::Always,
                then: conway::Then::Deny,
            },
            file(),
        )];
        state.permission_prompts = vec![(
            conway::PatternRule::parse("bash:rm").expect("valid rule"),
            file(),
        )];
        state.structured_prompt_rules = vec![(
            conway::Rule {
                select: conway::Select::Tools(vec!["bash".to_string(), "read".to_string()]),
                when: conway::When::Always,
                then: conway::Then::Prompt,
            },
            file(),
        )];
    }

    /// Every active deny and prompt rule -- flat AND structured -- renders
    /// with its origin, in its own section, as a NON-selectable static row
    /// that promises no `Enter` action.
    #[test]
    fn deny_and_prompt_rules_render_with_their_origins_as_static_rows() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        populate_deny_and_prompt(&mut state);

        let rows = build_tree(&state).rows();
        let text = rows
            .iter()
            .map(|r| r.label.clone())
            .collect::<Vec<_>>()
            .join("\n");

        // The sections exist as their own groups, distinct from allow's.
        for section in ["allow", "deny", "prompt"] {
            assert!(
                rows.iter().any(
                    |r| r.label == section && matches!(r.kind, menu::MenuRowKind::Group { .. })
                ),
                "the {section} section must be its own group: {text}"
            );
        }

        // Flat and structured rules alike, each with its origin's path.
        let expected = [
            "[/repo/.conway/permissions.json] `bash` commands starting with `curl`",
            "[/repo/.conway/permissions.json] [bash, read] (any call)",
            "[/repo/.conway/permissions.json] `bash` commands starting with `rm`",
        ];
        for needle in expected {
            assert!(text.contains(needle), "missing row: {needle}\n{text}");
        }

        // Every deny/prompt entry row is a Static row (never selectable,
        // never highlighted) and names no Enter action -- a read-only row
        // must not LOOK actionable (see `MenuNode::Static`'s own doc).
        let deny_prompt_rows: Vec<_> = rows
            .iter()
            .filter(|r| r.label.contains("/repo/.conway/permissions.json"))
            .collect();
        assert_eq!(deny_prompt_rows.len(), 4, "{text}");
        for row in deny_prompt_rows {
            assert_eq!(
                row.kind,
                menu::MenuRowKind::Static,
                "a deny/prompt row must be read-only: {row:?}"
            );
            assert!(
                !row.label.contains("(Enter"),
                "a read-only row must not promise an action: {}",
                row.label
            );
        }
    }

    /// Empty sections say so honestly, as static rows (the cursor never
    /// lands on the placeholder either).
    #[test]
    fn deny_and_prompt_sections_have_honest_empty_states() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();

        let rows = build_tree(&state).rows();
        for needle in ["no active deny rules", "no active prompt rules"] {
            let row = rows
                .iter()
                .find(|r| r.label == needle)
                .unwrap_or_else(|| panic!("missing empty-state row: {needle}"));
            assert_eq!(row.kind, menu::MenuRowKind::Static, "{row:?}");
        }
    }

    /// Driven through the real navigation primitive: pressing Down across
    /// the whole tree never rests the cursor on a deny/prompt row.
    #[test]
    fn the_cursor_can_never_land_on_a_deny_or_prompt_row() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        populate_deny_and_prompt(&mut state);

        let mut tree = build_tree(&state);
        let row_count = tree.rows().len();
        for _ in 0..row_count {
            let selected = tree.selected_row().expect("nonempty tree");
            assert!(
                !matches!(selected.kind, menu::MenuRowKind::Static),
                "the cursor landed on a read-only row: {selected:?}"
            );
            tree.move_selection(1);
        }
    }

    // ---- Structured allow rows: visible AND revocable (A2) ----

    fn a_structured_allow_rule() -> conway::Rule {
        // Multi-tool + Always: a rule the flat form cannot express (its
        // `to_pattern_rule()` is None), the kind A2 makes revocable.
        conway::Rule {
            select: conway::Select::Tools(vec!["bash".to_string(), "read".to_string()]),
            when: conway::When::Always,
            then: conway::Then::Allow,
        }
    }

    /// A structured allow rule renders in the allow section as a SELECTABLE,
    /// revocable leaf -- `[origin] description (Enter to revoke)`, the same
    /// shape as a flat grant row -- but keyed by its OWN leaf-id prefix, so
    /// its index space can never collide with the flat rows'.
    #[test]
    fn structured_allow_rules_render_as_revocable_leaves_with_their_origins() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.permission_grants = vec![(
            conway::PatternRule::parse("bash:git status").expect("valid rule"),
            conway::PatternOrigin::Interactive,
        )];
        state.structured_allow_rules = vec![(
            a_structured_allow_rule(),
            conway::PatternOrigin::File(std::path::PathBuf::from("/repo/.conway/permissions.json")),
            conway::GrantScope::Session,
        )];

        let rows = build_tree(&state).rows();

        let structured_row = rows
            .iter()
            .find(|r| r.label.contains("[bash, read] (any call)"))
            .expect("the structured allow row must render");
        assert!(structured_row.label.starts_with("granted:"));
        assert!(
            structured_row
                .label
                .contains("/repo/.conway/permissions.json"),
            "the row must name its origin: {}",
            structured_row.label
        );
        assert!(
            structured_row.label.contains("(Enter to revoke)"),
            "a revocable row must name its action: {}",
            structured_row.label
        );
        assert!(
            !structured_row.label.contains("scope:"),
            "a session-wide grant is the default -- annotating it would be noise: {}",
            structured_row.label
        );
        assert_eq!(
            structured_row.kind,
            menu::MenuRowKind::Leaf {
                id: format!("{LEAF_REVOKE_STRUCTURED_ALLOW_PREFIX}0")
            },
            "keyed by the structured prefix, in its own index space"
        );

        // The flat sibling keeps its own prefix and index space -- index 0
        // in EACH list addresses a different row, and the prefixes keep the
        // two resolves apart.
        let flat_row = rows
            .iter()
            .find(|r| r.label.contains("git status"))
            .expect("the flat grant row must render");
        assert_eq!(
            flat_row.kind,
            menu::MenuRowKind::Leaf {
                id: format!("{LEAF_REVOKE_GRANT_PREFIX}0")
            }
        );

        // Both are selectable (never Static), and revoke-all is still there.
        let text = rows
            .iter()
            .map(|r| r.label.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("revoke all grants"), "{text}");
        assert!(!text.contains("no active grants"), "{text}");
    }

    /// A structured allow rule granted at a NARROWER scope than the session
    /// must say so -- the row otherwise implies it covers every agent.
    #[test]
    fn a_structured_allow_rule_with_a_narrower_scope_is_annotated() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.structured_allow_rules = vec![(
            a_structured_allow_rule(),
            conway::PatternOrigin::Interactive,
            conway::GrantScope::Agent(AgentId::new()),
        )];

        let rows = build_tree(&state).rows();
        let row = rows
            .iter()
            .find(|r| r.label.contains("[bash, read] (any call)"))
            .expect("the structured allow row must render");
        assert!(
            row.label.contains("scope: agent"),
            "a non-session scope must be visible: {}",
            row.label
        );
    }

    /// The whole point of `GrantScope::Agent`/`::Subtree` carrying an
    /// `AgentId` (Stage 2b, board items `01KZVYZM7BZRQ54RRB8P814KV9`/
    /// `01KZWRZ4JBAVCRCZ99BFZFF01K`): the review row must name WHICH agent a
    /// per-agent or per-subtree grant covers, not merely that it is
    /// narrower than the session. Two rows granted to two DIFFERENT agents
    /// must render two DIFFERENT labels -- a regression that dropped the
    /// `AgentId` (e.g. `GrantScope` degrading to a bare `PermissionScope`-
    /// shaped enum, or `describe()` returning a constant `"agent"` string)
    /// would collapse both to the same text and fail this test, unlike
    /// `a_structured_allow_rule_with_a_narrower_scope_is_annotated` above,
    /// which only checks the word "agent" appears.
    #[test]
    fn the_structured_allow_scope_annotation_names_which_agent_it_covers() {
        let agent_a = AgentId::new();
        let agent_b = AgentId::new();
        assert_ne!(agent_a, agent_b, "the two granting agents must differ");

        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.structured_allow_rules = vec![
            (
                a_structured_allow_rule(),
                conway::PatternOrigin::Interactive,
                conway::GrantScope::Agent(agent_a),
            ),
            (
                a_structured_allow_rule(),
                conway::PatternOrigin::File(std::path::PathBuf::from(
                    "/repo/.conway/permissions.json",
                )),
                conway::GrantScope::Subtree(agent_b),
            ),
        ];

        let rows = build_tree(&state).rows();
        let labels: Vec<&str> = rows
            .iter()
            .filter(|r| r.label.contains("[bash, read] (any call)"))
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(
            labels.len(),
            2,
            "both structured rows must render: {labels:?}"
        );

        let agent_a_str = agent_a.to_string();
        let agent_b_str = agent_b.to_string();
        assert!(
            labels.iter().any(|l| l.contains(&agent_a_str)),
            "the per-agent row must name agent {agent_a_str}'s own id, not just \
             the word \"agent\": {labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains(&agent_b_str)),
            "the per-subtree row must name agent {agent_b_str}'s own id: {labels:?}"
        );
        assert_ne!(
            labels[0], labels[1],
            "two grants to two different agents must render two different \
             rows -- identical rows would mean the AgentId was dropped \
             somewhere on the way from `GrantScope` to the render: {labels:?}"
        );
    }

    /// An allow section holding ONLY structured rules must not render the
    /// "no active grants" empty state (that message would be a lie), and
    /// revoke-all remains available for them.
    #[test]
    fn an_allow_section_with_only_structured_rules_has_no_false_empty_state() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.structured_allow_rules = vec![(
            a_structured_allow_rule(),
            conway::PatternOrigin::Interactive,
            conway::GrantScope::Session,
        )];

        let text = plain_rows(&state);
        assert!(
            !text.contains("no active grants"),
            "a structured rule IS an active grant: {text}"
        );
        assert!(text.contains("[bash, read] (any call)"), "{text}");
        assert!(text.contains("revoke all grants"), "{text}");
    }

    // ---- Hooks review section: visible AND revocable ----

    fn a_hook_rule(id: &str, event: &str, match_tool: Option<&str>) -> conway::HookRuleView {
        conway::HookRuleView {
            id: id.to_string(),
            event: event.to_string(),
            match_tool: match_tool.map(str::to_string),
            origin: "settings.json (merged config)".to_string(),
        }
    }

    /// The hooks section exists, is its own group distinct from allow/deny/
    /// prompt, and an empty list says so honestly rather than rendering
    /// nothing (mirroring the deny/prompt empty states).
    #[test]
    fn the_hooks_group_has_an_honest_empty_state() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();

        let rows = build_tree(&state).rows();
        assert!(
            rows.iter()
                .any(|r| r.label == "hooks" && matches!(r.kind, menu::MenuRowKind::Group { .. })),
            "the hooks section must be its own group: {rows:?}"
        );
        let empty_row = rows
            .iter()
            .find(|r| r.label == "no active hook rules")
            .expect("an empty hook list must say so honestly");
        assert_eq!(empty_row.kind, menu::MenuRowKind::Static);
    }

    /// A `pre_tool_use` rule and a `prompt_submitted` rule -- one with a
    /// matcher, one without -- each render as a SELECTABLE, revocable leaf
    /// naming its id, event, matcher, and origin, addressed by its own
    /// index-keyed leaf id (mirroring the allow section's revocable rows).
    #[test]
    fn hook_rules_render_as_revocable_leaves_naming_id_event_match_and_origin() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.hook_rules = vec![
            a_hook_rule("deny-writes", "pre_tool_use", Some("fs.write")),
            a_hook_rule("deny-prompts", "prompt_submitted", None),
        ];

        let rows = build_tree(&state).rows();
        let hook_rows: Vec<_> = rows
            .iter()
            .filter(|r| r.label.starts_with("hook:"))
            .collect();
        assert_eq!(hook_rows.len(), 2, "{rows:?}");

        assert!(
            hook_rows[0].label.contains("deny-writes")
                && hook_rows[0].label.contains("pre_tool_use")
                && hook_rows[0].label.contains("fs.write")
                && hook_rows[0].label.contains("settings.json (merged config)")
                && hook_rows[0].label.contains("(Enter to revoke)"),
            "{}",
            hook_rows[0].label
        );
        assert_eq!(
            hook_rows[0].kind,
            menu::MenuRowKind::Leaf {
                id: format!("{LEAF_REVOKE_HOOK_PREFIX}0")
            }
        );

        // The second row (no matcher) says so honestly rather than
        // omitting the field or claiming a pattern that isn't there.
        assert!(
            hook_rows[1].label.contains("deny-prompts")
                && hook_rows[1].label.contains("prompt_submitted")
                && hook_rows[1].label.contains("every call"),
            "{}",
            hook_rows[1].label
        );
        assert_eq!(
            hook_rows[1].kind,
            menu::MenuRowKind::Leaf {
                id: format!("{LEAF_REVOKE_HOOK_PREFIX}1")
            }
        );
    }

    /// A hook row is never `MenuNode::Static` -- unlike deny/prompt rows,
    /// a hook rule IS revocable from this menu (`Conway::revoke_hook_rule`'s
    /// own doc: session-scoped, never a file rewrite, so there is no
    /// safety-rule reason to lock it the way a deny PATTERN is).
    #[test]
    fn a_hook_row_is_selectable_not_static() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.hook_rules = vec![a_hook_rule("deny-writes", "pre_tool_use", None)];

        let rows = build_tree(&state).rows();
        let row = rows
            .iter()
            .find(|r| r.label.starts_with("hook:"))
            .expect("the hook row must render");
        assert!(
            !matches!(row.kind, menu::MenuRowKind::Static),
            "a revocable hook row must not be read-only: {row:?}"
        );
    }

    // ---- Plugins section: a single shortcut into `/plugin` (board item
    // `01M0VR5RCCB8NDGG2JEQW8X7XR`) ----

    /// The header row states counts across ALL THREE kinds -- not only the
    /// compiled-in subset this section used to browse directly -- and the
    /// only OTHER row is the shortcut leaf.
    #[test]
    fn the_plugins_group_shows_counts_across_every_kind_and_a_single_shortcut() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.plugin_browser = vec![crate::tui::state::PluginBrowserEntry {
            id: "conway.memory".to_string(),
            version: "0.9.0".to_string(),
            installed: true,
            description: conway::plugin::PluginDescription::default(),
        }];
        state.subprocess_plugins = vec![crate::tui::state::ConfiguredPluginEntry {
            id: "acme.review".to_string(),
            command: vec!["acme-review".to_string()],
        }];
        state.mcp_plugins = vec![crate::tui::state::ConfiguredPluginEntry {
            id: "acme.search".to_string(),
            command: vec!["acme-search".to_string()],
        }];

        let text = plain_rows(&state);
        assert!(text.contains("1 compiled-in installed"), "{text}");
        assert!(text.contains("0 compiled-in available"), "{text}");
        assert!(text.contains("1 subprocess"), "{text}");
        assert!(text.contains("1 mcp"), "{text}");
        assert!(
            text.contains("open the full plugin listing -- /plugin (Enter)"),
            "{text}"
        );
        // Nothing per-plugin renders here any more -- neither the
        // compiled-in id nor the subprocess/mcp ids appear as their own
        // rows in THIS menu; that listing lives at `/plugin` now.
        assert!(!text.contains("conway.memory"), "{text}");
        assert!(!text.contains("acme.review"), "{text}");
        assert!(!text.contains("acme.search"), "{text}");
    }

    /// The shortcut row is a selectable [`MenuNode::Leaf`], keyed by
    /// [`LEAF_OPEN_PLUGINS`] -- `input::activate_settings_selection`
    /// resolves it to opening `/plugin`.
    #[test]
    fn the_shortcut_row_is_a_selectable_leaf_keyed_by_leaf_open_plugins() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();

        let rows = build_tree(&state).rows();
        let shortcut = rows
            .iter()
            .find(|r| r.label.contains("open the full plugin listing"))
            .expect("the shortcut row must render");
        assert_eq!(
            shortcut.kind,
            menu::MenuRowKind::Leaf {
                id: LEAF_OPEN_PLUGINS.to_string()
            }
        );
    }

    #[test]
    fn draw_never_panics_with_plugins_configured() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.subprocess_plugins = vec![crate::tui::state::ConfiguredPluginEntry {
            id: "acme.review".to_string(),
            command: vec!["acme-review".to_string()],
        }];
        for (w, h) in [(80u16, 1u16), (80, 2), (1, 24), (0, 0)] {
            let backend = TestBackend::new(w.max(1), h.max(1));
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| draw(f, f.area(), &state, &Theme::default()))
                .unwrap_or_else(|e| panic!("panicked/errored at {w}x{h}: {e}"));
        }
    }

    // ---- Providers section: board item `01M11XWB4T8ZADNDB4M8R482MA` ----

    fn a_backend_entry(kind: &str) -> conway::config::schema::BackendEntry {
        conway::config::schema::BackendEntry {
            kind: kind.to_string(),
            ..Default::default()
        }
    }

    /// Acceptance 1: every configured provider is listed, naming its id and
    /// kind.
    #[test]
    fn the_providers_group_lists_every_configured_provider() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.provider_entries = std::collections::BTreeMap::from([
            ("kimi".to_string(), a_backend_entry("openai-compat")),
            ("anthropic".to_string(), a_backend_entry("anthropic")),
        ]);

        let text = plain_rows(&state);
        assert!(text.contains("providers"), "{text}");
        assert!(text.contains("kimi (openai-compat)"), "{text}");
        assert!(text.contains("anthropic (anthropic)"), "{text}");
    }

    /// An empty fleet says so honestly rather than rendering nothing,
    /// mirroring the deny/prompt/hooks sections' own empty-state idiom.
    #[test]
    fn an_empty_provider_fleet_has_an_honest_empty_state() {
        let state = AppState::new(AgentId::new());
        let text = plain_rows(&state);
        assert!(text.contains("no providers configured"), "{text}");
    }

    /// Acceptance 2: a provider whose environment variable is unset is
    /// shown as not working, and the reason names the VARIABLE -- straight
    /// from `Unusable`'s own `Display`, never a re-phrasing.
    #[test]
    fn an_unset_credential_variable_is_shown_as_not_working_and_names_the_variable() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.provider_entries = std::collections::BTreeMap::from([(
            "kimi".to_string(),
            a_backend_entry("openai-compat"),
        )]);
        state.provider_status = std::collections::BTreeMap::from([(
            "kimi".to_string(),
            Usability::Unusable(
                conway::backend_usability::Unusable::CredentialVariableUnset {
                    variable: "KIMI_API_KEY".to_string(),
                },
            ),
        )]);

        let text = plain_rows(&state);
        assert!(text.contains("not working"), "{text}");
        assert!(
            text.contains("KIMI_API_KEY"),
            "the reason must name the variable, straight from Unusable::Display: {text}"
        );
    }

    /// Acceptance 3, and the trap this item's own spec names explicitly: an
    /// `Undetermined` provider must render VISIBLY DIFFERENTLY from an
    /// `Unusable` one -- never collapsed to a single "broken" wording. A
    /// test asserting only "not shown as working" would pass against that
    /// collapse; this asserts the two renderings are actually distinct
    /// strings.
    #[test]
    fn undetermined_and_unusable_render_visibly_differently() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.provider_entries = std::collections::BTreeMap::from([
            ("broken".to_string(), a_backend_entry("openai-compat")),
            ("unsure".to_string(), a_backend_entry("openai-compat")),
        ]);
        state.provider_status = std::collections::BTreeMap::from([
            (
                "broken".to_string(),
                Usability::Unusable(conway::backend_usability::Unusable::EndpointRefused {
                    base_url: "http://localhost:11434/v1".to_string(),
                }),
            ),
            (
                "unsure".to_string(),
                Usability::Undetermined(conway::backend_usability::Undetermined::NotProbed),
            ),
        ]);

        let rows = build_tree(&state).rows();
        let broken_row = rows
            .iter()
            .find(|r| r.label.starts_with("broken ("))
            .expect("the broken row must render")
            .label
            .clone();
        let unsure_row = rows
            .iter()
            .find(|r| r.label.starts_with("unsure ("))
            .expect("the unsure row must render")
            .label
            .clone();

        assert!(broken_row.contains("not working"), "{broken_row}");
        assert!(unsure_row.contains("undetermined"), "{unsure_row}");
        assert_ne!(
            broken_row, unsure_row,
            "an Unusable and an Undetermined row must never render identically"
        );
        assert!(
            !unsure_row.contains("not working"),
            "an Undetermined provider must not read as a failure: {unsure_row}"
        );
    }

    /// A provider present in the fleet but not yet classified (the window
    /// before the background probe's reply arrives) renders its own fourth,
    /// honest "checking..." state -- never silently defaulting to looking
    /// like either `Usable` or a failure.
    #[test]
    fn an_unclassified_provider_renders_as_checking_not_as_broken_or_working() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.provider_entries = std::collections::BTreeMap::from([(
            "kimi".to_string(),
            a_backend_entry("openai-compat"),
        )]);
        // provider_status deliberately left empty -- classification has not
        // returned yet.

        let text = plain_rows(&state);
        assert!(text.contains("checking..."), "{text}");
        assert!(!text.contains("not working"), "{text}");
        assert!(!text.contains("undetermined"), "{text}");
    }

    /// Acceptance 8/P-14: this section reuses `crate::first_run::
    /// HOSTED_CHOICES` verbatim rather than restating which provider shapes
    /// exist -- a line-anchored source check (not a bare substring), the
    /// same shape `first_run.rs`'s own `main_rs_calls_should_offer_guided_
    /// setup_rather_than_restating_its_condition` test uses for the
    /// analogous P-14 claim one item over.
    #[test]
    fn the_add_provider_leaves_reuse_first_run_hosted_choices_verbatim() {
        let this_file = include_str!("settings.rs");
        assert!(
            this_file.contains("for choice in crate::first_run::HOSTED_CHOICES {"),
            "the add-provider leaves must iterate the SAME table `first_run.rs` uses, not a \
             second, independent list of provider shapes"
        );
    }

    /// Each `add {label}` leaf renders and is keyed by its own choice id,
    /// resolvable back to an `Action::AddProviderChoice`.
    #[test]
    fn add_provider_leaves_render_one_per_hosted_choice() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();

        let rows = build_tree(&state).rows();
        for choice in crate::first_run::HOSTED_CHOICES {
            let expected_id = format!("{LEAF_ADD_PROVIDER_PREFIX}{}", choice.id);
            assert!(
                rows.iter().any(|r| matches!(
                    &r.kind,
                    menu::MenuRowKind::Leaf { id } if id == &expected_id
                )),
                "missing an add-provider leaf for {}: {rows:?}",
                choice.id
            );
        }
    }

    /// A configured provider's own row is a selectable, revocable leaf --
    /// `Enter` removes it -- keyed by the map key itself (never an index).
    #[test]
    fn a_configured_provider_row_is_a_selectable_remove_leaf_keyed_by_id() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.provider_entries = std::collections::BTreeMap::from([(
            "kimi".to_string(),
            a_backend_entry("openai-compat"),
        )]);

        let rows = build_tree(&state).rows();
        let row = rows
            .iter()
            .find(|r| r.label.starts_with("kimi ("))
            .expect("the kimi row must render");
        assert_eq!(
            row.kind,
            menu::MenuRowKind::Leaf {
                id: format!("{LEAF_REMOVE_PROVIDER_PREFIX}kimi")
            }
        );
        assert!(row.label.contains("(Enter to remove)"), "{}", row.label);
    }

    #[test]
    fn draw_never_panics_with_providers_configured() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.provider_entries = std::collections::BTreeMap::from([(
            "kimi".to_string(),
            a_backend_entry("openai-compat"),
        )]);
        state.provider_status = std::collections::BTreeMap::from([(
            "kimi".to_string(),
            Usability::Unusable(
                conway::backend_usability::Unusable::CredentialVariableUnset {
                    variable: "KIMI_API_KEY".to_string(),
                },
            ),
        )]);
        for (w, h) in [(80u16, 1u16), (80, 2), (1, 24), (0, 0)] {
            let backend = TestBackend::new(w.max(1), h.max(1));
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| draw(f, f.area(), &state, &Theme::default()))
                .unwrap_or_else(|e| panic!("panicked/errored at {w}x{h}: {e}"));
        }
    }

    // ---------------------------------------------------------------
    // Board item `01M18Q7P25DTSKQJDJJCC3E800`: the "defaults" section.
    // ---------------------------------------------------------------

    /// ACCEPTANCE 2: `/settings` shows a default model and a default role,
    /// each labelled as a default. The discriminating observable: both
    /// labels contain the CURRENT snapshot value AND the literal word
    /// "default" -- a row showing the value without that word would not
    /// satisfy the design page's "labelled as defaults" requirement, and
    /// this fails the moment either goes missing.
    #[test]
    fn build_tree_shows_default_role_and_default_model_each_labelled_as_a_default() {
        let mut state = AppState::new(AgentId::new());
        state.default_role_snapshot = "coder".to_string();
        state.default_model_snapshot = Some("anthropic/claude-sonnet-4-6".to_string());

        let rows = build_tree(&state).rows();
        let labels: Vec<String> = rows.iter().map(|r| r.label.clone()).collect();

        let role_label = labels
            .iter()
            .find(|l| l.contains("default role"))
            .unwrap_or_else(|| panic!("no default-role row: {labels:?}"));
        assert!(role_label.contains("coder"), "{role_label}");
        assert!(role_label.contains("(default)"), "{role_label}");

        let model_label = labels
            .iter()
            .find(|l| l.contains("default model"))
            .unwrap_or_else(|| panic!("no default-model row: {labels:?}"));
        assert!(
            model_label.contains("anthropic/claude-sonnet-4-6"),
            "{model_label}"
        );
        assert!(model_label.contains("default"), "{model_label}");
    }

    /// A default model of `None` (the default role has no chain yet) reads
    /// "not configured", never a blank or a synthesized guess.
    #[test]
    fn default_model_row_reads_not_configured_when_none() {
        let mut state = AppState::new(AgentId::new());
        state.default_role_snapshot = "coder".to_string();
        state.default_model_snapshot = None;

        let rows = build_tree(&state).rows();
        let model_label = rows
            .iter()
            .find(|r| r.label.contains("default model"))
            .expect("default-model row must exist")
            .label
            .clone();
        assert!(model_label.contains("not configured"), "{model_label}");
    }

    /// ACCEPTANCE 4 ("no second source of truth for model selection"),
    /// enforced structurally: the default-model row is `MenuNode::Static`,
    /// never a `Leaf` -- there is no `Enter` action that could write a
    /// second, independent value for it. The discriminating observable:
    /// if a future edit turned this into a settable leaf (reintroducing
    /// the rejected `default_model` scalar this item's own decision
    /// record argues against), this assertion is exactly what would catch
    /// it.
    #[test]
    fn default_model_row_is_static_never_a_settable_leaf() {
        let mut state = AppState::new(AgentId::new());
        state.default_role_snapshot = "coder".to_string();
        state.default_model_snapshot = Some("anthropic/claude-sonnet-4-6".to_string());

        let rows = build_tree(&state).rows();
        let model_row = rows
            .iter()
            .find(|r| r.label.contains("default model"))
            .expect("default-model row must exist");
        assert_eq!(model_row.kind, menu::MenuRowKind::Static, "{model_row:?}");
    }

    // ---------------------------------------------------------------
    // Board item `01M1AWGSTD7084VFVGN1GK9AS8`: LEAF_PROMOTE_SESSION_MODEL,
    // the writable path for the derived default -- a SIBLING leaf, not
    // the static row above turned settable.
    // ---------------------------------------------------------------

    /// ACCEPTANCE 1/4: a session running a DIFFERENT model than the
    /// persistent default gets an actionable row naming BOTH values --
    /// this is the fix's own discriminating case, the operator's exact
    /// report (`/model ollama_cloud/glm-5.2`, then a restart silently
    /// reverting to the chain head).
    #[test]
    fn a_diverged_session_model_renders_a_selectable_promote_leaf_naming_both_values() {
        let mut state = AppState::new(AgentId::new());
        state.default_role_snapshot = "coder".to_string();
        state.default_model_snapshot = Some("local/qwen3.8:27b-mlx".to_string());
        state.focused_model = Some("ollama_cloud/glm-5.2".to_string());

        let rows = build_tree(&state).rows();
        let promote_row = rows
            .iter()
            .find(|r| r.label.contains("Enter to make it"))
            .expect("a diverged session model must render a promote row");
        assert!(
            promote_row.label.contains("ollama_cloud/glm-5.2"),
            "must name the session's own model: {}",
            promote_row.label
        );
        assert_eq!(
            promote_row.kind,
            menu::MenuRowKind::Leaf {
                id: LEAF_PROMOTE_SESSION_MODEL.to_string()
            },
            "{promote_row:?}"
        );
        // The static "default model" row is UNCHANGED and still names its
        // own (still current) value -- the two rows together are what
        // make the session/persistent distinction legible, never merged
        // into one.
        assert!(
            rows.iter().any(|r| r.label.contains("default model")
                && r.label.contains("local/qwen3.8:27b-mlx")
                && r.kind == menu::MenuRowKind::Static),
            "{rows:?}"
        );
    }

    /// The session's model already matches the persistent default: no
    /// promote row at all -- there is nothing to promote, and an always-
    /// present row would be a no-op action pretending to be a decision.
    #[test]
    fn a_session_model_matching_the_default_renders_no_promote_leaf() {
        let mut state = AppState::new(AgentId::new());
        state.default_role_snapshot = "coder".to_string();
        state.default_model_snapshot = Some("anthropic/claude-sonnet-4-6".to_string());
        state.focused_model = Some("anthropic/claude-sonnet-4-6".to_string());

        let rows = build_tree(&state).rows();
        assert!(
            !rows
                .iter()
                .any(|r| matches!(&r.kind, menu::MenuRowKind::Leaf { id } if id == LEAF_PROMOTE_SESSION_MODEL)),
            "{rows:?}"
        );
    }

    /// No `ModelDecision` has arrived for this session yet
    /// (`focused_model` is `None`, `AppState::new`'s own default): no
    /// promote row -- there is no session choice to name yet.
    #[test]
    fn no_focused_model_renders_no_promote_leaf() {
        let mut state = AppState::new(AgentId::new());
        state.default_role_snapshot = "coder".to_string();
        state.default_model_snapshot = Some("anthropic/claude-sonnet-4-6".to_string());
        assert!(state.focused_model.is_none());

        let rows = build_tree(&state).rows();
        assert!(
            !rows
                .iter()
                .any(|r| matches!(&r.kind, menu::MenuRowKind::Leaf { id } if id == LEAF_PROMOTE_SESSION_MODEL)),
            "{rows:?}"
        );
    }

    #[test]
    fn draw_never_panics_with_a_diverged_session_model() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.default_role_snapshot = "coder".to_string();
        state.default_model_snapshot = Some("local/qwen3.8:27b-mlx".to_string());
        state.focused_model = Some("ollama_cloud/glm-5.2".to_string());
        for (w, h) in [(80u16, 1u16), (80, 2), (1, 24), (0, 0)] {
            let backend = TestBackend::new(w.max(1), h.max(1));
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| draw(f, f.area(), &state, &Theme::default()))
                .unwrap_or_else(|e| panic!("panicked/errored at {w}x{h}: {e}"));
        }
    }

    /// The default-role row IS a settable leaf, addressed by
    /// `LEAF_DEFAULT_ROLE` -- `input::activate_settings_selection`
    /// resolves `Enter` on it to `Action::CycleDefaultRole`.
    #[test]
    fn default_role_row_is_a_leaf_addressed_by_its_stable_id() {
        let mut state = AppState::new(AgentId::new());
        state.default_role_snapshot = "coder".to_string();

        let rows = build_tree(&state).rows();
        let role_row = rows
            .iter()
            .find(|r| r.label.contains("default role"))
            .expect("default-role row must exist");
        assert_eq!(
            role_row.kind,
            menu::MenuRowKind::Leaf {
                id: LEAF_DEFAULT_ROLE.to_string()
            },
            "{role_row:?}"
        );
    }

    /// The freshly-built tree reflects the CURRENT snapshot, not a stale
    /// one -- same "fresh tree every call" contract every other row in
    /// this menu already follows.
    #[test]
    fn defaults_rows_reflect_a_changed_snapshot_on_the_next_build() {
        let mut state = AppState::new(AgentId::new());
        state.default_role_snapshot = "coder".to_string();
        state.default_model_snapshot = Some("anthropic/claude-sonnet-4-6".to_string());
        let before = build_tree(&state).rows();
        assert!(before
            .iter()
            .any(|r| r.label.contains("coder") && r.label.contains("default role")));

        state.default_role_snapshot = "reviewer".to_string();
        state.default_model_snapshot = Some("kimi/k3".to_string());
        let after = build_tree(&state).rows();
        assert!(
            after
                .iter()
                .any(|r| r.label.contains("reviewer") && r.label.contains("default role")),
            "{after:?}"
        );
        assert!(
            after
                .iter()
                .any(|r| r.label.contains("kimi/k3") && r.label.contains("default model")),
            "{after:?}"
        );
    }

    #[test]
    fn draw_never_panics_with_defaults_populated() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.default_role_snapshot = "coder".to_string();
        state.default_model_snapshot = Some("anthropic/claude-sonnet-4-6".to_string());
        state.known_role_names = vec!["coder".to_string(), "reviewer".to_string()];
        for (w, h) in [(80u16, 1u16), (80, 2), (1, 24), (0, 0)] {
            let backend = TestBackend::new(w.max(1), h.max(1));
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal
                .draw(|f| draw(f, f.area(), &state, &Theme::default()))
                .unwrap_or_else(|e| panic!("panicked/errored at {w}x{h}: {e}"));
        }
    }
}
