//! The first-run guided-setup flow -- board item `01M11XVEHNMYY942JE63F7MAFH`.
//!
//! When `conway::backend_usability::FleetUsability::should_offer_guided_setup`
//! says nothing usable is configured, this module replaces the old hard
//! `"no backends configured"` error with a short, interactive fix: detect a
//! local provider already running, offer it in one keypress, otherwise ask
//! for one of the [`HOSTED_CHOICES`] and a credential, save it
//! (`conway::config::set_backend_provider`) AND give it a place in the
//! `default_role` routing chain (`conway::config::ensure_default_role` +
//! `set_role_chain` -- see [`finish_setup`]'s own doc for why a backend
//! entry alone left every guided run unable to route, board item
//! `01M1A2HKMDGNK961ZFV1EGZDQ0`), prove it with one real completion, offer
//! to add another, and get out of the way.
//!
//! **Once a provider is configured, this module also installs conway's own
//! opinion set** (board item `01M1FS34GNZEVZP4ZBVC90VD6J`, decision
//! `01M1FQFP5D0R3M9GC8R8Z24F5N`, 2026-09-01): before this, a fresh operator
//! walked away with a model that carried no system prompt, no loop guard,
//! no memory, and no shell, with nothing on screen saying so.
//! [`apply_opinion_set`] writes [`crate::first_party_plugins::
//! DEFAULT_OPINION_SET`] into `plugins.install`, [`offer_opinion_set_and_
//! shell`] prints exactly what was installed and how to remove any single
//! piece, then asks one plain yes/no about the one member of that set that
//! is not merely an opinion but a genuine widening of what a session can
//! reach -- the bash shell tool ([`apply_shell_choice`]).
//!
//! # Appetite, restated here because it is easy to over-build this
//!
//! **Detect, offer, verify, get out of the way.** No model-pinning
//! question, no roles/fallback-chain QUESTION -- exactly the local-or-
//! hosted-choices menu below, one credential prompt at most, one verify,
//! one "add another?" prompt after each success. The flow still never asks
//! an operator to design a chain: order added IS the chain order, decided
//! by what was just done, not by a fourth question put to them. Writing a
//! chain that actually routes is not new scope this appetite forbids -- it
//! is the ONE thing "verify, get out of the way" already promised and
//! board item `01M1A2HKMDGNK961ZFV1EGZDQ0` found this module failing to
//! deliver: the verify step proved a chain shaped exactly like this works,
//! then nothing preserved that shape on disk. [`ProviderChoice`]'s own doc
//! states which hosted choices are admissible here, and why that is a
//! lower bar than "closed at two forever".
//!
//! This budget is `run_backend_setup`'s own -- the detect/offer/verify/
//! add-another loop. The opinion-set install and the shell yes/no
//! (`offer_opinion_set_and_shell`, above) are a separate, later step,
//! ruled separately (board item `01M1FS34GNZEVZP4ZBVC90VD6J`) and run
//! exactly once per whole `run_guided_setup` call, never once per provider
//! -- see that function's own doc for why.
//!
//! # How this is structured for testability without a terminal
//!
//! Everything a test can assert on without driving a real TTY lives in pure
//! functions in the first half of this file: [`resolve_credential_plan`],
//! [`validate_credential_input`], [`validate_context_window_input`],
//! [`context_window_is_verified`], [`backend_entry_json`],
//! [`local_offer_entry_json`], [`non_interactive_guidance`], [`chain_entry`],
//! `decline_or_keep`. Only the very
//! last function, [`run_guided_setup`], touches a real terminal (via
//! `crossterm` raw-mode reads) -- it is a thin imperative shell over the
//! pure functions above and is not, and cannot be, exercised by this crate's
//! own `assert_cmd` suite (no pty is available and none is added -- C-04).
//! [`verify_backend`] and [`detect_local_provider`] are async and touch the
//! network/filesystem, but neither one touches a terminal, so both are
//! covered by ordinary `#[tokio::test]`s against a real mock HTTP server
//! (`crates/conway-cli/tests/first_run.rs`), the same shape
//! `tests/common/mock_backend.rs` already provides for the one-shot suite.
//! [`discover_setup_context_window`] (board item: context-window
//! declaration honesty, num_ctx) joins that same bucket -- async, network
//! only, no terminal -- covered by `#[tokio::test]`s in this file's own
//! test module against a wiremock server standing in for Ollama's native
//! `/api/show`. [`context_window_setup_notice`] is pure string formatting,
//! tested alongside the other pure functions above; [`context_window_is_
//! verified`] and [`persist_context_window`] join it -- the former reads
//! only compile-time-embedded profile data, the latter touches disk (via
//! `conway::config::metadata_path_for`/`set_context_window`) but no
//! terminal or network, so both are covered directly, no pty needed.
//! `ask_and_persist_context_window` (the setup-time ASK half of "discover,
//! or ask if discovery fails") is the one new function in THIS pair that
//! DOES touch a terminal (`read_plain_line`) -- it joins [`run_guided_setup`]
//! in the untestable-without-a-pty bucket; [`validate_context_window_input`]
//! is what a test asserts on instead, the same split
//! [`validate_credential_input`]/`read_secret_line` already established.
//!
//! # The setup-time context-window PERSIST half -- file/scope decision
//!
//! Board item (setup-time context window, ASK + PERSIST): a prior item
//! built DISCOVER only and disclosed the rest rather than faking it (see
//! [`discover_setup_context_window`]'s own doc, "Deliberately does NOT
//! persist the result itself"). This item closes both gaps:
//! `ask_and_persist_context_window` (this file) / `tui::app::
//! provider_manage`'s `Mode::AddProviderContextWindow` (the TUI's own
//! equivalent surface, since a raw-terminal read here would fight
//! ratatui's screen control) is ASK; [`persist_context_window`] (this file)
//! -> `conway::config::metadata_path_for` -> `conway::config::
//! set_context_window` is PERSIST, shared by both a successful discovery
//! AND a typed answer, and by both setup entrances. **The file/scope
//! decision itself -- which `models.json`, in which scope -- is recorded
//! once, in `conway::config::merge::metadata_path_for`'s own doc, not
//! restated here**: it writes into whichever `models.json` the CURRENT
//! process would actually read back for its own `cwd`, honoring any
//! existing `[models].metadata_path` override rather than ever picking a
//! new location of its own. `crates/conway/src/config/model_metadata.rs`'s
//! `set_context_window` records the companion decision (a whole-document
//! rewrite, not `config::writer`'s byte-preserving splicer, and why that is
//! safe for this specific file's narrow schema).
//!
//! A pre-existing config with no recorded window (an operator who
//! configured a provider before this item shipped) is handled by NOT this
//! item at all: `conway_plugin_backends::capabilities::ContextTokensSource::
//! Unverified` already exists for exactly this state (a model with no
//! override, no metadata entry, and a dialect whose own baseline is
//! unsourced) and this item builds nothing new on top of it -- it only
//! makes that state rarer going forward by asking at setup time, never
//! retroactively.
//! [`finish_setup`] belongs in that same bucket -- it touches disk (via
//! `conway::config`'s writers) and, on success, the network (via
//! [`verify_backend`]), but reads the terminal only on its OWN failure
//! path (a raw-mode "retry?" keypress) -- so a test driving its SUCCESS
//! path never touches a pty either. It is `pub` for exactly this reason:
//! board item `01M1A2HKMDGNK961ZFV1EGZDQ0`'s own acceptance 1 requires a
//! test that builds a real [`conway::ConwayBuilder`] from the exact file
//! `finish_setup` wrote and completes a real turn from it -- a
//! [`verify_backend`] call alone proves only that ITS OWN throwaway
//! in-memory config routes, which is precisely the gap this board item
//! closes.
//!
//! [`apply_opinion_set`]/[`apply_shell_choice`] (board item
//! `01M1FS34GNZEVZP4ZBVC90VD6J`) are `pub` on the identical footing:
//! disk-only, no terminal, covered directly against a real `settings.json`
//! in `crates/conway-cli/tests/first_run.rs`. `opinion_set_transcript`
//! joins the pure-string-formatting bucket alongside
//! [`context_window_setup_notice`] -- tested by asserting on the returned
//! `String` directly, since [`run_guided_setup`] (now via its own
//! `offer_opinion_set_and_shell` step) is still the one caller no
//! automated test can drive end to end.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use conway::backend_usability::{classify_entry, ProbePolicy, Usability, DEFAULT_PROBE_TIMEOUT};
use conway::config::schema::BackendEntry;
use conway::{ConwayBuilder, ResultStatus, SessionSpec};
use crossterm::event::{Event, KeyCode, KeyEventKind};

/// The exact phrase every guided-setup surface opens with -- interactive
/// banner and non-interactive degrade message alike. Acceptance 8's own
/// grep-checkable anchor: this crate's tests key off this constant (never a
/// restated literal) to tell "the flow opened" apart from every other way
/// `conway` can fail to start, and `crates/conway-cli/tests/first_run.rs`
/// greps `src/main.rs` for the call this constant is named beside --
/// `FleetUsability::should_offer_guided_setup()` -- rather than for a
/// restatement of its condition (P-14).
pub const GUIDED_SETUP_MARKER: &str = "conway can't reach a working model provider yet";

/// Ollama's own documented default port. The **only** local endpoint this
/// flow ever probes -- "Detect" in the board item's own flow list names
/// Ollama specifically as "the obvious case", not a general local-server
/// scanner; a longer probe list is exactly the "fourth question" the
/// appetite ruling forbids.
pub const LOCAL_OLLAMA_BASE_URL: &str = "http://127.0.0.1:11434/v1";

/// The `backends.<id>` key this flow writes for the local-detected case.
pub const LOCAL_OLLAMA_ID: &str = "local";

/// The `roles.<role>` this flow writes a real chain into, and the
/// `default_role` value it points there -- board item
/// `01M1A2HKMDGNK961ZFV1EGZDQ0`'s own writer half. Deliberately the SAME
/// name `conway::config::merge`'s baked-in validation floor already uses
/// (`is_baked_in_role_floor`'s own doc calls it `BASELINE_ROLE_NAME`,
/// private to that module, not reused here across the crate boundary --
/// this is a plain chosen string, not shared resolution logic, so P-14
/// does not apply to keeping the literal in sync, only to the chain-entry
/// FORMAT itself, see [`chain_entry`]). Reusing that name rather than
/// inventing a second one is deliberate: guided setup only ever runs when
/// nothing usable is configured (`FleetUsability::should_offer_guided_setup`),
/// so whatever `default_role` an operator's file already names is already
/// proven unusable -- writing into a role literally called `"default"` and
/// re-pointing `default_role` there converges on the one thing that is
/// GUARANTEED to work afterward, rather than layering a second, possibly
/// also-broken role next to whatever was there. The moment this role
/// carries a real chain, `is_baked_in_role_floor` no longer matches it
/// (that predicate compares the VALUE too, not just the name -- see its
/// own doc), so nothing downstream mistakes it for the floor.
pub const GUIDED_SETUP_ROLE: &str = "default";

