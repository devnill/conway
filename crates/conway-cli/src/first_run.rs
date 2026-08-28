//! The first-run guided-setup flow -- board item `01M11XVEHNMYY942JE63F7MAFH`.
//!
//! When `conway::backend_usability::FleetUsability::should_offer_guided_setup`
//! says nothing usable is configured, this module replaces the old hard
//! `"no backends configured"` error with a short, interactive fix: detect a
//! local provider already running, offer it in one keypress, otherwise ask
//! for one of the two provider shapes `conway-plugin-backends` ships and a
//! credential, save it (`conway::config::set_backend_provider`), prove it
//! with one real completion, and get out of the way.
//!
//! # Appetite, restated here because it is easy to over-build this
//!
//! **Detect, offer, verify, get out of the way.** No model-pinning
//! question, no roles/fallback-chain question -- exactly the local-or-two-
//! hosted-shapes menu below, one credential prompt at most, one verify.
//!
//! # How this is structured for testability without a terminal
//!
//! Everything a test can assert on without driving a real TTY lives in pure
//! functions in the first half of this file: [`resolve_credential_plan`],
//! [`validate_credential_input`], [`backend_entry_json`],
//! [`local_offer_entry_json`], [`non_interactive_guidance`]. Only the very
//! last function, [`run_guided_setup`], touches a real terminal (via
//! `crossterm` raw-mode reads) -- it is a thin imperative shell over the
//! pure functions above and is not, and cannot be, exercised by this crate's
//! own `assert_cmd` suite (no pty is available and none is added -- C-04).
//! [`verify_backend`] and [`detect_local_provider`] are async and touch the
//! network/filesystem, but neither one touches a terminal, so both are
//! covered by ordinary `#[tokio::test]`s against a real mock HTTP server
//! (`crates/conway-cli/tests/first_run.rs`), the same shape
//! `tests/common/mock_backend.rs` already provides for the one-shot suite.

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

/// One of the two provider shapes offered when no local server answered.
/// Deliberately just these two: `docs/providers.md`'s own table names them
/// as the only two `kind`s `conway-plugin-backends` ships, and the appetite
/// ruling ("a fourth question" bar) forbids a longer menu or a free-text
/// base-url prompt that would require also asking about a model.
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
        "roles": { role: { "chain": [format!("{id}/{model}")] } },
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

/// The interactive flow itself: detect, offer, verify, get out of the way.
/// Only reachable when the caller has already confirmed a real, writable
/// terminal is attached (`main.rs`'s own `interactive` computation) --
/// see this module's own doc for why this function is the one part of the
/// module no automated test drives directly.
pub async fn run_guided_setup(env: &HashMap<String, String>) -> GuidedSetupOutcome {
    let Some(path) = conway::config::discovery::user_config_path(env) else {
        println!(
            "conway: could not determine where to write a user config (no home directory \
             found); skipping guided setup."
        );
        return GuidedSetupOutcome::Declined;
    };

    println!();
    println!("{GUIDED_SETUP_MARKER}.");
    println!("Let's fix that. Press Esc at any point to skip and continue without one.");
    println!();
    print!("Looking for a local model server (Ollama on 127.0.0.1:11434)... ");
    let _ = std::io::stdout().flush();

    if let Some(offer) = detect_local_provider(env).await {
        println!("found one, model \"{}\".", offer.model);
        println!("Press Enter to use it, or any other key to see other providers.");
        match read_single_key() {
            Some(KeyCode::Enter) => {
                let entry_json = local_offer_entry_json(&offer);
                return finish_setup(&path, LOCAL_OLLAMA_ID, &entry_json, &offer.model).await;
            }
            Some(KeyCode::Esc) => return decline(),
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
            return decline();
        };
        let choice = match key {
            KeyCode::Esc => return decline(),
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
                    return decline();
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
        return finish_setup(&path, choice.id, &entry_json, choice.default_model).await;
    }
}

fn decline() -> GuidedSetupOutcome {
    println!();
    println!(
        "Skipping setup. No model provider is configured, so turns will fail until you add \
         one -- see docs/getting-started.md, or run conway again to retry this."
    );
    GuidedSetupOutcome::Declined
}

/// Saves `entry_json` under `id`, verifies it with one real turn, and
/// offers a retry loop on failure (acceptance 3: "the single most likely
/// failure in this entire flow"). A failed literal-credential attempt is
/// rolled back (the bad entry removed) before re-prompting, so a decline
/// mid-retry leaves the fleet exactly as it started.
async fn finish_setup(path: &Path, id: &str, entry_json: &str, model: &str) -> GuidedSetupOutcome {
    if let Err(e) = conway::config::set_backend_provider(path, id, entry_json, true) {
        println!("Could not save this provider to {}: {e}", path.display());
        return decline();
    }
    println!("Saved to {}.", path.display());
    print!("Verifying with a real request... ");
    let _ = std::io::stdout().flush();

    match verify_backend(id, entry_json, model).await {
        Ok(()) => {
            println!("it works.");
            GuidedSetupOutcome::Configured
        }
        Err(msg) => {
            println!("that didn't work:");
            println!("  {msg}");
            println!("Try again with a different key? [y/N]");
            match read_single_key() {
                Some(KeyCode::Char('y')) | Some(KeyCode::Char('Y')) => {
                    let _ = conway::config::set_backend_provider(path, id, entry_json, false);
                    retry_credential_and_finish(path, id).await
                }
                _ => {
                    let _ = conway::config::set_backend_provider(path, id, entry_json, false);
                    decline()
                }
            }
        }
    }
}

/// The retry half of [`finish_setup`]: only reachable for a `settings.json`
/// entry that was JUST removed there, so this always re-prompts for a
/// literal credential rather than re-checking the environment (an
/// `api_key_env` retry has nothing new to try without restarting the whole
/// process -- see `resolve_credential_plan`'s own doc for why that path is
/// declined outright by [`finish_setup`] instead of looping here).
async fn retry_credential_and_finish(path: &Path, id: &str) -> GuidedSetupOutcome {
    let Some(choice) = HOSTED_CHOICES.iter().find(|c| c.id == id) else {
        // The local-Ollama offer has no credential to retry at all -- a
        // verification failure there is never a "wrong key" (there is no
        // key), so retrying can only mean "pick a different provider",
        // which is the ordinary decline-and-rerun path.
        return decline();
    };
    print!("Paste your {} key: ", choice.label);
    let _ = std::io::stdout().flush();
    let Some(raw) = read_secret_line() else {
        return decline();
    };
    let key = match validate_credential_input(&raw) {
        Ok(key) => key,
        Err(msg) => {
            println!("{msg}");
            return decline();
        }
    };
    let entry_json = backend_entry_json(choice, &CredentialSource::Literal(key));
    Box::pin(finish_setup(path, id, &entry_json, choice.default_model)).await
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
            .expect("openai is one of the two shipped choices");
        let json = backend_entry_json(choice, &CredentialSource::EnvVar("OPENAI_API_KEY".into()));
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(value["kind"], "openai-compat");
        assert_eq!(value["dialect"], "openai");
        assert_eq!(value["base_url"], "https://api.openai.com/v1");
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
