//! The four mutually-exclusive modal-bearing surfaces
//! ([`Mode::AwaitingPermission`], [`Mode::AskModal`],
//! [`Mode::IntentConfirm`], [`Mode::TrustPreview`]) and their shared
//! park/promote priority queue ([`AppState::promote_next_surface`]): the
//! `/ask` single-turn modal (B5: [`AskModal`], [`AskFate`]), the
//! permission-prompt surface itself ([`AppState::offer_prompt`],
//! [`AppState::resolve_current_prompt`]), the trust-preview card
//! ([`TrustPreviewCard`], [`TrustDecision`] -- board item, split from
//! `01KZHVFCN6ZEAXV7K5JHRQN1YB`), and the two informational overlays that
//! share its mutual-exclusion rules without being a [`Mode`] variant
//! ([`AppState::open_help`], [`AppState::open_settings`] -- see [`Mode`]'s
//! own doc for why).
//!
//! The NL intent confirmation card's own state (`IntentConfirm`,
//! `IntentChoice`) and its `offer`/`close`/`begin_edit` lifecycle live on
//! [`super::AppState`] itself, not here -- `crates/conway-cli/tests/
//! intent_confirm.rs` pins their definitions to `state.rs`'s own text as a
//! source-level surface check, so this seam only owns
//! [`AppState::take_pending_intent_confirm`] (the drain the quit path
//! uses) and the `Mode::IntentConfirm` variant/park-queue plumbing.
//! [`TrustPreviewCard`]/[`TrustDecision`] carry no such external pin, so
//! this seam owns their full lifecycle
//! ([`AppState::offer_trust_preview`]/[`AppState::close_trust_preview`]/
//! [`AppState::fail_trust_preview`]/[`AppState::take_pending_trust_preview`])
//! directly, the same way it owns [`AskModal`]'s.

use super::*;
use crate::tui::form::PendingFormAsk;

/// The `/ask` modal's state (B5): one answered ephemeral fork-ask waiting
/// for the user to choose its fate. The modal opens only once the child's
/// single turn has COMPLETED (`app.rs` drives `SessionHandle::ask` +
/// `TurnHandle::text` to the finished reply, then `offer_ask_modal`), so
/// `answer` is always the final reply text -- never a pending placeholder.
/// `child` is the ephemeral fork child's [`AgentId`] (from
/// `TurnHandle::agent`), the value all three fates' facade ops take.
/// `error` is `Some` only after a fate attempt FAILED -- the modal stays
/// open with the error shown (the user still must choose; a failed fate
/// never silently falls through to another one).
#[derive(Debug, Clone, PartialEq)]
pub struct AskModal {
    pub question: String,
    pub child: AgentId,
    pub answer: String,
    pub error: Option<String>,
}

/// One of the three forced fates closing the `/ask` modal (B5) -- exactly one
/// of these runs; there is no fourth way out (quitting with the modal open
/// purges, wired in `app.rs`'s quit path). Each maps to exactly one facade op
/// (`commands::apply_ask_fate`): `Fork` -> `Conway::promote` (B3: keep -- the
/// node loses its `(ephemeral)` marking and becomes a session in its own
/// right), `PullIn` -> `Conway::pull_in` (B4: the question+answer merge into
/// the parent's own log, child purged), `Discard` -> `Conway::purge` (the
/// explicit exception to provenance being permanent and visible: the answer is
/// thrown away).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskFate {
    Fork,
    PullIn,
    Discard,
}

/// The trust-preview surface's state (board item, split from
/// `01KZHVFCN6ZEAXV7K5JHRQN1YB`'s `(kind, id, digest)`/plugin-subject
/// generalisation, which this does not pre-empt): `path`'s current
/// content, read once by `commands::execute`'s `SlashCommand::Trust` arm
/// via `Host::preview_trust_target`, shown to the operator BEFORE any
/// trust decision is recorded. `status` came from the SAME preview call --
/// `conway::TrustStatus::New`/`Changed`/`Unchanged` -- and is what lets one
/// surface carry two (really three) modes of wording rather than needing a
/// second card type: see `view::draw_trust_preview`'s own doc for exactly
/// how each status renders and, for `Changed`, the plain statement that the
/// PRIOR content cannot be shown (`conway::TrustStore` never retains it --
/// see that module's own doc).
///
/// `error` mirrors [`AskModal::error`] exactly: `Some` only after a confirm
/// attempt FAILED (`commands::apply_trust_decision`'s error path) -- the
/// card stays open with the error shown, since a failed trust attempt must
/// never silently vanish the way falling through to "cancelled" would.
#[derive(Debug, Clone, PartialEq)]
pub struct TrustPreviewCard {
    pub path: std::path::PathBuf,
    pub contents: String,
    pub status: conway::TrustStatus,
    pub error: Option<String>,
}

/// The trust-preview card's two ways out -- there is no third: quitting
/// with the card open is the cancel outcome (`app.rs`'s quit path drops it
/// on the floor, mirroring the intent-confirm card exactly, since nothing
/// has been created or written yet). Each maps to at most one facade call
/// (`commands::apply_trust_decision`): `Confirm` -> `Host::
/// trust_permission_file` (the SAME call the surface used to make
/// immediately, with no preview, before this item); `Cancel` makes no
/// facade call at all -- there is nothing to undo when nothing was ever
/// written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustDecision {
    Confirm,
    Cancel,
}

/// Board item `01M19NH39AE2D5AMJK0RZRQY86`: `ask_question`'s own modal state
/// -- one question a model-called tool is blocked waiting on, plus which
/// option the operator currently has highlighted. `ask` carries the
/// [`PendingFormAsk`] itself (the request AND the reply channel the blocked
/// `TuiFormSurface::ask_select` call is awaiting) -- unlike [`AskModal`]/
/// [`TrustPreviewCard`], this card's own answer travels back over that
/// channel directly, entirely inside `AppState`, never through a
/// `commands::Host` facade call.
pub struct UiFormState {
    pub ask: PendingFormAsk,
    /// The currently-highlighted option's index into `ask.request.options`
    /// -- always in bounds (`ask.request.options` is never empty:
    /// `conway_plugin_ui::AskSelectRequest`'s own producer refuses an empty
    /// list before it ever reaches a surface), moved by `up`/`down` in
    /// `input::handle_ui_form_key`.
    pub selected: usize,
}

/// `Mode::UiForm`'s two ways out -- there is no third: quitting with the
/// card open drops it on the floor (`shutdown.rs`'s quit path), which drops
/// `ask.reply` and fails the blocked tool call closed as `FormSurfaceError`
/// naming `"cancelled"` (`TuiFormSurface::ask_select`'s own fail-closed
/// fallback), the identical "nothing left to do but let the channel closing
/// speak for itself" posture the intent-confirm/trust-preview cards already
/// take on quit. `Answer` sends the currently-`selected` option back
/// verbatim; `Cancel` sends a named refusal instead -- neither ever fails
/// (a closed reply channel on the OTHER end -- the tool call already gave
/// up -- is not an error here either, mirroring [`AskModal::error`]'s own
/// "there is nothing left to notify" reasoning one layer down).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiFormDecision {
    Answer,
    Cancel,
}

/// Board item `01M11XWB4T8ZADNDB4M8R482MA`: the settings providers
/// section's own one-line credential prompt -- opened when the operator
/// picks a hosted provider shape (`view/settings.rs`'s `add_provider:<id>`
/// leaf) whose own `credential_env` is NOT already set in the process
/// environment (`crate::first_run::resolve_credential_plan`'s
/// `PromptForLiteral` case; `ReuseEnvVar` never opens this at all -- the add
/// happens in one keystroke, see `App::apply_add_provider_choice`'s own
/// doc). A small, SELF-CONTAINED single-line editor (`input`/`cursor`,
/// mirroring `AppState::input`/`cursor`'s own char-index convention) rather
/// than reusing the main input box: the credential must never be echoed
/// into the transcript/history/palette the main box feeds (P-10's own "a
/// credential comes from a human typing" boundary, and the exact promise
/// `first_run.rs::read_secret_line`'s doc already makes for the pre-TUI
/// flow), and this state's own doc is where that promise is kept for the
/// TUI surface instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddProviderCredentialState {
    /// The chosen [`crate::first_run::ProviderChoice::id`] this credential
    /// is for -- resolved back against `crate::first_run::HOSTED_CHOICES` by
    /// `App::apply_add_provider_credential` once `Enter` validates.
    pub choice_id: String,
    /// The choice's own display label (`crate::first_run::ProviderChoice::
    /// label`), for the card's own header -- captured at open time so the
    /// card never has to re-resolve `choice_id` against the choice table
    /// just to render its own title.
    pub label: String,
    /// The choice's own `credential_env` name, shown in the card so the
    /// operator can tell which variable a `Ctrl-C`-free `Esc` would have let
    /// them set instead.
    pub credential_env: String,
    /// The typed-so-far secret. Never rendered in the clear -- `view/
    /// settings.rs::draw_add_provider_credential` renders one `*` per
    /// character, exactly like `first_run.rs::read_secret_line`'s own
    /// terminal-level masking.
    pub input: String,
    /// Cursor position within `input`, as a *char* index -- same convention
    /// as [`super::AppState::cursor`].
    pub cursor: usize,
    /// Set by `input::handle_add_provider_credential_key`'s `Enter` arm when
    /// `crate::first_run::validate_credential_input` rejects the current
    /// `input` (empty, or implausibly long) -- the card stays open with the
    /// reason shown, mirroring [`AskModal::error`]/[`TrustPreviewCard::
    /// error`]'s own "a failed attempt never silently vanishes" contract.
    pub error: Option<String>,
}