/// One of the hosted provider shapes offered when no local server answered.
///
/// **The admissibility rule, restated because it is easy to misread the
/// original two-entry menu as a closed list:** a hosted choice belongs here
/// when adding it needs **no extra question** -- a known `kind`, a known
/// `base_url` (or none, for a `kind` with a sensible built-in default), and
/// a known `default_model`, so the flow's own appetite (detect, offer,
/// verify, get out of the way -- see this module's own doc) is unaffected.
/// The appetite ruling's "a fourth question" bar forbids a longer menu ONLY
/// in the sense of forbidding a free-text base-url prompt or a model-pinning
/// question that a genuinely unknown provider would require -- it does not
/// cap the menu at two. Anthropic and OpenAI were the first two entries
/// because they were the only two `kind`s `conway-plugin-backends` shipped
/// at the time this list was written; Ollama Cloud is a third because it
/// is `openai-compat` (a `kind` this list already needed no new question
/// for) with its own known `base_url` and `default_model` -- zero new
/// questions, not a longer menu in the sense the ruling forbids.
pub struct ProviderChoice {
    pub id: &'static str,
    pub label: &'static str,
    pub kind: &'static str,
    pub dialect: Option<&'static str>,
    pub base_url: Option<&'static str>,
    pub default_model: &'static str,
    pub credential_env: &'static str,
}

pub const HOSTED_CHOICES: &[ProviderChoice] = &[
    ProviderChoice {
        id: "anthropic",
        label: "Anthropic (Claude)",
        kind: "anthropic",
        dialect: None,
        // `None` -- `AnthropicBackendFactory` already defaults to
        // `https://api.anthropic.com` when no `base_url` is set
        // (`docs/providers.md`); no reason to restate that default here.
        base_url: None,
        default_model: "claude-sonnet-4-6",
        credential_env: "ANTHROPIC_API_KEY",
    },
    ProviderChoice {
        id: "openai",
        label: "OpenAI",
        kind: "openai-compat",
        dialect: Some("openai"),
        base_url: Some("https://api.openai.com/v1"),
        default_model: "gpt-4o-mini",
        credential_env: "OPENAI_API_KEY",
    },
    ProviderChoice {
        id: "ollama_cloud",
        label: "Ollama Cloud",
        kind: "openai-compat",
        // `"ollama"`, not `"openai"` -- confirmed by the operator's own
        // working `~/.conway/settings.json` (archived 2026-08-13, two
        // `.bak` copies) using this exact dialect for this exact
        // `base_url`, which settles what would otherwise be a judgment
        // call between the two.
        dialect: Some("ollama"),
        // Confirmed live 2026-08-30: `GET https://ollama.com/v1/models`
        // returns 200 with a real model roster; `/v1`, not `/api/v1`
        // (that path is Ollama's native, non-OpenAI-compat surface).
        // **Not stated in `docs.ollama.com`'s own prose** -- those pages
        // document only the local `http://localhost:11434/v1` form, so a
        // future reader re-deriving this from the docs alone will not find
        // it there; this value is confirmed by the live server and by a
        // config that was actually running against it, not by the docs.
        base_url: Some("https://ollama.com/v1"),
        // `glm-5.2`, deliberately NOT `gpt-oss:20b` despite the latter
        // being the smaller, cheaper model in the roster. Reason: this is
        // the model `openai_compat::wire`'s tool-call-content-type
        // workaround (see that module's `assistant_message`, the
        // `content: ""` vs `null` comment) was actually debugged against,
        // and the one the operator ran in production -- `gpt-oss:20b` has
        // never been through conway's wire layer at all. A first-run
        // default that hits an unhandled dialect quirk on a new user's
        // FIRST tool call is the worst possible first experience;
        // cheapness does not compensate for that risk. **Expected to
        // age**: `docs.ollama.com/cloud` states Ollama "will occasionally
        // deprecate and retire older cloud models" -- this id is not
        // expected to be permanent, and a future reader finding it gone
        // from the roster should replace it, not treat its disappearance
        // as a conway defect.
        default_model: "glm-5.2",
        credential_env: "OLLAMA_API_KEY",
    },
];

/// What [`detect_local_provider`] found, if anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalOffer {
    pub base_url: String,
    pub model: String,
}

/// Which credential a chosen [`ProviderChoice`] should be saved with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialPlan {
    /// `choice.credential_env` is already set (non-empty) in the caller's
    /// own environment -- reuse it via `api_key_env`, never prompting for
    /// the value at all. This is the spec's own stated preference: "the
    /// operator's own working config uses `api_key_env` for exactly this
    /// reason".
    ReuseEnvVar,
    /// Nothing usable is already set; the flow must prompt for a literal
    /// value.
    PromptForLiteral,
}

/// Pure: whether `choice`'s own credential variable is already usable in
/// `env`, so the flow can skip asking for it entirely. "Usable" mirrors
/// `backend_usability::classify_entry`'s own rule for `api_key_env`: unset
/// or whitespace-only does not count.
pub fn resolve_credential_plan(
    choice: &ProviderChoice,
    env: &HashMap<String, String>,
) -> CredentialPlan {
    match env.get(choice.credential_env) {
        Some(v) if !v.trim().is_empty() => CredentialPlan::ReuseEnvVar,
        _ => CredentialPlan::PromptForLiteral,
    }
}

/// P-10: a human types this. Rejected before it ever reaches a JSON literal
/// or a file write -- never a panic path for an empty paste or an
/// implausible one.
pub fn validate_credential_input(raw: &str) -> Result<String, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("no key was entered");
    }
    // A real API key is short; a whole file or document pasted by accident
    // is not. 4096 is generous headroom over every credential shape this
    // codebase's own docs/tests use, chosen to catch a mis-paste without
    // rejecting any plausible real key.
    if trimmed.chars().count() > 4096 {
        return Err(
            "that's too long to be an API key (over 4096 characters) -- refusing to save it",
        );
    }
    Ok(trimmed.to_string())
}

/// Pure: parses the setup-time context-window ASK prompt's raw typed line
/// (both TTY entrances share this — see `ask_and_persist_context_window`
/// and the TUI's `input::handle_add_provider_context_window_key`).
///
/// Empty/whitespace-only means "skip" (`Ok(None)`) -- the operator declines
/// to supply a number and the model's window stays whatever it already was
/// (unrecorded, if this is the first time; `Unverified` at read time -- see
/// this module's own top doc). This is the ONE place a blank answer is
/// accepted as a real, non-error outcome: the operator's own ruling forbids
/// ever filling an unknown window with an invented number, and "press Enter
/// with nothing typed" is how an interactive prompt says "I don't know
/// either" without that turning into an error state.
///
/// A non-empty value must parse as a plain base-10 `u32` greater than zero
/// and no larger than `MAX_PLAUSIBLE_CONTEXT_WINDOW` -- generous headroom
/// above the largest window this crate's own investigation observed live
/// (`glm-5.2`'s `1_048_576` -- `probe.rs`'s own "2026-08-30
/// re-confirmation" doc) while still catching an obvious mis-paste (a
/// pasted API key, a stray decimal) before it reaches a config write, the
/// same P-10 boundary [`validate_credential_input`] already applies to a
/// human's typed input.
pub fn validate_context_window_input(raw: &str) -> Result<Option<u32>, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    match trimmed.parse::<u32>() {
        Ok(0) => Err(
            "a context window of 0 tokens is not usable -- press Enter with nothing typed to \
             skip instead",
        ),
        Ok(n) if n > MAX_PLAUSIBLE_CONTEXT_WINDOW => Err(
            "that's larger than any known real model context window -- refusing to save it \
             (press Enter with nothing typed to skip)",
        ),
        Ok(n) => Ok(Some(n)),
        Err(_) => Err("not a whole number of tokens"),
    }
}

/// [`validate_context_window_input`]'s own upper bound: comfortably above
/// the largest real window this crate's own investigation has observed live
/// (`glm-5.2`'s `1_048_576`), never itself presented as a plausible answer
/// -- just a mis-paste backstop.
const MAX_PLAUSIBLE_CONTEXT_WINDOW: u32 = 4_000_000;

/// Where a saved entry's credential came from -- the two shapes
/// [`backend_entry_json`] can write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialSource {
    EnvVar(String),
    Literal(String),
}

/// The JSON object literal [`conway::config::set_backend_provider`] splices
/// in for a chosen hosted provider. Built by serializing a real
/// [`BackendEntry`] through `serde_json` -- never hand-formatted -- so a
/// credential value containing a quote or backslash is always escaped
/// correctly (P-10, at the boundary where a human's paste becomes JSON
/// text).
pub fn backend_entry_json(choice: &ProviderChoice, credential: &CredentialSource) -> String {
    let mut entry = BackendEntry {
        kind: choice.kind.to_string(),
        ..BackendEntry::default()
    };
    if let Some(base_url) = choice.base_url {
        entry.base_url = base_url.to_string();
    }
    if let Some(dialect) = choice.dialect {
        entry.dialect = Some(dialect.to_string());
    }
    match credential {
        CredentialSource::EnvVar(name) => entry.api_key_env = name.clone(),
        CredentialSource::Literal(key) => entry.api_key = key.clone(),
    }
    serde_json::to_string(&entry).expect("BackendEntry always serializes to a JSON object")
}

/// The JSON object literal for the local-detected (Ollama) case. No
/// credential field at all -- `BackendEntry::local`'s own doc: a local
/// server needing no key is the ordinary case, never guessed at.
pub fn local_offer_entry_json(offer: &LocalOffer) -> String {
    let entry = BackendEntry {
        kind: "openai-compat".to_string(),
        dialect: Some("ollama".to_string()),
        base_url: offer.base_url.clone(),
        local: true,
        ..BackendEntry::default()
    };
    serde_json::to_string(&entry).expect("BackendEntry always serializes to a JSON object")
}

/// The DISCOVER half of the operator's "discover, or ask if discovery
/// fails" setup-time ruling. The ASK half is
/// `ask_and_persist_context_window`; the PERSIST half (shared by both
/// DISCOVER's successful result and ASK's typed answer) is
/// [`persist_context_window`]; `handle_context_window_at_setup` is the one
/// orchestrator that calls all three in the right order, from both setup
/// entrances — see this module's own top doc.
///
/// Calls `conway_plugin_backends::probe::discover_context_window` (the ONE
/// shared discovery primitive both guided first-run and `/settings` →
/// providers → add call — see that function's own doc for why one
/// implementation, never two, was load-bearing here). Returns `None`
/// (never an error, never a panic) when: `dialect` is `None` or names a
/// profile with no known discovery endpoint (only `"ollama"` has one
/// today), `base_url` fails to parse, or discovery itself finds nothing
/// (unreachable server, unrecognized model, timeout).
///
/// **Deliberately does NOT persist the result itself** — that is
/// [`persist_context_window`]'s own, separately-scoped job (this function's
/// only concern is the network read); see this module's own top doc for the
/// file/scope decision that unblocked writing it.
pub async fn discover_setup_context_window(
    base_url: &str,
    dialect: Option<&str>,
    model: &str,
) -> Option<u32> {
    let Some("ollama") = dialect else {
        return None;
    };
    let Ok(base) = base_url.parse() else {
        return None;
    };
    let profile = conway_plugin_backends::config::Dialect::Ollama.profile();
    conway_plugin_backends::probe::discover_context_window(&base, &profile, None, model).await
}

