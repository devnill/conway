//! Key handling: translates a `crossterm::event::KeyEvent` into an
//! [`Action`] the app loop (`app.rs`) carries out. Pure with respect to the
//! input line itself (`AppState::input`/`AppState::cursor` are mutated here
//! directly, since that's local editing state with no async effect), but
//! never calls `SessionHandle`/`Conway` -- every side-effecting action is
//! returned to the caller instead.
//!
//! `AppState::cursor` is a *char* index into `input`, not a byte offset --
//! `byte_index` converts to the corresponding byte offset (always on a
//! char boundary, since it walks `char_indices`) right before any `String`
//! mutation, so multi-byte UTF-8 input can never split a character.

use conway::{AgentId, PermissionDecision, PermissionScope};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::state::{AppState, AskFate, IntentChoice, Mode, TrustDecision};

/// What a keypress means for the app loop to carry out.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Nothing to do beyond whatever local `AppState` edit already happened.
    None,
    /// The input line was submitted (`Enter`) -- `SessionHandle::prompt`
    /// text, or (if it starts with `/`) a slash command's raw input. Which
    /// one it is is not this module's concern (module notes: "the dispatch
    /// hook is defined here, handlers land elsewhere") -- the app loop
    /// branches on the leading `/`.
    Submit(String),
    /// A permission prompt was answered.
    PermissionDecision(PermissionDecision),
    /// `Ctrl-C`: cancel the running turn (first press) or exit 130 (second
    /// press within 2s) -- the app loop owns the timing, this just signals
    /// "a Ctrl-C happened".
    CtrlC,
    /// `Ctrl-D` on an empty input line: exit 0.
    Quit,
    ScrollUp,
    ScrollDown,
    /// V3: a ONE-LINE transcript scroll, distinct from the page-sized
    /// [`Action::ScrollUp`]/[`Action::ScrollDown`]. Bound to bare
    /// `Up`/`Down`, which is what a terminal's *alternate scroll* mode
    /// (DECSET 1007) translates wheel events into while the alternate
    /// screen is active -- so this is the action a two-finger scroll
    /// actually produces.
    ScrollLineUp,
    ScrollLineDown,
    /// V2b: the operator accepted the offered pattern grant from the
    /// permission prompt. The app loop installs it, persists it
    /// best-effort (session scope only -- a per-agent/subtree grant names
    /// live agent ids, meaningless in a file read at the next launch), and
    /// resolves the pending prompt as an allow. The carried
    /// [`PermissionScope`] is the one the prompt's `s` key had cycled to --
    /// never hardcoded, so a narrowed grant the operator asked for is the
    /// grant the broker records.
    GrantPermissionPattern(conway::PatternRule, PermissionScope),
    /// The structured-argument counterpart to [`Action::GrantPermissionPattern`]:
    /// a `When::ArgsMatch` allow rule built by the `[p]` field editor
    /// (`Mode::EditingPattern`) from the fields the operator pinned. The
    /// carried [`PermissionScope`] is `state.permission_grant_scope` -- the
    /// one the prompt's `s` key had cycled to, the same source
    /// [`Action::GrantPermissionPattern`] reads. The app loop installs it via
    /// `Conway::grant_permission_rule` and resolves THIS call as `AllowOnce`
    /// (the grant covers FUTURE calls; this one is decided separately), so a
    /// pinned-field grant the operator asked for is the grant the broker
    /// records -- never a hardcoded scope or an all-wildcard default.
    GrantPermissionRule(conway::Rule, PermissionScope),
    /// V2b: cycle prompt -> plan -> AUTO-ALLOW. The app loop writes the
    /// broker (the authority) and refreshes the display mirror together.
    CyclePermissionMode,
    /// V2b: drop every pattern grant and cached allow-always.
    RevokePermissionGrants,
    /// Revoke exactly ONE pattern
    /// grant, carrying the same `(rule, origin)` identity the settings row
    /// itself rendered -- never a bare index, so the app loop's call into
    /// `Conway::revoke_permission_pattern` can never address a different
    /// grant than the one the operator actually selected.
    RevokePermissionPattern(conway::PatternRule, conway::PatternOrigin),
    /// A2: revoke exactly ONE STRUCTURED allow rule, carrying
    /// the same `(rule, origin)` identity the settings row itself rendered
    /// (from `state.structured_allow_rules`) -- the Rule-identity
    /// counterpart to [`Action::RevokePermissionPattern`], which can never
    /// name a structured rule (its `PatternRule` key collapses every
    /// structured rule to `None` at the broker). The grant scope rides in
    /// the mirror for display only; the revoke key is `(rule, origin)`,
    /// exactly as the flat path's is.
    RevokeStructuredAllowRule(conway::Rule, conway::PatternOrigin, conway::GrantScope),
    /// Revoke exactly ONE hook-backed
    /// rule, carrying the same `(event, id)` identity the settings row
    /// itself rendered -- never a bare index, so the app loop's call into
    /// `Conway::revoke_hook_rule` can never address a different rule than
    /// the one the operator actually selected.
    RevokeHookRule(String, String),
    /// Board item `01M0KARX71A64NTSYTDBVANVPF`: `Enter` on a plugin's own
    /// toggle leaf in `/settings`' plugins section -- carries the
    /// plugin's own manifest id (never an index; the row that produced
    /// this action is keyed by id, see `view::settings::
    /// LEAF_TOGGLE_PLUGIN_PREFIX`'s own doc) and the DESIRED new
    /// `installed` state, resolved from `AppState::plugin_browser`'s
    /// current mirror in the SAME call that built the tree this row came
    /// from -- mirroring `RevokePermissionPattern`'s own "resolved
    /// against the mirror in the same call" shape, so the app loop never
    /// has to re-derive "on or off" from state that may have changed by
    /// the time it acts.
    TogglePlugin(String, bool),
    /// `End` (T6): snap the transcript straight to its own tail --
    /// re-engages `follow_tail`. Fires only while the input line is empty
    /// (mirroring the dual-meaning precedent `Enter`'s empty-input arm
    /// already sets for this same key range -- see `handle_normal_key`'s own
    /// doc on `Home`/`End`); with text typed, `End` keeps its ordinary
    /// cursor-to-end-of-line meaning, since a jump key has no work to do
    /// while the user is mid-edit and stealing the key would break basic
    /// line editing.
    JumpToTail,
    /// `Home`'s counterpart to [`Action::JumpToTail`] (T6): jump the
    /// transcript straight to its own top. Same empty-input gating.
    JumpToTop,
    /// Switch the transcript pane to `AgentId`'s own conversation.
    /// Emitted by `Enter` on the `/agents` panel's highlighted row (any
    /// row, including the root's own -- focusing the root row is one of
    /// the two documented ways back), and by `Esc` while focused on a
    /// non-root agent (the other way back, per the item's own doc). The
    /// app loop (out of this module's scope) both mutates
    /// `AppState::focus_agent` and re-subscribes `handle.agent_events`.
    FocusAgent(AgentId),
    /// B5: a fate key was pressed while the `/ask` modal was open (`f` fork
    /// / `p` pull-in / `Esc` discard). The app loop runs the corresponding
    /// facade op (`commands::apply_ask_fate`); this module only reports
    /// which fate was chosen.
    AskFate(AskFate),
    /// C2: a choice key was pressed while the NL intent confirmation card
    /// was open (`Enter` confirm / `e` edit / `Esc` manual). The app loop
    /// runs `commands::execute_intent_confirm`; this module only reports
    /// which choice was chosen (and, for `Edit`, has already dropped the
    /// classified prompt into `state.input` and closed the card via
    /// `AppState::begin_intent_confirm_edit`).
    IntentConfirm(IntentChoice),
    /// A decision key was pressed while the trust-preview card was open
    /// (`y`/`Enter` confirm / `n`/`Esc` cancel). The app loop runs
    /// `commands::apply_trust_decision`; this module only reports which
    /// decision was made.
    TrustDecision(TrustDecision),
}

/// Routes a keypress based on `state.mode`, mutating `state.input`/`cursor`
/// directly for plain editing and returning an [`Action`] for anything the
/// app loop must act on.
///
/// T7: the `/help` overlay is checked FIRST, ahead of the `mode` match --
/// but only while `mode` is `Normal` (`state.help_open` is a plain flag, not
/// a `Mode` variant; see that field's own doc for why). This ordering is
/// what keeps the overlay from ever swallowing a key meant for an active
/// permission prompt / `/ask` modal / intent-confirm card: those can only
/// arrive while `mode` is already something other than `Normal`, so the
/// guard here simply never fires for them, and `handle_key` falls through
/// to the ordinary `mode` match exactly as it did before this item.
pub fn handle_key(state: &mut AppState, key: KeyEvent) -> Action {
    // Board item `01M0RWFH6V709B7WTAFRZGFKG3`: while an `/ask` is in
    // flight, `Ctrl-C` reaches `Action::CtrlC` (-> `App::handle_ctrl_c` ->
    // `App::abandon_ask`) no matter which surface is currently showing --
    // in particular `Mode::AwaitingPermission`, whose own key handler below
    // (`handle_permission_key`) swallows every chorded key including this
    // one (`if !key.modifiers.is_empty() { return Action::None; }`), which
    // is exactly the surface an ask waiting on a tool permission decision
    // is showing. Without this, an ask stuck on a prompt the operator does
    // not want to answer had no reachable abandon key at all: `Esc` there
    // is already `DenyWithFeedback` (a real, different action -- denies
    // JUST that call and lets the ask continue), not an abandon.
    // Checked before every other guard/dispatch in this function, mirroring
    // how the quit keys are documented to "pass through" every other
    // modal-bearing surface (`Mode`'s own doc) -- this is the same rule,
    // widened to the one in-flight case that predates any `Mode` variant
    // existing for it at all.
    if state.ask_in_flight
        && key.modifiers == KeyModifiers::CONTROL
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
    {
        return Action::CtrlC;
    }
    // V4: the `/settings` menu is checked the same way `/help` is just
    // below -- informational, gated on `Mode::Normal`, never a `Mode`
    // variant (see `AppState::settings_open`'s own doc) -- and mutually
    // exclusive with `/help` by construction (`AppState::open_settings`/
    // `open_help` each clear the other), so at most one of these two guards
    // ever actually fires.
    if state.settings_open && matches!(state.mode, Mode::Normal) {
        return handle_settings_key(state, key);
    }
    if state.help_open && matches!(state.mode, Mode::Normal) {
        return handle_help_key(state, key);
    }
    match &state.mode {
        Mode::AwaitingPermission(_) => handle_permission_key(state, key),
        Mode::AskModal(_) => handle_ask_modal_key(state, key),
        Mode::IntentConfirm(_) => handle_intent_confirm_key(state, key),
        Mode::TrustPreview(_) => handle_trust_preview_key(state, key),
        Mode::EditingPattern(_) => handle_editing_pattern_key(state, key),
        Mode::Normal => handle_normal_key(state, key),
    }
}

/// The `/help` overlay's key handling (T7): read-only, so almost everything
/// is SWALLOWED -- `Esc` is the only way to close it (module spec: "No
/// hotkey for help ... Esc closes it"). Mirrors
/// [`handle_ask_modal_key`]/[`handle_intent_confirm_key`]'s shape: the quit
/// keys (`Ctrl-C`/`Ctrl-D`) still pass through as `Action::CtrlC`/
/// `Action::Quit` -- there is no live resource to purge first (unlike the
/// `/ask` modal's child), so quitting with the overlay open needs no special
/// handling at all beyond letting the keys through.
fn handle_help_key(state: &mut AppState, key: KeyEvent) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('C') => return Action::CtrlC,
            KeyCode::Char('d') | KeyCode::Char('D') => return Action::Quit,
            _ => {}
        }
    }
    match key.code {
        KeyCode::Esc => state.close_help(),
        // V1: the overlay now scrolls past its capped height instead of
        // clipping (`view/help.rs`'s own doc) -- mirrors
        // [`handle_permission_key`]'s `PageUp`/`PageDown` handling exactly,
        // sharing `AppState::modal_scroll` with every other modal-bearing
        // surface.
        KeyCode::PageDown => adjust_modal_scroll(state, 1),
        KeyCode::PageUp => adjust_modal_scroll(state, -1),
        _ => {}
    }
    Action::None
}

/// The `/settings` menu's key handling (V4): mirrors [`handle_help_key`]'s
/// shape exactly (informational, not decision-owed -- see
/// `AppState::settings_open`'s own doc), with its own navigation layered on
/// top. `Up`/`Down` move the cursor, `Enter` toggles a boolean leaf or
/// expands/collapses a group, `Left`/`Right` step the one numeric leaf
/// (`tool_preview_lines`), and `Esc` closes -- everything else is
/// SWALLOWED, exactly like the other three `handle_*_key` fns. Owns
/// `Up`/`Down` COMPLETELY while open (never falls through to
/// `handle_normal_key`'s palette/agent-panel/wheel-scroll chain -- see
/// `view/menu.rs`'s own module doc, "How this slots into V3's arrow-key
/// priority chain") and releases them the instant it closes, the same trade
/// the agent panel already makes for its own arrows.
fn handle_settings_key(state: &mut AppState, key: KeyEvent) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('C') => return Action::CtrlC,
            KeyCode::Char('d') | KeyCode::Char('D') => return Action::Quit,
            _ => {}
        }
    }
    match key.code {
        KeyCode::Esc => state.close_settings(),
        KeyCode::Up => settings_move_selection(state, -1),
        KeyCode::Down => settings_move_selection(state, 1),
        // V2b: Enter may now yield an action (permission mode / revoke),
        // which the app loop applies against the broker.
        KeyCode::Enter => {
            if let Some(action) = activate_settings_selection(state) {
                return action;
            }
        }
        KeyCode::Left => step_settings_numeric(state, -1),
        KeyCode::Right => step_settings_numeric(state, 1),
        _ => {}
    }
    Action::None
}

/// `Up`/`Down` on the settings menu (V4): rebuilds the current tree (see
/// `view/settings.rs::build_tree`'s own doc for why it is rebuilt fresh
/// rather than kept as a long-lived value), moves the selection, and writes
/// the resulting (already-clamped) index back onto
/// `AppState::settings_selected` -- the one piece of navigation state that
/// persists across calls.
fn settings_move_selection(state: &mut AppState, delta: isize) {
    let mut menu = super::view::settings::build_tree(state);
    menu.move_selection(delta);
    state.settings_selected = menu.selected_index();
}