/// The generic wording `Esc` on the permission prompt used to send
/// unconditionally, before this item -- kept as the fallback [`AppState::
/// submit_deny_feedback`] uses when the operator submits [`DenyFeedbackState`]
/// with nothing typed, so a bare `Esc`-then-`Enter` (no typing at all) still
/// reproduces exactly the old one-keystroke behavior.
pub const DEFAULT_DENY_FEEDBACK: &str = "user declined; try another approach";

/// Board item `01M1A9M2EVJNR0HBN86A8E40EA`: the permission prompt's own
/// "deny with feedback" text entry, opened by `Esc` on `Mode::
/// AwaitingPermission` instead of resolving the call immediately.
///
/// **Why this exists.** Before this item, the overlay's own footer read
/// `[Esc] deny with feedback`, but `Esc` sent [`conway::PermissionDecision::
/// DenyWithFeedback`] with a single hardcoded message (now
/// [`DEFAULT_DENY_FEEDBACK`]) and no way for the operator to type anything of
/// their own -- a control that claimed to collect feedback but never asked
/// for it (GP-14: a UI affordance describing itself incorrectly is the same
/// defect as a doc comment that does). The channel the feedback travels over
/// already existed end to end (`conway::PermissionDecision::DenyWithFeedback
/// { message }` -> `conway_runtime::permission::PermissionOutcome::Deny {
/// rendered_error: message }` -> the model's own tool-result error text,
/// `conway/src/gates.rs`'s own doc on why `DenyWithFeedback` rather than
/// `Deny` is used for every gate rejection) -- what was missing was the
/// COLLECTION step, which this state/mode exists to provide.
///
/// Mirrors [`EditingPatternState`]'s own "opened from `AwaitingPermission`,
/// cancel restores it, submit resolves it" shape exactly: the
/// [`PendingPrompt`] is MOVED out of `mode` (it is not `Clone`), so
/// cancelling never loses it, and this modal never stacks against the other
/// modal-bearing surfaces (it can only open FROM `AwaitingPermission` and
/// returns there).
pub struct DenyFeedbackState {
    pub prompt: PendingPrompt,
    /// The typed-so-far feedback message -- a small, self-contained
    /// single-line editor (`input`/`cursor`), mirroring
    /// [`AddProviderCredentialState::input`]/[`AddProviderCredentialState::
    /// cursor`]'s own char-index convention. Starts empty; unlike the
    /// credential prompt, this text is NOT a secret and renders in the
    /// clear.
    pub input: String,
    /// Cursor position within `input`, as a *char* index -- same convention
    /// as [`super::AppState::cursor`].
    pub cursor: usize,
}

impl std::fmt::Debug for DenyFeedbackState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DenyFeedbackState")
            .field("tool", &self.prompt.request.tool)
            .field("input", &self.input)
            .field("cursor", &self.cursor)
            .finish()
    }
}

impl PartialEq for DenyFeedbackState {
    fn eq(&self, other: &Self) -> bool {
        self.input == other.input && self.cursor == other.cursor
    }
}

/// `Normal` (the input line submits a prompt or a `/command`) or
/// `AwaitingPermission` (the input line is inert; `y`/`a`/`n`/`Esc` resolve
/// the pending prompt -- see `input.rs`). Only one prompt is shown at a
/// time; concurrent requests queue in `pending_prompts` (module notes:
/// "concurrent requests queue in arrival order").
pub enum Mode {
    Normal,
    AwaitingPermission(PendingPrompt),
    /// The `/ask` single-turn modal (B5). While this is the mode, the input
    /// line is inert and `/agents` is neither visible nor available --
    /// `input.rs::handle_ask_modal_key` swallows every key except the three
    /// fate keys (`f`/`p`/`Esc`) and the quit keys (`Ctrl-C`/`Ctrl-D`,
    /// which purge before exiting -- see `app.rs`). A permission prompt
    /// arriving while the modal is open queues in `queued_prompts` exactly
    /// as it does behind another prompt, and an ask answer arriving while a
    /// permission prompt is showing parks in `pending_ask_modal` until the
    /// prompt resolves -- the two modals never stack.
    AskModal(AskModal),
    /// The NL intent confirmation card (C2). While this is the mode, the
    /// input line is inert and `/agents` is neither visible nor available
    /// -- `input.rs::handle_intent_confirm_key` swallows every key except
    /// `Enter` (confirm), `e` (edit -- drops the classified prompt into the
    /// input line and closes the card) and `Esc` (manual fallback), plus
    /// the quit keys (`Ctrl-C`/`Ctrl-D`, which pass through -- unlike the
    /// `/ask` modal there is no live child to purge, since the card opens
    /// BEFORE any agent is created). A permission prompt arriving while the
    /// card is open queues in `queued_prompts` exactly as it does behind
    /// another prompt, and an intent card arriving while a permission prompt
    /// OR an `/ask` modal is showing parks in `pending_intent_confirm` until
    /// the surface clears -- the three modal-bearing surfaces
    /// (`AwaitingPermission`, `AskModal`, `IntentConfirm`) never stack.
    IntentConfirm(IntentConfirm),
    /// The trust-preview card (board item, split from
    /// `01KZHVFCN6ZEAXV7K5JHRQN1YB`): `/trust permissions` opens this
    /// FIRST, showing `path`'s current content and status, rather than
    /// installing and trusting in the same action the way it used to.
    /// While this is the mode, the input line is inert and
    /// `input.rs::handle_trust_preview_key` swallows every key except
    /// `y`/`Enter` (confirm), `n`/`Esc` (cancel), and the quit keys
    /// (`Ctrl-C`/`Ctrl-D`, which pass through -- like the intent-confirm
    /// card, and unlike the `/ask` modal, no agent has been created yet, so
    /// there is nothing to purge). A permission prompt arriving while the
    /// card is open queues in `queued_prompts` exactly as it does behind
    /// another prompt, and a trust preview arriving while any of the other
    /// three modal-bearing surfaces is showing parks in
    /// `pending_trust_preview` until the surface clears -- none of the four
    /// modal-bearing surfaces (`AwaitingPermission`, `AskModal`,
    /// `IntentConfirm`, `TrustPreview`) ever stack.
    TrustPreview(TrustPreviewCard),
    /// The `[p]` field editor (opened from `AwaitingPermission` for a
    /// `RenderKind::Structured` tool). While this is the mode, the input
    /// line is inert and `input.rs::handle_editing_pattern_key` swallows
    /// every key except the field-navigation/toggle/grant/cancel keys and
    /// the quit keys. It does not stack: it opens only from
    /// `AwaitingPermission`, and cancel returns there, submit restores
    /// there (the dispatch arm then resolves the prompt).
    EditingPattern(EditingPatternState),
    /// Board item `01M11XWB4T8ZADNDB4M8R482MA`: the settings providers
    /// section's one-line credential prompt (see
    /// [`AddProviderCredentialState`]'s own doc for why this exists and why
    /// it is not the main input box). While this is the mode, the input
    /// line is inert and `input::handle_add_provider_credential_key`
    /// swallows every key except ordinary character/`Backspace`/`Left`/
    /// `Right` editing, `Enter` (validate and add), `Esc` (cancel -- no
    /// write, matching `EditingPattern`'s own "nothing was created or
    /// written yet" cancel posture), and the quit keys. Reachable only from
    /// `Mode::Normal` with `AppState::settings_open` -- there is no other
    /// entry point -- so, like `EditingPattern`, it never needs to consider
    /// parking behind another of the four modal-bearing surfaces on OPEN; a
    /// permission prompt arriving WHILE this card is open still queues in
    /// `queued_prompts` exactly as it would behind any other mode, and
    /// `Enter`/`Esc` both restore `Mode::Normal` via
    /// `AppState::promote_next_surface` so a prompt queued in the
    /// meantime is not stranded.
    AddProviderCredential(AddProviderCredentialState),
    /// Board item `01M19NH39AE2D5AMJK0RZRQY86`: `ask_question`'s own modal
    /// -- a model-called tool is blocked awaiting the operator's answer.
    /// While this is the mode, the input line is inert and
    /// `input.rs::handle_ui_form_key` swallows every key except `up`/`down`
    /// (move the highlighted option), `enter` (answer with it), `esc`
    /// (cancel), and the quit keys (`Ctrl-C`/`Ctrl-D`, which pass through --
    /// like the trust-preview/intent-confirm cards, and unlike the `/ask`
    /// modal, no session-visible side effect has happened yet, so quitting
    /// needs no purge, only letting the reply channel close). A permission
    /// prompt arriving while this modal is open queues in `queued_prompts`
    /// exactly as it does behind another prompt, and a question arriving
    /// while any of the other four modal-bearing surfaces is showing parks
    /// in `pending_ui_form` until the surface clears -- this is the FIFTH
    /// modal-bearing surface joining the SAME never-stack discipline
    /// (`AwaitingPermission`, `AskModal`, `IntentConfirm`, `TrustPreview`,
    /// `UiForm`), lowest priority, checked last in
    /// `AppState::promote_next_surface`.
    UiForm(UiFormState),
    /// Board item `01M1A9M2EVJNR0HBN86A8E40EA`: the permission prompt's own
    /// "deny with feedback" text entry -- see [`DenyFeedbackState`]'s own
    /// doc for why this exists. While this is the mode, the input line is
    /// inert and `input.rs::handle_deny_feedback_key` swallows every key
    /// except ordinary character/`Backspace`/`Left`/`Right` editing, `Enter`
    /// (submit -- resolves the call as `DenyWithFeedback`), `Esc` (cancel --
    /// no decision at all, returns the prompt to the screen unresolved,
    /// mirroring `EditingPattern`'s own cancel), and the quit keys. Opens
    /// only from `AwaitingPermission` and returns there either way, so it
    /// never stacks against the other modal-bearing surfaces, the same
    /// non-stacking guarantee `EditingPattern` already has.
    EditingDenyFeedback(DenyFeedbackState),
}

