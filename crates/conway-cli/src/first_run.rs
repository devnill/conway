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
//! [`validate_credential_input`], [`backend_entry_json`],
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
//! `/api/show`. [`context_window_setup_notice`]/`models_json_snippet` are
//! pure string formatting, tested alongside the other pure functions above.
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

/// The `.conway/models.json` snippet [`context_window_setup_notice`] prints
/// naming `model`'s discovered `window` — the exact, copy-pasteable shape
/// `getting-started.md`'s own worked examples use for this file, built the
/// same "always serialize a real value, never hand-format JSON" way
/// [`backend_entry_json`] is (P-10: this text ends up on a real terminal).
fn models_json_snippet(model: &str, window: u32) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "models": {
            model: {
                "max_context_tokens": window,
                "tool_calling": "yes",
                "reasoning": false,
                "reliability_tier": "community"
            }
        }
    }))
    .expect("a constructed JSON value always serializes")
}

/// The DISCOVER half of the operator's "discover, or ask if discovery
/// fails" setup-time ruling — the ASK half (a UI prompt for the value when
/// discovery finds nothing) is a disclosed follow-up, not implemented by
/// this item; see this module's own top doc.
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
/// **Deliberately does NOT persist the result.** `conway`'s facade reads a
/// per-model context-window override exclusively from the SEPARATE
/// `.conway/models.json` file (`[models].metadata_path`, resolved relative
/// to the process's own working directory — see `getting-started.md`),
/// never from any key inside a `[backends.<id>]` entry itself (confirmed by
/// reading `conway::builder::build_backend_context`/`models_overrides_for`:
/// neither reads `BackendEntry.extra` for a `models` sub-key, and
/// `conway_plugin_backends::factory`'s own module doc states outright that
/// `ctx.extra`'s precedence-merge overlay is exercised for the `anthropic`
/// kind only — `"openai-compat"`'s `Profile` is "resolved as a whole
/// bundle, not an overlay map `ctx.extra` could sensibly patch key-for-
/// key"). Writing a real, working entry into `.conway/models.json` needs
/// its own path-resolution decision (project-scope-relative by default,
/// versus every other setup-time write in this module being user-scope
/// only) and its own writer (a fourth JSON-file shape this crate's byte-
/// preserving splicer has never handled) — a real, separately-scoped
/// follow-up, not invented here unverified. [`context_window_setup_notice`]
/// is the honest interim: it tells the operator exactly what was
/// discovered and exactly what to paste to make it take effect, rather
/// than silently writing to a location nothing reads (which this module's
/// very first implementation of this function did, and which is exactly
/// the kind of claim GP-14 — the whole governing principle of this item —
/// forbids: never presenting an inert write as if it configured anything).
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