/// Whether `kind`/`dialect`'s own BASELINE `max_context_tokens` is already a
/// real, sourced fact (`ContextTokensSource::DialectDefaultFloor` once no
/// override/metadata names this model — `capabilities.rs`'s own doc) rather
/// than an unsourced placeholder (`ContextTokensSource::Unverified`, the
/// state this whole item's ask step exists to avoid landing on silently).
/// `anthropic` (200,000) and the `"openai"` dialect (128,000) are the only
/// two built-in profiles this is true for today
/// (`conway_plugin_backends::profile`'s own
/// `only_openai_declares_its_context_window_verified` test) — every other
/// dialect this flow can ever configure (`"ollama"`, for both the local
/// offer and Ollama Cloud) is not, so `handle_context_window_at_setup`
/// only ever asks when discovery ALSO found nothing for one of those.
///
/// Reads the SAME `context_window_verified` flag `capabilities::
/// build_capabilities` itself consults
/// (`conway_plugin_backends::capabilities::anthropic_defaults`/
/// `conway_plugin_backends::profile::ProfileStore::built_ins`), never a
/// second, hand-maintained opinion about which providers are trustworthy
/// (P-14) — this is a lookup on an EXISTING sourced-ness flag, not the
/// static context-window-VALUE table the operator's ruling forbids
/// ("prefer discovery over a table"). An unrecognized `dialect` string (or
/// none at all, for a non-`"anthropic"` kind) is conservatively `false` —
/// the same "an unfamiliar provider is never assumed verified by silence"
/// rule `Profile::context_window_verified`'s own doc states for a
/// hand-authored profile that omits the field.
pub fn context_window_is_verified(kind: &str, dialect: Option<&str>) -> bool {
    if kind == "anthropic" {
        return conway_plugin_backends::capabilities::anthropic_defaults().context_window_verified;
    }
    let Some(dialect) = dialect else {
        return false;
    };
    conway_plugin_backends::profile::ProfileStore::built_ins()
        .resolve(dialect)
        .map(|p| p.context_window_verified)
        .unwrap_or(false)
}

/// The PERSIST half of the setup-time "discover, or ask if discovery fails"
/// ruling, shared by both a successful [`discover_setup_context_window`]
/// result and an operator's own typed answer
/// (`ask_and_persist_context_window`/the TUI's `Action::
/// SubmitProviderContextWindow`) — one write path for both origins, never
/// two (P-14).
///
/// **The file/scope decision, made here and recorded in
/// `conway::config::merge::metadata_path_for`'s own doc (the function this
/// resolves the write location through) rather than restated a second
/// time:** the window is written into whichever `models.json` THIS process
/// would actually read back for its own `cwd` right now — the same
/// `[models].metadata_path` resolution (default < user < project < env)
/// `conway::config::load` itself performs, computed without requiring a
/// full, validated config to already exist (a setup flow's whole point is
/// that one does not yet). See `metadata_path_for`'s own doc for the two
/// rejected alternatives (a fixed user-scope `models.json` with an injected
/// `metadata_path` override, and the confirmed-inert
/// `backends.<id>.models.<model>.max_context_tokens` channel) and why each
/// was rejected.
///
/// `key` is the `"backend/model"` form [`chain_entry`] already builds — the
/// exact key `conway::config::model_metadata::ModelMetadata` itself uses.
/// Returns the resolved path on success (for the caller's own confirmation
/// message); `Err(message)` naming exactly why not, never silently
/// swallowed (GP-14: a failed write must be as loud as a successful one is
/// quiet).
pub fn persist_context_window(
    env: &HashMap<String, String>,
    key: &str,
    window: u32,
) -> Result<std::path::PathBuf, String> {
    let cwd = std::env::current_dir()
        .map_err(|e| format!("could not resolve the working directory: {e}"))?;
    persist_context_window_at(&cwd, env, key, window)
}

/// [`persist_context_window`]'s own `cwd`-parameterized half. Two reasons
/// this is `pub(crate)` rather than private:
/// - **A test can exercise the real resolution/write logic against a
///   fixture `cwd`** without mutating THIS PROCESS's actual working
///   directory, which `cargo test`'s default parallel execution makes an
///   unsafe thing for any one test to do (every other test in the same
///   binary runs concurrently and would observe the mutated value).
/// - **`tui::app::provider_manage::App::write_provider_entry_and_refresh`
///   already has a real `cwd` threaded in** (the same one `wire_provider_
///   into_default_chain` uses) -- calling THIS function with it, rather
///   than [`persist_context_window`] (which would silently re-derive `cwd`
///   from `std::env::current_dir()` a second, possibly-different way), is
///   what keeps every write inside one `write_provider_entry_and_refresh`
///   call resolving `cwd` identically.
pub(crate) fn persist_context_window_at(
    cwd: &Path,
    env: &HashMap<String, String>,
    key: &str,
    window: u32,
) -> Result<std::path::PathBuf, String> {
    let path = conway::config::metadata_path_for(cwd, env).map_err(|e| e.to_string())?;
    conway::config::set_context_window(&path, key, window).map_err(|e| e.to_string())?;
    Ok(path)
}

/// The operator-facing message printed after a window is discovered (or
/// typed) AND successfully persisted — names the model, the window, and the
/// file it now lives in, per `INTENT.md` §8.3 ("refuse and name what
/// changed"). `key` is the `"backend/model"` form [`chain_entry`] builds --
/// the exact key the printed `path` now records it under.
pub fn context_window_setup_notice(key: &str, window: u32, path: &std::path::Path) -> String {
    format!(
        "conway recorded a {window}-token context window for {key} in {} -- routing and \
         admission now reflect it.",
        path.display()
    )
}

/// The exact, non-interactive degrade message: names the file to edit and
/// the precise content to add, per `INTENT.md` §8.3 ("refuse and name what
/// changed") and this item's own acceptance 5. Uses the Anthropic shape as
/// the one worked example -- picking one concrete, complete snippet rather
/// than an abstract description of "add a backend" is what makes this
/// copy-pasteable by someone who has never seen `settings.json` before.
pub fn non_interactive_guidance(path: &Path) -> String {
    format!(
        "{GUIDED_SETUP_MARKER}, and this isn't an interactive terminal, so conway can't ask you \
         about it here.\n\
         \n\
         Add a provider by hand: edit (or create) {path} and add:\n\
         \n\
         {{\n  \"backends\": {{\n    \"anthropic\": {{\n      \"kind\": \"anthropic\",\n      \
         \"api_key_env\": \"ANTHROPIC_API_KEY\"\n    }}\n  }},\n  \"roles\": {{\n    \"coder\": \
         {{ \"chain\": [\"anthropic/claude-sonnet-4-6\"] }}\n  }}\n}}\n\
         \n\
         then export ANTHROPIC_API_KEY and run conway again. See docs/getting-started.md for \
         other providers, including a local server.",
        path = path.display()
    )
}

/// Probes for a local Ollama server already running and, if one answers,
/// asks it which model it actually has loaded (never guessed -- a wrong
/// guess here would make "the best path in the whole feature" fail for
/// almost everyone, since a model tag has to already be pulled to exist).
///
/// Reuses [`classify_entry`] (never re-derives its probe) for the
/// reachability half; the model-listing half is this flow's own, genuinely
/// new capability -- `backend_usability` explicitly never performs
/// inference or lists models (its own module doc).
pub async fn detect_local_provider(env: &HashMap<String, String>) -> Option<LocalOffer> {
    let probe_entry = BackendEntry {
        local: true,
        base_url: LOCAL_OLLAMA_BASE_URL.to_string(),
        ..BackendEntry::default()
    };
    let usability = classify_entry(
        &probe_entry,
        env,
        ProbePolicy::LocalOnly,
        DEFAULT_PROBE_TIMEOUT,
    )
    .await;
    if !matches!(usability, Usability::Usable) {
        return None;
    }
    let model = first_available_model(LOCAL_OLLAMA_BASE_URL).await?;
    Some(LocalOffer {
        base_url: LOCAL_OLLAMA_BASE_URL.to_string(),
        model,
    })
}

/// `GET {base}/models`, OpenAI-shaped (`{"data":[{"id":...}]}` --
/// `conway-plugin-backends::probe.rs`'s own doc names this exact shape).
/// Short timeout: the endpoint already answered a TCP connect in
/// [`detect_local_provider`], so a real reply is expected in well under a
/// second.
async fn first_available_model(base_url: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .ok()?;
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let resp = client.get(url).send().await.ok()?;
    let body: serde_json::Value = resp.json().await.ok()?;
    body.get("data")?
        .as_array()?
        .first()?
        .get("id")?
        .as_str()
        .map(|s| s.to_string())
}

/// The `roles.<role>.chain` entry format ("backend/model" --
/// `conway::config::schema`'s own module doc, `chain: Vec<String>`) built
/// ONCE and reused by [`verify_backend`]'s own throwaway one-entry probe
/// chain AND by [`finish_setup`]'s real, persisted chain -- board item
/// `01M1A2HKMDGNK961ZFV1EGZDQ0` was exactly this format constructed twice
/// (once here, inline, for verification; never at all for the write) and
/// the two silently diverging: verification proved a shape the file on
/// disk never had. One function, called from both places, makes that
/// divergence impossible rather than merely unlikely (P-14).
pub fn chain_entry(id: &str, model: &str) -> String {
    format!("{id}/{model}")
}

/// Proves a just-saved backend entry can serve a real turn -- one real
/// completion, never a reachability ping (`backend_usability` deliberately
/// never performs inference; this is the step that closes that gap).
///
/// Builds a throwaway, isolated `Conway` (its own temp session-store root,
/// never the operator's real sessions directory) with exactly this one
/// backend and a one-entry role chain naming it, sends one short prompt,
/// and reports `Ok(())` only if the turn actually completed. A backend
/// failure (wrong key, unreachable host, no such model) surfaces as
/// `Err(message)` built from the real `ResultStatus::Failed` text the
/// runtime itself produced -- never a message this function invents.
pub async fn verify_backend(id: &str, entry_json: &str, model: &str) -> Result<(), String> {
    let entry_value: serde_json::Value = serde_json::from_str(entry_json)
        .map_err(|e| format!("internal error building the verification config: {e}"))?;
    let role = "first_run_verify";
    let config_value = serde_json::json!({
        "default_role": role,
        "roles": { role: { "chain": [chain_entry(id, model)] } },
        "backends": { id: entry_value },
    });
    let mut config: conway::config::ConwayConfig = serde_json::from_value(config_value)
        .map_err(|e| format!("internal error building the verification config: {e}"))?;

    // A dedicated temp directory, never the operator's real sessions root
    // (`~/.conway/sessions` or a project's `.conway/sessions`) -- this is a
    // throwaway probe turn, not a session the operator should ever see
    // listed. Cleaned up unconditionally on every exit path below.
    let tmp_root = std::env::temp_dir().join(format!(
        "conway-first-run-verify-{}-{}",
        std::process::id(),
        conway_core_ulid_free_suffix()
    ));
    std::fs::create_dir_all(&tmp_root)
        .map_err(|e| format!("could not create a temp dir to verify in: {e}"))?;
    config.session.root = Some(tmp_root.clone());

    let outcome = run_one_verify_turn(config, id, model).await;
    let _ = std::fs::remove_dir_all(&tmp_root);
    outcome
}

/// A process-unique suffix with no new dependency: a monotonic counter
/// keyed by an address on this thread's own stack, cheap and sufficient for
/// "never collides within one process's lifetime", which is all a temp-dir
/// name here needs.
fn conway_core_ulid_free_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