impl std::fmt::Debug for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mode::Normal => write!(f, "Normal"),
            Mode::AwaitingPermission(p) => {
                write!(f, "AwaitingPermission({})", p.request.call_id)
            }
            Mode::AskModal(m) => write!(f, "AskModal(child={})", m.child),
            Mode::IntentConfirm(ic) => {
                write!(f, "IntentConfirm(recipe={:?})", ic.intent.recipe)
            }
            Mode::TrustPreview(card) => {
                write!(f, "TrustPreview(path={})", card.path.display())
            }
            Mode::EditingPattern(ed) => {
                write!(f, "EditingPattern(tool={})", ed.tool)
            }
            Mode::AddProviderCredential(cred) => {
                write!(f, "AddProviderCredential(choice_id={})", cred.choice_id)
            }
            Mode::UiForm(form) => {
                write!(f, "UiForm(prompt={:?})", form.ask.request.prompt)
            }
            Mode::EditingDenyFeedback(fb) => {
                write!(f, "EditingDenyFeedback(tool={})", fb.prompt.request.tool)
            }
        }
    }
}

impl AppState {
    /// Opens the `/ask` modal for one answered ask (B5), parking it in
    /// `pending_ask_modal` instead whenever another modal surface (a
    /// permission prompt, an intent confirmation card, or another ask
    /// modal) currently owns `mode` -- mirroring [`Self::offer_prompt`]'s
    /// own queue-if-busy behavior, so the modal-bearing surfaces never
    /// stack. `Self::promote_next_surface` opens the parked modal once
    /// the surface ahead of it clears.
    pub fn offer_ask_modal(&mut self, modal: AskModal) {
        if matches!(self.mode, Mode::Normal) {
            self.mode = Mode::AskModal(modal);
            // V1: a freshly opened modal starts scrolled to its own top --
            // see `Self::modal_scroll`'s own doc on why one field can serve
            // every modal-bearing surface.
            self.modal_scroll = 0;
        } else {
            self.pending_ask_modal = Some(modal);
        }
    }

    /// Drains a modal parked in `pending_ask_modal` (B5's M1 fix). Used by
    /// `app.rs::purge_open_ask_modal` so the quit path also discards a
    /// modal that was queued behind a permission prompt when the ask
    /// completed -- without this the parked modal's child leaks as residue
    /// (reaped by the next startup sweep, but still a fourth way out of an
    /// undecided ask). Returns the parked modal if one was waiting, else
    /// `None`; either way `pending_ask_modal` is cleared.
    pub fn take_pending_ask_modal(&mut self) -> Option<AskModal> {
        self.pending_ask_modal.take()
    }

    /// Closes the `/ask` modal after a fate SUCCEEDED (B5 --
    /// `commands::apply_ask_fate`'s success path), promoting the next
    /// parked/queued surface via `Self::promote_next_surface` (a queued
    /// permission prompt, a parked intent card, or a parked ask -- in that
    /// priority order). A no-op when no ask modal is open.
    pub fn close_ask_modal(&mut self) {
        if !matches!(self.mode, Mode::AskModal(_)) {
            return;
        }
        self.mode = Mode::Normal;
        self.promote_next_surface();
    }

    /// Records a fate attempt's FAILURE on the open modal (B5 --
    /// `commands::apply_ask_fate`'s error path): the modal STAYS OPEN with
    /// the error shown, so the user still must choose a fate -- a failed
    /// fate never silently falls through to another one. A no-op when no
    /// modal is open.
    pub fn fail_ask_modal(&mut self, error: String) {
        if let Mode::AskModal(modal) = &mut self.mode {
            modal.error = Some(error);
        }
    }

    /// Drains a card parked in `pending_intent_confirm`. Used by
    /// `app.rs`'s quit path so a card parked behind a permission prompt
    /// when the user quits does not leave a dangling classified intent --
    /// unlike the `/ask` modal there is no live child to purge (the card
    /// opens BEFORE any agent is created), so "draining" here just means
    /// dropping it on the floor. Returns the parked card if one was
    /// waiting, else `None`; either way `pending_intent_confirm` is
    /// cleared.
    pub fn take_pending_intent_confirm(&mut self) -> Option<IntentConfirm> {
        self.pending_intent_confirm.take()
    }

    /// Opens the trust-preview card (board item, split from
    /// `01KZHVFCN6ZEAXV7K5JHRQN1YB`), parking it in `pending_trust_preview`
    /// instead whenever another modal surface currently owns `mode` --
    /// mirrors [`Self::offer_ask_modal`]/[`Self::offer_intent_confirm`]'s
    /// own queue-if-busy shape exactly.
    pub fn offer_trust_preview(&mut self, card: TrustPreviewCard) {
        if matches!(self.mode, Mode::Normal) {
            self.mode = Mode::TrustPreview(card);
            self.modal_scroll = 0;
        } else {
            self.pending_trust_preview = Some(card);
        }
    }

    /// Closes the trust-preview card after a decision that needs no further
    /// input from it (a successful confirm, via `commands::
    /// apply_trust_decision`'s success path, or a cancel), promoting the
    /// next parked/queued surface via `Self::promote_next_surface`. A
    /// no-op when no trust-preview card is open.
    pub fn close_trust_preview(&mut self) {
        if !matches!(self.mode, Mode::TrustPreview(_)) {
            return;
        }
        self.mode = Mode::Normal;
        self.promote_next_surface();
    }

    /// Records a confirm attempt's FAILURE on the open card (`commands::
    /// apply_trust_decision`'s error path): the card STAYS OPEN with the
    /// error shown, mirroring [`Self::fail_ask_modal`] exactly -- a failed
    /// trust attempt never silently vanishes. A no-op when no card is open.
    pub fn fail_trust_preview(&mut self, error: String) {
        if let Mode::TrustPreview(card) = &mut self.mode {
            card.error = Some(error);
        }
    }

    /// Drains a card parked in `pending_trust_preview`. Used by `app.rs`'s
    /// quit path so a card parked behind another surface when the user
    /// quits does not leave a dangling trust decision -- mirrors
    /// [`Self::take_pending_intent_confirm`] exactly: no live child to
    /// purge (nothing has been created OR written yet), so draining here
    /// just means dropping it on the floor. Returns the parked card if one
    /// was waiting, else `None`; either way `pending_trust_preview` is
    /// cleared.
    pub fn take_pending_trust_preview(&mut self) -> Option<TrustPreviewCard> {
        self.pending_trust_preview.take()
    }

    /// The shared "what surfaces gets promoted next after a modal/prompt
    /// closes" logic (C2 generalizes B5's two-surface version to three;
    /// a later item generalizes it again to four; board item
    /// `01M19NH39AE2D5AMJK0RZRQY86` generalizes it once more, to five).
    /// Called with `mode` already reset to `Mode::Normal` by the caller
    /// ([`Self::close_ask_modal`], [`Self::close_intent_confirm`],
    /// [`Self::close_trust_preview`], [`Self::resolve_current_prompt`],
    /// [`Self::resolve_ui_form`]). Priority order:
    /// 1. A queued permission prompt ([`Self::queued_prompts`]) -- the
    ///    gate's pending prompts are always the highest-priority surface
    ///    (a tool call is waiting on a decision).
    /// 2. A parked `/ask` modal ([`Self::pending_ask_modal`]) -- an ask
    ///    that completed while a prompt was showing.
    /// 3. A parked intent card ([`Self::pending_intent_confirm`]) -- a
    ///    classify that completed while a prompt or an ask was showing.
    /// 4. A parked trust-preview card ([`Self::pending_trust_preview`]) --
    ///    a `/trust permissions` that completed while any of the above was
    ///    showing.
    /// 5. A parked `ask_question` card ([`Self::pending_ui_form`]) -- a
    ///    model-raised question that arrived while any of the above was
    ///    showing. Lowest priority: a question a model is waiting on is
    ///    still less urgent than a decision already forcing the operator's
    ///    attention.
    /// 6. Nothing -- `mode` stays `Normal`.
    ///
    /// Exactly one surface (at most) is promoted per call; the next call
    /// happens when THAT surface closes.
    pub(super) fn promote_next_surface(&mut self) {
        // V1: every branch below resets `modal_scroll` -- the newly
        // promoted surface starts scrolled to its own top, never carrying
        // over wherever a PREVIOUS, unrelated surface's content happened to
        // be scrolled (see `Self::modal_scroll`'s own doc on why one field
        // safely serves all of them).
        if let Some(next) = self.queued_prompts.pop_front() {
            self.mode = Mode::AwaitingPermission(next);
            self.modal_scroll = 0;
            // A scope chosen for the PREVIOUS prompt must not leak into
            // this one -- see `Self::permission_grant_scope`'s own doc.
            self.permission_grant_scope = conway::PermissionScope::Session;
            return;
        }
        if let Some(modal) = self.pending_ask_modal.take() {
            self.mode = Mode::AskModal(modal);
            self.modal_scroll = 0;
            return;
        }
        if let Some(card) = self.pending_intent_confirm.take() {
            self.mode = Mode::IntentConfirm(card);
            self.modal_scroll = 0;
            return;
        }
        if let Some(card) = self.pending_trust_preview.take() {
            self.mode = Mode::TrustPreview(card);
            self.modal_scroll = 0;
            return;
        }
        if let Some(ask) = self.pending_ui_form.take() {
            self.mode = Mode::UiForm(UiFormState { ask, selected: 0 });
            self.modal_scroll = 0;
        }
    }