/// The operator-facing message printed after a successful discovery — names
/// the model, the discovered window, and the exact `.conway/models.json`
/// snippet that makes it take effect, per `INTENT.md` §8.3 ("refuse and
/// name what changed") and this module's own `non_interactive_guidance`
/// precedent for a copy-pasteable snippet over a vague description.
pub fn context_window_setup_notice(model: &str, window: u32) -> String {
    format!(
        "conway discovered a {window}-token context window for {model} from the server. This \
         isn't persisted automatically yet -- add it to .conway/models.json to make routing and \
         admission reflect it:\n\n{}",
        models_json_snippet(model, window)
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
                if let Some(window) =
                    discover_setup_context_window(&offer.base_url, Some("ollama"), &offer.model)
                        .await
                {
                    println!();
                    println!("{}", context_window_setup_notice(&offer.model, window));
                }
                match finish_setup(
                    path,
                    LOCAL_OLLAMA_ID,
                    &entry_json,
                    &offer.model,
                    &mut chain,
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
        if let Some(base_url) = choice.base_url {
            if let Some(window) =
                discover_setup_context_window(base_url, choice.dialect, choice.default_model).await
            {
                println!();
                println!(
                    "{}",
                    context_window_setup_notice(choice.default_model, window)
                );
            }
        }
        match finish_setup(
            path,
            choice.id,
            &entry_json,
            choice.default_model,
            &mut chain,
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
pub async fn finish_setup(
    path: &Path,
    id: &str,
    entry_json: &str,
    model: &str,
    chain_so_far: &mut Vec<String>,
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
                    retry_credential_and_finish(path, id, chain_so_far).await
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

/// Formats the guided-setup transcript for a just-installed opinion set:
/// one row per `(id, summary)` in `rows` (`first_party_plugins::
/// opinion_set_summaries`'s own return shape), followed by exactly ONE
/// removal sentence naming both routes an operator has to undo any single
/// entry -- a `conway plugin remove <id>` subcommand (tracked on the board,
/// not yet landed -- see this function's own inline note) and hand-editing
/// `plugins.install` in `settings_path` directly.
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
    // "(subcommand tracked on the board)": `conway plugin remove <id>`
    // itself is a sibling board item's own scope, not this one's -- see
    // this module's own WHAT NOT TO BUILD. Drop the parenthetical the day
    // that subcommand ships; nothing here re-derives its presence
    // automatically, so this is a plain, deliberate literal to revisit by
    // hand.
    out.push_str(&format!(
        "Remove any of these with `conway plugin remove <id>` (subcommand tracked on the \
         board) or by deleting its line from plugins.install in {}.\n",
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

/// The imperative shell around [`apply_opinion_set`]/[`apply_shell_choice`]:
/// prints the opinion-set transcript, installs it, then asks the one new
/// yes/no question this board item adds. Only reached from [`run_guided_
/// setup`], after the backend flow itself already settled on `Configured`
/// -- see that function's own doc for why this runs exactly once per run
/// regardless of which internal return point reached `Configured`.
///
/// `Esc` (or any key other than `y`/`Y`) keeps the SAME skip semantics
/// every other prompt in this module already has: declining costs nothing
/// beyond "ask again later" (`docs/getting-started.md`'s own "Enabling
/// bash" section, printed here as the one-liner to run it later).
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
async fn retry_credential_and_finish(
    path: &Path,
    id: &str,
    chain_so_far: &mut Vec<String>,
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
    Box::pin(finish_setup(
        path,
        id,
        &entry_json,
        choice.default_model,
        chain_so_far,
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

    // ---- context_window_setup_notice / models_json_snippet (board item: ----
    // ---- context-window declaration honesty, num_ctx) ----

    #[test]
    fn context_window_setup_notice_names_the_model_window_and_a_pasteable_models_json_snippet() {
        let msg = context_window_setup_notice("glm-5.2", 1_048_576);
        assert!(msg.contains("glm-5.2"));
        assert!(msg.contains("1048576"));
        assert!(
            msg.contains(".conway/models.json"),
            "must name the file that actually reads this value: {msg}"
        );
        // The embedded snippet must itself be valid, complete JSON --
        // parse it out and check the exact key path a real
        // `.conway/models.json` reader (`conway::config::model_metadata`)
        // expects.
        let start = msg.find('{').expect("snippet has an opening brace");
        let snippet = &msg[start..];
        let parsed: serde_json::Value =
            serde_json::from_str(snippet).expect("the printed snippet must itself be valid JSON");
        assert_eq!(parsed["models"]["glm-5.2"]["max_context_tokens"], 1_048_576);
    }

    // ---- opinion_set_transcript: board item `01M1FS34GNZEVZP4ZBVC90VD6J`, ----
    // ---- acceptance 5's own observable half (no pty involved) ----

    #[test]
    fn opinion_set_transcript_lists_every_row_and_exactly_one_removal_sentence() {
        let path = std::path::Path::new("/home/op/.conway/settings.json");
        let rows = vec![
            ("conway.idiom", "a short harness primer".to_string()),
            ("conway.stepguard", "notices repeated tool calls".to_string()),
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