async fn run_one_verify_turn(
    config: conway::config::ConwayConfig,
    id: &str,
    model: &str,
) -> Result<(), String> {
    let conway = ConwayBuilder::from_parts(config)
        .with_backend_factory(Arc::new(conway_plugin_backends::AnthropicBackendFactory))
        .with_backend_factory(Arc::new(conway_plugin_backends::OpenAiCompatBackendFactory))
        .with_permission_gate(Arc::new(conway::gates::DenyAllGate))
        .build()
        .map_err(|e| format!("could not start a session with {id}/{model}: {e}"))?;

    let handle = conway
        .new_session(SessionSpec::default())
        .await
        .map_err(|e| format!("could not start a session with {id}/{model}: {e}"))?;
    let turn = handle
        .prompt("Reply with exactly one word: ok")
        .await
        .map_err(|e| format!("the request failed: {e}"))?;
    let result = turn
        .result()
        .await
        .map_err(|e| format!("the request failed: {e}"))?;

    match result.status {
        ResultStatus::Completed => Ok(()),
        ResultStatus::Failed { error } => Err(error),
        other => Err(format!("verification did not complete normally: {other:?}")),
    }
}

/// What the interactive flow ended with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuidedSetupOutcome {
    /// A backend was saved to the user-scope `settings.json` and verified
    /// with a real completion.
    Configured,
    /// The operator declined -- no backend was saved (or one that was
    /// saved mid-flow was rolled back), and the caller must proceed
    /// leaving the fleet exactly as it was.
    Declined,
}

/// Reads exactly one key press in raw mode (no line buffering, no echo of
/// anything but what the caller explicitly prints), for a menu choice or a
/// yes/no. `None` on any terminal error -- treated as a decline by every
/// caller, never a hang.
fn read_single_key() -> Option<KeyCode> {
    crossterm::terminal::enable_raw_mode().ok()?;
    let key = loop {
        match crossterm::event::read() {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => break Some(k.code),
            Ok(_) => continue,
            Err(_) => break None,
        }
    };
    let _ = crossterm::terminal::disable_raw_mode();
    key
}

/// Reads one line of input in raw mode with **no character ever echoed** --
/// a `*` is printed per keystroke as the only feedback, which is not an
/// echo of the value (this item's own hard requirement: a key typed here
/// must never be echoed to the terminal, written into a transcript, or
/// captured in a session log). `None` on `Esc` or a terminal error, both
/// treated as a decline by the caller.
fn read_secret_line() -> Option<String> {
    crossterm::terminal::enable_raw_mode().ok()?;
    let mut buf = String::new();
    let outcome = loop {
        match crossterm::event::read() {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => match k.code {
                KeyCode::Enter => break Some(buf.clone()),
                KeyCode::Esc => break None,
                KeyCode::Backspace => {
                    if buf.pop().is_some() {
                        print!("\u{8} \u{8}");
                        let _ = std::io::stdout().flush();
                    }
                }
                KeyCode::Char(c) => {
                    buf.push(c);
                    print!("*");
                    let _ = std::io::stdout().flush();
                }
                _ => {}
            },
            Ok(_) => continue,
            Err(_) => break None,
        }
    };
    let _ = crossterm::terminal::disable_raw_mode();
    println!();
    outcome
}

/// [`read_secret_line`]'s un-masked sibling -- echoes each typed character
/// verbatim rather than a `*`. Used only for the setup-time context-window
/// number prompt (`ask_and_persist_context_window`): a token count is not
/// a credential and has no reason to be hidden. `None` on `Esc` or a
/// terminal error, both treated as "skip" by every caller here, mirroring
/// [`read_secret_line`]'s own contract for its own two failure paths.
fn read_plain_line() -> Option<String> {
    crossterm::terminal::enable_raw_mode().ok()?;
    let mut buf = String::new();
    let outcome = loop {
        match crossterm::event::read() {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => match k.code {
                KeyCode::Enter => break Some(buf.clone()),
                KeyCode::Esc => break None,
                KeyCode::Backspace => {
                    if buf.pop().is_some() {
                        print!("\u{8} \u{8}");
                        let _ = std::io::stdout().flush();
                    }
                }
                KeyCode::Char(c) => {
                    buf.push(c);
                    print!("{c}");
                    let _ = std::io::stdout().flush();
                }
                _ => {}
            },
            Ok(_) => continue,
            Err(_) => break None,
        }
    };
    let _ = crossterm::terminal::disable_raw_mode();
    println!();
    outcome
}

/// The ASK half of the operator's "discover, or ask if discovery fails"
/// setup-time ruling, for the pre-TUI guided-setup entrance (`run_
/// backend_setup`/`retry_credential_and_finish`, via [`handle_context_
/// window_at_setup`]) -- the TUI's `/settings` → providers → add entrance
/// has its own equivalent surface (`tui::app::provider_manage`'s `Mode::
/// AddProviderContextWindow`), since a raw-terminal keypress read here would
/// fight ratatui's own screen control; both share every PURE decision this
/// module makes ([`validate_context_window_input`],
/// [`context_window_is_verified`], [`persist_context_window`]) and differ
/// only in how the keystrokes themselves are collected (P-14 applied to
/// everything except the terminal I/O itself, which cannot be shared across
/// a raw-mode read and a ratatui widget).
///
/// `Esc` (via [`read_plain_line`] returning `None`) is treated exactly like
/// an empty `Enter` -- both mean "skip", never an error: declining costs
/// the operator nothing beyond leaving this ONE model's window unrecorded,
/// which already has an honest name for the resulting state --
/// `conway_plugin_backends::capabilities::ContextTokensSource::Unverified`
/// -- rather than a silently invented number.
fn ask_and_persist_context_window(env: &HashMap<String, String>, key: &str) {
    println!();
    println!(
        "conway could not determine {key}'s context window automatically -- this profile has \
         no known discovery endpoint, or the server did not answer."
    );
    println!("Enter it in tokens (e.g. 131072), or press Enter to leave it unverified for now:");
    print!("> ");
    let _ = std::io::stdout().flush();
    let raw = read_plain_line().unwrap_or_default();
    match validate_context_window_input(&raw) {
        Ok(None) => {
            println!(
                "Skipped -- {key}'s context window remains unverified; conway will not send a \
                 num_ctx hint and admission uses the dialect's conservative floor until you set \
                 one (docs/providers.md)."
            );
        }
        Ok(Some(window)) => match persist_context_window(env, key, window) {
            Ok(path) => println!("{}", context_window_setup_notice(key, window, &path)),
            Err(e) => println!("Could not save {key}'s context window: {e}"),
        },
        Err(msg) => println!("{msg} -- {key}'s context window remains unverified."),
    }
}

/// The one orchestrator every setup-time call site (`run_backend_setup`'s
/// local-offer and hosted-choice branches, and `retry_credential_and_
/// finish`) calls instead of hand-rolling the discover-then-maybe-ask
/// sequence itself (P-14) -- see this module's own top doc for the full
/// three-function split this composes:
/// [`discover_setup_context_window`] (network), [`context_window_is_
/// verified`] (decide whether asking is even warranted), [`persist_context_
/// window`] (the shared write), `ask_and_persist_context_window` (this
/// entrance's own TTY read).
///
/// `base_url` is `None` only for the one hosted choice with no fixed base
/// URL at all (`anthropic`) -- discovery is skipped entirely in that case
/// (there is no server to ask), and since `anthropic`'s own baseline is
/// verified (`context_window_is_verified("anthropic", None) == true`),
/// nothing is asked either: the fall-through below reaches the `if
/// context_window_is_verified(...) { return; }` guard exactly as if
/// discovery had been attempted and found nothing.
async fn handle_context_window_at_setup(
    env: &HashMap<String, String>,
    key: &str,
    base_url: Option<&str>,
    dialect: Option<&str>,
    kind: &str,
    model: &str,
) {
    if let Some(base_url) = base_url {
        if let Some(window) = discover_setup_context_window(base_url, dialect, model).await {
            println!();
            match persist_context_window(env, key, window) {
                Ok(path) => println!("{}", context_window_setup_notice(key, window, &path)),
                Err(e) => println!(
                    "conway discovered a {window}-token context window for {key} but could not \
                     save it: {e}"
                ),
            }
            return;
        }
    }
    if context_window_is_verified(kind, dialect) {
        // The dialect's own baseline is a real, sourced figure (Anthropic's
        // 200k, OpenAI's 128k) -- nothing to ask, and asking anyway would
        // be exactly the noise the operator's own ruling (a setup-time
        // question is warranted only to avoid an UNSOURCED placeholder,
        // never to double-check a fact conway already has) argues against.
        return;
    }
    ask_and_persist_context_window(env, key);
}

/// The interactive flow itself: detect, offer, verify, offer to add
/// another, get out of the way. Only reachable when the caller has already
/// confirmed a real, writable terminal is attached (`main.rs`'s own
/// `interactive` computation) -- see this module's own doc for why this
/// function is the one part of the module no automated test drives
/// directly.
///
/// **Board item `01M1FS34GNZEVZP4ZBVC90VD6J`: the thin outer wrapper.**
/// `run_backend_setup` below is this function's OLD body, unchanged in
/// every other respect -- resolving `path` first, then delegating, is what
/// lets this wrapper run `offer_opinion_set_and_shell` exactly ONCE,
/// after the backend flow settles on `Configured`, regardless of which of
/// its several internal return points got there (a single provider
/// accepted and no "add another," two providers added then declined, the
/// local-Ollama one-keypress accept). Duplicating the opinion-set/shell
/// offer at every one of those return points instead would either offer it
/// more than once in a two-provider run or miss a return point a future
/// edit adds -- doing it here, once, after the backend flow's own outcome
/// is already decided, is not reachable by either failure mode.
pub async fn run_guided_setup(env: &HashMap<String, String>) -> GuidedSetupOutcome {
    let Some(path) = conway::config::discovery::user_config_path(env) else {
        println!(
            "conway: could not determine where to write a user config (no home directory \
             found); skipping guided setup."
        );
        return GuidedSetupOutcome::Declined;
    };

    let outcome = run_backend_setup(env, &path).await;
    if outcome == GuidedSetupOutcome::Configured {
        offer_opinion_set_and_shell(&path);
    }
    outcome
}