    /// Opens `ask_question`'s modal (board item `01M19NH39AE2D5AMJK0RZRQY86`),
    /// parking it in `pending_ui_form` instead whenever another modal-bearing
    /// surface currently owns `mode` -- mirrors [`Self::offer_trust_preview`]
    /// exactly, the lowest-priority slot in `Self::promote_next_surface`.
    pub fn offer_ui_form(&mut self, ask: PendingFormAsk) {
        if matches!(self.mode, Mode::Normal) {
            self.mode = Mode::UiForm(UiFormState { ask, selected: 0 });
            self.modal_scroll = 0;
        } else {
            self.pending_ui_form = Some(ask);
        }
    }

    /// Drains a question parked in `pending_ui_form`. Used by `app.rs`'s
    /// quit path so a question parked behind another surface when the user
    /// quits does not leave a dangling reply channel -- mirrors
    /// [`Self::take_pending_trust_preview`] exactly: dropping the returned
    /// [`PendingFormAsk`] drops its `oneshot::Sender` half, which is exactly
    /// `TuiFormSurface::ask_select`'s own documented fail-closed fallback
    /// (a `FormSurfaceError` naming `"cancelled"` on a dropped reply
    /// channel) -- the process is exiting either way, so there is nothing
    /// left to answer into. Returns the parked ask if one was waiting, else
    /// `None`; either way `pending_ui_form` is cleared.
    pub fn take_pending_ui_form(&mut self) -> Option<PendingFormAsk> {
        self.pending_ui_form.take()
    }

    /// Moves the highlighted option by `delta` (wrapping), while
    /// `Mode::UiForm` is open. A no-op otherwise. `delta` is typically `1`
    /// (down) or `-1` (up) -- `input::handle_ui_form_key`'s own callers.
    pub fn move_ui_form_selection(&mut self, delta: isize) {
        let Mode::UiForm(form) = &mut self.mode else {
            return;
        };
        let len = form.ask.request.options.len() as isize;
        if len == 0 {
            // Unreachable in practice (an empty-options request is refused
            // before it ever reaches a surface -- see `conway_plugin_ui`'s
            // own `ask` function), but never a divide-by-zero if it somehow
            // were.
            return;
        }
        let current = form.selected as isize;
        let next = (current + delta).rem_euclid(len);
        form.selected = next as usize;
    }