/// `Enter` on the settings menu (V4): toggles a boolean leaf, expands/
/// collapses a group, or is a no-op on the one numeric leaf (which has its
/// own `Left`/`Right` stepper -- see [`step_settings_numeric`]) -- mirrors
/// how `handle_normal_key`'s Enter-on-the-`/agents`-panel resolves through
/// the filtered row list (`view/menu.rs::MenuState::selected_leaf_id`'s own
/// doc: "the caller's activate action").
fn activate_settings_selection(state: &mut AppState) -> Option<Action> {
    let mut menu = super::view::settings::build_tree(state);
    let row = menu.selected_row()?;
    match row.kind {
        // A read-only row (the deny/prompt review lists' entries) is never
        // the selection -- `MenuState::selected_index` resolves off it --
        // and has nothing to activate even defensively.
        super::view::menu::MenuRowKind::Static => {}
        super::view::menu::MenuRowKind::Group { .. } => {
            // Reuses `MenuState::toggle_group_at_selection` itself (the
            // primitive's own flip + re-clamp) rather than recomputing an
            // equivalent expand/collapse decision independently -- this
            // `menu` is a throwaway, freshly built copy (see
            // `view/settings.rs::build_tree`'s own doc), so its OWN
            // `expanded` flag can't persist on its own; what persists is
            // `AppState::settings_collapsed_groups`, read back from the
            // SAME row (looked up by its stable label, since toggling
            // never changes descendant ORDER) right after the toggle.
            let label = row.label.clone();
            menu.toggle_group_at_selection();
            let now_expanded = menu.rows().iter().any(|r| {
                r.label == label
                    && matches!(
                        r.kind,
                        super::view::menu::MenuRowKind::Group { expanded: true }
                    )
            });
            if now_expanded {
                state.settings_collapsed_groups.remove(&label);
            } else {
                state.settings_collapsed_groups.insert(label);
            }
            state.settings_selected = menu.selected_index();
        }
        super::view::menu::MenuRowKind::Leaf { id } => {
            if id == super::view::settings::LEAF_SHOW_REASONING {
                state.toggle_thinking();
            } else if id == super::view::settings::LEAF_SHOW_TIMESTAMPS {
                state.toggle_timestamps();
            } else if id == super::view::settings::LEAF_PERMISSION_MODE {
                // V2b: cycles the DISPLAY mirror only. The app loop sees
                // the returned action and writes the broker, which is the
                // authority -- doing it here would leave the two able to
                // disagree, and the broker's answer is the one that gates
                // actual calls.
                return Some(Action::CyclePermissionMode);
            } else if id == super::view::settings::LEAF_REVOKE_GRANTS {
                return Some(Action::RevokePermissionGrants);
            } else if let Some(idx) = id
                .strip_prefix(super::view::settings::LEAF_REVOKE_GRANT_PREFIX)
                .and_then(|rest| rest.parse::<usize>().ok())
            {
                // resolved against
                // `state.permission_grants` right here, in the SAME call
                // that just built the tree this row came from -- so the
                // index cannot have drifted (nothing mutates
                // `permission_grants` between the two). The `Action`
                // itself carries the resolved `(rule, origin)` value, not
                // the index, so nothing downstream needs to re-derive it.
                if let Some((rule, origin)) = state.permission_grants.get(idx) {
                    return Some(Action::RevokePermissionPattern(
                        rule.clone(),
                        origin.clone(),
                    ));
                }
            } else if let Some(idx) = id
                .strip_prefix(super::view::settings::LEAF_REVOKE_STRUCTURED_ALLOW_PREFIX)
                .and_then(|rest| rest.parse::<usize>().ok())
            {
                // A2: the structured-allow counterpart of the arm
                // above, resolved against `state.structured_allow_rules` in
                // the SAME call that built this tree -- the two prefixes
                // name disjoint id spaces, so a structured row can never
                // resolve into the flat mirror (or vice versa).
                if let Some((rule, origin, scope)) = state.structured_allow_rules.get(idx) {
                    return Some(Action::RevokeStructuredAllowRule(
                        rule.clone(),
                        origin.clone(),
                        *scope,
                    ));
                }
            } else if let Some(idx) = id
                .strip_prefix(super::view::settings::LEAF_REVOKE_HOOK_PREFIX)
                .and_then(|rest| rest.parse::<usize>().ok())
            {
                // resolved against
                // `state.hook_rules` in the SAME call that built this
                // tree, exactly like the two grant prefixes above -- the
                // index cannot have drifted.
                if let Some(rule) = state.hook_rules.get(idx) {
                    return Some(Action::RevokeHookRule(rule.event.clone(), rule.id.clone()));
                }
            } else if let Some(plugin_id) =
                id.strip_prefix(super::view::settings::LEAF_TOGGLE_PLUGIN_PREFIX)
            {
                // resolved against
                // `state.plugin_browser` in the SAME call that just built
                // the tree this row came from -- the desired state is the
                // OPPOSITE of whatever this entry's mirror currently says,
                // never re-derived downstream from a possibly-stale read.
                if let Some(entry) = state
                    .plugin_browser
                    .iter()
                    .find(|entry| entry.id == plugin_id)
                {
                    return Some(Action::TogglePlugin(
                        plugin_id.to_string(),
                        !entry.installed,
                    ));
                }
            }
            // `LEAF_TOOL_PREVIEW_LINES`: Enter has nothing to activate on
            // the numeric leaf -- it is adjusted with Left/Right instead
            // (see `step_settings_numeric`).
        }
    }
    None
}

/// `Left`/`Right` on the settings menu (V4): the numeric stepper for the
/// one non-boolean leaf (`tool_preview_lines`) -- a no-op on every other
/// row (there is nothing to step). See
/// `AppState::adjust_tool_preview_lines`'s own doc for why a continuous
/// step, not a cycled preset list.
fn step_settings_numeric(state: &mut AppState, delta: i32) {
    let menu = super::view::settings::build_tree(state);
    let is_numeric_leaf =
        menu.selected_leaf_id().as_deref() == Some(super::view::settings::LEAF_TOOL_PREVIEW_LINES);
    if is_numeric_leaf {
        state.adjust_tool_preview_lines(delta);
    }
}

/// The `/ask` modal's key handling (B5): exactly three ways out, each
/// forcing one fate -- `f` (fork/keep), `p` (pull in), `Esc` (discard).
/// Everything else is SWALLOWED: the input line is inert (no typing, no
/// palette, no scrolling), and the `/agents` panel is neither visible nor
/// available while the modal is open (user decision, binding) -- the panel
/// toggle (a typed `/agents` command) can never be entered here. The quit
/// keys (`Ctrl-C`/`Ctrl-D`) still pass through as `Action::CtrlC`/
/// `Action::Quit` -- quitting with the modal open purges the child first
/// (wired in `app.rs`'s quit path), so there is no fourth, fate-less way
/// out.
fn handle_ask_modal_key(state: &mut AppState, key: KeyEvent) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('C') => return Action::CtrlC,
            KeyCode::Char('d') | KeyCode::Char('D') => return Action::Quit,
            _ => {}
        }
    }
    // V1: the modal now scrolls past its capped height (`view/mod.rs::
    // draw_ask_modal`'s own doc) -- checked before the fate keys' bare-
    // keypress guard below since `PageUp`/`PageDown` carry no modifiers of
    // their own and are not fate keys either way.
    match key.code {
        KeyCode::PageDown => {
            adjust_modal_scroll(state, 1);
            return Action::None;
        }
        KeyCode::PageUp => {
            adjust_modal_scroll(state, -1);
            return Action::None;
        }
        _ => {}
    }
    // Fate keys only fire on a bare keypress -- a modifier held (Ctrl-F,
    // Alt-P, ...) is NOT a fate. Without this guard the second match below
    // inspected `key.code` alone, so Ctrl-F/Ctrl-P leaked through as
    // fork/pull-in while the user was reaching for some other binding.
    if !key.modifiers.is_empty() {
        return Action::None;
    }
    match key.code {
        KeyCode::Char('f') | KeyCode::Char('F') => Action::AskFate(AskFate::Fork),
        KeyCode::Char('p') | KeyCode::Char('P') => Action::AskFate(AskFate::PullIn),
        KeyCode::Esc => Action::AskFate(AskFate::Discard),
        _ => Action::None,
    }
}

/// The NL intent confirmation card's key handling (C2): exactly three ways
/// out -- `Enter` (confirm), `e` (edit), `Esc` (manual fallback). Everything
/// else is SWALLOWED: the input line is inert (no typing, no palette, no
/// scrolling), and `/agents` is neither visible nor available while the card
/// is open -- the panel toggle (a typed `/agents` command) can never be
/// entered here. The quit keys (`Ctrl-C`/`Ctrl-D`) still pass through as
/// `Action::CtrlC`/`Action::Quit` -- quitting with the card open IS the
/// manual fallback (nothing has been created yet, so there is nothing to
/// purge, unlike the `/ask` modal), and the app loop never reaches
/// `execute_intent_confirm` for them.
///
/// `e` (edit) fires only on a bare keypress -- a modifier held (Ctrl-E,
/// Alt-E, ...) is NOT the edit choice, mirroring B5's M2 fix for the fate
/// keys. `Enter` and `Esc` are not modifier-sensitive in crossterm's key
/// model (a bare Enter/Esc is what the user types). On `e`, the classified
/// `intent.prompt` is dropped into `state.input` (replacing whatever was
/// there) and the card closes via `AppState::begin_intent_confirm_edit`,
/// then `Action::IntentConfirm(IntentChoice::Edit)` is returned -- the app
/// loop's arm for it is a no-op, the state mutation already happened here.
fn handle_intent_confirm_key(state: &mut AppState, key: KeyEvent) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('C') => return Action::CtrlC,
            KeyCode::Char('d') | KeyCode::Char('D') => return Action::Quit,
            _ => {}
        }
    }
    // V1: the card now scrolls past its capped height (`view/mod.rs::
    // draw_intent_confirm`'s own doc) -- checked before the choice keys'
    // bare-keypress guard below, mirroring [`handle_ask_modal_key`].
    match key.code {
        KeyCode::PageDown => {
            adjust_modal_scroll(state, 1);
            return Action::None;
        }
        KeyCode::PageUp => {
            adjust_modal_scroll(state, -1);
            return Action::None;
        }
        _ => {}
    }
    // `e` only fires on a bare keypress -- a modifier held (Ctrl-E, Alt-E,
    // ...) is NOT the edit choice. Without this guard the second match
    // below inspected `key.code` alone, so Ctrl-E would leak through as
    // edit while the user was reaching for some other binding (B5's M2
    // fix, applied to the same shape here).
    if !key.modifiers.is_empty() {
        return Action::None;
    }
    match key.code {
        KeyCode::Enter => Action::IntentConfirm(IntentChoice::Confirm),
        KeyCode::Char('e') | KeyCode::Char('E') => {
            state.begin_intent_confirm_edit();
            Action::IntentConfirm(IntentChoice::Edit)
        }
        KeyCode::Esc => Action::IntentConfirm(IntentChoice::Manual),
        _ => Action::None,
    }
}

/// The trust-preview card's key handling (board item, split from
/// `01KZHVFCN6ZEAXV7K5JHRQN1YB`): exactly two ways out -- `y`/`Enter`
/// (confirm) and `n`/`Esc` (cancel). Everything else is SWALLOWED, mirroring
/// [`handle_ask_modal_key`]/[`handle_intent_confirm_key`]'s shape exactly:
/// the input line is inert, `/agents` is neither visible nor available, and
/// the quit keys (`Ctrl-C`/`Ctrl-D`) still pass through as `Action::CtrlC`/
/// `Action::Quit` -- quitting with the card open IS the cancel outcome
/// (nothing has been created or written yet, so there is nothing to purge
/// or undo; `app.rs`'s quit path drops a parked card on the floor the same
/// way it does the intent-confirm card).
///
/// The card scrolls past its capped height exactly like the other three
/// (`view/mod.rs::draw_trust_preview`'s own doc) -- checked before the
/// decision keys' bare-keypress guard, mirroring every other modal-bearing
/// surface's key handler here.
///
/// `y`/`n` only fire on a bare keypress -- a modifier held (Ctrl-Y, Alt-N,
/// ...) is NOT a decision, the same B5 M2 guard every other modal-bearing
/// surface's key handler applies.
fn handle_trust_preview_key(state: &mut AppState, key: KeyEvent) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('C') => return Action::CtrlC,
            KeyCode::Char('d') | KeyCode::Char('D') => return Action::Quit,
            _ => {}
        }
    }
    match key.code {
        KeyCode::PageDown => {
            adjust_modal_scroll(state, 1);
            return Action::None;
        }
        KeyCode::PageUp => {
            adjust_modal_scroll(state, -1);
            return Action::None;
        }
        _ => {}
    }
    if !key.modifiers.is_empty() {
        return Action::None;
    }
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
            Action::TrustDecision(TrustDecision::Confirm)
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            Action::TrustDecision(TrustDecision::Cancel)
        }
        _ => Action::None,
    }
}

/// The `[p]` field editor's key handling (`Mode::EditingPattern`). Mirrors
/// [`handle_intent_confirm_key`]'s shape: `Ctrl-C`/`Ctrl-D` pass through as
/// quit (no live resource to purge -- the prompt is parked in
/// [`EditingPatternState`] and restored on cancel, but a hard quit drops
/// the whole session anyway), `PageUp`/`PageDown` scroll the field list, and
/// every other key is bare-only (a held modifier is NOT a field action --
/// same M2-shape guard as the other modals). The field keys: `Up`/`Down`/`
/// `Tab` move the selected row, `Space` toggles a field between pinned
/// (match this exact value) and wildcard (match any value), `s` cycles the
/// grant scope (the shared `permission_grant_scope`, same as the prompt's
/// `s`), `Enter` builds the `When::ArgsMatch` rule from the pinned fields
/// and returns [`Action::GrantPermissionRule`] (submitting also restores
/// `AwaitingPermission` so the dispatch arm can resolve THIS call via the
/// existing `resolve_current_prompt` path), and `Esc` cancels back to the
/// prompt.
fn handle_editing_pattern_key(state: &mut AppState, key: KeyEvent) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('C') => return Action::CtrlC,
            KeyCode::Char('d') | KeyCode::Char('D') => return Action::Quit,
            _ => {}
        }
    }
    match key.code {
        KeyCode::PageDown => {
            adjust_modal_scroll(state, 1);
            return Action::None;
        }
        KeyCode::PageUp => {
            adjust_modal_scroll(state, -1);
            return Action::None;
        }
        _ => {}
    }
    // Every field key fires only on a BARE keypress -- a held modifier is
    // NOT a field action (same guard as the other modals' decision keys).
    if !key.modifiers.is_empty() {
        return Action::None;
    }
    if let Mode::EditingPattern(ed) = &mut state.mode {
        match key.code {
            KeyCode::Up => {
                if ed.cursor > 0 {
                    ed.cursor -= 1;
                }
                Action::None
            }
            KeyCode::Down | KeyCode::Tab => {
                if ed.cursor + 1 < ed.fields.len() {
                    ed.cursor += 1;
                }
                Action::None
            }
            // Toggle the selected field's pinned state. A pinned field must
            // match its value exactly; an unpinned one is wildcard. Every
            // field starts wildcard, so the all-wildcard default IS today's
            // `tool:*` grant -- the operator pins to narrow from there.
            KeyCode::Char(' ') => {
                if let Some(f) = ed.fields.get_mut(ed.cursor) {
                    f.pinned = !f.pinned;
                }
                Action::None
            }
            // Cycle the grant scope the prompt's `s` key cycles -- the edit
            // modal and the prompt share one scope source.
            KeyCode::Char('s') | KeyCode::Char('S') => {
                state.cycle_permission_grant_scope();
                Action::None
            }
            KeyCode::Enter => match state.submit_editing_pattern() {
                Some((rule, scope)) => Action::GrantPermissionRule(rule, scope),
                None => Action::None,
            },
            KeyCode::Esc => {
                state.cancel_editing_pattern();
                Action::None
            }
            _ => Action::None,
        }
    } else {
        Action::None
    }
}

/// The shared modal body scroll step (originated as the permission
/// overlay's own step, bug fix; V1 generalizes it
/// to every modal-bearing surface via [`adjust_modal_scroll`]):
/// `PageUp`/`PageDown` doesn't collide with any surface's own decision/fate/
/// choice keys, so it's free to drive `AppState::modal_scroll` without
/// touching decision handling at all. A generous fixed step (rather than
/// deriving one from the actual overlay viewport height, which this module
/// has no `Rect` for) -- each `view/mod.rs::draw_*`/`view/help.rs::draw`
/// clamps the value it's actually handed to its own content's wrapped line
/// count (`view/modal.rs::clamp_scroll`), so an over-large step here can
/// never scroll past real content; it just lands on the true bottom/top.
const MODAL_SCROLL_STEP: u16 = 5;

/// Steps `AppState::modal_scroll` by [`MODAL_SCROLL_STEP`] in `direction`'s
/// sign (positive: `PageDown`, further into the content; negative:
/// `PageUp`, back toward the top) -- the one place every modal-bearing
/// surface's `PageUp`/`PageDown` handling lands, so the step size and the
/// saturating add/sub can never drift apart across
/// [`handle_permission_key`]/[`handle_ask_modal_key`]/
/// [`handle_intent_confirm_key`]/[`handle_help_key`].
fn adjust_modal_scroll(state: &mut AppState, direction: i8) {
    state.modal_scroll = if direction >= 0 {
        state.modal_scroll.saturating_add(MODAL_SCROLL_STEP)
    } else {
        state.modal_scroll.saturating_sub(MODAL_SCROLL_STEP)
    };
}