/// [`run_guided_setup`]'s own former body: detect, offer, verify, offer to
/// add another, get out of the way -- see that function's own doc for why
/// it now takes `path` as a parameter instead of resolving it itself.
async fn run_backend_setup(env: &HashMap<String, String>, path: &Path) -> GuidedSetupOutcome {
    println!();
    println!("{GUIDED_SETUP_MARKER}.");
    println!("Let's fix that. Press Esc at any point to skip and continue without one.");

    // Order added = chain order (acceptance 3): every backend this RUN
    // configures, in the order it was configured, is exactly what
    // `finish_setup`/`persist_chain` write to
    // `roles.<GUIDED_SETUP_ROLE>.chain`.
    let mut chain: Vec<String> = Vec::new();

    println!();
    print!("Looking for a local model server (Ollama on 127.0.0.1:11434)... ");
    let _ = std::io::stdout().flush();

    // The local probe/offer happens at most ONCE per run, win or lose --
    // "add another" (below) only ever loops back into the hosted menu, not
    // back to this probe. Re-probing on every loop iteration would either
    // re-offer the identical server (harmless but pointless once already
    // accepted) or re-print "none found" forever for an operator who is
    // clearly going the hosted route -- neither is the fourth question the
    // appetite ruling forbids, but both are noise this flow's own "get out
    // of the way" already argues against.
    if let Some(offer) = detect_local_provider(env).await {
        println!("found one, model \"{}\".", offer.model);
        // Accurate about all three outcomes, not just two of them: `Enter`
        // accepts, `Esc` abandons the WHOLE flow (matching every other
        // `Esc` in this module), and anything else moves on to the hosted
        // menu without configuring this one. The old wording ("or any
        // other key to see other providers") lumped `Esc` in with "any
        // other key", which reads as "just another way to browse" but
        // actually means "give up entirely" -- exactly the model just
        // offered, abandoned by a key someone pressed expecting to keep
        // looking.
        println!(
            "Press Enter to use it, any other key to see other providers, or Esc to skip setup."
        );
        match read_single_key() {
            Some(KeyCode::Enter) => {
                let entry_json = local_offer_entry_json(&offer);
                handle_context_window_at_setup(
                    env,
                    &chain_entry(LOCAL_OLLAMA_ID, &offer.model),
                    Some(&offer.base_url),
                    Some("ollama"),
                    "openai-compat",
                    &offer.model,
                )
                .await;
                match finish_setup(
                    path,
                    LOCAL_OLLAMA_ID,
                    &entry_json,
                    &offer.model,
                    &mut chain,
                    env,
                )
                .await
                {
                    GuidedSetupOutcome::Configured => {
                        if !prompt_add_another() {
                            return GuidedSetupOutcome::Configured;
                        }
                    }
                    GuidedSetupOutcome::Declined => return decline_or_keep(&chain),
                }
            }
            Some(KeyCode::Esc) => return decline_or_keep(&chain),
            _ => {}
        }
    } else {
        println!("none found.");
    }

    loop {
        println!();
        println!("Pick a provider:");
        for (i, choice) in HOSTED_CHOICES.iter().enumerate() {
            println!("  [{}] {}", i + 1, choice.label);
        }
        println!("  [Esc] skip setup for now");

        let Some(key) = read_single_key() else {
            return decline_or_keep(&chain);
        };
        let choice = match key {
            KeyCode::Esc => return decline_or_keep(&chain),
            KeyCode::Char(c) => c
                .to_digit(10)
                .and_then(|n| (n as usize).checked_sub(1))
                .and_then(|idx| HOSTED_CHOICES.get(idx)),
            _ => None,
        };
        let Some(choice) = choice else { continue };

        let credential = match resolve_credential_plan(choice, env) {
            CredentialPlan::ReuseEnvVar => {
                println!("Found {} already set -- using it.", choice.credential_env);
                CredentialSource::EnvVar(choice.credential_env.to_string())
            }
            CredentialPlan::PromptForLiteral => {
                println!();
                println!(
                    "This will be written in PLAIN TEXT to {} (as backends.{}.api_key).",
                    path.display(),
                    choice.id
                );
                print!("Paste your {} key: ", choice.label);
                let _ = std::io::stdout().flush();
                let Some(raw) = read_secret_line() else {
                    return decline_or_keep(&chain);
                };
                match validate_credential_input(&raw) {
                    Ok(key) => CredentialSource::Literal(key),
                    Err(msg) => {
                        println!("{msg}");
                        continue;
                    }
                }
            }
        };

        let entry_json = backend_entry_json(choice, &credential);
        handle_context_window_at_setup(
            env,
            &chain_entry(choice.id, choice.default_model),
            choice.base_url,
            choice.dialect,
            choice.kind,
            choice.default_model,
        )
        .await;
        match finish_setup(
            path,
            choice.id,
            &entry_json,
            choice.default_model,
            &mut chain,
            env,
        )
        .await
        {
            GuidedSetupOutcome::Configured => {
                if !prompt_add_another() {
                    return GuidedSetupOutcome::Configured;
                }
                // Loop back to the hosted menu -- see the comment above
                // the local probe for why "add another" never re-offers
                // it.
            }
            GuidedSetupOutcome::Declined => return decline_or_keep(&chain),
        }
    }
}

/// Asked once after EVERY successful [`finish_setup`], never before or
/// instead of it -- **the appetite ruling's own accounting for this
/// question, stated here because a reader who only sees a one-line prompt
/// under a loop could otherwise assume the ruling was simply forgotten.**
/// [`ProviderChoice`]'s own doc records "detect, offer, verify, get out of
/// the way" with "one credential prompt at most"; this is the ONE new
/// prompt this board item adds to that budget, and it costs an operator
/// who wants exactly one provider precisely nothing extra in the failure
/// mode that matters: declining (anything other than `y`/`Y`, including a
/// terminal error) reproduces today's single-provider outcome byte-for-
/// byte in every way except the one this whole item exists to fix -- the
/// resulting config now actually routes. The question only exists at all
/// because a SECOND provider changes what gets written (a longer chain,
/// acceptance 3) in a way nothing else in this flow could infer -- there
/// is no way to build that chain without asking, once, whether there is
/// another entry to put in it.
fn prompt_add_another() -> bool {
    println!();
    println!("Add another provider? [y/N]");
    matches!(
        read_single_key(),
        Some(KeyCode::Char('y')) | Some(KeyCode::Char('Y'))
    )
}

fn decline() -> GuidedSetupOutcome {
    println!();
    println!(
        "Skipping setup. No model provider is configured, so turns will fail until you add \
         one -- see docs/getting-started.md, or run conway again to retry this."
    );
    GuidedSetupOutcome::Declined
}

/// [`decline`]'s sibling for a decline reached AFTER at least one provider
/// was already configured and persisted earlier in this same run
/// (acceptance 4: declining the "add another?" loop leaves a WORKING
/// single- or multi-provider config, never a discarded one). `decline()`'s
/// own "no model provider is configured" is only ever true when `chain` is
/// still empty -- printing it over a file that already routes somewhere
/// would be a plain false statement, exactly the kind GP-14 forbids.
fn decline_or_keep(chain: &[String]) -> GuidedSetupOutcome {
    if chain.is_empty() {
        return decline();
    }
    println!();
    println!(
        "Continuing with what's already configured ({} provider{}).",
        chain.len(),
        if chain.len() == 1 { "" } else { "s" }
    );
    GuidedSetupOutcome::Configured
}

/// Saves `entry_json` under `id`, verifies it with one real turn, and --
/// only once verification succeeds -- gives it a place in the persisted
/// `default_role` chain (`persist_chain`). Board item
/// `01M1A2HKMDGNK961ZFV1EGZDQ0`: writing `backends.<id>` alone left every
/// guided run with nothing to route to ("no candidate for role default (0
/// considered)"), even though the [`verify_backend`] call right above had
/// just proven a chain shaped exactly like the one that needed to be
/// written actually works. Also offers a retry loop on verification
/// failure (acceptance 3 of the ORIGINAL board item this module shipped
/// under, `01M11XVEHNMYY942JE63F7MAFH`: "the single most likely failure in
/// this entire flow"). A failed literal-credential attempt is rolled back
/// (the bad entry removed) before re-prompting, so a decline mid-retry
/// leaves the fleet exactly as it started.
///
/// `chain_so_far` is `run_backend_setup`'s own ordered accumulator of
/// every `"id/model"` entry this RUN has already configured successfully,
/// threaded through by mutable reference so two providers added in one run
/// land in the persisted chain in the order they were added (acceptance
/// 3). Only extended on an actual, persisted success -- a verification
/// failure, or a persist failure, leaves it untouched.
///
/// `pub`: see this module's own top doc, "How this is structured for
/// testability without a terminal" -- this function reads the terminal
/// only on ITS OWN failure path, so a test driving the success path (the
/// one acceptance 1 needs) never touches a pty.
///
/// `env` is threaded through for exactly one reason: a verification failure
/// here can lead to `retry_credential_and_finish`, which -- like every
/// other setup-time credential/model pairing -- must ALSO run
/// `handle_context_window_at_setup` before its own recursive call back
/// into this function (board item: setup-time context window, ASK +
/// PERSIST, acceptance 4 -- this retry path previously attempted no
/// discovery at all, a gap named explicitly and closed here rather than
/// left disclosed). Every OTHER use in this function is unaffected: the
/// two ordinary call sites in `run_backend_setup` already run that step
/// themselves, before ever calling `finish_setup`.
pub async fn finish_setup(
    path: &Path,
    id: &str,
    entry_json: &str,
    model: &str,
    chain_so_far: &mut Vec<String>,
    env: &HashMap<String, String>,
) -> GuidedSetupOutcome {
    if let Err(e) = conway::config::set_backend_provider(path, id, entry_json, true) {
        println!("Could not save this provider to {}: {e}", path.display());
        return decline_or_keep(chain_so_far.as_slice());
    }
    println!("Saved to {}.", path.display());
    print!("Verifying with a real request... ");
    let _ = std::io::stdout().flush();

    match verify_backend(id, entry_json, model).await {
        Ok(()) => {
            let mut candidate_chain = chain_so_far.clone();
            candidate_chain.push(chain_entry(id, model));
            match persist_chain(path, &candidate_chain) {
                Ok(()) => {
                    *chain_so_far = candidate_chain;
                    println!("it works.");
                    GuidedSetupOutcome::Configured
                }
                Err(e) => {
                    println!("it works, but the routing config could not be saved: {e}");
                    decline_or_keep(chain_so_far.as_slice())
                }
            }
        }
        Err(msg) => {
            println!("that didn't work:");
            println!("  {msg}");
            println!("Try again with a different key? [y/N]");
            match read_single_key() {
                Some(KeyCode::Char('y')) | Some(KeyCode::Char('Y')) => {
                    let _ = conway::config::set_backend_provider(path, id, entry_json, false);
                    retry_credential_and_finish(path, id, chain_so_far, env).await
                }
                _ => {
                    let _ = conway::config::set_backend_provider(path, id, entry_json, false);
                    decline_or_keep(chain_so_far.as_slice())
                }
            }
        }
    }
}

/// The two writes [`finish_setup`] must make together once a backend is
/// verified: `default_role` naming [`GUIDED_SETUP_ROLE`] -- inventing the
/// key when this is the very first provider this file has ever had, see
/// `conway::config::ensure_default_role`'s own doc for why the narrower
/// `set_default_role` cannot be used here -- and the FULL ordered `chain`
/// (`conway::config::set_role_chain`, which replaces the whole array each
/// call rather than appending -- see that function's own doc for why
/// replacing the whole thing, built from `chain_so_far` fresh every call,
/// is simpler and no less correct than an in-place append). Both calls
/// target the SAME file `set_backend_provider` (just above, in
/// [`finish_setup`]) already guaranteed exists by the time this runs.
fn persist_chain(path: &Path, chain: &[String]) -> Result<(), String> {
    conway::config::ensure_default_role(path, GUIDED_SETUP_ROLE).map_err(|e| e.to_string())?;
    conway::config::set_role_chain(path, GUIDED_SETUP_ROLE, chain).map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------
// conway's own opinion set + the shell offer -- board item
// `01M1FS34GNZEVZP4ZBVC90VD6J`, decision `01M1FQFP5D0R3M9GC8R8Z24F5N`
// (2026-09-01): after `run_guided_setup` gets a working provider, a fresh
// operator currently gets a model with no system prompt, no loop guard, no
// memory, and no shell -- and nothing on screen tells them. The two
// functions below are the pure, TTY-free half of the fix (this module's
// own "How this is structured for testability" doc); `offer_opinion_set_
// and_shell`, immediately after, is the thin imperative shell that prints
// their output and reads the one new keypress.
// ---------------------------------------------------------------------

/// Installs conway's own default first-run opinion set
/// ([`crate::first_party_plugins::DEFAULT_OPINION_SET`]) into the settings
/// file at `settings_path`, one [`conway::config::set_plugin_installed`]
/// call per id. **Idempotent**: `set_plugin_installed` is itself a no-op
/// once an id is already present (`conway::config::writer`'s own "Safety
/// posture" doc), so calling this twice against the same file writes
/// nothing the second time.
///
/// Returns the ids actually applied -- always the full set, in
/// [`crate::first_party_plugins::DEFAULT_OPINION_SET`]'s own order -- so a
/// caller can print a transcript table without re-deriving the list
/// (`offer_opinion_set_and_shell`, below, is the one production caller).
///
/// `pub`: covered directly against a real `settings.json` in
/// `crates/conway-cli/tests/first_run.rs`, mirroring [`finish_setup`]'s own
/// reason for being `pub` -- this touches disk only, never a terminal, so a
/// test driving it never touches a pty either.
pub fn apply_opinion_set(settings_path: &Path) -> Result<Vec<&'static str>, String> {
    for id in crate::first_party_plugins::DEFAULT_OPINION_SET {
        conway::config::set_plugin_installed(settings_path, id, true)
            .map_err(|e| format!("could not install \"{id}\": {e}"))?;
    }
    Ok(crate::first_party_plugins::DEFAULT_OPINION_SET.to_vec())
}