    /// Carries out `ask_question`'s decision (board item
    /// `01M19NH39AE2D5AMJK0RZRQY86`): sends the answer (or a named
    /// cancellation) back over `ask.reply`, the SAME `oneshot` channel the
    /// blocked `TuiFormSurface::ask_select` call is awaiting -- unlike
    /// [`crate::tui::commands::apply_ask_fate`]/[`crate::tui::commands::apply_trust_decision`], this
    /// needs no facade call at all, since answering a question the model
    /// asked is entirely local to this process's own channel. A no-op when
    /// no question is open. Promotes the next parked/queued surface via
    /// `Self::promote_next_surface` afterward, exactly like every other
    /// modal-bearing surface's close path.
    ///
    /// **Returns the chosen option on `Answer`, `None` on `Cancel`/no-open
    /// (board item `01M1A35S609TZ613GAECPEHX8D`).** Added for `/model`
    /// bare's own menu: `run.rs`'s `Action::UiFormDecision` arm reads this
    /// return value to decide whether to also run `commands::
    /// apply_model_switch` (gated on `Self::model_picker_active`, so a
    /// REAL model-raised question -- which never sets that flag -- is
    /// completely unaffected by this addition; its own answer still travels
    /// only over `ask.reply`, exactly as before).
    pub fn resolve_ui_form(&mut self, decision: UiFormDecision) -> Option<String> {
        let Mode::UiForm(_) = &self.mode else {
            return None;
        };
        let Mode::UiForm(form) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            unreachable!("guarded by the matches! check above")
        };
        let answered = match decision {
            UiFormDecision::Answer => {
                let selected = form.ask.request.options[form.selected].clone();
                form.ask.resolve(Ok(conway_plugin_ui::AskSelectAnswer {
                    selected: selected.clone(),
                }));
                Some(selected)
            }
            UiFormDecision::Cancel => {
                form.ask
                    .resolve(Err(conway_plugin_ui::FormSurfaceError::new(
                        "the operator cancelled the question",
                    )));
                None
            }
        };
        self.promote_next_surface();
        answered
    }

    /// Enqueues a freshly arrived prompt from the gate channel, promoting it
    /// to `mode` immediately if nothing is currently showing.
    pub fn offer_prompt(&mut self, prompt: PendingPrompt) {
        if matches!(self.mode, Mode::Normal) {
            self.mode = Mode::AwaitingPermission(prompt);
            // A freshly promoted prompt starts scrolled to the top of its
            // own command -- never carries over wherever a PREVIOUS,
            // unrelated prompt's overlay happened to be scrolled.
            self.modal_scroll = 0;
            // ...and at the DEFAULT grant scope -- a narrower scope chosen
            // for an earlier, unrelated prompt must not silently apply to
            // this one (see `Self::permission_grant_scope`'s own doc).
            self.permission_grant_scope = conway::PermissionScope::Session;
        } else {
            self.queued_prompts.push_back(prompt);
        }
    }

    /// Cycles the scope the prompt's remembered-grant keys (`a`/`p`) grant
    /// at: `Session` -> `Agent` -> `AgentSubtree` -> `Session`. Bound to
    /// the prompt's `s` key in `input.rs`; the overlay states the current
    /// scope in words next to the grant keys so the operator never grants
    /// narrower or broader than they can see.
    pub fn cycle_permission_grant_scope(&mut self) {
        self.permission_grant_scope = match self.permission_grant_scope {
            conway::PermissionScope::Session => conway::PermissionScope::Agent,
            conway::PermissionScope::Agent => conway::PermissionScope::AgentSubtree,
            // `AgentSubtree`, and any future variant (`PermissionScope` is
            // `#[non_exhaustive]`): back to the default. A future scope
            // sorts itself into the cycle only by a deliberate edit here,
            // never by accident.
            _ => conway::PermissionScope::Session,
        };
    }

    /// The agent whose call the current permission prompt is asking about,
    /// if one is pending. This -- NOT `focused_agent` -- is the agent a
    /// per-agent or per-subtree grant must be recorded against: the broker
    /// narrows such a grant to the GRANTING agent's identity, and the call
    /// being decided belongs to the requester in the prompt, which need
    /// not be the agent whose transcript the operator is looking at.
    pub fn pending_permission_agent(&self) -> Option<conway::AgentId> {
        let Mode::AwaitingPermission(pending) = &self.mode else {
            return None;
        };
        Some(pending.request.agent_id)
    }

    /// Resolves the currently-shown prompt (if any) and promotes the next
    /// queued one, if there is one.
    pub fn resolve_current_prompt(&mut self, decision: conway::PermissionDecision) {
        let Mode::AwaitingPermission(_) = &self.mode else {
            return;
        };
        let Mode::AwaitingPermission(prompt) = std::mem::replace(&mut self.mode, Mode::Normal)
        else {
            unreachable!()
        };
        prompt.resolve(decision);
        // C2: the prompt queue drains first (highest priority -- a tool
        // call is waiting on a decision); then a parked `/ask` modal (B5);
        // then a parked intent card (C2). [`Self::promote_next_surface`]
        // encodes that fixed priority order so the three modal-bearing
        // surfaces never stack and never drift out of sync across the
        // close/resolve call sites.
        self.promote_next_surface();
    }

    /// Discards any pending permission prompt belonging to `agent` -- the
    /// currently-showing one (if `mode` is `AwaitingPermission` for THIS
    /// agent) and any behind it in `queued_prompts` (board item
    /// `01M0RWFH6V709B7WTAFRZGFKG3`). Dropping a [`PendingPrompt`] drops its
    /// `oneshot::Sender` half, which is exactly `TuiGate::check`'s own
    /// documented fail-closed fallback (`Deny { reason: "cancelled" }` on a
    /// dropped reply channel) -- **this is what actually frees an agent
    /// parked awaiting the gate's reply.** `SessionHandle::cancel`'s
    /// `CancellationToken` is checked cooperatively at specific points in
    /// the agent loop, and the call site that blocks on a permission
    /// decision (`conway-runtime/src/tools/runner.rs`'s `broker.decide(..)
    /// .await`, BEFORE the `tokio::select!` that later races the tool's own
    /// `invoke` against cancellation) is never one of them -- cancelling an
    /// agent stuck there alone does nothing until its pending prompt is
    /// separately discarded, which is what this method is for. Called from
    /// `App::abandon_ask` before `SessionHandle::cancel`, so an ask child
    /// parked on the gate is not left running.
    ///
    /// A no-op (for the live half) if the currently-showing prompt belongs
    /// to a DIFFERENT agent -- an unrelated permission decision in front of
    /// an abandoned ask is left exactly as it was, still awaiting its own
    /// answer.
    pub fn discard_prompts_for_agent(&mut self, agent: AgentId) {
        if let Mode::AwaitingPermission(prompt) = &self.mode {
            if prompt.request.agent_id == agent {
                self.mode = Mode::Normal;
                self.promote_next_surface();
            }
        }
        self.queued_prompts.retain(|p| p.request.agent_id != agent);
    }

    /// Opens the `/help` keybinding overlay (T7). See [`Self::help_open`]'s
    /// own doc for why this is a plain flag flip rather than a `mode`
    /// transition/park -- `commands.rs`'s `SlashCommand::Help` arm can only
    /// ever reach this while `mode` is already `Normal` (the input line is
    /// inert otherwise), so there is nothing to park against. V1: also
    /// resets `modal_scroll` -- see that field's own doc on why one shared
    /// field serves every modal-bearing surface, `/help` included.
    pub fn open_help(&mut self) {
        self.help_open = true;
        self.modal_scroll = 0;
        // V4: mutually exclusive with the settings menu -- see
        // `Self::settings_open`'s own doc for why both flags clear each
        // other on open rather than relying on check-order to keep at most
        // one of them showing. Board item `01M0VR5RCCB8NDGG2JEQW8X7XR`
        // extends the same rule to the `/plugin` listing.
        self.settings_open = false;
        self.plugins_open = false;
    }

    /// Closes the `/help` keybinding overlay (T7's `Esc` binding, wired in
    /// `input.rs`). A no-op when it is already closed.
    pub fn close_help(&mut self) {
        self.help_open = false;
    }

    /// Opens the `/settings` menu (V4). Mirrors [`Self::open_help`] exactly
    /// -- see [`Self::settings_open`]'s own doc for the full "informational,
    /// gated ahead of `Mode::Normal`, mutually exclusive with `/help`"
    /// reasoning. Deliberately does NOT reset [`Self::settings_selected`] or
    /// [`Self::settings_collapsed_groups`] -- re-opening the menu within the
    /// same session restores wherever the cursor/collapse state was left,
    /// the same way re-opening the `/agents` panel does not reset
    /// `agent_selected`.
    /// V2b: the pattern grant Conway would offer for the pending
    /// permission prompt, if any.
    ///
    /// Shaped by the pending call's own `render_kind` (carried on the
    /// request from the broker -- the same declaration the evaluation side
    /// matched against): a `ShellCommand` tool gets the narrow two-token
    /// prefix, or no offer at all when the command carries shell
    /// metacharacters; a `Structured` tool gets the registerable wildcard
    /// (`tool:*`), the only rule shape F12's registration check admits
    /// against a JSON-dump rendering. See
    /// `permission_pattern::suggested_rule` for the full reasoning.
    pub fn offered_permission_rule(&self) -> Option<conway::PatternRule> {
        let Mode::AwaitingPermission(pending) = &self.mode else {
            return None;
        };
        conway::permission_pattern::suggested_rule(
            pending.request.tool.as_str(),
            &pending.request.rendered,
            pending.request.render_kind,
        )
    }

    pub fn open_settings(&mut self) {
        self.settings_open = true;
        self.help_open = false;
        self.plugins_open = false;
    }

    /// Closes the `/settings` menu (V4's `Esc` binding, wired in
    /// `input.rs`). A no-op when it is already closed. Cursor/collapse
    /// state is left untouched (see [`Self::open_settings`]'s own doc).
    pub fn close_settings(&mut self) {
        self.settings_open = false;
    }

    /// Opens the `/plugin` listing (board item
    /// `01M0VR5RCCB8NDGG2JEQW8X7XR`, `view/plugins.rs`) -- mirrors
    /// [`Self::open_settings`] exactly, including the same three-way mutual
    /// exclusion with `/help` and `/settings`. Reachable two ways: typing
    /// `/plugin` (`commands::SlashCommand::Plugins`), or `Enter` on
    /// `/settings`' own plugins-section shortcut row
    /// (`view::settings::LEAF_OPEN_PLUGINS`) -- the settings menu no longer
    /// implements its own plugin listing; it links into this one instead
    /// (see `view/settings.rs`'s own doc, "Plugins: one home, not two").
    pub fn open_plugins(&mut self) {
        self.plugins_open = true;
        self.help_open = false;
        self.settings_open = false;
    }

    /// Closes the `/plugin` listing. A no-op when it is already closed.
    /// Cursor state ([`Self::plugins_selected`]) is left untouched, mirroring
    /// [`Self::close_settings`]'s own "re-opening restores where the cursor
    /// was left" behaviour.
    pub fn close_plugins(&mut self) {
        self.plugins_open = false;
    }

    /// Opens the settings providers section's credential prompt (board item
    /// `01M11XWB4T8ZADNDB4M8R482MA`) -- only from `Mode::Normal`, mirroring
    /// [`Self::offer_ask_modal`]'s own guard, though in practice the ONE
    /// caller (`App::apply_add_provider_choice`) only ever reaches this
    /// while `mode` is already `Normal` (the settings menu itself is only
    /// reachable there). A no-op otherwise, rather than clobbering whatever
    /// modal-bearing surface currently owns `mode` -- an operator's typed
    /// keystroke pattern-matched as an add-provider choice must never
    /// silently steal the floor from an unrelated pending decision.
    pub fn begin_add_provider_credential(
        &mut self,
        choice_id: &str,
        label: &str,
        credential_env: &str,
    ) {
        if !matches!(self.mode, Mode::Normal) {
            return;
        }
        self.mode = Mode::AddProviderCredential(AddProviderCredentialState {
            choice_id: choice_id.to_string(),
            label: label.to_string(),
            credential_env: credential_env.to_string(),
            input: String::new(),
            cursor: 0,
            error: None,
        });
        self.modal_scroll = 0;
    }

    /// Closes the credential prompt with NO write -- `Esc`, or after a
    /// successful `Enter` has already extracted the validated secret (the
    /// caller, `input::handle_add_provider_credential_key`, reads `input`
    /// out before calling this). Promotes the next parked/queued surface
    /// exactly like every other modal-bearing surface's close path. A no-op
    /// when the mode is something else.
    pub fn close_add_provider_credential(&mut self) {
        if !matches!(self.mode, Mode::AddProviderCredential(_)) {
            return;
        }
        self.mode = Mode::Normal;
        self.promote_next_surface();
    }

    /// Opens the "deny with feedback" text entry from a permission prompt
    /// (board item `01M1A9M2EVJNR0HBN86A8E40EA`) -- mirrors
    /// [`Self::offer_editing_pattern`] exactly: only callable while a prompt
    /// is showing (a no-op otherwise), and the [`PendingPrompt`] is MOVED out
    /// of `mode` into [`DenyFeedbackState`] (it is not `Clone`), so the
    /// prompt is not lost -- cancel restores it, submit resolves it.
    pub fn offer_deny_feedback(&mut self) {
        if !matches!(self.mode, Mode::AwaitingPermission(_)) {
            return;
        }
        let Mode::AwaitingPermission(prompt) = std::mem::replace(&mut self.mode, Mode::Normal)
        else {
            unreachable!()
        };
        self.mode = Mode::EditingDenyFeedback(DenyFeedbackState {
            prompt,
            input: String::new(),
            cursor: 0,
        });
        self.modal_scroll = 0;
    }

    /// Cancels the feedback entry and returns the prompt to the screen
    /// unresolved -- the operator can press `y`/`a`/`n`/`p`/`Esc` again.
    /// Mirrors [`Self::cancel_editing_pattern`] exactly: unlike
    /// [`Self::submit_deny_feedback`], this makes NO decision at all.
    pub fn cancel_deny_feedback(&mut self) {
        if !matches!(self.mode, Mode::EditingDenyFeedback(_)) {
            return;
        }
        let Mode::EditingDenyFeedback(fb) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            unreachable!()
        };
        self.mode = Mode::AwaitingPermission(fb.prompt);
        self.modal_scroll = 0;
    }

    /// Submits the feedback entry: restores the prompt to
    /// `AwaitingPermission` (so the app loop's EXISTING
    /// `Action::PermissionDecision` arm resolves it via
    /// `Self::resolve_current_prompt`, mirroring [`Self::
    /// submit_editing_pattern`]'s own "restore, then let the dispatch arm
    /// resolve" shape) and returns the message to send: the typed text,
    /// trimmed, or [`DEFAULT_DENY_FEEDBACK`] when the operator typed nothing
    /// at all -- a bare `Esc`-then-`Enter` still reproduces exactly the old
    /// one-keystroke behavior, one keystroke later. Returns `None` if no
    /// feedback entry is open.
    pub fn submit_deny_feedback(&mut self) -> Option<String> {
        if !matches!(self.mode, Mode::EditingDenyFeedback(_)) {
            return None;
        }
        let Mode::EditingDenyFeedback(fb) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            unreachable!()
        };
        let trimmed = fb.input.trim();
        let message = if trimmed.is_empty() {
            DEFAULT_DENY_FEEDBACK.to_string()
        } else {
            trimmed.to_string()
        };
        self.mode = Mode::AwaitingPermission(fb.prompt);
        self.modal_scroll = 0;
        Some(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway::ToolName;

    fn ask_modal(question: &str) -> AskModal {
        AskModal {
            question: question.to_string(),
            child: AgentId::new(),
            answer: "the answer".to_string(),
            error: None,
        }
    }

    fn permission_prompt(rendered: &str) -> crate::tui::gate::PendingPrompt {
        let (prompt, _rx) =
            crate::tui::gate::PendingPrompt::new_for_test(conway::PermissionRequest {
                agent_id: AgentId::new(),
                agent_path: Vec::new(),
                tool: ToolName::new("bash"),
                category: conway::ToolCategory::Execute,
                arguments: serde_json::json!({}),
                rendered: rendered.to_string(),
                call_id: "tc_1".to_string(),
                render_kind: conway::RenderKind::ShellCommand,
            });
        prompt
    }

    #[test]
    fn offer_ask_modal_opens_immediately_in_normal_mode() {
        let mut state = AppState::new(AgentId::new());
        assert!(matches!(state.mode, Mode::Normal));

        state.offer_ask_modal(ask_modal("q"));

        assert!(
            matches!(&state.mode, Mode::AskModal(m) if m.question == "q" && m.error.is_none()),
            "the modal must open immediately, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn offer_ask_modal_parks_behind_a_permission_prompt_and_opens_once_it_resolves() {
        let mut state = AppState::new(AgentId::new());
        state.offer_prompt(permission_prompt("bash: ls"));
        assert!(matches!(state.mode, Mode::AwaitingPermission(_)));

        state.offer_ask_modal(ask_modal("q"));

        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "the permission prompt must keep the floor; the modal parks, got: {:?}",
            state.mode
        );

        state.resolve_current_prompt(conway::PermissionDecision::AllowOnce);

        assert!(
            matches!(&state.mode, Mode::AskModal(m) if m.question == "q"),
            "the parked modal must open once the prompt queue drains, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn take_pending_ask_modal_drains_a_parked_modal_and_clears_the_slot() {
        // M1's quit-path fix: `purge_open_ask_modal` must be able to reach a
        // modal that was parked behind a permission prompt when the ask
        // completed, or its child leaks as residue. `take_pending_ask_modal`
        // is the accessor that lets app.rs drain it without `pending_ask_modal`
        // being pub.
        let mut state = AppState::new(AgentId::new());
        state.offer_prompt(permission_prompt("bash: ls"));
        state.offer_ask_modal(ask_modal("parked"));

        let drained = state.take_pending_ask_modal();
        assert!(
            matches!(&drained, Some(m) if m.question == "parked"),
            "the parked modal must be returned, got: {drained:?}"
        );
        assert!(
            state.take_pending_ask_modal().is_none(),
            "the slot must be cleared after the take"
        );
        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "take must NOT clobber the surface currently owning `mode`"
        );
    }

    #[test]
    fn take_pending_ask_modal_is_none_when_nothing_is_parked() {
        let mut state = AppState::new(AgentId::new());
        assert!(state.take_pending_ask_modal().is_none());
        // An open (live) modal is in `mode`, not in the parking slot.
        state.offer_ask_modal(ask_modal("live"));
        assert!(
            state.take_pending_ask_modal().is_none(),
            "a live modal is not parked -- take must return None"
        );
    }

    #[test]
    fn close_ask_modal_returns_to_normal() {
        let mut state = AppState::new(AgentId::new());
        state.offer_ask_modal(ask_modal("q"));

        state.close_ask_modal();

        assert!(matches!(state.mode, Mode::Normal));
    }

    #[test]
    fn close_ask_modal_promotes_a_prompt_queued_while_the_modal_was_open() {
        let mut state = AppState::new(AgentId::new());
        state.offer_ask_modal(ask_modal("q"));
        // A permission request arrives WHILE the modal owns the floor --
        // `offer_prompt` queues it, exactly as behind another prompt.
        state.offer_prompt(permission_prompt("bash: ls"));
        assert!(matches!(state.mode, Mode::AskModal(_)));

        state.close_ask_modal();

        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "closing the modal must promote the queued prompt, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn fail_ask_modal_keeps_the_modal_open_with_the_error_shown() {
        let mut state = AppState::new(AgentId::new());
        state.offer_ask_modal(ask_modal("q"));

        state.fail_ask_modal("pull_in refused".to_string());

        match &state.mode {
            Mode::AskModal(m) => {
                assert_eq!(m.error.as_deref(), Some("pull_in refused"));
                assert_eq!(m.question, "q", "the modal's content is untouched");
            }
            other => panic!("a failed fate must KEEP the modal open, got: {other:?}"),
        }
    }

    fn intent_card(prompt: &str) -> IntentConfirm {
        IntentConfirm {
            intent: AgentIntent {
                recipe: SubagentMode::Spawn,
                agent_def: None,
                prompt: prompt.to_string(),
            },
            default_recipe: SubagentMode::Spawn,
            raw_text: prompt.to_string(),
            parent: AgentId::new(),
        }
    }

    #[test]
    fn offer_intent_confirm_opens_immediately_in_normal_mode() {
        let mut state = AppState::new(AgentId::new());
        assert!(matches!(state.mode, Mode::Normal));

        state.offer_intent_confirm(intent_card("refactor the parser"));

        assert!(
            matches!(&state.mode, Mode::IntentConfirm(ic) if ic.intent.prompt == "refactor the parser"),
            "the card must open immediately, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn offer_intent_confirm_parks_behind_a_permission_prompt_and_opens_once_it_resolves() {
        let mut state = AppState::new(AgentId::new());
        state.offer_prompt(permission_prompt("bash: ls"));
        assert!(matches!(state.mode, Mode::AwaitingPermission(_)));

        state.offer_intent_confirm(intent_card("parked"));

        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "the permission prompt must keep the floor; the card parks, got: {:?}",
            state.mode
        );

        state.resolve_current_prompt(conway::PermissionDecision::AllowOnce);

        assert!(
            matches!(&state.mode, Mode::IntentConfirm(ic) if ic.intent.prompt == "parked"),
            "the parked card must open once the prompt queue drains, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn offer_intent_confirm_parks_behind_an_ask_modal_and_opens_once_it_closes() {
        // The three modal-bearing surfaces never stack: an intent card
        // arriving while an /ask modal owns the floor parks in
        // `pending_intent_confirm`, and `close_ask_modal` promotes it via
        // `promote_next_surface`.
        let mut state = AppState::new(AgentId::new());
        state.offer_ask_modal(ask_modal("q"));
        assert!(matches!(state.mode, Mode::AskModal(_)));

        state.offer_intent_confirm(intent_card("parked-behind-ask"));

        assert!(
            matches!(state.mode, Mode::AskModal(_)),
            "the ask modal must keep the floor; the card parks, got: {:?}",
            state.mode
        );

        state.close_ask_modal();

        assert!(
            matches!(&state.mode, Mode::IntentConfirm(ic) if ic.intent.prompt == "parked-behind-ask"),
            "the parked card must open once the ask modal closes, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn take_pending_intent_confirm_drains_a_parked_card_and_clears_the_slot() {
        let mut state = AppState::new(AgentId::new());
        state.offer_prompt(permission_prompt("bash: ls"));
        state.offer_intent_confirm(intent_card("parked"));

        let drained = state.take_pending_intent_confirm();
        assert!(
            matches!(&drained, Some(ic) if ic.intent.prompt == "parked"),
            "the parked card must be returned, got: {drained:?}"
        );
        assert!(
            state.take_pending_intent_confirm().is_none(),
            "the slot must be cleared after the take"
        );
        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "take must NOT clobber the surface currently owning `mode`"
        );
    }

    #[test]
    fn close_intent_confirm_returns_to_normal() {
        let mut state = AppState::new(AgentId::new());
        state.offer_intent_confirm(intent_card("q"));

        state.close_intent_confirm();

        assert!(matches!(state.mode, Mode::Normal));
    }

    #[test]
    fn close_intent_confirm_promotes_a_prompt_queued_while_the_card_was_open() {
        let mut state = AppState::new(AgentId::new());
        state.offer_intent_confirm(intent_card("q"));
        state.offer_prompt(permission_prompt("bash: ls"));
        assert!(matches!(state.mode, Mode::IntentConfirm(_)));

        state.close_intent_confirm();

        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "closing the card must promote the queued prompt, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn close_intent_confirm_promotes_a_parked_ask_modal() {
        // Priority order: queued prompts, then parked ask modal, then
        // parked intent card. With no queued prompt, closing the card
        // promotes a parked ask modal.
        let mut state = AppState::new(AgentId::new());
        state.offer_intent_confirm(intent_card("q"));
        // Park an ask behind the card (offer_ask_modal parks when mode !=
        // Normal).
        state.offer_ask_modal(ask_modal("parked-ask"));
        assert!(matches!(state.mode, Mode::IntentConfirm(_)));

        state.close_intent_confirm();

        assert!(
            matches!(&state.mode, Mode::AskModal(m) if m.question == "parked-ask"),
            "closing the card must promote the parked ask modal, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn begin_intent_confirm_edit_drops_the_classified_prompt_into_the_input_line() {
        let mut state = AppState::new(AgentId::new());
        state.input = "stale text".to_string();
        state.cursor = state.input.chars().count();
        state.offer_intent_confirm(IntentConfirm {
            intent: AgentIntent {
                recipe: SubagentMode::Spawn,
                agent_def: Some("reviewer".to_string()),
                prompt: "review the diff carefully".to_string(),
            },
            default_recipe: SubagentMode::Spawn,
            raw_text: "review the diff".to_string(),
            parent: AgentId::new(),
        });

        state.begin_intent_confirm_edit();

        assert_eq!(
            state.input, "review the diff carefully",
            "the classified prompt (not the raw text) must land in the input line"
        );
        assert_eq!(
            state.cursor,
            state.input.chars().count(),
            "the cursor must be at the end of the dropped prompt"
        );
        assert!(
            matches!(state.mode, Mode::Normal),
            "the card must close after edit, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn begin_intent_confirm_edit_is_a_noop_when_no_card_is_open() {
        let mut state = AppState::new(AgentId::new());
        state.input = "keep me".to_string();

        state.begin_intent_confirm_edit();

        assert_eq!(
            state.input, "keep me",
            "a no-card edit must not touch the input line"
        );
        assert!(matches!(state.mode, Mode::Normal));
    }

    fn trust_card(path: &str) -> TrustPreviewCard {
        TrustPreviewCard {
            path: std::path::PathBuf::from(path),
            contents: "{}".to_string(),
            status: conway::TrustStatus::New,
            error: None,
        }
    }

    #[test]
    fn offer_trust_preview_opens_immediately_in_normal_mode() {
        let mut state = AppState::new(AgentId::new());
        assert!(matches!(state.mode, Mode::Normal));

        state.offer_trust_preview(trust_card("/repo/.conway/permissions.json"));

        assert!(
            matches!(&state.mode, Mode::TrustPreview(c) if c.path.display().to_string() == "/repo/.conway/permissions.json"),
            "the card must open immediately, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn offer_trust_preview_parks_behind_a_permission_prompt_and_opens_once_it_resolves() {
        let mut state = AppState::new(AgentId::new());
        state.offer_prompt(permission_prompt("bash: ls"));
        assert!(matches!(state.mode, Mode::AwaitingPermission(_)));

        state.offer_trust_preview(trust_card("/parked"));

        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "the permission prompt must keep the floor; the card parks, got: {:?}",
            state.mode
        );

        state.resolve_current_prompt(conway::PermissionDecision::AllowOnce);

        assert!(
            matches!(&state.mode, Mode::TrustPreview(c) if c.path.display().to_string() == "/parked"),
            "the parked card must open once the prompt queue drains, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn offer_trust_preview_parks_behind_an_intent_confirm_card_and_opens_once_it_closes() {
        // None of the four modal-bearing surfaces ever stack: a trust
        // preview arriving while the intent-confirm card owns the floor
        // parks in `pending_trust_preview`, and `close_intent_confirm`
        // promotes it via `promote_next_surface` (the lowest-priority slot,
        // checked last).
        let mut state = AppState::new(AgentId::new());
        state.offer_intent_confirm(intent_card("q"));
        assert!(matches!(state.mode, Mode::IntentConfirm(_)));

        state.offer_trust_preview(trust_card("/parked-behind-intent"));

        assert!(
            matches!(state.mode, Mode::IntentConfirm(_)),
            "the intent card must keep the floor; the trust card parks, got: {:?}",
            state.mode
        );

        state.close_intent_confirm();

        assert!(
            matches!(&state.mode, Mode::TrustPreview(c) if c.path.display().to_string() == "/parked-behind-intent"),
            "the parked trust card must open once the intent card closes, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn take_pending_trust_preview_drains_a_parked_card_and_clears_the_slot() {
        let mut state = AppState::new(AgentId::new());
        state.offer_prompt(permission_prompt("bash: ls"));
        state.offer_trust_preview(trust_card("/parked"));

        let drained = state.take_pending_trust_preview();
        assert!(
            matches!(&drained, Some(c) if c.path.display().to_string() == "/parked"),
            "the parked card must be returned, got: {drained:?}"
        );
        assert!(
            state.take_pending_trust_preview().is_none(),
            "the slot must be cleared after the take"
        );
        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "take must NOT clobber the surface currently owning `mode`"
        );
    }

    #[test]
    fn close_trust_preview_returns_to_normal() {
        let mut state = AppState::new(AgentId::new());
        state.offer_trust_preview(trust_card("/repo/.conway/permissions.json"));

        state.close_trust_preview();

        assert!(matches!(state.mode, Mode::Normal));
    }

    #[test]
    fn close_trust_preview_promotes_a_prompt_queued_while_the_card_was_open() {
        let mut state = AppState::new(AgentId::new());
        state.offer_trust_preview(trust_card("/repo/.conway/permissions.json"));
        state.offer_prompt(permission_prompt("bash: ls"));
        assert!(matches!(state.mode, Mode::TrustPreview(_)));

        state.close_trust_preview();

        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "closing the card must promote the queued prompt, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn fail_trust_preview_keeps_the_card_open_with_the_error_shown() {
        let mut state = AppState::new(AgentId::new());
        state.offer_trust_preview(trust_card("/repo/.conway/permissions.json"));

        state.fail_trust_preview(
            "could not trust /repo/.conway/permissions.json: denied".to_string(),
        );

        match &state.mode {
            Mode::TrustPreview(c) => {
                assert_eq!(
                    c.error.as_deref(),
                    Some("could not trust /repo/.conway/permissions.json: denied")
                );
                assert_eq!(
                    c.path.display().to_string(),
                    "/repo/.conway/permissions.json",
                    "the card's content is untouched"
                );
            }
            other => panic!("a failed confirm must KEEP the card open, got: {other:?}"),
        }
    }

    #[test]
    fn open_settings_and_help_are_mutually_exclusive() {
        let mut state = AppState::new(AgentId::new());
        state.open_help();
        assert!(state.help_open);

        state.open_settings();
        assert!(state.settings_open, "/settings must open");
        assert!(!state.help_open, "opening settings must close help");

        state.open_help();
        assert!(state.help_open, "/help must open");
        assert!(!state.settings_open, "opening help must close settings");
    }

    #[test]
    fn close_settings_is_a_noop_when_already_closed() {
        let mut state = AppState::new(AgentId::new());
        assert!(!state.settings_open);
        state.close_settings();
        assert!(!state.settings_open);
    }

    /// Board item `01M0VR5RCCB8NDGG2JEQW8X7XR`: the `/plugin` listing joins
    /// `/help`/`/settings`' own three-way mutual exclusion -- opening any
    /// one of the three closes the other two, in every direction, not just
    /// the two directions the pre-existing test above already covered.
    #[test]
    fn open_plugins_help_and_settings_are_mutually_exclusive_three_ways() {
        let mut state = AppState::new(AgentId::new());

        state.open_plugins();
        assert!(state.plugins_open, "/plugin must open");

        state.open_help();
        assert!(state.help_open, "/help must open");
        assert!(!state.plugins_open, "opening help must close /plugin");

        state.open_settings();
        assert!(state.settings_open, "/settings must open");
        assert!(!state.help_open, "opening settings must close help");
        assert!(!state.plugins_open, "opening settings must close /plugin");

        state.open_plugins();
        assert!(state.plugins_open, "/plugin must open");
        assert!(!state.settings_open, "opening /plugin must close settings");
        assert!(!state.help_open, "opening /plugin must close help");
    }

    #[test]
    fn close_plugins_is_a_noop_when_already_closed() {
        let mut state = AppState::new(AgentId::new());
        assert!(!state.plugins_open);
        state.close_plugins();
        assert!(!state.plugins_open);
    }

    // -----------------------------------------------------------------
    // Board item `01M19NH39AE2D5AMJK0RZRQY86`: `ask_question`'s modal --
    // the FIFTH modal-bearing surface joining the SAME never-stack
    // discipline every test above already pins for the other four.
    // -----------------------------------------------------------------

    fn ui_form_ask(prompt: &str) -> PendingFormAsk {
        let (ask, _reply_rx) = PendingFormAsk::new_for_test(conway_plugin_ui::AskSelectRequest {
            prompt: prompt.to_string(),
            options: vec!["yes".to_string(), "no".to_string()],
        });
        ask
    }

    #[test]
    fn offer_ui_form_opens_immediately_in_normal_mode() {
        let mut state = AppState::new(AgentId::new());
        assert!(matches!(state.mode, Mode::Normal));

        state.offer_ui_form(ui_form_ask("q"));

        assert!(
            matches!(&state.mode, Mode::UiForm(f) if f.ask.request.prompt == "q" && f.selected == 0),
            "the modal must open immediately, got: {:?}",
            state.mode
        );
    }

    /// **VERIFICATION ANCHOR, acceptance 4, board item
    /// `01M19NH39AE2D5AMJK0RZRQY86`.** A question raised while a permission
    /// prompt is up PARKS, rather than stacking -- and is promoted, per the
    /// existing discipline, once the prompt resolves.
    #[test]
    fn offer_ui_form_parks_behind_a_permission_prompt_and_opens_once_it_resolves() {
        let mut state = AppState::new(AgentId::new());
        state.offer_prompt(permission_prompt("bash: ls"));
        assert!(matches!(state.mode, Mode::AwaitingPermission(_)));

        state.offer_ui_form(ui_form_ask("parked"));

        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "the permission prompt must keep the floor; the question parks, got: {:?}",
            state.mode
        );

        state.resolve_current_prompt(conway::PermissionDecision::AllowOnce);

        assert!(
            matches!(&state.mode, Mode::UiForm(f) if f.ask.request.prompt == "parked"),
            "the parked question must open once the prompt queue drains, got: {:?}",
            state.mode
        );
    }

    /// Same "never stack" proof, against the `/ask` modal specifically --
    /// the FIRST of the four pre-existing surfaces, and the one whose own
    /// module doc this item's spec pointed at as the precedent to read.
    #[test]
    fn offer_ui_form_parks_behind_an_ask_modal_and_opens_once_it_closes() {
        let mut state = AppState::new(AgentId::new());
        state.offer_ask_modal(ask_modal("q"));
        assert!(matches!(state.mode, Mode::AskModal(_)));

        state.offer_ui_form(ui_form_ask("parked-behind-ask"));

        assert!(
            matches!(state.mode, Mode::AskModal(_)),
            "the ask modal must keep the floor; the question parks, got: {:?}",
            state.mode
        );

        state.close_ask_modal();

        assert!(
            matches!(&state.mode, Mode::UiForm(f) if f.ask.request.prompt == "parked-behind-ask"),
            "the parked question must open once the ask modal closes, got: {:?}",
            state.mode
        );
    }

    /// Same proof against the intent-confirm card.
    #[test]
    fn offer_ui_form_parks_behind_an_intent_confirm_card_and_opens_once_it_closes() {
        let mut state = AppState::new(AgentId::new());
        state.offer_intent_confirm(intent_card("q"));
        assert!(matches!(state.mode, Mode::IntentConfirm(_)));

        state.offer_ui_form(ui_form_ask("parked-behind-intent"));

        assert!(
            matches!(state.mode, Mode::IntentConfirm(_)),
            "the intent card must keep the floor; the question parks, got: {:?}",
            state.mode
        );

        state.close_intent_confirm();

        assert!(
            matches!(&state.mode, Mode::UiForm(f) if f.ask.request.prompt == "parked-behind-intent"),
            "the parked question must open once the intent card closes, got: {:?}",
            state.mode
        );
    }

    /// Same proof against the trust-preview card -- the lowest-priority of
    /// the four pre-existing surfaces, checked last, so this is also the
    /// discriminating test that `ask_question`'s own park slot is checked
    /// AFTER it, not before (priority order 5, per `promote_next_surface`'s
    /// own doc).
    #[test]
    fn offer_ui_form_parks_behind_a_trust_preview_card_and_opens_once_it_closes() {
        let mut state = AppState::new(AgentId::new());
        state.offer_trust_preview(trust_card("/repo/.conway/permissions.json"));
        assert!(matches!(state.mode, Mode::TrustPreview(_)));

        state.offer_ui_form(ui_form_ask("parked-behind-trust"));

        assert!(
            matches!(state.mode, Mode::TrustPreview(_)),
            "the trust-preview card must keep the floor; the question parks, got: {:?}",
            state.mode
        );

        state.close_trust_preview();

        assert!(
            matches!(&state.mode, Mode::UiForm(f) if f.ask.request.prompt == "parked-behind-trust"),
            "the parked question must open once the trust-preview card closes, got: {:?}",
            state.mode
        );
    }

    /// The reverse direction: a question already open keeps the floor, and
    /// a LATER permission prompt queues behind it rather than stealing it --
    /// mirrors `close_ask_modal_promotes_a_prompt_queued_while_the_modal_
    /// was_open`'s own proof for the `/ask` modal.
    #[test]
    fn a_permission_prompt_arriving_while_ui_form_is_open_queues_and_is_promoted_on_answer() {
        let mut state = AppState::new(AgentId::new());
        state.offer_ui_form(ui_form_ask("q"));
        state.offer_prompt(permission_prompt("bash: ls"));
        assert!(
            matches!(state.mode, Mode::UiForm(_)),
            "the question must keep the floor; the prompt queues, got: {:?}",
            state.mode
        );

        state.resolve_ui_form(UiFormDecision::Answer);

        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "answering must promote the queued prompt, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn take_pending_ui_form_drains_a_parked_question_and_clears_the_slot() {
        let mut state = AppState::new(AgentId::new());
        state.offer_prompt(permission_prompt("bash: ls"));
        state.offer_ui_form(ui_form_ask("parked"));

        let drained = state.take_pending_ui_form();
        let drained_prompt = drained.as_ref().map(|ask| ask.request.prompt.clone());
        assert_eq!(
            drained_prompt.as_deref(),
            Some("parked"),
            "the parked question must be returned"
        );
        assert!(
            state.take_pending_ui_form().is_none(),
            "the slot must be cleared after the take"
        );
        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "take must NOT clobber the surface currently owning `mode`"
        );
    }

    #[test]
    fn take_pending_ui_form_is_none_when_nothing_is_parked() {
        let mut state = AppState::new(AgentId::new());
        assert!(state.take_pending_ui_form().is_none());
        // A live (open) question is in `mode`, not in the parking slot.
        state.offer_ui_form(ui_form_ask("live"));
        assert!(
            state.take_pending_ui_form().is_none(),
            "a live question is not parked -- take must return None"
        );
    }

    #[test]
    fn move_ui_form_selection_wraps_in_both_directions() {
        let mut state = AppState::new(AgentId::new());
        state.offer_ui_form(ui_form_ask("q")); // two options: yes, no
        assert!(matches!(&state.mode, Mode::UiForm(f) if f.selected == 0));

        state.move_ui_form_selection(1);
        assert!(matches!(&state.mode, Mode::UiForm(f) if f.selected == 1));

        // Wraps forward past the end back to the start.
        state.move_ui_form_selection(1);
        assert!(matches!(&state.mode, Mode::UiForm(f) if f.selected == 0));

        // Wraps backward past the start to the end.
        state.move_ui_form_selection(-1);
        assert!(matches!(&state.mode, Mode::UiForm(f) if f.selected == 1));
    }

    #[test]
    fn move_ui_form_selection_is_a_noop_when_no_question_is_open() {
        let mut state = AppState::new(AgentId::new());
        state.move_ui_form_selection(1);
        assert!(matches!(state.mode, Mode::Normal));
    }

    /// **VERIFICATION ANCHOR, acceptance 1 (TUI half of "receives the
    /// chosen answer").** Answering sends the SELECTED option (never index
    /// 0, never the request's own default) back over the reply channel, and
    /// closes the modal.
    #[tokio::test]
    async fn resolve_ui_form_answer_sends_the_selected_option_and_closes() {
        let mut state = AppState::new(AgentId::new());
        let (ask, reply_rx) = PendingFormAsk::new_for_test(conway_plugin_ui::AskSelectRequest {
            prompt: "proceed?".to_string(),
            options: vec!["yes".to_string(), "no".to_string()],
        });
        state.offer_ui_form(ask);
        state.move_ui_form_selection(1); // -> "no"

        state.resolve_ui_form(UiFormDecision::Answer);

        assert!(
            matches!(state.mode, Mode::Normal),
            "answering must close the modal"
        );
        let answer = reply_rx
            .await
            .expect("the reply sender is alive")
            .expect("Answer must resolve Ok");
        assert_eq!(
            answer.selected, "no",
            "the answer must be the option the operator actually selected, not the default"
        );
    }

    #[tokio::test]
    async fn resolve_ui_form_cancel_sends_a_named_refusal_and_closes() {
        let mut state = AppState::new(AgentId::new());
        let (ask, reply_rx) = PendingFormAsk::new_for_test(conway_plugin_ui::AskSelectRequest {
            prompt: "proceed?".to_string(),
            options: vec!["yes".to_string(), "no".to_string()],
        });
        state.offer_ui_form(ask);

        state.resolve_ui_form(UiFormDecision::Cancel);

        assert!(
            matches!(state.mode, Mode::Normal),
            "cancelling must close the modal"
        );
        let err = reply_rx
            .await
            .expect("the reply sender is alive")
            .expect_err("Cancel must resolve Err");
        assert_eq!(err.message, "the operator cancelled the question");
    }

    #[test]
    fn resolve_ui_form_is_a_noop_when_no_question_is_open() {
        let mut state = AppState::new(AgentId::new());
        state.resolve_ui_form(UiFormDecision::Cancel);
        assert!(matches!(state.mode, Mode::Normal));
    }
}