fn handle_permission_key(state: &mut AppState, key: KeyEvent) -> Action {
    // Bug fix: a long command's argument
    // used to clip the decision keys off-screen with no way to see the
    // rest of it. `PageUp`/`PageDown` page the overlay's own command
    // body while the decision keys keep working exactly as below --
    // mutated directly here (like `handle_normal_key`'s plain-editing
    // keys) rather than via a new `Action` variant, since nothing here
    // needs a live facade call. These come before the modifier guard:
    // a chorded PageUp/PageDown is still a scroll, not a decision.
    match key.code {
        KeyCode::PageDown => {
            adjust_modal_scroll(state, 1);
            return Action::None;
        }
        KeyCode::PageUp => {
            adjust_modal_scroll(state, -1);
            return Action::None;
        }
        _ => {}
    }
    // The decision keys fire only on a BARE keypress -- a modifier held
    // (Ctrl-A, Ctrl-S, Alt-P, ...) is NOT a decision. Without this guard
    // the match below inspected `key.code` alone, so Ctrl-A leaked through
    // as `AllowAlways` and Ctrl-S silently cycled the grant scope (same
    // shape as B5's M2 fix in `handle_intent_confirm_key`; the scope line
    // on screen makes a mis-grant visible, but a decision that isn't the
    // operator's own keystroke is a defect regardless).
    if !key.modifiers.is_empty() {
        return Action::None;
    }
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            Action::PermissionDecision(PermissionDecision::AllowOnce)
        }
        KeyCode::Char('a') | KeyCode::Char('A') => {
            Action::PermissionDecision(PermissionDecision::AllowAlways {
                // The scope the prompt's `s` key cycled to -- `Session`
                // unless the operator deliberately narrowed it.
                scope: state.permission_grant_scope,
            })
        }
        // The remembered-grant scope key: cycles Session -> this agent ->
        // this agent's subtree, applying to BOTH remembered-grant keys
        // (`a` and `p`). The overlay states the current scope in words
        // next to the grant keys, and the choice resets to `Session` for
        // every new prompt (`AppState::offer_prompt`/
        // `promote_next_surface`), so a narrowing is always a deliberate,
        // per-prompt act -- never a sticky hidden mode.
        KeyCode::Char('s') | KeyCode::Char('S') => {
            state.cycle_permission_grant_scope();
            Action::None
        }
        // V2b: `p` opens the structured-argument field editor -- the
        // operator pins per-field to narrow (a pinned field must match
        // exactly; an unpinned one is wildcard), then `Enter` grants a
        // `When::ArgsMatch` rule covering FUTURE calls and resolves THIS
        // call as `AllowOnce`. Today's all-wildcard default preserves the
        // old `[p]`-then-grant = `tool:*` semantics; the operator narrows
        // from there. Offered only for `RenderKind::Structured` tools (where
        // `suggested_rule` returns `Some`): a shell command gets no offer
        // and this key does nothing, rather than granting a text prefix the
        // gate refuses to honor (board 01KZDDPC5MMD49F6JPV9CW4TVM) or
        // pretending a JSON dump is a prefix.
        KeyCode::Char('p') | KeyCode::Char('P') => match state.offered_permission_rule() {
            Some(_) => {
                state.offer_editing_pattern();
                Action::None
            }
            None => Action::None,
        },
        KeyCode::Char('n') | KeyCode::Char('N') => {
            Action::PermissionDecision(PermissionDecision::Deny {
                reason: "user denied".to_string(),
            })
        }
        KeyCode::Esc => Action::PermissionDecision(PermissionDecision::DenyWithFeedback {
            message: "user declined; try another approach".to_string(),
        }),
        _ => Action::None,
    }
}

fn handle_normal_key(state: &mut AppState, key: KeyEvent) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('C') => return Action::CtrlC,
            KeyCode::Char('d') | KeyCode::Char('D') if state.input.is_empty() => {
                return Action::Quit
            }
            KeyCode::Char('w') | KeyCode::Char('W') => {
                delete_word_before_cursor(state);
                state.sync_palette_stem();
                return Action::None;
            }
            // V3: history recall lives on `Ctrl-P`/`Ctrl-N` (the readline
            // pairing) because bare `Up`/`Down` had to go back to scrolling
            // -- a terminal's alternate-scroll mode turns wheel events into
            // cursor keys, so bare arrows are not exclusively a keyboard
            // signal. See the `KeyCode::Up` arm for the full reasoning.
            //
            // These are unconditional: unlike the bare arrows, a control
            // chord is unambiguously a keystroke, so there is no surface to
            // yield priority to and no multi-line interior to navigate
            // first.
            KeyCode::Char('p') | KeyCode::Char('P') => {
                state.history_recall_prev();
                state.sync_palette_stem();
                return Action::None;
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                state.history_recall_next();
                state.sync_palette_stem();
                return Action::None;
            }
            // T5: Ctrl-E expands/collapses ALL tool entries in the
            // transcript at once (MVP -- no per-entry selection). A control
            // key, not a bare `e` (D-keys: a bare `e` must stay ordinary
            // text input for the always-on input box). Pure state mutation
            // -- `AppState::toggle_all_tool_entries_expanded` flips
            // `expanded` on every `Entry::Tool` and leaves `scroll`/
            // `follow_tail` untouched (the next render's clamp re-clamps
            // without snapping the viewport). No `Action` variant needed,
            // mirroring the `v` visibility-filter key's direct-mutation
            // pattern. Note: this RECLAIMS Ctrl-E from the T3 hint's
            // advertised-but-never-wired "Ctrl-E submit" -- unmodified
            // Enter was and remains the actual submit key; T8 adds
            // Alt-Enter/Shift-Enter for inserting a literal newline
            // instead, which is a distinct binding, not a move of submit
            // itself. The hint segment is updated in `view/status.rs`.
            KeyCode::Char('e') | KeyCode::Char('E') => {
                state.toggle_all_tool_entries_expanded();
                return Action::None;
            }
            _ => {}
        }
    }

    match key.code {
        // T8: Alt-Enter AND Shift-Enter both insert `\n` -- deliberately
        // both, not just one: some terminals encode Shift-Enter as a plain
        // Enter (no distinguishable modifier ever reaches crossterm), so
        // relying on Shift alone would silently lose multi-line entry on
        // those terminals, and Alt is the more universally-passed-through
        // modifier. Checked before the empty-input/submit logic below so
        // neither modifier combination can ever reach it.
        KeyCode::Enter
            if key.modifiers.contains(KeyModifiers::ALT)
                || key.modifiers.contains(KeyModifiers::SHIFT) =>
        {
            insert_newline(state);
            Action::None
        }
        KeyCode::Enter => {
            if state.input.is_empty() {
                // with the agent panel open and nothing typed,
                // Enter focuses the highlighted row's agent instead of
                // being a pure no-op (its only prior behavior) -- the
                // input line has nothing else to submit in this state, so
                // this cannot collide with a real prompt/command.
                // Item A2: `agent_selected` indexes the panel's FILTERED
                // rows, so the row lookup must walk the same filtered list
                // the draw code renders (`visible_agent_nodes`), not the
                // raw `tree.nodes`.
                if state.agent_view_open {
                    let row_count = state.visible_agent_nodes().count();
                    if let Some(node) = state
                        .visible_agent_nodes()
                        .nth(state.agent_selected.min(row_count.saturating_sub(1)))
                    {
                        return Action::FocusAgent(node.agent_id);
                    }
                }
                return Action::None;
            }
            let text = std::mem::take(&mut state.input);
            state.cursor = 0;
            state.clear_palette();
            Action::Submit(text)
        }
        KeyCode::Backspace => {
            if state.cursor > 0 {
                let end = byte_index(&state.input, state.cursor);
                let start = byte_index(&state.input, state.cursor - 1);
                state.input.replace_range(start..end, "");
                state.cursor -= 1;
                state.sync_palette_stem();
            }
            Action::None
        }
        KeyCode::Left => {
            state.cursor = state.cursor.saturating_sub(1);
            Action::None
        }
        KeyCode::Right => {
            state.cursor = (state.cursor + 1).min(char_count(&state.input));
            Action::None
        }
        // T6: `Home`/`End` are the transcript's jump-to-top/jump-to-tail
        // keys -- but ONLY while the input line is empty. With text typed,
        // both keep their ordinary cursor-to-start/cursor-to-end-of-line
        // meaning (the pre-T6 behavior, still pinned by
        // `home_and_end_jump_cursor_to_bounds`): a jump key has nothing
        // useful to do while composing a message, and unconditionally
        // stealing `Home`/`End` would break ordinary line editing. This
        // mirrors `Enter`'s own empty-input dual meaning just above (submit
        // vs. focus the highlighted agent-panel row) -- the established
        // precedent in this same function for "the key means something else
        // once the input line has nothing to submit/edit".
        KeyCode::Home => {
            if state.input.is_empty() {
                Action::JumpToTop
            } else {
                state.cursor = 0;
                Action::None
            }
        }
        KeyCode::End => {
            if state.input.is_empty() {
                Action::JumpToTail
            } else {
                state.cursor = char_count(&state.input);
                Action::None
            }
        }
        // Bug 3: arrows drive the
        // on-demand surfaces, in priority order. The slash-command palette
        // takes priority when it is showing (the user is composing a
        // command); otherwise the arrows scroll the agent panel when it is
        // open; otherwise -- T8 -- a multi-line draft's own interior lines
        // take the key (so a multi-line draft stays navigable
        // line-by-line); otherwise -- V3 -- bare Up/Down scroll the
        // transcript one line.
        //
        // V3 moved history recall OFF bare Up/Down (T8 had put it here) and
        // onto `Ctrl-P`/`Ctrl-N`. The reason is not aesthetic: terminals
        // implement *alternate scroll* (DECSET 1007), which translates
        // wheel events into cursor-key presses while the alternate screen
        // is active. So a two-finger scroll arrives here as `Up`/`Down`,
        // indistinguishable from a keystroke -- and under T8 it recalled
        // history instead of scrolling, which is what the user hit.
        //
        // Conway cannot tell the two apart, because the information that
        // would distinguish them is exactly what `EnableMouseCapture`
        // provides -- and enabling that would disable the terminal's own
        // click-drag text selection, which the transcript's clean-copy
        // guarantee exists to protect (see `view/transcript.rs`, and
        //). Given that the two cannot
        // be separated, the binding goes to the interaction that is both
        // more frequent and more surprising when broken: scrolling.
        //
        // Because the palette and agent-panel checks run FIRST and return
        // before reaching the scroll fallback, neither surface loses its
        // arrows -- by construction, not a separate flag.
        KeyCode::Up => {
            if palette_navigate(state, -1) {
                Action::None
            } else if state.agent_view_open {
                state.agent_scroll(-1);
                Action::None
            } else if move_cursor_line(state, -1) {
                Action::None
            } else {
                Action::ScrollLineUp
            }
        }
        KeyCode::Down => {
            if palette_navigate(state, 1) {
                Action::None
            } else if state.agent_view_open {
                state.agent_scroll(1);
                Action::None
            } else if move_cursor_line(state, 1) {
                Action::None
            } else {
                Action::ScrollLineDown
            }
        }
        KeyCode::Esc => {
            // Esc does ONE thing per press, innermost surface first.
            //
            // made Esc close the agent panel; a separate change made
            // it return to the root's conversation. Both fired on a single
            // press, so the natural flow -- open `/agents`, focus a child,
            // press Esc to dismiss the panel -- closed the panel AND threw
            // the focus away in one keystroke, with no way to keep the
            // focus you had just chosen. The `/help` overlay only ever
            // documented the panel-close half, so the coupling was a bug
            // rather than an intended shortcut.
            //
            // Now: if the panel is open, Esc closes it and stops. A second
            // Esc returns to the root. That matches how Esc behaves
            // everywhere else in this TUI (dismiss the thing in front of
            // you), and it makes "focus a child, then get the panel out of
            // the way" expressible, which it previously was not.
            if state.agent_view_open {
                state.agent_view_open = false;
                return Action::None;
            }
            // Panel already closed: Esc is the way back to the root's own
            // conversation. A no-op when already on the root, so it does
            // not force an unnecessary transcript clear+replay.
            if !state.is_root_focused() {
                return Action::FocusAgent(state.root_agent());
            }
            Action::None
        }
        KeyCode::PageUp => Action::ScrollUp,
        KeyCode::PageDown => Action::ScrollDown,
        // Item A2: `v` cycles the /agents panel's draw-time visibility
        // filter (ActiveOnly -> All -> FinishedOnly -> ActiveOnly). Bound
        // only while the panel is open AND the input line is empty -- with
        // any text typed, `v` stays ordinary text input (the same gating
        // Enter's focus-row binding above uses), so the filter key can
        // never eat a character of a prompt being composed. No other key
        // in this branch uses `v`: Enter/Up/Down/Esc/PageUp/PageDown are
        // the panel's existing bindings and `Char(c)` was plain text input.
        KeyCode::Char('v') if state.agent_view_open && state.input.is_empty() => {
            state.cycle_agent_visibility();
            Action::None
        }
        KeyCode::Char(c) => {
            let idx = byte_index(&state.input, state.cursor);
            state.input.insert(idx, c);
            state.cursor += 1;
            state.sync_palette_stem();
            Action::None
        }
        _ => Action::None,
    }
}

/// Moves the slash-command palette selection by `delta` and autofills
/// `input` with the newly-highlighted command. Returns whether the
/// palette was active and thus consumed the key.
///
/// The candidate list is anchored to [`AppState::palette_source`] (the stem
/// the user typed), NOT to the live `input` -- so autofilling `input` with a
/// whole command on each arrow press does not shrink the list to that one
/// entry; cycling stays over the full set the stem matched. A `None`
/// selection means "not navigating yet": the first `Down` lands on the first
/// match, the first `Up` on the last; further presses wrap.
fn palette_navigate(state: &mut AppState, delta: isize) -> bool {
    let candidates = super::view::palette::matches(state.palette_source(), &state.plugin_commands);
    let len = candidates.len();
    if len == 0 {
        return false;
    }
    let next = match state.palette_selected {
        None => {
            if delta > 0 {
                0
            } else {
                len - 1
            }
        }
        Some(i) => (i.min(len - 1) as isize + delta).rem_euclid(len as isize) as usize,
    };
    state.palette_selected = Some(next);
    state.input = candidates[next].name.to_string();
    state.cursor = char_count(&state.input);
    true
}

fn char_count(s: &str) -> usize {
    s.chars().count()
}

/// The byte offset of the `char_idx`-th char in `s` (always a char
/// boundary), or `s.len()` if `char_idx` is at or past the end.
fn byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

/// Inserts a literal `\n` at the cursor (T8: Alt-Enter/Shift-Enter) -- the
/// same insert-at-cursor shape `KeyCode::Char(c)` uses in
/// [`handle_normal_key`], just with a fixed `'\n'` instead of the typed
/// char.
fn insert_newline(state: &mut AppState) {
    let idx = byte_index(&state.input, state.cursor);
    state.input.insert(idx, '\n');
    state.cursor += 1;
    state.sync_palette_stem();
}

/// Inserts a whole pasted block as ONE edit at the cursor (T8: bracketed
/// paste, `CEvent::Paste` -- wired from `app.rs`, not through
/// [`handle_key`], since a paste is not a `KeyEvent`). Swallowed while any
/// modal-bearing surface is showing (`Mode` other than `Normal`) -- the
/// input line is inert there exactly as it is for ordinary typing (see
/// `handle_ask_modal_key`/`handle_intent_confirm_key`'s own "input line is
/// inert" docs) -- and, T7, while the `/help` overlay is open, for the same
/// reason `handle_help_key` swallows ordinary keys.
///
/// One `String::insert_str` call, not a loop over `handle_key` per
/// character: looping would (a) fire `sync_palette_stem`/history-adjacent
/// side effects once per pasted character instead of once for the whole
/// paste, and (b) treat a pasted `\n` as if the user had pressed a
/// character key rather than as literal text, which is exactly the
/// char-by-char-arrival bug this item exists to fix.
pub fn handle_paste(state: &mut AppState, text: &str) {
    if !matches!(state.mode, Mode::Normal) || state.help_open {
        return;
    }
    if text.is_empty() {
        return;
    }
    let idx = byte_index(&state.input, state.cursor);
    state.input.insert_str(idx, text);
    state.cursor += char_count(text);
    state.sync_palette_stem();
}

/// `Up`/`Down` within a multi-line draft (T8): moves the cursor to the
/// equivalent column on the line above (`delta < 0`) or below (`delta >
/// 0`) the cursor's current line. Returns whether it moved the cursor --
/// `false` (no mutation) when `input` is a single line, or when the cursor
/// is already on the first line (`delta < 0`) or last line (`delta > 0`) in
/// that direction, letting the caller's `Up`/`Down` fall through to history
/// recall instead. This mirrors the `Home`/`End` empty-input precedent
/// (`handle_normal_key`'s own doc on that pair): the key means one thing
/// while there is buffer-internal work left to do, and something else
/// (here, history) once you're at the boundary in that direction -- so a
/// recalled multi-line entry stays reachable line-by-line via `Up`/`Down`,
/// while `Up` at the first line and `Down` at the last line keep recalling
/// history, exactly as they do for a single-line draft.
fn move_cursor_line(state: &mut AppState, delta: isize) -> bool {
    let lines: Vec<&str> = state.input.split('\n').collect();
    if lines.len() < 2 {
        return false;
    }
    let (line_idx, col) = state.cursor_line_col();
    let target_line = if delta < 0 {
        if line_idx == 0 {
            return false;
        }
        line_idx - 1
    } else {
        if line_idx + 1 >= lines.len() {
            return false;
        }
        line_idx + 1
    };
    let target_len = char_count(lines[target_line]);
    let new_col = col.min(target_len);
    // The char-index start of `target_line`: the sum of every earlier
    // line's length plus one char for the `\n` that follows it.
    let target_start: usize = lines[..target_line].iter().map(|l| char_count(l) + 1).sum();
    state.cursor = target_start + new_col;
    true
}