/// Enables (`enabled: true`) or leaves entirely untouched (`enabled:
/// false`) the `conway.shell` built-in tool, via
/// [`conway::config::set_builtin_plugins`] -- `tools.builtin_plugins` set
/// to the harness's own preset list
/// (`conway::config::schema::ToolsConfig::default`'s three ids: `conway.fs`,
/// `conway.subagent`, `conway.report`) plus `"conway.shell"` when `enabled`.
///
/// **Writes nothing at all when `enabled` is `false`** -- `settings_path` is
/// left BYTE-IDENTICAL, not merely logically unchanged: a declining
/// operator is not a state that needs recording, since `tools.
/// builtin_plugins`'s own absence from the file already means the
/// harness's unmodified default (fs/subagent/report, no shell) -- adding
/// that same list as an explicit key would be a no-op with a byte cost, and
/// this function's own second acceptance criterion (board item
/// `01M1FS34GNZEVZP4ZBVC90VD6J`) is that it does not pay that cost.
///
/// `pub`: covered directly against a real `settings.json` in
/// `crates/conway-cli/tests/first_run.rs`, on the identical footing
/// [`apply_opinion_set`]'s own doc states.
pub fn apply_shell_choice(settings_path: &Path, enabled: bool) -> Result<(), String> {
    if !enabled {
        return Ok(());
    }
    let mut plugins = conway::config::schema::ToolsConfig::default().builtin_plugins;
    plugins.push("conway.shell".to_string());
    conway::config::set_builtin_plugins(settings_path, &plugins)
        .map_err(|e| format!("could not enable the shell tool: {e}"))?;
    Ok(())
}

/// [`apply_shell_choice`]'s sibling for `conway.confine` (harness gap review
/// 2026-09-01, decision `01M1FQG08GDQ71984T0W0RJ019`, question 6): enables
/// (`enabled: true`) or leaves entirely untouched (`enabled: false`) the
/// `confined_bash` tool. Unlike `conway.shell` (a compiled-in BUILT-IN,
/// toggled through `tools.builtin_plugins`), `conway.confine` is a
/// first-party PLUGIN, resolved the same way every other member of
/// [`crate::first_party_plugins::DEFAULT_OPINION_SET`] is --
/// [`conway::config::set_plugin_installed`], not `set_builtin_plugins`.
///
/// **Writes nothing at all when `enabled` is `false`**, the identical
/// byte-cost discipline [`apply_shell_choice`]'s own doc states.
///
/// `pub`: covered directly against a real `settings.json`, on the identical
/// footing [`apply_shell_choice`]'s own doc states.
pub fn apply_confine_choice(settings_path: &Path, enabled: bool) -> Result<(), String> {
    if !enabled {
        return Ok(());
    }
    conway::config::set_plugin_installed(settings_path, conway_plugin_confine::PLUGIN_ID, true)
        .map_err(|e| format!("could not enable the confined shell tool: {e}"))?;
    Ok(())
}

/// Formats the guided-setup transcript for a just-installed opinion set:
/// one row per `(id, summary)` in `rows` (`first_party_plugins::
/// opinion_set_summaries`'s own return shape), followed by exactly ONE
/// removal sentence naming both routes an operator has to undo any single
/// entry -- `conway plugin remove <id>` (board item
/// `01M1FSDRF20E2EGHCG3RK28DKH`, `commands::plugin::PluginAction::Remove`)
/// and hand-editing `plugins.install` in `settings_path` directly.
///
/// A pure string formatter, printed verbatim by [`offer_opinion_set_and_
/// shell`] -- mirrors [`context_window_setup_notice`]'s own "format as a
/// string, print it, test the string directly" shape (this module's own
/// top doc, "How this is structured for testability without a terminal").
/// [`run_guided_setup`] is the one caller no automated test can drive end
/// to end (no pty, C-04); this function is what a test CAN observe of that
/// same transcript.
fn opinion_set_transcript(rows: &[(&str, String)], settings_path: &Path) -> String {
    let mut out = String::from("Installing conway's own opinion set:\n");
    for (id, summary) in rows {
        out.push_str(&format!("  {id:<16} {summary}\n"));
    }
    out.push_str(&format!(
        "Remove any of these with `conway plugin remove <id>` or by deleting its line from \
         plugins.install in {}.\n",
        settings_path.display()
    ));
    out
}

/// The two-line caveat printed immediately above the shell y/n prompt --
/// bash is the one member of this offer that is not merely an opinion but a
/// genuine widening of what a session can reach, so it gets its own,
/// unmissable warning rather than folding into the opinion-set table above.
const SHELL_CAVEAT: &str = "  bash is NOT confined by --root -- a shell command reaches anything \
                             your user account can.\n  Every bash call still passes the \
                             permission prompt, same as every other tool.";

/// Detects `conway.confine`'s own OS containment primitive
/// (`sandbox-exec`/`bwrap`) at its default path -- the DETECT half of
/// question 6's "detect the primitive; offer the confined shell first"
/// (harness gap review 2026-09-01, decision `01M1FQG08GDQ71984T0W0RJ019`).
/// `Some` only when the primitive genuinely exists on THIS machine, never
/// merely "this OS usually has one" -- [`offer_opinion_set_and_shell`]
/// falls back to the plain, unconfined `bash` question when this returns
/// `None`, exactly the pre-existing behavior for every operator on a
/// machine (or a target this crate implements no primitive for at all)
/// where the confined tool could not actually run.
fn detect_confine_primitive() -> Option<std::path::PathBuf> {
    let path = conway_plugin_confine::default_primitive_path();
    path.is_file().then_some(path)
}

/// The imperative shell around [`apply_opinion_set`]/[`apply_shell_choice`]/
/// [`apply_confine_choice`]: prints the opinion-set transcript, installs it,
/// then asks the shell question this board item's own ruling shapes
/// (question 6: "detect the primitive; offer the confined shell first").
/// Only reached from [`run_guided_setup`], after the backend flow itself
/// already settled on `Configured` -- see that function's own doc for why
/// this runs exactly once per run regardless of which internal return point
/// reached `Configured`.
fn offer_opinion_set_and_shell(path: &Path) {
    match apply_opinion_set(path) {
        Ok(ids) => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let env: HashMap<String, String> = std::env::vars().collect();
            let summaries = crate::first_party_plugins::opinion_set_summaries(&cwd, &env);
            let rows: Vec<(&str, String)> = ids
                .iter()
                .map(|&id| {
                    let summary = summaries
                        .iter()
                        .find(|(sid, _)| *sid == id)
                        .map(|(_, s)| s.clone())
                        .unwrap_or_default();
                    (id, summary)
                })
                .collect();
            println!();
            print!("{}", opinion_set_transcript(&rows, path));
        }
        Err(e) => {
            println!();
            println!("Could not install conway's own opinion set: {e}");
        }
    }

    // Question 6: when this machine actually has a containment primitive,
    // the confined shell is offered FIRST, defaulting to yes -- it is the
    // strictly safer of the two (every write outside `--root` is refused
    // by the OS itself), so accepting it needs no caveat the way plain
    // `bash` does. Declining it (or `Esc`) falls through to the ORIGINAL
    // plain-bash question below, unchanged -- an operator who genuinely
    // wants the unconfined tool still gets it, on the identical terms as
    // before this item. A machine with no primitive at all skips straight
    // to the plain-bash question, exactly the pre-existing behavior.
    if let Some(primitive) = detect_confine_primitive() {
        println!();
        println!(
            "This machine has an OS containment primitive ({}). Enable conway.confine's \
             confined shell tool? Every command it runs is refused if it tries to write \
             outside this session's --root; reads and network are unaffected. [Y/n]",
            primitive.display()
        );
        match read_single_key() {
            Some(KeyCode::Char('n')) | Some(KeyCode::Char('N')) => {
                offer_plain_shell(path);
            }
            _ => match apply_confine_choice(path, true) {
                Ok(()) => {
                    println!(
                        "Enabled. Disable it later by removing \"conway.confine\" from \
                         plugins.install in {}.",
                        path.display()
                    );
                }
                Err(e) => {
                    println!("Could not enable the confined shell tool: {e}");
                    offer_plain_shell(path);
                }
            },
        }
    } else {
        offer_plain_shell(path);
    }
}

/// The original, unconfined-`bash` question -- unchanged from before this
/// item, factored out so [`offer_opinion_set_and_shell`] can reach it both
/// as the ordinary path (no containment primitive detected) and as the
/// fallback after an operator declines the confined offer.
///
/// `Esc` (or any key other than `y`/`Y`) keeps the SAME skip semantics
/// every other prompt in this module already has: declining costs nothing
/// beyond "ask again later" (`docs/getting-started.md`'s own "Enabling
/// bash" section, printed here as the one-liner to run it later).
fn offer_plain_shell(path: &Path) {
    println!();
    println!("Enable the bash shell tool? [y/N]");
    println!("{SHELL_CAVEAT}");
    match read_single_key() {
        Some(KeyCode::Char('y')) | Some(KeyCode::Char('Y')) => {
            match apply_shell_choice(path, true) {
                Ok(()) => println!(
                    "Enabled. Disable it later by removing \"conway.shell\" from \
                     tools.builtin_plugins in {}.",
                    path.display()
                ),
                Err(e) => println!("Could not enable the shell tool: {e}"),
            }
        }
        _ => {
            println!(
                "Skipped. Enable it later by adding \"conway.shell\" to tools.builtin_plugins \
                 in {} -- see docs/getting-started.md#enabling-bash-shell-commands.",
                path.display()
            );
        }
    }
}

/// The retry half of [`finish_setup`]: only reachable for a `settings.json`
/// entry that was JUST removed there, so this always re-prompts for a
/// literal credential rather than re-checking the environment (an
/// `api_key_env` retry has nothing new to try without restarting the whole
/// process -- see `resolve_credential_plan`'s own doc for why that path is
/// declined outright by [`finish_setup`] instead of looping here).
///
/// Runs `handle_context_window_at_setup` for the retried `choice` before
/// recursing back into [`finish_setup`], closing the gap this path
/// previously had (board item: setup-time context window, ASK + PERSIST,
/// acceptance 4): a retry with a corrected credential is just as real a
/// setup as the first attempt, and skipping this step here would have left
/// exactly the retried model's window unrecorded for no reason a caller
/// could see.
async fn retry_credential_and_finish(
    path: &Path,
    id: &str,
    chain_so_far: &mut Vec<String>,
    env: &HashMap<String, String>,
) -> GuidedSetupOutcome {
    let Some(choice) = HOSTED_CHOICES.iter().find(|c| c.id == id) else {
        // The local-Ollama offer has no credential to retry at all -- a
        // verification failure there is never a "wrong key" (there is no
        // key), so retrying can only mean "pick a different provider",
        // which is the ordinary decline-and-rerun path.
        return decline_or_keep(chain_so_far.as_slice());
    };
    print!("Paste your {} key: ", choice.label);
    let _ = std::io::stdout().flush();
    let Some(raw) = read_secret_line() else {
        return decline_or_keep(chain_so_far.as_slice());
    };
    let key = match validate_credential_input(&raw) {
        Ok(key) => key,
        Err(msg) => {
            println!("{msg}");
            return decline_or_keep(chain_so_far.as_slice());
        }
    };
    let entry_json = backend_entry_json(choice, &CredentialSource::Literal(key));
    handle_context_window_at_setup(
        env,
        &chain_entry(choice.id, choice.default_model),
        choice.base_url,
        choice.dialect,
        choice.kind,
        choice.default_model,
    )
    .await;
    Box::pin(finish_setup(
        path,
        id,
        &entry_json,
        choice.default_model,
        chain_so_far,
        env,
    ))
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // ---- discover_setup_context_window (board item: context-window ----
    // ---- declaration honesty, num_ctx) ----

    #[tokio::test]
    async fn discover_setup_context_window_finds_a_real_ollama_window() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/api/show"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "model_info": {
                        "general.architecture": "glm5.2",
                        "glm5.2.context_length": 1_048_576
                    }
                })),
            )
            .mount(&server)
            .await;

        let window = discover_setup_context_window(&server.uri(), Some("ollama"), "glm-5.2").await;
        assert_eq!(window, Some(1_048_576));
    }

    #[tokio::test]
    async fn discover_setup_context_window_is_none_for_a_non_ollama_dialect() {
        // No mock registered: proves no network call is even attempted for
        // a dialect with no known discovery endpoint.
        let server = wiremock::MockServer::start().await;
        let window =
            discover_setup_context_window(&server.uri(), Some("openai"), "gpt-4o-mini").await;
        assert_eq!(window, None);
    }

    #[tokio::test]
    async fn discover_setup_context_window_is_none_when_dialect_is_absent() {
        let server = wiremock::MockServer::start().await;
        let window = discover_setup_context_window(&server.uri(), None, "claude-sonnet-4-6").await;
        assert_eq!(window, None);
    }

    // ---- context_window_setup_notice (board item: setup-time context ----
    // ---- window, ASK + PERSIST) ----

    #[test]
    fn context_window_setup_notice_names_the_key_window_and_the_file_it_was_saved_to() {
        let path = std::path::Path::new("/home/op/.conway/models.json");
        let msg = context_window_setup_notice("ollama/glm-5.2", 1_048_576, path);
        assert!(msg.contains("ollama/glm-5.2"));
        assert!(msg.contains("1048576"));
        assert!(
            msg.contains("/home/op/.conway/models.json"),
            "must name the exact file the value was actually written to: {msg}"
        );
    }

    // ---- validate_context_window_input (board item: setup-time context ----
    // ---- window, ASK + PERSIST) ----

    #[test]
    fn validate_context_window_input_treats_a_blank_answer_as_skip_not_an_error() {
        assert_eq!(validate_context_window_input(""), Ok(None));
        assert_eq!(validate_context_window_input("   "), Ok(None));
    }

    #[test]
    fn validate_context_window_input_accepts_a_plausible_number() {
        assert_eq!(validate_context_window_input("131072"), Ok(Some(131_072)));
        assert_eq!(validate_context_window_input("  1048576  "), Ok(Some(1_048_576)));
    }

    #[test]
    fn validate_context_window_input_rejects_zero_non_numeric_and_implausibly_large() {
        assert!(validate_context_window_input("0").is_err());
        assert!(validate_context_window_input("not a number").is_err());
        assert!(validate_context_window_input("-5").is_err());
        assert!(validate_context_window_input("999999999999").is_err());
    }

    // ---- context_window_is_verified (board item: setup-time context ----
    // ---- window, ASK + PERSIST) ----

    #[test]
    fn context_window_is_verified_true_for_anthropic_and_the_openai_dialect() {
        assert!(context_window_is_verified("anthropic", None));
        assert!(context_window_is_verified("openai-compat", Some("openai")));
    }

    #[test]
    fn context_window_is_verified_false_for_ollama_and_anything_unrecognized() {
        assert!(!context_window_is_verified("openai-compat", Some("ollama")));
        assert!(!context_window_is_verified("openai-compat", None));
        assert!(!context_window_is_verified(
            "openai-compat",
            Some("totally-unknown")
        ));
    }

    // ---- persist_context_window (board item: setup-time context window, ----
    // ---- ASK + PERSIST) ----

    /// `CONWAY_CONFIG_DIR` pointed at a fresh, never-written scratch
    /// directory -- so `conway::config::metadata_path_for`'s own user-layer
    /// read resolves to "absent" deterministically, rather than the
    /// invoking user's REAL `~/.conway/settings.json` (which
    /// `conway::config::discovery::user_config_path`'s own doc says every
    /// caller falls back to whenever `CONWAY_CONFIG_DIR` is unset). Mirrors
    /// `crates/conway/tests/support/mod.rs::isolated_env`'s own documented
    /// reasoning -- the identical hermeticity hazard, in this crate's own
    /// unit tests instead of that crate's integration tests.
    fn isolated_env() -> HashMap<String, String> {
        let home = std::env::temp_dir().join(format!(
            "conway-cli-first-run-test-isolated-home-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        env_with(&[("CONWAY_CONFIG_DIR", home.to_string_lossy().as_ref())])
    }

    #[test]
    fn persist_context_window_writes_into_dot_conway_models_json_under_cwd() {
        let dir = tempfile::tempdir().expect("tempdir");

        let path =
            persist_context_window_at(dir.path(), &isolated_env(), "ollama/glm-5.2", 1_048_576)
                .expect("persist must succeed against a writable temp dir");

        assert_eq!(path, dir.path().join(".conway").join("models.json"));
        let text = std::fs::read_to_string(&path).expect("read the written file");
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            parsed["models"]["ollama/glm-5.2"]["max_context_tokens"],
            1_048_576
        );
    }

    #[test]
    fn persist_context_window_is_idempotent_for_the_same_value() {
        let dir = tempfile::tempdir().expect("tempdir");
        let env = isolated_env();
        persist_context_window_at(dir.path(), &env, "ollama/glm-5.2", 1_048_576).unwrap();
        let path = dir.path().join(".conway").join("models.json");
        let before = std::fs::read_to_string(&path).unwrap();

        persist_context_window_at(dir.path(), &env, "ollama/glm-5.2", 1_048_576).unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn persist_context_window_honors_an_existing_metadata_path_override_in_cwd() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".conway")).unwrap();
        std::fs::write(
            dir.path().join(".conway").join("settings.json"),
            r#"{"models":{"metadata_path":"custom-models.json"}}"#,
        )
        .unwrap();

        let path =
            persist_context_window_at(dir.path(), &isolated_env(), "ollama/glm-5.2", 1_048_576)
                .expect("persist must succeed");

        assert_eq!(path, dir.path().join("custom-models.json"));
    }

    // ---- opinion_set_transcript: board item `01M1FS34GNZEVZP4ZBVC90VD6J`, ----
    // ---- acceptance 5's own observable half (no pty involved) ----

    #[test]
    fn opinion_set_transcript_lists_every_row_and_exactly_one_removal_sentence() {
        let path = std::path::Path::new("/home/op/.conway/settings.json");
        let rows = vec![
            ("conway.idiom", "a short harness primer".to_string()),
            (
                "conway.stepguard",
                "notices repeated tool calls".to_string(),
            ),
        ];
        let transcript = opinion_set_transcript(&rows, path);

        for (id, summary) in &rows {
            assert!(
                transcript.contains(*id),
                "transcript must name every installed id: {transcript}"
            );
            assert!(
                transcript.contains(summary.as_str()),
                "transcript must carry every id's own summary: {transcript}"
            );
        }
        assert_eq!(
            transcript.matches("conway plugin remove").count(),
            1,
            "exactly one removal sentence naming the subcommand route: {transcript}"
        );
        assert_eq!(
            transcript.matches("plugins.install").count(),
            1,
            "exactly one removal sentence naming the hand-edit route: {transcript}"
        );
        assert!(
            transcript.contains("/home/op/.conway/settings.json"),
            "the removal sentence must name the actual settings path: {transcript}"
        );
    }

    // ---- apply_shell_choice: pure, no network, no terminal ----

    #[test]
    fn apply_shell_choice_false_leaves_the_file_byte_identical() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{\"backends\": {}}\n").expect("write fixture");
        let before = std::fs::read_to_string(&path).expect("read before");

        apply_shell_choice(&path, false).expect("declining must never fail");

        let after = std::fs::read_to_string(&path).expect("read after");
        assert_eq!(
            before, after,
            "declining the shell offer must not touch the file at all"
        );
    }

    #[test]
    fn apply_shell_choice_true_adds_conway_shell_alongside_the_preset_ids() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{\"backends\": {}}\n").expect("write fixture");

        apply_shell_choice(&path, true).expect("enabling must succeed against a fresh file");

        let text = std::fs::read_to_string(&path).expect("read after");
        let value: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        let ids: Vec<String> = value["tools"]["builtin_plugins"]
            .as_array()
            .expect("builtin_plugins must be an array")
            .iter()
            .map(|v| v.as_str().expect("each id is a string").to_string())
            .collect();
        assert!(ids.contains(&"conway.shell".to_string()), "{ids:?}");
        assert!(ids.contains(&"conway.fs".to_string()), "{ids:?}");
        assert!(ids.contains(&"conway.subagent".to_string()), "{ids:?}");
        assert!(ids.contains(&"conway.report".to_string()), "{ids:?}");
    }

    // ---- apply_confine_choice: pure, no network, no terminal ----

    #[test]
    fn apply_confine_choice_false_leaves_the_file_byte_identical() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{\"backends\": {}}\n").expect("write fixture");
        let before = std::fs::read_to_string(&path).expect("read before");

        apply_confine_choice(&path, false).expect("declining must never fail");

        let after = std::fs::read_to_string(&path).expect("read after");
        assert_eq!(
            before, after,
            "declining the confine offer must not touch the file at all"
        );
    }

    #[test]
    fn apply_confine_choice_true_adds_conway_confine_to_plugins_install() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{\"backends\": {}}\n").expect("write fixture");

        apply_confine_choice(&path, true).expect("enabling must succeed against a fresh file");

        let text = std::fs::read_to_string(&path).expect("read after");
        let value: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        let ids: Vec<String> = value["plugins"]["install"]
            .as_array()
            .expect("plugins.install must be an array")
            .iter()
            .map(|v| v.as_str().expect("each id is a string").to_string())
            .collect();
        assert!(
            ids.contains(&conway_plugin_confine::PLUGIN_ID.to_string()),
            "{ids:?}"
        );
    }

    /// `conway.confine` is a first-party PLUGIN (`plugins.install`), never a
    /// built-in (`tools.builtin_plugins`) -- the exact distinction
    /// [`apply_confine_choice`]'s own doc draws against `apply_shell_choice`.
    /// This regression-guards that the two writers never converge on the
    /// same key by accident.
    #[test]
    fn apply_confine_choice_never_touches_tools_builtin_plugins() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{\"backends\": {}}\n").expect("write fixture");

        apply_confine_choice(&path, true).expect("enabling must succeed against a fresh file");

        let text = std::fs::read_to_string(&path).expect("read after");
        let value: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert!(
            value.get("tools").is_none(),
            "apply_confine_choice must never write tools.builtin_plugins: {value}"
        );
    }

    // ---- chain_entry: the ONE construction verify_backend and the real ----
    // ---- written chain both use (P-14) ----

    #[test]
    fn chain_entry_formats_backend_slash_model() {
        assert_eq!(chain_entry("local", "qwen3:4b"), "local/qwen3:4b");
        assert_eq!(
            chain_entry("anthropic", "claude-sonnet-4-6"),
            "anthropic/claude-sonnet-4-6"
        );
    }

    // ---- decline_or_keep: acceptance 4's own discriminating message ----

    #[test]
    fn decline_or_keep_declines_outright_when_nothing_was_ever_configured() {
        assert_eq!(decline_or_keep(&[]), GuidedSetupOutcome::Declined);
    }

    /// Acceptance 4: declining the "add another?" loop after at least one
    /// provider already succeeded must report `Configured`, never
    /// `Declined` -- printing `decline()`'s own "no model provider is
    /// configured" over a file that already routes somewhere would be a
    /// straightforward lie (GP-14).
    #[test]
    fn decline_or_keep_reports_configured_when_a_provider_already_succeeded() {
        assert_eq!(
            decline_or_keep(&["local/qwen3:4b".to_string()]),
            GuidedSetupOutcome::Configured
        );
        assert_eq!(
            decline_or_keep(&[
                "local/qwen3:4b".to_string(),
                "ollama_cloud/glm-5.2".to_string()
            ]),
            GuidedSetupOutcome::Configured
        );
    }

    // ---- resolve_credential_plan ----

    #[test]
    fn reuses_an_already_set_env_var_rather_than_prompting() {
        let choice = &HOSTED_CHOICES[0];
        let env = env_with(&[(choice.credential_env, "sk-real-value")]);
        assert_eq!(
            resolve_credential_plan(choice, &env),
            CredentialPlan::ReuseEnvVar
        );
    }

    /// Acceptance 2 (env-var half), pinned against `ollama_cloud`
    /// specifically rather than only the generic `HOSTED_CHOICES[0]` case
    /// above: `resolve_credential_plan` is generic over every choice, but
    /// this is the one this item actually adds, so it gets its own direct
    /// assertion.
    #[test]
    fn ollama_cloud_reuses_ollama_api_key_when_already_set() {
        let choice = HOSTED_CHOICES
            .iter()
            .find(|c| c.id == "ollama_cloud")
            .expect("ollama_cloud is one of the shipped choices");
        let env = env_with(&[("OLLAMA_API_KEY", "sk-real-value")]);
        assert_eq!(
            resolve_credential_plan(choice, &env),
            CredentialPlan::ReuseEnvVar
        );
    }

    #[test]
    fn ollama_cloud_prompts_for_a_literal_when_ollama_api_key_is_unset() {
        let choice = HOSTED_CHOICES
            .iter()
            .find(|c| c.id == "ollama_cloud")
            .expect("ollama_cloud is one of the shipped choices");
        assert_eq!(
            resolve_credential_plan(choice, &HashMap::new()),
            CredentialPlan::PromptForLiteral
        );
    }

    #[test]
    fn an_unset_env_var_prompts_for_a_literal() {
        let choice = &HOSTED_CHOICES[0];
        let env = HashMap::new();
        assert_eq!(
            resolve_credential_plan(choice, &env),
            CredentialPlan::PromptForLiteral
        );
    }

    #[test]
    fn a_whitespace_only_env_var_counts_as_unset() {
        let choice = &HOSTED_CHOICES[0];
        let env = env_with(&[(choice.credential_env, "   ")]);
        assert_eq!(
            resolve_credential_plan(choice, &env),
            CredentialPlan::PromptForLiteral
        );
    }

    // ---- validate_credential_input (P-10 boundary) ----

    #[test]
    fn an_empty_credential_is_rejected_not_panicked_on() {
        assert!(validate_credential_input("").is_err());
        assert!(validate_credential_input("   \n  ").is_err());
    }

    #[test]
    fn an_implausibly_long_credential_is_rejected() {
        let huge = "x".repeat(5000);
        assert!(validate_credential_input(&huge).is_err());
    }

    #[test]
    fn a_credential_containing_json_metacharacters_is_accepted_and_trimmed() {
        let got = validate_credential_input("  sk-\"weird\\value\"  \n").expect("valid");
        assert_eq!(got, "sk-\"weird\\value\"");
    }

    // ---- backend_entry_json / local_offer_entry_json: valid JSON objects, ----
    // ---- and the credential lands in the field the spec asks for ----

    #[test]
    fn a_literal_credential_is_written_as_api_key_never_api_key_env() {
        let choice = &HOSTED_CHOICES[0];
        let json = backend_entry_json(choice, &CredentialSource::Literal("sk-abc".to_string()));
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json object");
        assert_eq!(value["api_key"], "sk-abc");
        assert_eq!(value["api_key_env"], "");
        assert_eq!(value["kind"], choice.kind);
    }

    #[test]
    fn an_env_var_credential_is_written_as_api_key_env_never_the_secret_itself() {
        let choice = &HOSTED_CHOICES[0];
        let json = backend_entry_json(
            choice,
            &CredentialSource::EnvVar("ANTHROPIC_API_KEY".to_string()),
        );
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json object");
        assert_eq!(value["api_key_env"], "ANTHROPIC_API_KEY");
        assert_eq!(value["api_key"], "");
        // LOAD-BEARING for acceptance 7: the literal secret value must
        // never appear anywhere in this written artifact when the operator
        // already had it in the environment.
        assert!(!json.contains("sk-"));
    }

    #[test]
    fn the_openai_choice_carries_its_own_dialect_and_base_url() {
        let choice = HOSTED_CHOICES
            .iter()
            .find(|c| c.id == "openai")
            .expect("openai is one of the shipped choices");
        let json = backend_entry_json(choice, &CredentialSource::EnvVar("OPENAI_API_KEY".into()));
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(value["kind"], "openai-compat");
        assert_eq!(value["dialect"], "openai");
        assert_eq!(value["base_url"], "https://api.openai.com/v1");
    }

    /// Acceptance 1: picking Ollama Cloud writes an entry matching the
    /// proven shape from the operator's own working `~/.conway/
    /// settings.json` (archived 2026-08-13) -- `kind: "openai-compat"`,
    /// `dialect: "ollama"` (NOT `"openai"` -- this is the exact judgment
    /// call that config settles), the confirmed-live `base_url`, and the
    /// deliberately-chosen `default_model` (`glm-5.2`, not the smaller
    /// `gpt-oss:20b` -- see the constant's own doc for why).
    #[test]
    fn the_ollama_cloud_choice_carries_its_own_dialect_base_url_and_model() {
        let choice = HOSTED_CHOICES
            .iter()
            .find(|c| c.id == "ollama_cloud")
            .expect("ollama_cloud is one of the shipped choices");
        let json = backend_entry_json(choice, &CredentialSource::EnvVar("OLLAMA_API_KEY".into()));
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(value["kind"], "openai-compat");
        assert_eq!(value["dialect"], "ollama");
        assert_eq!(value["base_url"], "https://ollama.com/v1");
        assert_eq!(choice.default_model, "glm-5.2");
        assert_eq!(choice.credential_env, "OLLAMA_API_KEY");
    }

    /// Acceptance 1, the other half: a literal-credential Ollama Cloud
    /// entry writes `api_key`, never `api_key_env` -- the same two-shape
    /// contract [`backend_entry_json`] gives every other hosted choice,
    /// proven here rather than assumed from the generic tests above (which
    /// deliberately exercise `HOSTED_CHOICES[0]`, not this new entry).
    #[test]
    fn the_ollama_cloud_choice_also_accepts_a_literal_credential() {
        let choice = HOSTED_CHOICES
            .iter()
            .find(|c| c.id == "ollama_cloud")
            .expect("ollama_cloud is one of the shipped choices");
        let json = backend_entry_json(choice, &CredentialSource::Literal("sk-cloud".to_string()));
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(value["api_key"], "sk-cloud");
        assert_eq!(value["api_key_env"], "");
    }

    #[test]
    fn hosted_choices_offers_exactly_three_entries_anthropic_openai_ollama_cloud() {
        let ids: Vec<&str> = HOSTED_CHOICES.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec!["anthropic", "openai", "ollama_cloud"]);
    }

    #[test]
    fn local_offer_entry_declares_local_true_and_no_credential_field_set() {
        let offer = LocalOffer {
            base_url: LOCAL_OLLAMA_BASE_URL.to_string(),
            model: "qwen3:4b".to_string(),
        };
        let json = local_offer_entry_json(&offer);
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(value["kind"], "openai-compat");
        assert_eq!(value["dialect"], "ollama");
        assert_eq!(value["base_url"], LOCAL_OLLAMA_BASE_URL);
        assert_eq!(value["local"], true);
        assert_eq!(value["api_key"], "");
        assert_eq!(value["api_key_env"], "");
    }

    // ---- non_interactive_guidance: names the file AND the exact snippet ----

    #[test]
    fn non_interactive_guidance_names_the_path_and_a_pasteable_snippet() {
        let path = Path::new("/home/alice/.conway/settings.json");
        let msg = non_interactive_guidance(path);
        assert!(msg.starts_with(GUIDED_SETUP_MARKER));
        assert!(msg.contains("/home/alice/.conway/settings.json"));
        // Must be a real, complete, copy-pasteable JSON snippet, not a
        // vague description -- parse the embedded object literally.
        let start = msg.find('{').expect("snippet has an opening brace");
        let end = msg.rfind('}').expect("snippet has a closing brace");
        let snippet = &msg[start..=end];
        let parsed: serde_json::Value =
            serde_json::from_str(snippet).expect("the printed snippet must itself be valid JSON");
        assert!(parsed["backends"]["anthropic"]["api_key_env"].is_string());
    }

    // ---- acceptance 8's own grep check: production code calls the real ----
    // ---- predicate rather than restating its condition (P-14) ----

    #[test]
    fn main_rs_calls_should_offer_guided_setup_rather_than_restating_its_condition() {
        let main_rs = include_str!("main.rs");
        // The exact code shape, not merely the bare method-name substring:
        // a doc comment ABOUT this call also contains the bare substring
        // `"should_offer_guided_setup()"`, so checking for that alone would
        // stay green even if the real `if` condition were replaced with a
        // hand-rolled `matches!()` right below an unchanged comment --
        // exactly the gap P-15 exists to catch, and one this test's own
        // first draft had (confirmed by deliberately reverting the call
        // below to a restated `matches!()` while leaving the surrounding
        // comment untouched: the bare-substring version of this assertion
        // stayed green; this line-anchored one goes red).
        assert!(
            main_rs.contains("if fleet_usability.should_offer_guided_setup() {"),
            "main.rs must call FleetUsability::should_offer_guided_setup() directly, as the \
             actual branch condition -- P-14 forbids a second copy of its rule"
        );
        // And it must not hand-roll the matches!() this predicate already
        // encapsulates anywhere else in the file -- a restatement is
        // exactly the drift P-14 exists to prevent, even if it happens to
        // agree with the real rule today.
        assert!(
            !main_rs.contains("NoBackendsConfigured | FleetUsability::AllUnusable")
                && !main_rs.contains(
                    "NoBackendsConfigured | conway::backend_usability::FleetUsability::AllUnusable"
                ),
            "main.rs must not restate should_offer_guided_setup()'s own condition"
        );
    }
}