/// Deletes the word immediately before the cursor: trailing whitespace
/// first, then the run of non-whitespace before that (conventional `Ctrl-W`
/// behavior), moving the cursor to the start of what was deleted.
fn delete_word_before_cursor(state: &mut AppState) {
    if state.cursor == 0 {
        return;
    }
    let chars: Vec<char> = state.input.chars().collect();
    let mut start = state.cursor;
    while start > 0 && chars[start - 1].is_whitespace() {
        start -= 1;
    }
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    let start_byte = byte_index(&state.input, start);
    let end_byte = byte_index(&state.input, state.cursor);
    state.input.replace_range(start_byte..end_byte, "");
    state.cursor = start;
}

#[cfg(test)]
mod tests {
    use conway::AgentId;
    use ratatui::layout::Rect;

    use super::*;
    use crate::tui::state::AppState;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    /// T8 (worker-flagged gap): moving between lines of DIFFERENT lengths.
    /// The boundary cases were covered, but not the ordinary one -- a
    /// cursor deep into a long line moving onto a shorter one has to clamp
    /// to that line's end, and `target_start` has to account for each `\n`
    /// it skips. Off-by-one here would silently land the cursor on the
    /// wrong line, which is the classic multi-line editor bug.
    #[test]
    fn moving_between_lines_clamps_the_column_onto_a_shorter_line() {
        let mut state = AppState::new(AgentId::new());
        state.input = "a-very-long-first-line\nshort\nanother-long-line".to_string();

        // Cursor at column 20 of line 0.
        state.cursor = 20;
        assert_eq!(state.cursor_line_col(), (0, 20));

        // Down onto "short" (5 chars) must clamp to its end, not overshoot
        // into the line after it.
        assert!(move_cursor_line(&mut state, 1));
        assert_eq!(
            state.cursor_line_col(),
            (1, 5),
            "column must clamp to the shorter line's length"
        );

        // Down again onto the long line: the column is now 5 (clamped), and
        // must stay 5 rather than restoring the original 20.
        assert!(move_cursor_line(&mut state, 1));
        assert_eq!(state.cursor_line_col(), (2, 5));

        // And back up, verifying `target_start` skipped exactly the right
        // number of newline chars on the way.
        assert!(move_cursor_line(&mut state, -1));
        assert_eq!(state.cursor_line_col(), (1, 5));
        assert!(move_cursor_line(&mut state, -1));
        assert_eq!(state.cursor_line_col(), (0, 5));
    }

    /// The cursor index must land on the actual character the (line, col)
    /// pair names -- proving `target_start`'s newline accounting, not just
    /// that the derived pair looks right.
    #[test]
    fn moving_between_lines_lands_on_the_right_absolute_char_index() {
        let mut state = AppState::new(AgentId::new());
        state.input = "abc\ndefgh\nij".to_string();

        // Line 1 ("defgh") starts at index 4: "abc" (3) + "\n" (1).
        state.cursor = 1; // line 0, col 1
        assert!(move_cursor_line(&mut state, 1));
        assert_eq!(state.cursor, 5, "line 1 start (4) + col 1");
        assert_eq!(state.cursor_line_col(), (1, 1));

        // Line 2 ("ij") starts at index 10: 3 + 1 + 5 + 1.
        assert!(move_cursor_line(&mut state, 1));
        assert_eq!(state.cursor, 11, "line 2 start (10) + col 1");
        assert_eq!(state.cursor_line_col(), (2, 1));
    }

    #[test]
    fn typing_appends_to_input() {
        let mut state = AppState::new(AgentId::new());
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Char('h'))),
            Action::None
        );
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Char('i'))),
            Action::None
        );
        assert_eq!(state.input, "hi");
    }

    #[test]
    fn enter_submits_and_clears_input() {
        let mut state = AppState::new(AgentId::new());
        state.input = "hello".to_string();
        let action = handle_key(&mut state, key(KeyCode::Enter));
        assert_eq!(action, Action::Submit("hello".to_string()));
        assert!(state.input.is_empty());
    }

    // ---- palette arrow navigation + agent-panel scroll ----

    fn type_str(state: &mut AppState, s: &str) {
        for c in s.chars() {
            handle_key(state, key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn palette_down_autofills_successive_matches_without_collapsing() {
        let mut state = AppState::new(AgentId::new());
        type_str(&mut state, "/a"); // matches [/ask, /agents]
                                    // First Down lands on the first match and autofills it.
        handle_key(&mut state, key(KeyCode::Down));
        assert_eq!(state.input, "/ask");
        assert_eq!(state.palette_selected, Some(0));
        // Second Down advances even though `input` is now a full command --
        // the list stayed anchored to the "/a" stem, so it did not collapse.
        handle_key(&mut state, key(KeyCode::Down));
        assert_eq!(state.input, "/agents");
        assert_eq!(state.palette_selected, Some(1));
        // Wraps back to the top.
        handle_key(&mut state, key(KeyCode::Down));
        assert_eq!(state.input, "/ask");
    }

    #[test]
    fn palette_up_from_no_selection_lands_on_the_last_match() {
        let mut state = AppState::new(AgentId::new());
        type_str(&mut state, "/a");
        handle_key(&mut state, key(KeyCode::Up));
        assert_eq!(state.input, "/agents");
        assert_eq!(state.palette_selected, Some(1));
    }

    #[test]
    fn typing_resets_the_palette_highlight() {
        let mut state = AppState::new(AgentId::new());
        type_str(&mut state, "/");
        handle_key(&mut state, key(KeyCode::Down));
        assert!(state.palette_selected.is_some());
        handle_key(&mut state, key(KeyCode::Char('h')));
        assert_eq!(state.palette_selected, None);
    }

    #[test]
    fn esc_closes_the_agent_panel() {
        let mut state = AppState::new(AgentId::new());
        state.agent_view_open = true;
        handle_key(&mut state, key(KeyCode::Esc));
        assert!(!state.agent_view_open);
    }

    // ---- T6: bare Up/Down do NOT scroll the transcript any more (removed
    // in favor of PageUp/PageDown + Home/End; T8 reassigns bare Up/Down to
    // input history -- with an empty history, `history_recall_prev`/`_next`
    // are no-ops, so these tests (empty history, no palette/panel active)
    // still pin the "no transcript scroll" behavior unchanged). bug 3
    //'s palette/panel priority is unchanged --
    // only the lowest-priority fallback changed. ----

    // The small viewport `test_support`'s own PageUp test uses to force the
    // 30-line transcript below to overflow -- mirrored here so the Up/Down
    // tests exercise the same overflow condition (at the default 80x24
    // size, 30 short lines fit with no scrolling at all, which would make
    // `scroll > 0` assertions vacuous).
    fn small_viewport() -> Rect {
        Rect::new(0, 0, 20, 10)
    }

    /// V3 (regression fix): bare `Up`/`Down` scroll the transcript one
    /// line. This is the wheel path -- a terminal's alternate-scroll mode
    /// (DECSET 1007) turns a two-finger scroll into cursor-key presses
    /// while the alternate screen is active, so this test IS the
    /// two-finger-scroll test. T8 had bound these to history recall, which
    /// is what broke scrolling; history moved to `Ctrl-P`/`Ctrl-N`.
    #[test]
    fn bare_up_down_scroll_the_transcript_one_line() {
        use crate::tui::state::Entry;
        use crate::tui::test_support::press;

        let mut state = AppState::new(AgentId::new());
        for i in 0..30 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }
        assert!(state.follow_tail);

        let up_action = press(&mut state, key(KeyCode::Up), small_viewport());
        assert_eq!(
            up_action,
            Action::ScrollLineUp,
            "bare Up must scroll, not recall history -- this is the wheel path"
        );
        assert!(state.scroll > 0, "Up must move `scroll` off the bottom");
        assert!(!state.follow_tail, "scrolling up disengages auto-follow");

        // And back down again, one line at a time.
        let before = state.scroll;
        let down_action = press(&mut state, key(KeyCode::Down), small_viewport());
        assert_eq!(down_action, Action::ScrollLineDown);
        assert!(
            state.scroll > before || state.follow_tail,
            "Down must move back toward the tail"
        );
    }

    /// The scroll must be ONE line, not a page -- otherwise a wheel tick
    /// would jump a whole screen and the two actions would be redundant.
    #[test]
    fn bare_arrow_scroll_is_one_line_while_page_keys_move_a_full_page() {
        use crate::tui::state::Entry;
        use crate::tui::test_support::press;

        // Two independently-built identical states (`AppState` is not
        // `Clone`, and deriving it just for a test would widen the type's
        // contract for no production reason).
        let build = || {
            let mut s = AppState::new(AgentId::new());
            for i in 0..60 {
                s.transcript.push(Entry::Assistant {
                    text: format!("line {i}"),
                    model: None,
                    summary: None,
                    ts: None,
                });
            }
            s
        };
        let mut line_state = build();
        let mut page_state = build();

        press(&mut line_state, key(KeyCode::Up), small_viewport());
        press(&mut page_state, key(KeyCode::PageUp), small_viewport());

        assert!(
            page_state.scroll < line_state.scroll,
            "PageUp must travel further from the tail than a single-line Up \
             (line={}, page={})",
            line_state.scroll,
            page_state.scroll
        );
    }
    #[test]
    fn page_up_and_page_down_scroll_a_full_page() {
        use crate::tui::state::Entry;
        use crate::tui::test_support::press;

        let mut state = AppState::new(AgentId::new());
        for i in 0..30 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }

        // PageUp: a real, full-page scroll that disengages follow.
        let up_action = press(&mut state, key(KeyCode::PageUp), small_viewport());
        assert_eq!(up_action, Action::ScrollUp);
        assert!(state.scroll > 0, "PageUp must move `scroll` off the bottom");
        assert!(!state.follow_tail);
    }

    #[test]
    fn page_up_and_page_down_still_move_a_full_page_unlike_the_arrows() {
        use crate::tui::state::Entry;
        use crate::tui::test_support::press;

        let mut state = AppState::new(AgentId::new());
        for i in 0..30 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }
        // First PageUp disengages `follow_tail` and establishes a real
        // `scroll` baseline, same reasoning as the arrow test above.
        press(&mut state, key(KeyCode::PageUp), small_viewport());
        let before = state.scroll;

        let up_action = press(&mut state, key(KeyCode::PageUp), small_viewport());
        let after_page_up = state.scroll;

        assert_eq!(up_action, Action::ScrollUp);
        let page_step = before - after_page_up;
        assert!(
            page_step > 1,
            "PageUp's step ({page_step}) must remain a full page, not the \
             one-line arrow step"
        );

        let down_action = press(&mut state, key(KeyCode::PageDown), small_viewport());

        assert_eq!(down_action, Action::ScrollDown);
        assert_eq!(
            state.scroll,
            after_page_up + page_step,
            "PageDown's step must be the same full-page size as PageUp's"
        );
    }

    #[test]
    fn agent_panel_open_keeps_up_down_driving_panel_nav_not_scroll() {
        use crate::tui::state::{Entry, NodeStatus, TreeNode};
        use crate::tui::test_support::press_key;

        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        state.tree.nodes.push(TreeNode {
            agent_id: child,
            parent: Some(root),
            agent_def: Some("reviewer".to_string()),
            status: NodeStatus::Running,
            kind: None,
            inherited_upto: None,
            ephemeral: false,
        });
        for i in 0..30 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }
        state.agent_view_open = true;
        assert_eq!(state.agent_selected, 0);

        let action = press_key(&mut state, KeyCode::Down);

        assert_eq!(
            action,
            Action::None,
            "the agent panel must consume Down, not the transcript scroll"
        );
        assert_eq!(state.agent_selected, 1, "panel selection must have moved");
        assert!(
            state.follow_tail,
            "the transcript must be untouched while the panel owns the key"
        );

        let action = press_key(&mut state, KeyCode::Up);

        assert_eq!(action, Action::None);
        assert_eq!(state.agent_selected, 0);
        assert!(state.follow_tail);
    }

    #[test]
    fn palette_composing_keeps_up_down_driving_the_palette_not_scroll() {
        use crate::tui::state::Entry;
        use crate::tui::test_support::press_key;

        let mut state = AppState::new(AgentId::new());
        for i in 0..30 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }
        type_str(&mut state, "/a"); // matches [/ask, /agents]; palette active

        let action = press_key(&mut state, KeyCode::Down);

        assert_eq!(
            action,
            Action::None,
            "the palette must consume Down, not the transcript scroll"
        );
        assert_eq!(state.input, "/ask");
        assert!(
            state.follow_tail,
            "the transcript must be untouched while the palette owns the key"
        );

        let action = press_key(&mut state, KeyCode::Up);

        assert_eq!(action, Action::None);
        assert!(state.follow_tail);
    }

    // ---- focused-agent switch ----

    #[test]
    fn enter_on_an_empty_input_with_the_agent_panel_open_focuses_the_highlighted_row() {
        use crate::tui::state::{NodeStatus, TreeNode};

        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        state.tree.nodes.push(TreeNode {
            agent_id: child,
            parent: Some(root),
            agent_def: Some("reviewer".to_string()),
            status: NodeStatus::Running,
            kind: None,
            inherited_upto: None,
            ephemeral: false,
        });
        state.agent_view_open = true;
        state.agent_selected = 1; // the child row

        let action = handle_key(&mut state, key(KeyCode::Enter));

        assert_eq!(action, Action::FocusAgent(child));
    }

    #[test]
    fn enter_on_an_empty_input_with_the_panel_closed_is_still_a_noop() {
        let mut state = AppState::new(AgentId::new());
        assert!(!state.agent_view_open);
        assert_eq!(handle_key(&mut state, key(KeyCode::Enter)), Action::None);
    }

    #[test]
    fn esc_while_focused_off_root_returns_focus_to_root() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        state.focus_agent(child);

        let action = handle_key(&mut state, key(KeyCode::Esc));

        assert_eq!(action, Action::FocusAgent(root));
    }

    #[test]
    fn esc_while_already_on_root_does_not_emit_a_focus_switch() {
        let mut state = AppState::new(AgentId::new());
        assert!(state.is_root_focused());

        let action = handle_key(&mut state, key(KeyCode::Esc));

        assert_eq!(action, Action::None);
    }

    /// Esc does ONE thing per press. Closing the panel must NOT also throw
    /// away the focus the user just chose.
    ///
    /// The reported bug: fork, open `/agents`, focus the new child, press
    /// Esc to dismiss the panel -- and you were bounced back to the root,
    /// with no way to keep the child focused and still get the panel out of
    /// the way. Two work items had independently bound Esc (panel-close,
    /// focus-root) and both fired at once; only the panel-close half was
    /// ever documented in `/help`.
    #[test]
    fn esc_closes_the_panel_without_discarding_the_focused_agent() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        state.focus_agent(child);
        state.agent_view_open = true;

        let action = handle_key(&mut state, key(KeyCode::Esc));

        assert!(!state.agent_view_open, "the panel closes");
        assert_eq!(
            action,
            Action::None,
            "but the focused child is KEPT -- closing the panel is not a \
             request to change which conversation is shown"
        );
        assert_eq!(
            state.focused_agent, child,
            "the focus the user chose survives the panel closing"
        );
    }

    /// The second press is what returns to the root -- so the escape route
    /// still exists, it just is not fused to dismissing the panel.
    #[test]
    fn a_second_esc_returns_to_the_root_once_the_panel_is_closed() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        state.focus_agent(child);
        state.agent_view_open = true;

        // First press: panel only.
        handle_key(&mut state, key(KeyCode::Esc));
        assert!(!state.agent_view_open);

        // Second press: back to the root.
        let action = handle_key(&mut state, key(KeyCode::Esc));
        assert_eq!(action, Action::FocusAgent(root));
    }

    /// With the panel already closed, Esc goes straight back to the root --
    /// no wasted keypress for a panel that is not showing.
    #[test]
    fn esc_with_no_panel_open_returns_to_the_root_immediately() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.focus_agent(AgentId::new());
        assert!(!state.agent_view_open);

        let action = handle_key(&mut state, key(KeyCode::Esc));
        assert_eq!(action, Action::FocusAgent(root));
    }

    #[test]
    fn palette_arrows_take_priority_over_the_agent_panel() {
        let mut state = AppState::new(AgentId::new());
        state.agent_view_open = true;
        type_str(&mut state, "/a"); // palette active
        handle_key(&mut state, key(KeyCode::Down));
        // The palette consumed the key: input autofilled, panel selection
        // untouched.
        assert_eq!(state.input, "/ask");
        assert_eq!(state.agent_selected, 0);
    }

    // ---- Item A2: `v` cycles the /agents panel's visibility filter ----

    #[test]
    fn v_with_the_panel_open_and_empty_input_cycles_the_visibility_filter() {
        use crate::tui::state::AgentVisibility;

        // V5: the default flipped to All; the cycle order itself is
        // unchanged (ActiveOnly -> All -> FinishedOnly -> ActiveOnly), so
        // starting from All the next three presses land on FinishedOnly,
        // ActiveOnly, then back to All.
        let mut state = AppState::new(AgentId::new());
        state.agent_view_open = true;
        assert_eq!(state.agent_visibility, AgentVisibility::All);

        for expected in [
            AgentVisibility::FinishedOnly,
            AgentVisibility::ActiveOnly,
            AgentVisibility::All,
        ] {
            assert_eq!(
                handle_key(&mut state, key(KeyCode::Char('v'))),
                Action::None
            );
            assert_eq!(state.agent_visibility, expected);
            assert!(
                state.input.is_empty(),
                "the filter key must not type into the input line"
            );
        }
    }

    #[test]
    fn v_with_the_panel_closed_types_a_v() {
        use crate::tui::state::AgentVisibility;

        let mut state = AppState::new(AgentId::new());
        assert!(!state.agent_view_open);

        handle_key(&mut state, key(KeyCode::Char('v')));

        assert_eq!(state.input, "v");
        assert_eq!(
            state.agent_visibility,
            AgentVisibility::All,
            "the filter must not cycle while the panel is closed"
        );
    }

    #[test]
    fn v_with_text_typed_stays_text_input_even_with_the_panel_open() {
        use crate::tui::state::AgentVisibility;

        let mut state = AppState::new(AgentId::new());
        state.agent_view_open = true;
        type_str(&mut state, "ga");

        handle_key(&mut state, key(KeyCode::Char('v')));

        assert_eq!(
            state.input, "gav",
            "with a prompt being composed, v must remain ordinary text input"
        );
        assert_eq!(state.agent_visibility, AgentVisibility::All);
    }

    #[test]
    fn enter_focuses_the_filtered_row_not_the_raw_tree_index() {
        use crate::tui::state::{AgentVisibility, NodeStatus, TreeNode};

        // root(Starting), done(Finished), live(Running). Under ActiveOnly
        // the visible rows are [root, live], so row index 1 is `live` --
        // NOT the raw tree's index 1 (`done`).
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let done = AgentId::new();
        let live = AgentId::new();
        for (id, status) in [(done, NodeStatus::Finished), (live, NodeStatus::Running)] {
            state.tree.nodes.push(TreeNode {
                agent_id: id,
                parent: Some(root),
                agent_def: None,
                status,
                kind: None,
                inherited_upto: None,
                ephemeral: false,
            });
        }
        state.agent_view_open = true;
        state.agent_selected = 1;
        state.agent_visibility = AgentVisibility::ActiveOnly;

        let action = handle_key(&mut state, key(KeyCode::Enter));
        assert_eq!(
            action,
            Action::FocusAgent(live),
            "Enter must resolve the selection through the filtered rows"
        );

        // Under All the same index 1 is the raw tree's `done` row.
        state.agent_visibility = AgentVisibility::All;
        let action = handle_key(&mut state, key(KeyCode::Enter));
        assert_eq!(action, Action::FocusAgent(done));
    }

    #[test]
    fn v_reclamps_the_selection_when_terminal_rows_disappear() {
        use crate::tui::state::{AgentVisibility, NodeStatus, TreeNode};

        // root(Starting), a(Finished), b(Finished). Under All (3 rows) the
        // last row is selected; cycling `v` to FinishedOnly (2 rows) must
        // re-clamp the selection to 1.
        let root = AgentId::new();
        let mut state = AppState::new(root);
        for _ in 0..2 {
            state.tree.nodes.push(TreeNode {
                agent_id: AgentId::new(),
                parent: Some(root),
                agent_def: None,
                status: NodeStatus::Finished,
                kind: None,
                inherited_upto: None,
                ephemeral: false,
            });
        }
        state.agent_view_open = true;
        state.agent_visibility = AgentVisibility::All;
        state.agent_selected = 2;

        handle_key(&mut state, key(KeyCode::Char('v')));

        assert_eq!(state.agent_visibility, AgentVisibility::FinishedOnly);
        assert_eq!(state.agent_selected, 1);
    }

    #[test]
    fn enter_on_empty_input_is_a_noop() {
        let mut state = AppState::new(AgentId::new());
        assert_eq!(handle_key(&mut state, key(KeyCode::Enter)), Action::None);
    }

    #[test]
    fn ctrl_d_on_empty_input_quits() {
        let mut state = AppState::new(AgentId::new());
        assert_eq!(
            handle_key(&mut state, ctrl_key(KeyCode::Char('d'))),
            Action::Quit
        );
    }

    #[test]
    fn ctrl_d_with_text_present_does_not_quit() {
        let mut state = AppState::new(AgentId::new());
        state.input = "x".to_string();
        assert_eq!(
            handle_key(&mut state, ctrl_key(KeyCode::Char('d'))),
            Action::None
        );
    }

    #[test]
    fn ctrl_c_signals_regardless_of_input() {
        let mut state = AppState::new(AgentId::new());
        assert_eq!(
            handle_key(&mut state, ctrl_key(KeyCode::Char('c'))),
            Action::CtrlC
        );
    }

    // ---- T5: Ctrl-E toggles all tool entries' `expanded` flag ----

    use crate::tui::state::{Entry, ToolStatus};

    #[test]
    fn ctrl_e_toggles_all_tool_entries_expanded() {
        let mut state = AppState::new(AgentId::new());
        state.transcript.push(Entry::Tool {
            call_id: "c1".to_string(),
            name: "bash".to_string(),
            status: ToolStatus::Finished { is_error: false },
            preview: "a\nb\nc".to_string(),
            args: String::new(),
            progress: String::new(),
            expanded: false,
            ts: None,
        });
        state.transcript.push(Entry::Tool {
            call_id: "c2".to_string(),
            name: "bash".to_string(),
            status: ToolStatus::Finished { is_error: false },
            preview: "x\ny".to_string(),
            args: String::new(),
            progress: String::new(),
            expanded: false,
            ts: None,
        });

        // Ctrl-E: pure state mutation, returns `Action::None` (mirrors the
        // `v` visibility-filter key's direct-mutation pattern).
        assert_eq!(
            handle_key(&mut state, ctrl_key(KeyCode::Char('e'))),
            Action::None,
            "Ctrl-E must report Action::None (the toggle is pure state)"
        );
        let all_expanded: Vec<bool> = state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Tool { expanded, .. } => Some(*expanded),
                _ => None,
            })
            .collect();
        assert_eq!(all_expanded, vec![true, true], "Ctrl-E must expand ALL");

        // A second Ctrl-E collapses them again (involution).
        handle_key(&mut state, ctrl_key(KeyCode::Char('e')));
        let all_expanded: Vec<bool> = state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Tool { expanded, .. } => Some(*expanded),
                _ => None,
            })
            .collect();
        assert_eq!(all_expanded, vec![false, false]);
    }

    /// Ctrl-E must NOT touch `scroll`/`follow_tail` (the no-snap contract).
    #[test]
    fn ctrl_e_does_not_touch_scroll_or_follow_tail() {
        let mut state = AppState::new(AgentId::new());
        state.transcript.push(Entry::Tool {
            call_id: "c1".to_string(),
            name: "bash".to_string(),
            status: ToolStatus::Finished { is_error: false },
            preview: "a\nb\nc".to_string(),
            args: String::new(),
            progress: String::new(),
            expanded: false,
            ts: None,
        });
        state.scroll = 5;
        state.follow_tail = false;

        handle_key(&mut state, ctrl_key(KeyCode::Char('e')));

        assert_eq!(state.scroll, 5, "Ctrl-E must not change scroll");
        assert!(!state.follow_tail, "Ctrl-E must not change follow_tail");
    }

    /// A bare `e` (no modifier) must remain ordinary text input -- Ctrl-E
    /// is the binding, not `e` (D-keys: no bare printable keys as
    /// bindings). This is the load-bearing reason the binding is on Ctrl-E.
    #[test]
    fn bare_e_types_into_the_input_box_not_toggles() {
        let mut state = AppState::new(AgentId::new());
        state.transcript.push(Entry::Tool {
            call_id: "c1".to_string(),
            name: "bash".to_string(),
            status: ToolStatus::Finished { is_error: false },
            preview: "a\nb\nc".to_string(),
            args: String::new(),
            progress: String::new(),
            expanded: false,
            ts: None,
        });

        assert_eq!(
            handle_key(&mut state, key(KeyCode::Char('e'))),
            Action::None
        );
        assert_eq!(state.input, "e", "bare `e` must type into the input box");
        // The tool entry is untouched.
        match &state.transcript[0] {
            Entry::Tool { expanded, .. } => assert!(!*expanded, "bare `e` must not toggle"),
            other => panic!("expected Tool, got {other:?}"),
        }
    }

    #[test]
    fn permission_mode_keys_map_to_decisions() {
        // Exercised directly (mode-independent of how `Mode::AwaitingPermission`
        // gets populated -- that wiring is `app.rs`'s job, via `AppState::offer_prompt`).
        let mut state = AppState::new(AgentId::new());
        assert_eq!(
            handle_permission_key(&mut state, key(KeyCode::Char('y'))),
            Action::PermissionDecision(PermissionDecision::AllowOnce)
        );
        assert_eq!(
            handle_permission_key(&mut state, key(KeyCode::Char('a'))),
            Action::PermissionDecision(PermissionDecision::AllowAlways {
                scope: PermissionScope::Session
            })
        );
        assert_eq!(
            handle_permission_key(&mut state, key(KeyCode::Char('n'))),
            Action::PermissionDecision(PermissionDecision::Deny {
                reason: "user denied".to_string()
            })
        );
        assert_eq!(
            handle_permission_key(&mut state, key(KeyCode::Esc)),
            Action::PermissionDecision(PermissionDecision::DenyWithFeedback {
                message: "user declined; try another approach".to_string()
            })
        );
    }

    /// Axis B: the prompt can produce
    /// a per-agent and a per-subtree grant. The `s` key cycles the scope
    /// and BOTH remembered-grant keys honor it -- `a` through the gate
    /// decision, `p` through the pattern-grant action.
    #[test]
    fn the_scope_key_cycles_and_both_grant_keys_honor_it() {
        let mut state = AppState::new(AgentId::new());
        assert_eq!(
            state.permission_grant_scope,
            PermissionScope::Session,
            "the default is the session -- today's behavior, unchanged"
        );

        // `s` cycles Session -> Agent -> AgentSubtree -> Session.
        assert_eq!(
            handle_permission_key(&mut state, key(KeyCode::Char('s'))),
            Action::None,
            "cycling the scope is a local state change, never a decision"
        );
        assert_eq!(state.permission_grant_scope, PermissionScope::Agent);
        assert_eq!(
            handle_permission_key(&mut state, key(KeyCode::Char('s'))),
            Action::None
        );
        assert_eq!(state.permission_grant_scope, PermissionScope::AgentSubtree);
        assert_eq!(
            handle_permission_key(&mut state, key(KeyCode::Char('s'))),
            Action::None
        );
        assert_eq!(state.permission_grant_scope, PermissionScope::Session);

        // `a` grants at the cycled scope...
        handle_permission_key(&mut state, key(KeyCode::Char('s')));
        assert_eq!(
            handle_permission_key(&mut state, key(KeyCode::Char('a'))),
            Action::PermissionDecision(PermissionDecision::AllowAlways {
                scope: PermissionScope::Agent
            }),
            "an `a` pressed after `s` must carry the narrowed scope, not Session"
        );
        handle_permission_key(&mut state, key(KeyCode::Char('s')));
        assert_eq!(
            handle_permission_key(&mut state, key(KeyCode::Char('a'))),
            Action::PermissionDecision(PermissionDecision::AllowAlways {
                scope: PermissionScope::AgentSubtree
            })
        );
    }

    /// The `[p]` field editor's `Enter` carries the cycled scope: a narrowed
    /// pattern grant reaches the broker as narrow as the operator asked.
    ///
    /// The flow is now two steps -- `p` opens `Mode::EditingPattern`
    /// (returns `Action::None`; the editor is the modal), and `Enter`
    /// submits, emitting `Action::GrantPermissionRule` carrying the scope
    /// the prompt's `s` key cycled to. (It used to be one step -- `p`
    /// granted `tool:*` immediately -- before the field editor generalized
    /// `[p]` into "choose which argument fields to pin"; the all-wildcard
    /// default preserves the old `tool:*` breadth when no field is pinned.)
    ///
    /// Driven through a `Structured` tool. It used to use `bash`, which is
    /// a `ShellCommand` tool and no longer has a pattern grant to offer at
    /// all -- the metacharacter gate was removed rather than tightened, so
    /// `suggested_rule` returns `None` for every shell rendering. The
    /// scope-carrying property this test exists for is unrelated to that
    /// change and is still real, so the fixture moves to a tool that still
    /// has an offer instead of the test being deleted.
    #[test]
    fn the_pattern_grant_action_carries_the_cycled_scope() {
        let mut state = AppState::new(AgentId::new());
        let (prompt, _rx) =
            crate::tui::gate::PendingPrompt::new_for_test(conway::PermissionRequest {
                agent_id: AgentId::new(),
                agent_path: Vec::new(),
                tool: conway::ToolName::new("report"),
                category: conway::ToolCategory::Read,
                arguments: serde_json::json!({}),
                rendered: r#"report({"summary":"ok"})"#.to_string(),
                call_id: "tc_1".to_string(),
                render_kind: conway::RenderKind::Structured,
            });
        state.mode = Mode::AwaitingPermission(prompt);

        // `s` cycles the grant scope Session -> Agent -> AgentSubtree.
        handle_permission_key(&mut state, key(KeyCode::Char('s')));
        assert_eq!(state.permission_grant_scope, PermissionScope::Agent);
        // `p` opens the field editor -- no grant action yet.
        let open = handle_permission_key(&mut state, key(KeyCode::Char('p')));
        assert!(
            matches!(open, Action::None),
            "`p` should open the editor (Action::None), not grant immediately; got {open:?}"
        );
        assert!(matches!(state.mode, Mode::EditingPattern(_)));
        // `Enter` submits the editor and carries the cycled scope. This
        // keystroke routes through `handle_key` (the mode dispatcher), not
        // `handle_permission_key`: the mode is now `EditingPattern`, so the
        // editing handler -- not the permission handler -- must field it.
        let action = handle_key(&mut state, key(KeyCode::Enter));
        match action {
            Action::GrantPermissionRule(_rule, scope) => {
                assert_eq!(
                    scope,
                    PermissionScope::Agent,
                    "the pattern grant must carry the scope the operator cycled to"
                );
            }
            other => panic!("expected a GrantPermissionRule action, got {other:?}"),
        }
    }

    /// The field editor's `Space` toggles a field to `pinned`, and `Enter`
    /// then builds an `ArgsMatch` rule carrying ONLY the pinned fields. With
    /// a single-field call, pinning that one field yields a rule whose
    /// `describe` names it; leaving it wildcard (no `Space`) yields the
    /// all-wildcard `tool:*`-equivalent ("any call"). This pins the
    /// narrowing-is-deliberate property: every field starts wildcard, the
    /// operator pins to narrow.
    #[test]
    fn editing_pattern_space_pins_a_field_and_enter_grants_only_pinned() {
        let mut state = AppState::new(AgentId::new());
        let (prompt, _rx) =
            crate::tui::gate::PendingPrompt::new_for_test(conway::PermissionRequest {
                agent_id: AgentId::new(),
                agent_path: Vec::new(),
                tool: conway::ToolName::new("report"),
                category: conway::ToolCategory::Read,
                arguments: serde_json::json!({ "path": "/a" }),
                rendered: r#"report({"path":"/a"})"#.to_string(),
                call_id: "tc_1".to_string(),
                render_kind: conway::RenderKind::Structured,
            });
        state.mode = Mode::AwaitingPermission(prompt);

        // No pin: Enter grants the all-wildcard (tool:*) equivalent.
        handle_permission_key(&mut state, key(KeyCode::Char('p')));
        assert!(matches!(state.mode, Mode::EditingPattern(_)));
        let wildcard = handle_key(&mut state, key(KeyCode::Enter));
        match wildcard {
            Action::GrantPermissionRule(rule, _scope) => {
                assert_eq!(
                    rule.describe(),
                    "report (any call)",
                    "an all-wildcard grant is the tool:* equivalent",
                );
            }
            other => panic!("expected GrantPermissionRule (wildcard), got {other:?}"),
        }

        // Now re-open and PIN the single field, then grant: the rule names
        // the pinned field. Cancel-first restores the prompt so `p` can open
        // the editor again (the wildcard grant above already resolved the
        // prompt, so re-establish one).
        let (prompt2, _rx2) =
            crate::tui::gate::PendingPrompt::new_for_test(conway::PermissionRequest {
                agent_id: AgentId::new(),
                agent_path: Vec::new(),
                tool: conway::ToolName::new("report"),
                category: conway::ToolCategory::Read,
                arguments: serde_json::json!({ "path": "/a" }),
                rendered: r#"report({"path":"/a"})"#.to_string(),
                call_id: "tc_2".to_string(),
                render_kind: conway::RenderKind::Structured,
            });
        state.mode = Mode::AwaitingPermission(prompt2);
        handle_permission_key(&mut state, key(KeyCode::Char('p')));
        // `Space` pins the selected (only) field.
        handle_key(&mut state, key(KeyCode::Char(' ')));
        let pinned = handle_key(&mut state, key(KeyCode::Enter));
        match pinned {
            Action::GrantPermissionRule(rule, _scope) => {
                assert_eq!(
                    rule.describe(),
                    "report with path=\"/a\" pinned",
                    "a pinned-field grant names exactly the pinned field",
                );
            }
            other => panic!("expected GrantPermissionRule (pinned), got {other:?}"),
        }

        // `Esc` cancels the editor and restores the prompt unresolved.
        let (prompt3, _rx3) =
            crate::tui::gate::PendingPrompt::new_for_test(conway::PermissionRequest {
                agent_id: AgentId::new(),
                agent_path: Vec::new(),
                tool: conway::ToolName::new("report"),
                category: conway::ToolCategory::Read,
                arguments: serde_json::json!({ "path": "/a" }),
                rendered: r#"report({"path":"/a"})"#.to_string(),
                call_id: "tc_3".to_string(),
                render_kind: conway::RenderKind::Structured,
            });
        state.mode = Mode::AwaitingPermission(prompt3);
        handle_permission_key(&mut state, key(KeyCode::Char('p')));
        assert!(matches!(state.mode, Mode::EditingPattern(_)));
        let cancel = handle_key(&mut state, key(KeyCode::Esc));
        assert!(matches!(cancel, Action::None), "Esc cancels with no action");
        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "Esc restores the prompt to the screen unresolved",
        );
    }

    /// The scope choice is per-prompt: a narrowing chosen for one call must
    /// not silently carry over to the next prompt (the same reason
    /// `modal_scroll` resets per surface).
    #[test]
    fn the_grant_scope_resets_to_session_for_each_new_prompt() {
        use crate::tui::state::AppState;
        let mut state = AppState::new(AgentId::new());
        let (first, _rx1) =
            crate::tui::gate::PendingPrompt::new_for_test(conway::PermissionRequest {
                agent_id: AgentId::new(),
                agent_path: Vec::new(),
                tool: conway::ToolName::new("bash"),
                category: conway::ToolCategory::Execute,
                arguments: serde_json::json!({}),
                rendered: "git status".to_string(),
                call_id: "tc_1".to_string(),
                render_kind: conway::RenderKind::ShellCommand,
            });
        state.offer_prompt(first);
        handle_permission_key(&mut state, key(KeyCode::Char('s')));
        assert_eq!(state.permission_grant_scope, PermissionScope::Agent);

        // Resolve the first prompt and offer the next: the scope must be
        // back at the default.
        state.resolve_current_prompt(PermissionDecision::AllowOnce);
        let (second, _rx2) =
            crate::tui::gate::PendingPrompt::new_for_test(conway::PermissionRequest {
                agent_id: AgentId::new(),
                agent_path: Vec::new(),
                tool: conway::ToolName::new("bash"),
                category: conway::ToolCategory::Execute,
                arguments: serde_json::json!({}),
                rendered: "git push".to_string(),
                call_id: "tc_2".to_string(),
                render_kind: conway::RenderKind::ShellCommand,
            });
        state.offer_prompt(second);
        assert_eq!(
            state.permission_grant_scope,
            PermissionScope::Session,
            "a new prompt must start at the session default, not inherit a \
             prior prompt's narrowing"
        );
    }

    /// The permission decision keys fire only on a BARE keypress: a chorded
    /// key (Ctrl-A, Ctrl-S, Alt-P, ...) is the operator reaching for some
    /// other binding, not a decision -- the same guard B5's M2 added to the
    /// intent-confirm modal. Ctrl-A silently granting `AllowAlways` is the
    /// worst case this prevents (code-review, grant-prompt item).
    #[test]
    fn permission_decision_keys_ignore_modifier_chords() {
        use crate::tui::state::AppState;
        let mut state = AppState::new(AgentId::new());
        let (prompt, _rx) =
            crate::tui::gate::PendingPrompt::new_for_test(conway::PermissionRequest {
                agent_id: AgentId::new(),
                agent_path: Vec::new(),
                tool: conway::ToolName::new("bash"),
                category: conway::ToolCategory::Execute,
                arguments: serde_json::json!({}),
                rendered: "git status".to_string(),
                call_id: "tc_chord".to_string(),
                render_kind: conway::RenderKind::ShellCommand,
            });
        state.offer_prompt(prompt);

        for code in ['y', 'a', 's', 'p', 'n'] {
            let chord = KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL);
            assert!(
                matches!(handle_permission_key(&mut state, chord), Action::None),
                "Ctrl-{code} must not decide a permission or cycle the grant scope"
            );
        }
        assert_eq!(
            state.permission_grant_scope,
            PermissionScope::Session,
            "a chorded `s` must not have cycled the scope"
        );

        // The guard must not swallow the REAL keys: bare `s` cycles, bare
        // `y` decides.
        handle_permission_key(&mut state, key(KeyCode::Char('s')));
        assert_eq!(state.permission_grant_scope, PermissionScope::Agent);
        assert!(matches!(
            handle_permission_key(&mut state, key(KeyCode::Char('y'))),
            Action::PermissionDecision(PermissionDecision::AllowOnce)
        ));
    }

    /// Bug fix companion: `PageDown`/`PageUp` while awaiting a permission
    /// decision must page `AppState::modal_scroll` (for a long command's
    /// overlay,) instead of falling through to
    /// `Action::None` doing nothing, and must never collide with the
    /// `y`/`a`/`n`/`Esc` decision keys tested above.
    #[test]
    fn permission_mode_page_keys_scroll_instead_of_deciding() {
        let mut state = AppState::new(AgentId::new());
        assert_eq!(state.modal_scroll, 0);

        assert_eq!(
            handle_permission_key(&mut state, key(KeyCode::PageDown)),
            Action::None
        );
        assert!(
            state.modal_scroll > 0,
            "PageDown must advance the overlay's own scroll offset"
        );

        let after_down = state.modal_scroll;
        assert_eq!(
            handle_permission_key(&mut state, key(KeyCode::PageUp)),
            Action::None
        );
        assert!(
            state.modal_scroll < after_down,
            "PageUp must step the scroll offset back down"
        );
    }

    // ---- V1: the shared `modal_scroll` field's PageUp/PageDown handling
    // generalizes to every modal-bearing surface, not just the permission
    // prompt -- the /ask modal, the intent-confirm card, and the
    // informational /help overlay each get the same PageUp/PageDown
    // scrolling now. ----

    #[test]
    fn ask_modal_page_keys_scroll_instead_of_choosing_a_fate() {
        let mut state = AppState::new(AgentId::new());
        state.offer_ask_modal(crate::tui::state::AskModal {
            question: "q".to_string(),
            child: AgentId::new(),
            answer: "a".to_string(),
            error: None,
        });
        assert_eq!(state.modal_scroll, 0);

        assert_eq!(
            handle_ask_modal_key(&mut state, key(KeyCode::PageDown)),
            Action::None,
            "PageDown must scroll, not fire a fate"
        );
        assert!(state.modal_scroll > 0);

        let after_down = state.modal_scroll;
        assert_eq!(
            handle_ask_modal_key(&mut state, key(KeyCode::PageUp)),
            Action::None
        );
        assert!(state.modal_scroll < after_down);
    }

    #[test]
    fn intent_confirm_page_keys_scroll_instead_of_choosing() {
        let mut state = AppState::new(AgentId::new());
        state.offer_intent_confirm(crate::tui::state::IntentConfirm {
            intent: conway::AgentIntent {
                recipe: conway::SubagentMode::Spawn,
                agent_def: None,
                prompt: "go".to_string(),
            },
            default_recipe: conway::SubagentMode::Spawn,
            raw_text: "go".to_string(),
            parent: AgentId::new(),
        });
        assert_eq!(state.modal_scroll, 0);

        assert_eq!(
            handle_intent_confirm_key(&mut state, key(KeyCode::PageDown)),
            Action::None,
            "PageDown must scroll, not fire a choice"
        );
        assert!(state.modal_scroll > 0);

        let after_down = state.modal_scroll;
        assert_eq!(
            handle_intent_confirm_key(&mut state, key(KeyCode::PageUp)),
            Action::None
        );
        assert!(state.modal_scroll < after_down);
    }

    #[test]
    fn help_overlay_page_keys_scroll_the_binding_list() {
        let mut state = AppState::new(AgentId::new());
        state.open_help();
        assert_eq!(state.modal_scroll, 0);

        assert_eq!(
            handle_help_key(&mut state, key(KeyCode::PageDown)),
            Action::None
        );
        assert!(state.modal_scroll > 0);

        let after_down = state.modal_scroll;
        assert_eq!(
            handle_help_key(&mut state, key(KeyCode::PageUp)),
            Action::None
        );
        assert!(state.modal_scroll < after_down);
    }

    // ---- B5: the /ask modal's forced-fate keys ----

    fn ask_modal_state() -> AppState {
        let mut state = AppState::new(AgentId::new());
        state.offer_ask_modal(crate::tui::state::AskModal {
            question: "q".to_string(),
            child: AgentId::new(),
            answer: "a".to_string(),
            error: None,
        });
        state
    }

    #[test]
    fn ask_modal_f_p_esc_map_to_the_three_fates() {
        let mut state = ask_modal_state();
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Char('f'))),
            Action::AskFate(AskFate::Fork)
        );
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Char('p'))),
            Action::AskFate(AskFate::PullIn)
        );
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Esc)),
            Action::AskFate(AskFate::Discard)
        );
        // The modal itself is untouched -- the app loop runs the fate.
        assert!(matches!(state.mode, Mode::AskModal(_)));
    }

    #[test]
    fn ask_modal_swallows_text_palette_and_panel_keys() {
        let mut state = ask_modal_state();
        // Ordinary text input is inert.
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Char('x'))),
            Action::None
        );
        assert!(state.input.is_empty(), "the input line must stay inert");
        // `/` never reaches the input line, so the palette/panel-toggle
        // command can never be composed while the modal is open.
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Char('/'))),
            Action::None
        );
        assert!(state.input.is_empty());
        assert!(!state.agent_view_open);
        // The `v` visibility-filter key and Enter/arrows are swallowed too.
        for code in [
            KeyCode::Char('v'),
            KeyCode::Enter,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::PageUp,
            KeyCode::PageDown,
        ] {
            assert_eq!(
                handle_key(&mut state, key(code)),
                Action::None,
                "{code:?} must be swallowed while the modal is open"
            );
        }
        assert!(matches!(state.mode, Mode::AskModal(_)));
    }

    #[test]
    fn ask_modal_quit_keys_pass_through_for_the_purge_then_quit_path() {
        let mut state = ask_modal_state();
        // Ctrl-C / Ctrl-D still report their actions -- `app.rs` purges the
        // modal's child before honoring them (no fate-less way out).
        assert_eq!(
            handle_key(&mut state, ctrl_key(KeyCode::Char('c'))),
            Action::CtrlC
        );
        assert_eq!(
            handle_key(&mut state, ctrl_key(KeyCode::Char('d'))),
            Action::Quit
        );
    }

    #[test]
    fn handle_key_routes_by_mode() {
        let mut state = AppState::new(AgentId::new());
        assert!(matches!(state.mode, Mode::Normal));
        // In `Normal` mode, `y` is ordinary text input, not a decision.
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Char('y'))),
            Action::None
        );
        assert_eq!(state.input, "y");
    }

    #[test]
    fn left_and_right_move_cursor_clamped_at_bounds() {
        let mut state = AppState::new(AgentId::new());
        state.input = "hi".to_string();
        state.cursor = 2;

        // Clamped at the left edge.
        handle_key(&mut state, key(KeyCode::Left));
        assert_eq!(state.cursor, 1);
        handle_key(&mut state, key(KeyCode::Left));
        assert_eq!(state.cursor, 0);
        handle_key(&mut state, key(KeyCode::Left));
        assert_eq!(state.cursor, 0);

        // Clamped at the right edge.
        handle_key(&mut state, key(KeyCode::Right));
        handle_key(&mut state, key(KeyCode::Right));
        assert_eq!(state.cursor, 2);
        handle_key(&mut state, key(KeyCode::Right));
        assert_eq!(state.cursor, 2);
    }

    #[test]
    fn home_and_end_jump_cursor_to_bounds() {
        let mut state = AppState::new(AgentId::new());
        state.input = "hello".to_string();
        state.cursor = 2;

        handle_key(&mut state, key(KeyCode::Home));
        assert_eq!(state.cursor, 0);

        handle_key(&mut state, key(KeyCode::End));
        assert_eq!(state.cursor, 5);
    }

    // ---- T6: Home/End are the transcript jump keys while the input line
    // is empty -- the cursor-editing behavior above is unchanged whenever
    // there is text to edit. ----

    #[test]
    fn home_on_empty_input_emits_jump_to_top() {
        let mut state = AppState::new(AgentId::new());
        assert!(state.input.is_empty());

        let action = handle_key(&mut state, key(KeyCode::Home));

        assert_eq!(action, Action::JumpToTop);
        // The router itself does not mutate `scroll`/`follow_tail` --
        // `Action::JumpToTop` needs the terminal-size-derived `max_scroll`
        // app.rs supplies; see `test_support::press` for the applied path.
    }

    #[test]
    fn end_on_empty_input_emits_jump_to_tail() {
        let mut state = AppState::new(AgentId::new());
        assert!(state.input.is_empty());

        let action = handle_key(&mut state, key(KeyCode::End));

        assert_eq!(action, Action::JumpToTail);
    }

    #[test]
    fn home_and_end_keep_editing_the_cursor_when_input_is_not_empty() {
        // The jump-key gating must never eat ordinary line editing: with
        // text typed, Home/End still move the cursor, exactly as
        // `home_and_end_jump_cursor_to_bounds` already pins.
        let mut state = AppState::new(AgentId::new());
        state.input = "hello".to_string();
        state.cursor = 2;

        assert_eq!(handle_key(&mut state, key(KeyCode::Home)), Action::None);
        assert_eq!(state.cursor, 0);

        assert_eq!(handle_key(&mut state, key(KeyCode::End)), Action::None);
        assert_eq!(state.cursor, 5);
    }

    #[test]
    fn press_end_jumps_to_the_tail_and_reengages_follow() {
        use crate::tui::state::Entry;
        use crate::tui::test_support::press;

        let mut state = AppState::new(AgentId::new());
        for i in 0..30 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }
        // Scroll away from the tail first, via a real PageUp, so End has
        // somewhere to jump back FROM.
        press(&mut state, key(KeyCode::PageUp), small_viewport());
        assert!(!state.follow_tail);

        let action = press(&mut state, key(KeyCode::End), small_viewport());

        assert_eq!(action, Action::JumpToTail);
        assert!(state.follow_tail, "End must re-engage follow_tail");
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn press_home_jumps_to_the_top_and_shows_the_oldest_entry() {
        use crate::tui::state::Entry;
        use crate::tui::test_support::{press, render_text};

        let mut state = AppState::new(AgentId::new());
        for i in 0..30 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }
        assert!(state.follow_tail);

        let action = press(&mut state, key(KeyCode::Home), small_viewport());

        assert_eq!(action, Action::JumpToTop);
        assert!(
            !state.follow_tail,
            "Home must disengage follow_tail -- the user is reviewing history"
        );
        assert_eq!(state.scroll, 0);
        let text = render_text(&state, small_viewport().width, small_viewport().height);
        assert!(
            text.contains("line 0 "),
            "Home must actually show the OLDEST entry, not just zero a field: {text:?}"
        );
        assert!(
            !text.contains("line 29"),
            "Home must not still show the newest entry: {text:?}"
        );
    }

    #[test]
    fn char_inserts_at_cursor_not_just_appends() {
        let mut state = AppState::new(AgentId::new());
        state.input = "helo".to_string();
        state.cursor = 3; // between 'l' and 'o'

        handle_key(&mut state, key(KeyCode::Char('l')));

        assert_eq!(state.input, "hello");
        assert_eq!(state.cursor, 4);
    }

    #[test]
    fn backspace_deletes_char_before_cursor_mid_string() {
        let mut state = AppState::new(AgentId::new());
        state.input = "hxello".to_string();
        state.cursor = 2; // right after the stray 'x'

        handle_key(&mut state, key(KeyCode::Backspace));

        assert_eq!(state.input, "hello");
        assert_eq!(state.cursor, 1);
    }

    #[test]
    fn backspace_at_start_of_input_is_a_noop() {
        let mut state = AppState::new(AgentId::new());
        state.input = "hi".to_string();
        state.cursor = 0;

        handle_key(&mut state, key(KeyCode::Backspace));

        assert_eq!(state.input, "hi");
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn ctrl_w_deletes_word_before_cursor() {
        let mut state = AppState::new(AgentId::new());
        state.input = "foo bar baz".to_string();
        state.cursor = 11; // end of string

        handle_key(&mut state, ctrl_key(KeyCode::Char('w')));

        assert_eq!(state.input, "foo bar ");
        assert_eq!(state.cursor, 8);
    }

    #[test]
    fn ctrl_w_from_mid_word_deletes_only_that_word_fragment() {
        let mut state = AppState::new(AgentId::new());
        state.input = "foo bar".to_string();
        state.cursor = 6; // between 'a' and 'r' of "bar"

        handle_key(&mut state, ctrl_key(KeyCode::Char('w')));

        assert_eq!(state.input, "foo r");
        assert_eq!(state.cursor, 4);
    }

    #[test]
    fn ctrl_w_skips_trailing_whitespace_before_deleting_word() {
        let mut state = AppState::new(AgentId::new());
        state.input = "foo bar   ".to_string();
        state.cursor = 10; // end of string, after trailing spaces

        handle_key(&mut state, ctrl_key(KeyCode::Char('w')));

        assert_eq!(state.input, "foo ");
        assert_eq!(state.cursor, 4);
    }

    #[test]
    fn cursor_stays_in_bounds_across_multi_byte_utf8_chars() {
        let mut state = AppState::new(AgentId::new());
        // "héllo": 'é' is a 2-byte UTF-8 char but a single `char`/cursor step.
        state.input = "héllo".to_string();
        state.cursor = 5; // end, in char units

        handle_key(&mut state, key(KeyCode::Left));
        handle_key(&mut state, key(KeyCode::Left));
        handle_key(&mut state, key(KeyCode::Left));
        assert_eq!(state.cursor, 2); // just after 'é'

        handle_key(&mut state, key(KeyCode::Backspace));
        assert_eq!(state.input, "hllo");
        assert_eq!(state.cursor, 1);
    }

    // ---- C2: the NL intent confirmation card's three choice keys ----

    use crate::tui::state::IntentConfirm;
    use conway::{AgentIntent, SubagentMode};

    fn intent_confirm_state(prompt: &str) -> AppState {
        let mut state = AppState::new(AgentId::new());
        state.offer_intent_confirm(IntentConfirm {
            intent: AgentIntent {
                recipe: SubagentMode::Spawn,
                agent_def: None,
                prompt: prompt.to_string(),
            },
            default_recipe: SubagentMode::Spawn,
            raw_text: prompt.to_string(),
            parent: AgentId::new(),
        });
        state
    }

    #[test]
    fn intent_confirm_enter_e_esc_map_to_the_three_choices() {
        let mut state = intent_confirm_state("refactor");
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Enter)),
            Action::IntentConfirm(IntentChoice::Confirm)
        );
        // The modal stays open after Confirm -- the app loop closes it via
        // `execute_intent_confirm` (which reads the card from `state.mode`).
        assert!(matches!(state.mode, Mode::IntentConfirm(_)));

        let mut state = intent_confirm_state("refactor");
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Esc)),
            Action::IntentConfirm(IntentChoice::Manual)
        );
        assert!(matches!(state.mode, Mode::IntentConfirm(_)));

        let mut state = intent_confirm_state("refactor");
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Char('e'))),
            Action::IntentConfirm(IntentChoice::Edit)
        );
    }

    #[test]
    fn intent_confirm_edit_drops_the_classified_prompt_into_the_input_line_and_closes() {
        let mut state = intent_confirm_state("review the diff carefully");

        handle_key(&mut state, key(KeyCode::Char('e')));

        assert_eq!(
            state.input, "review the diff carefully",
            "e must drop the classified prompt into the input line"
        );
        assert_eq!(
            state.cursor,
            state.input.chars().count(),
            "the cursor must be at the end of the dropped prompt"
        );
        assert!(
            matches!(state.mode, Mode::Normal),
            "e must close the card, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn intent_confirm_e_only_fires_on_a_bare_keypress() {
        // B5's M2 fix applied to the edit key: Ctrl-E / Alt-E must NOT be
        // the edit choice -- a modifier held is NOT a choice.
        let mut state = intent_confirm_state("refactor");

        let ctrl_e = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
        assert_eq!(
            handle_key(&mut state, ctrl_e),
            Action::None,
            "Ctrl-E must not fire edit"
        );
        assert!(
            matches!(state.mode, Mode::IntentConfirm(_)),
            "the card must stay open on Ctrl-E"
        );
        assert!(
            state.input.is_empty(),
            "Ctrl-E must not touch the input line"
        );

        let alt_e = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT);
        assert_eq!(handle_key(&mut state, alt_e), Action::None);
        assert!(matches!(state.mode, Mode::IntentConfirm(_)));
    }

    #[test]
    fn intent_confirm_swallows_text_palette_and_panel_keys() {
        let mut state = intent_confirm_state("refactor");
        // Ordinary text input is inert.
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Char('x'))),
            Action::None
        );
        assert!(state.input.is_empty(), "the input line must stay inert");
        // `/` never reaches the input line, so the palette/panel-toggle
        // command can never be composed while the card is open.
        assert_eq!(
            handle_key(&mut state, key(KeyCode::Char('/'))),
            Action::None
        );
        assert!(state.input.is_empty());
        assert!(!state.agent_view_open);
        // The `v` visibility-filter key and arrows are swallowed too.
        for code in [
            KeyCode::Char('v'),
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::PageUp,
            KeyCode::PageDown,
        ] {
            assert_eq!(
                handle_key(&mut state, key(code)),
                Action::None,
                "{code:?} must be swallowed while the card is open"
            );
        }
        assert!(matches!(state.mode, Mode::IntentConfirm(_)));
    }

    #[test]
    fn intent_confirm_quit_keys_pass_through() {
        // Ctrl-C / Ctrl-D still report their actions -- quitting with the
        // card open IS the manual fallback (no live child to purge, unlike
        // the /ask modal -- the card opens BEFORE any agent is created).
        let mut state = intent_confirm_state("refactor");
        assert_eq!(
            handle_key(&mut state, ctrl_key(KeyCode::Char('c'))),
            Action::CtrlC
        );
        assert_eq!(
            handle_key(&mut state, ctrl_key(KeyCode::Char('d'))),
            Action::Quit
        );
    }

    // ---- T8: multi-line input (Alt-Enter/Shift-Enter), paste, and
    // Up/Down history recall vs. multi-line cursor movement ----

    fn alt_enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)
    }

    fn shift_enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)
    }

    #[test]
    fn alt_enter_inserts_a_newline_instead_of_submitting() {
        let mut state = AppState::new(AgentId::new());
        type_str(&mut state, "line one");
        let action = handle_key(&mut state, alt_enter());
        assert_eq!(action, Action::None, "Alt-Enter must not submit");
        assert_eq!(state.input, "line one\n");
        assert_eq!(state.cursor, "line one\n".chars().count());
    }

    #[test]
    fn shift_enter_inserts_a_newline_instead_of_submitting() {
        let mut state = AppState::new(AgentId::new());
        type_str(&mut state, "line one");
        let action = handle_key(&mut state, shift_enter());
        assert_eq!(action, Action::None, "Shift-Enter must not submit");
        assert_eq!(state.input, "line one\n");
    }

    #[test]
    fn plain_enter_still_submits_a_multi_line_buffer() {
        let mut state = AppState::new(AgentId::new());
        type_str(&mut state, "line one");
        handle_key(&mut state, alt_enter());
        type_str(&mut state, "line two");

        let action = handle_key(&mut state, key(KeyCode::Enter));

        assert_eq!(action, Action::Submit("line one\nline two".to_string()));
        assert!(state.input.is_empty());
    }

    #[test]
    fn plain_enter_on_empty_input_is_unaffected_by_the_newline_binding() {
        // Regression guard: the empty-input Enter arm (submit no-op / focus
        // the agent panel's highlighted row) must still be reachable --
        // adding the ALT/SHIFT guard arm above it must not shadow it.
        let mut state = AppState::new(AgentId::new());
        assert_eq!(handle_key(&mut state, key(KeyCode::Enter)), Action::None);
        assert!(state.input.is_empty());
    }

    // ---- T8: bracketed paste ----

    #[test]
    fn paste_inserts_the_whole_string_as_one_edit_at_the_cursor() {
        let mut state = AppState::new(AgentId::new());
        state.input = "ab".to_string();
        state.cursor = 1; // between 'a' and 'b'

        handle_paste(&mut state, "PASTED\ntext");

        assert_eq!(state.input, "aPASTED\ntextb");
        assert_eq!(state.cursor, 1 + "PASTED\ntext".chars().count());
    }

    #[test]
    fn paste_is_swallowed_while_a_modal_bearing_surface_is_open() {
        let mut state = AppState::new(AgentId::new());
        state.input = "before".to_string();
        state.cursor = 6;
        let (prompt, _rx) =
            crate::tui::gate::PendingPrompt::new_for_test(conway::PermissionRequest {
                agent_id: AgentId::new(),
                agent_path: Vec::new(),
                tool: conway::ToolName::new("bash"),
                category: conway::ToolCategory::Execute,
                arguments: serde_json::json!({}),
                rendered: "bash: ls".to_string(),
                call_id: "tc_1".to_string(),
                render_kind: conway::RenderKind::ShellCommand,
            });
        state.mode = Mode::AwaitingPermission(prompt);

        handle_paste(&mut state, "sneaky");

        assert_eq!(
            state.input, "before",
            "a paste while the input line is inert must not mutate it"
        );
    }

    #[test]
    fn paste_of_an_empty_string_is_a_no_op() {
        let mut state = AppState::new(AgentId::new());
        state.input = "x".to_string();
        state.cursor = 1;
        handle_paste(&mut state, "");
        assert_eq!(state.input, "x");
        assert_eq!(state.cursor, 1);
    }

    // ---- T8: Up/Down disambiguation between multi-line cursor movement
    // and history recall ----

    #[test]
    fn up_within_a_multi_line_draft_moves_the_cursor_not_history() {
        let mut state = AppState::new(AgentId::new());
        state.push_history("an old prompt".to_string());
        state.input = "first\nsecond".to_string();
        state.cursor = char_count(&state.input); // end of "second"

        let action = handle_key(&mut state, key(KeyCode::Up));

        assert_eq!(action, Action::None);
        assert_eq!(
            state.input, "first\nsecond",
            "Up on an interior line must move the cursor, not recall history"
        );
        assert_eq!(state.cursor_line_col(), (0, 5));
    }

    #[test]
    fn up_on_the_first_line_of_a_multi_line_draft_falls_through_to_scroll() {
        let mut state = AppState::new(AgentId::new());
        state.push_history("recalled".to_string());
        state.input = "first\nsecond".to_string();
        state.cursor = 2; // on line 0, column 2 -- already the top line

        let action = handle_key(&mut state, key(KeyCode::Up));

        assert_eq!(
            action,
            Action::ScrollLineUp,
            "V3: Up on the FIRST line falls through to scroll, not history"
        );
        assert_eq!(
            state.input, "first\nsecond",
            "the draft is untouched -- history is on Ctrl-P now"
        );
    }

    #[test]
    fn down_on_the_last_line_of_a_multi_line_draft_falls_through_to_scroll() {
        let mut state = AppState::new(AgentId::new());
        state.push_history("older".to_string());
        state.input = "first\nsecond".to_string();
        state.cursor = char_count(&state.input); // last line

        let action = handle_key(&mut state, key(KeyCode::Down));

        assert_eq!(
            action,
            Action::ScrollLineDown,
            "V3: Down on the LAST line falls through to scroll, not history"
        );
        assert_eq!(
            state.input, "first\nsecond",
            "the draft is untouched -- history is on Ctrl-N now"
        );
    }

    #[test]
    fn ctrl_p_and_ctrl_n_recall_history_when_no_palette_panel_or_multiline_draft() {
        let mut state = AppState::new(AgentId::new());
        state.push_history("remembered".to_string());

        let action = handle_key(&mut state, ctrl_key(KeyCode::Char('p')));

        assert_eq!(action, Action::None);
        assert_eq!(
            state.input, "remembered",
            "Ctrl-P recalls the previous history entry"
        );

        // Ctrl-N walks back off it, restoring the (empty) in-progress draft.
        handle_key(&mut state, ctrl_key(KeyCode::Char('n')));
        assert_eq!(
            state.input, "",
            "Ctrl-N past the newest entry restores the in-progress draft"
        );
    }

    #[test]
    fn history_recall_does_not_fire_while_the_palette_is_open() {
        let mut state = AppState::new(AgentId::new());
        state.push_history("should not appear".to_string());
        type_str(&mut state, "/a"); // opens the palette (matches /ask, /agents)

        handle_key(&mut state, key(KeyCode::Up));

        assert_eq!(
            state.input, "/agents",
            "with the palette open, Up must navigate the palette, not recall history"
        );
    }

    #[test]
    fn history_recall_does_not_fire_while_the_agent_panel_is_focused() {
        use crate::tui::state::{NodeStatus, TreeNode};

        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.push_history("should not appear".to_string());
        let child = AgentId::new();
        state.tree.nodes.push(TreeNode {
            agent_id: child,
            parent: Some(root),
            agent_def: Some("reviewer".to_string()),
            status: NodeStatus::Running,
            kind: None,
            inherited_upto: None,
            ephemeral: false,
        });
        state.agent_view_open = true;

        handle_key(&mut state, key(KeyCode::Down));

        assert_eq!(
            state.agent_selected, 1,
            "with the panel open, Down must move the panel selection, not recall history"
        );
        assert!(
            state.input.is_empty(),
            "history must not have been recalled into the input line"
        );
    }

    // ---- T7: the /help keybinding overlay's key handling ----

    #[test]
    fn esc_closes_the_help_overlay() {
        let mut state = AppState::new(AgentId::new());
        state.open_help();
        assert!(state.help_open);

        let action = handle_key(&mut state, key(KeyCode::Esc));

        assert_eq!(action, Action::None);
        assert!(!state.help_open, "Esc must close the overlay");
    }

    #[test]
    fn help_overlay_swallows_ordinary_typing_and_navigation() {
        let mut state = AppState::new(AgentId::new());
        state.open_help();

        for code in [
            KeyCode::Char('x'),
            KeyCode::Char('/'),
            KeyCode::Enter,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::PageUp,
            KeyCode::PageDown,
        ] {
            assert_eq!(
                handle_key(&mut state, key(code)),
                Action::None,
                "{code:?} must be swallowed while the help overlay is open"
            );
        }
        assert!(state.input.is_empty(), "the input line must stay inert");
        assert!(state.help_open, "only Esc closes the overlay");
    }

    #[test]
    fn help_overlay_quit_keys_still_pass_through() {
        let mut state = AppState::new(AgentId::new());
        state.open_help();

        assert_eq!(
            handle_key(&mut state, ctrl_key(KeyCode::Char('c'))),
            Action::CtrlC
        );
        assert_eq!(
            handle_key(&mut state, ctrl_key(KeyCode::Char('d'))),
            Action::Quit
        );
        // The overlay itself is untouched -- there is no live resource to
        // purge first, unlike the /ask modal.
        assert!(state.help_open);
    }

    /// T7 acceptance: the overlay must not steal keys meant for an active
    /// permission prompt -- `state.help_open` staying `true` in the
    /// background (set before the prompt arrived) must not matter once
    /// `mode` is `AwaitingPermission`.
    #[test]
    fn help_overlay_does_not_intercept_keys_while_a_permission_prompt_is_active() {
        let mut state = AppState::new(AgentId::new());
        state.open_help();
        let (prompt, _rx) =
            crate::tui::gate::PendingPrompt::new_for_test(conway::PermissionRequest {
                agent_id: AgentId::new(),
                agent_path: Vec::new(),
                tool: conway::ToolName::new("bash"),
                category: conway::ToolCategory::Execute,
                arguments: serde_json::json!({}),
                rendered: "bash: ls".to_string(),
                call_id: "tc_1".to_string(),
                render_kind: conway::RenderKind::ShellCommand,
            });
        state.mode = Mode::AwaitingPermission(prompt);

        let action = handle_key(&mut state, key(KeyCode::Char('y')));

        assert_eq!(
            action,
            Action::PermissionDecision(PermissionDecision::AllowOnce),
            "the permission prompt's own `y` binding must still resolve, not be \
             swallowed by the (backgrounded) help overlay"
        );
    }

    // ---- V4: the /settings menu's key handling ----

    #[test]
    fn esc_closes_the_settings_menu() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        assert!(state.settings_open);

        let action = handle_key(&mut state, key(KeyCode::Esc));

        assert_eq!(action, Action::None);
        assert!(!state.settings_open, "Esc must close the menu");
    }

    #[test]
    fn settings_quit_keys_still_pass_through() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();

        assert_eq!(
            handle_key(&mut state, ctrl_key(KeyCode::Char('c'))),
            Action::CtrlC
        );
        assert_eq!(
            handle_key(&mut state, ctrl_key(KeyCode::Char('d'))),
            Action::Quit
        );
        assert!(
            state.settings_open,
            "no live resource to purge -- stays open"
        );
    }

    /// Acceptance: toggling through the menu produces the SAME state change
    /// the old `/thinking` slash command did (`AppState::show_reasoning`
    /// flips true -> false -> true), driven through the real `Up`/`Down`/
    /// `Enter` keys rather than calling `toggle_thinking` directly.
    #[test]
    fn enter_on_the_show_reasoning_row_toggles_it_exactly_like_the_old_thinking_command() {
        let mut state = AppState::new(AgentId::new());
        assert!(state.show_reasoning, "defaults true");
        state.open_settings();

        // Row 0 is the "display" group; row 1 is "show reasoning traces".
        handle_key(&mut state, key(KeyCode::Down));
        assert!(
            crate::tui::view::settings::build_tree(&state)
                .selected_row()
                .unwrap()
                .label
                .contains("show reasoning"),
            "sanity: the cursor must be on the show-reasoning row"
        );

        handle_key(&mut state, key(KeyCode::Enter));
        assert!(
            !state.show_reasoning,
            "the SAME toggle /thinking used to drive"
        );

        handle_key(&mut state, key(KeyCode::Enter));
        assert!(
            state.show_reasoning,
            "toggles back, an involution exactly like before"
        );
    }

    /// Same acceptance, for the `show_timestamps` row (was `/timestamps`).
    #[test]
    fn enter_on_the_show_timestamps_row_toggles_it_exactly_like_the_old_timestamps_command() {
        let mut state = AppState::new(AgentId::new());
        assert!(!state.show_timestamps, "defaults false");
        state.open_settings();

        // Row 0 "display", row 1 "show reasoning", row 2 "show timestamps".
        handle_key(&mut state, key(KeyCode::Down));
        handle_key(&mut state, key(KeyCode::Down));
        assert!(
            crate::tui::view::settings::build_tree(&state)
                .selected_row()
                .unwrap()
                .label
                .contains("show timestamps"),
            "sanity: the cursor must be on the show-timestamps row"
        );

        handle_key(&mut state, key(KeyCode::Enter));
        assert!(
            state.show_timestamps,
            "the SAME toggle /timestamps used to drive"
        );

        handle_key(&mut state, key(KeyCode::Enter));
        assert!(
            !state.show_timestamps,
            "toggles back, an involution exactly like before"
        );
    }

    /// Acceptance: the numeric setting can actually be changed (Left/Right)
    /// and clamps safely -- driven through the real key router.
    #[test]
    fn left_right_on_the_tool_preview_lines_row_steps_it_and_clamps() {
        let mut state = AppState::new(AgentId::new());
        assert_eq!(state.tool_preview_lines, 3);
        state.open_settings();

        // Navigate onto the numeric leaf: display group (0), reasoning (1),
        // timestamps (2), tool output group (3), tool preview lines (4).
        for _ in 0..4 {
            handle_key(&mut state, key(KeyCode::Down));
        }
        assert!(
            crate::tui::view::settings::build_tree(&state)
                .selected_leaf_id()
                .as_deref()
                == Some(crate::tui::view::settings::LEAF_TOOL_PREVIEW_LINES),
            "sanity: the cursor must be on the numeric leaf"
        );

        handle_key(&mut state, key(KeyCode::Right));
        assert_eq!(state.tool_preview_lines, 4);
        handle_key(&mut state, key(KeyCode::Left));
        handle_key(&mut state, key(KeyCode::Left));
        assert_eq!(state.tool_preview_lines, 2);

        // Drive it all the way down past the floor -- must clamp at 1, not
        // panic or wrap.
        for _ in 0..10 {
            handle_key(&mut state, key(KeyCode::Left));
        }
        assert_eq!(
            state.tool_preview_lines, 1,
            "must clamp at the floor, never below"
        );
    }

    /// Left/Right must be a no-op while the cursor is on a boolean row or a
    /// group row -- there is nothing to step there.
    #[test]
    fn left_right_is_a_noop_off_the_numeric_row() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        // Row 0: the "display" group.
        assert_eq!(state.tool_preview_lines, 3);

        handle_key(&mut state, key(KeyCode::Right));
        handle_key(&mut state, key(KeyCode::Left));

        assert_eq!(
            state.tool_preview_lines, 3,
            "Left/Right off the numeric row must not change it"
        );
    }

    /// `Enter` on a group row expands/collapses it (V1's `MenuState::
    /// toggle_group_at_selection`, driven end to end through the real key
    /// router) and re-clamps the cursor.
    #[test]
    fn enter_on_a_group_row_collapses_and_expands_it() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        assert!(state.settings_collapsed_groups.is_empty());

        // Row 0 is the "display" group -- collapse it.
        handle_key(&mut state, key(KeyCode::Enter));
        assert!(
            state.settings_collapsed_groups.contains("display"),
            "Enter on an expanded group must collapse it"
        );

        // Its two children must now be hidden from the flattened tree.
        let rows_after_collapse = crate::tui::view::settings::build_tree(&state).rows();
        assert!(
            !rows_after_collapse
                .iter()
                .any(|r| r.label.contains("show reasoning")),
            "a collapsed group's children must be hidden: {rows_after_collapse:?}"
        );

        // Expand it again -- an involution.
        handle_key(&mut state, key(KeyCode::Enter));
        assert!(!state.settings_collapsed_groups.contains("display"));
        let rows_after_expand = crate::tui::view::settings::build_tree(&state).rows();
        assert!(
            rows_after_expand
                .iter()
                .any(|r| r.label.contains("show reasoning")),
            "re-expanding must restore the children: {rows_after_expand:?}"
        );
    }

    /// Acceptance: arrows navigate the menu WHILE it is open.
    #[test]
    fn up_down_navigate_the_settings_menu_while_open() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        assert_eq!(state.settings_selected, 0);

        handle_key(&mut state, key(KeyCode::Down));
        assert_eq!(state.settings_selected, 1);
        handle_key(&mut state, key(KeyCode::Down));
        assert_eq!(state.settings_selected, 2);
        handle_key(&mut state, key(KeyCode::Up));
        assert_eq!(state.settings_selected, 1);
    }

    /// `Enter` on a hook-rule row
    /// resolves to `Action::RevokeHookRule` carrying that EXACT row's
    /// `(event, id)` -- never a bare index -- resolved against
    /// `state.hook_rules` in the same call that built the tree, mirroring
    /// how the grant/structured-allow prefixes already resolve.
    #[test]
    fn enter_on_a_hook_row_yields_revoke_hook_rule_with_its_event_and_id() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.hook_rules = vec![
            conway::HookRuleView {
                id: "deny-writes".to_string(),
                event: "pre_tool_use".to_string(),
                match_tool: Some("fs.write".to_string()),
                origin: "settings.json (merged config)".to_string(),
            },
            conway::HookRuleView {
                id: "deny-prompts".to_string(),
                event: "prompt_submitted".to_string(),
                match_tool: None,
                origin: "settings.json (merged config)".to_string(),
            },
        ];

        let rows = crate::tui::view::settings::build_tree(&state).rows();
        let idx = rows
            .iter()
            .position(|r| r.label.contains("deny-prompts"))
            .expect("the second hook row must render");
        state.settings_selected = idx;

        let action = activate_settings_selection(&mut state);
        assert_eq!(
            action,
            Some(Action::RevokeHookRule(
                "prompt_submitted".to_string(),
                "deny-prompts".to_string(),
            )),
            "must name the SECOND row's own event/id, not the first"
        );
    }

    /// Board item `01M0KARX71A64NTSYTDBVANVPF`: `Enter` on an ON plugin's
    /// own toggle row resolves to `Action::TogglePlugin(id, false)` -- the
    /// OPPOSITE of its current mirrored state, carrying the id, never an
    /// index.
    #[test]
    fn enter_on_an_installed_plugins_toggle_row_yields_toggle_plugin_false() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.plugin_browser = vec![crate::tui::state::PluginBrowserEntry {
            id: "conway.memory".to_string(),
            version: "0.9.0".to_string(),
            installed: true,
            description: conway::plugin::PluginDescription::default(),
        }];

        let rows = crate::tui::view::settings::build_tree(&state).rows();
        let idx = rows
            .iter()
            .position(|r| {
                matches!(
                    &r.kind,
                    crate::tui::view::menu::MenuRowKind::Leaf { id }
                        if id.starts_with(crate::tui::view::settings::LEAF_TOGGLE_PLUGIN_PREFIX)
                )
            })
            .expect("the toggle row must render");
        state.settings_selected = idx;

        let action = activate_settings_selection(&mut state);
        assert_eq!(
            action,
            Some(Action::TogglePlugin("conway.memory".to_string(), false)),
            "an installed plugin's toggle must resolve to turning it OFF"
        );
    }

    /// The OFF-plugin counterpart: resolves to `Action::TogglePlugin(id,
    /// true)`.
    #[test]
    fn enter_on_an_uninstalled_plugins_toggle_row_yields_toggle_plugin_true() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        state.plugin_browser = vec![crate::tui::state::PluginBrowserEntry {
            id: "conway.trim".to_string(),
            version: "0.9.0".to_string(),
            installed: false,
            description: conway::plugin::PluginDescription::default(),
        }];

        let rows = crate::tui::view::settings::build_tree(&state).rows();
        let idx = rows
            .iter()
            .position(|r| {
                matches!(
                    &r.kind,
                    crate::tui::view::menu::MenuRowKind::Leaf { id }
                        if id.starts_with(crate::tui::view::settings::LEAF_TOGGLE_PLUGIN_PREFIX)
                )
            })
            .expect("the toggle row must render");
        state.settings_selected = idx;

        let action = activate_settings_selection(&mut state);
        assert_eq!(
            action,
            Some(Action::TogglePlugin("conway.trim".to_string(), true)),
            "an uninstalled plugin's toggle must resolve to turning it ON"
        );
    }

    /// Acceptance: transcript line-scroll (V3's bare Up/Down wheel path)
    /// still works once the settings menu is CLOSED -- the menu claims the
    /// arrows only while open, exactly like the agent panel already does.
    #[test]
    fn bare_up_down_scroll_the_transcript_once_settings_is_closed() {
        use crate::tui::state::Entry;
        use crate::tui::test_support::press;
        use ratatui::layout::Rect;

        let mut state = AppState::new(AgentId::new());
        for i in 0..30 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }
        state.open_settings();
        state.close_settings();
        assert!(state.follow_tail);

        let small_viewport = Rect::new(0, 0, 20, 10);
        let action = press(&mut state, key(KeyCode::Up), small_viewport);

        assert_eq!(
            action,
            Action::ScrollLineUp,
            "once closed, bare Up must fall back to transcript wheel-scroll, \
             exactly as it did before the menu ever opened"
        );
        assert!(state.scroll > 0);
    }

    /// `Enter` on a grant row must
    /// resolve to THAT row's own `(rule, origin)` -- not the first grant,
    /// not a bare index the app loop would have to re-resolve (and could
    /// re-resolve against a since-changed list).
    #[test]
    fn enter_on_a_grant_row_resolves_to_the_exact_rule_and_origin_selected() {
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

        let rows = crate::tui::view::settings::build_tree(&state).rows();
        let second_grant_row = rows
            .iter()
            .position(|r| r.label.contains("read"))
            .expect("the second grant row must be present");
        state.settings_selected = second_grant_row;

        let action = handle_key(&mut state, key(KeyCode::Enter));
        assert_eq!(
            action,
            Action::RevokePermissionPattern(
                conway::PatternRule::parse("read:*").expect("valid rule"),
                conway::PatternOrigin::File(std::path::PathBuf::from(
                    "/repo/.conway/permissions.json"
                )),
            ),
            "Enter on the second grant row must resolve to THAT grant's own \
             (rule, origin), not the first"
        );
    }

    /// A2: `Enter` on a STRUCTURED allow row must resolve to THAT
    /// row's own `(rule, origin)` through the Rule-identity action -- never
    /// into the flat mirror (the two leaf-id prefixes name disjoint index
    /// spaces), and never as a bare index the app loop could re-resolve
    /// against a since-changed list.
    #[test]
    fn enter_on_a_structured_allow_row_resolves_to_the_exact_rule_and_origin() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        // A flat grant sits ALONGSIDE the structured rule, so the test also
        // proves the structured row's index resolves against
        // `structured_allow_rules`, not `permission_grants`.
        state.permission_grants = vec![(
            conway::PatternRule::parse("bash:git status").expect("valid rule"),
            conway::PatternOrigin::Interactive,
        )];
        let rule = conway::Rule {
            select: conway::Select::Tools(vec!["bash".to_string(), "read".to_string()]),
            when: conway::When::Always,
            then: conway::Then::Allow,
        };
        let origin =
            conway::PatternOrigin::File(std::path::PathBuf::from("/repo/.conway/permissions.json"));
        state.structured_allow_rules =
            vec![(rule.clone(), origin.clone(), conway::GrantScope::Session)];

        let rows = crate::tui::view::settings::build_tree(&state).rows();
        let structured_row = rows
            .iter()
            .position(|r| r.label.contains("[bash, read] (any call)"))
            .expect("the structured allow row must be present");
        state.settings_selected = structured_row;

        let action = handle_key(&mut state, key(KeyCode::Enter));
        assert_eq!(
            action,
            Action::RevokeStructuredAllowRule(rule, origin, conway::GrantScope::Session),
            "Enter on the structured allow row must resolve to THAT rule's own \
             (rule, origin, scope) via the Rule-identity action"
        );
    }

    /// The settings menu must not steal keys meant for an active permission
    /// prompt -- `state.settings_open` staying `true` in the background
    /// (set before the prompt arrived) must not matter once `mode` is
    /// `AwaitingPermission`.
    #[test]
    fn settings_menu_does_not_intercept_keys_while_a_permission_prompt_is_active() {
        let mut state = AppState::new(AgentId::new());
        state.open_settings();
        let (prompt, _rx) =
            crate::tui::gate::PendingPrompt::new_for_test(conway::PermissionRequest {
                agent_id: AgentId::new(),
                agent_path: Vec::new(),
                tool: conway::ToolName::new("bash"),
                category: conway::ToolCategory::Execute,
                arguments: serde_json::json!({}),
                rendered: "bash: ls".to_string(),
                call_id: "tc_1".to_string(),
                render_kind: conway::RenderKind::ShellCommand,
            });
        state.mode = Mode::AwaitingPermission(prompt);

        let action = handle_key(&mut state, key(KeyCode::Char('y')));

        assert_eq!(
            action,
            Action::PermissionDecision(PermissionDecision::AllowOnce),
            "the permission prompt's own `y` binding must still resolve, not be \
             swallowed by the (backgrounded) settings menu"
        );
    }
}
