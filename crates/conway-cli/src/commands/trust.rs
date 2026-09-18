//! `conway trust settings|list|revoke` (board item
//! `01M2TTWSQ53CDWB9VRGSX05XNQ`): the headless half of consent for a
//! project-scoped `.conway/settings.json` -- the same
//! `conway::config::trust::TrustStore` writer the rest of the tree already
//! uses, and the same `<user settings dir>/trust.json` file, reachable from
//! a script or a terminal with no TUI in sight. Deliberately the shape
//! `crate::commands::plugin`'s own module doc already sets for `conway
//! plugin list|install|remove`.
//!
//! # The defect this closes
//!
//! `conway::config::trust::guard_untrusted_project_settings` refuses to
//! start conway when an untrusted project `settings.json` is reachable from
//! the cwd. That refusal is correct and stays exactly as loud as it was:
//! such a file can redirect a backend's `base_url`/`api_key`, which is a
//! credential-exfiltration path, and the friction is the point.
//!
//! What was broken was the way OUT. The refusal named two remedies: the
//! TUI's `/trust settings` command, and the `TrustStore::trust_settings`
//! Rust API. The TUI is the thing that had just refused to start, so it
//! could not be reached; "write an embedder" is not an operator remedy; and
//! `/trust settings` did not actually exist (`crate::tui::commands` parses
//! `/trust permissions`, and nothing else). The only real escape was to
//! delete the file -- which the message never mentioned, and which an
//! operator who WANTS the project config cannot use. It is reachable by
//! accident: a teammate commits a project `settings.json`, you pull, and
//! conway stops starting.
//!
//! This module is the reachable remedy. The refusal's own wording now names
//! it (`conway::config::trust::UntrustedProjectSettings`'s `Display`).
//!
//! # Why this runs before `build_conway`
//!
//! `main.rs` dispatches `Command::Trust` at its own entry point, ahead of
//! the single `build_conway` call every other subcommand goes through. That
//! is not an optimization -- it is the entire point. `build_conway` calls
//! `conway::ConwayBuilder::discover`, which is where the guard fires, so a
//! `conway trust` that waited for a built `Conway` would be refused by
//! exactly the condition it exists to clear. Nothing here needs a `Conway`:
//! the trust store is a standalone file keyed on paths, resolved from the
//! process cwd and environment alone.
//!
//! # What this deliberately does NOT do
//!
//! - **No auto-trust, and no blanket flag.** There is no
//!   `--trust-project-settings` a script could set once and forget. Every
//!   invocation names one file (explicitly, or by the walk from the cwd
//!   that the guard itself uses) and records a decision about that one
//!   file's current BYTES. An edit afterwards re-arms the guard, by design
//!   (`conway::config::trust`'s own "trust subject" doc).
//! - **No warn-and-ignore.** Nothing here downgrades the hard failure.
//!   Without a `conway trust settings`, the refusal is byte-for-byte the
//!   refusal it always was, minus the unreachable advice.
//! - **No degraded TUI.** Consent is never collected by starting the thing
//!   whose config is in question.
//!
//! # Review, then record -- in that order, in one output
//!
//! The `settings` action prints the full contents of the file before it writes
//! anything, and records consent for THOSE bytes (via
//! `conway::config::trust::TrustStore::trust_settings_bytes`, which exists
//! so the bytes shown and the bytes recorded cannot drift apart if the file
//! is rewritten in between). Recording trust does not APPLY the config --
//! the next `conway` invocation does -- so an operator who reads the
//! printed contents and dislikes them has `conway trust revoke <path>`
//! before anything the file says has ever been acted on. That is the same
//! "restart to apply" separation `conway plugin install` already relies on,
//! and it is why this command needs no interactive confirmation prompt of
//! its own: typing the command, naming one file, IS the explicit act of
//! consent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use conway::config::trust::{TrustStatus, TrustStore};

use crate::diag;
use crate::exit::ExitCode;

#[derive(Args, Debug)]
pub struct TrustArgs {
    #[command(subcommand)]
    pub action: TrustAction,
}

// `conway trust` with no action prints clap's own generated help and exits
// 2 -- clap's ordinary behavior for a required `#[command(subcommand)]`,
// the same as `conway plugin`. Doc comments below are kept short on
// purpose: clap renders them verbatim as `--help` text, and the longer
// rationale lives in this module's own top doc instead.
#[derive(Subcommand, Debug)]
pub enum TrustAction {
    /// Review this project's .conway/settings.json and record consent for
    /// exactly the bytes it holds right now.
    Settings {
        /// Trust this file instead of the one found by walking up from the
        /// current directory.
        #[arg(long, value_name = "PATH")]
        path: Option<PathBuf>,
    },
    /// Show every decision recorded in trust.json, and whether each file
    /// still holds the bytes that were trusted.
    List,
    /// Withdraw the decision recorded for a path.
    Revoke {
        /// The file whose recorded decision should be removed.
        path: PathBuf,
    },
}

/// Dispatches [`TrustArgs::action`].
///
/// `cwd` is the process working directory `main.rs` resolved once, AFTER
/// applying `--cwd` -- the same value `conway::config::LoadOptions::
/// default` would compute for the run being unblocked, so the walk this
/// performs reaches the same file the guard would have refused. `env` is
/// `main.rs`'s own resolved-once process environment, so `CONWAY_CONFIG_DIR`
/// steers which `trust.json` is written exactly as it does for every other
/// user-scoped file this binary touches.
///
/// Returns an [`ExitCode`] rather than `conway::Result<ExitCode>`: nothing
/// here produces a `conway::FacadeError`, and the caller is an early return
/// in `main` that precedes the `Conway` this binary's `dispatch` signature
/// is built around.
pub fn run(args: &TrustArgs, cwd: &Path, env: &HashMap<String, String>) -> ExitCode {
    match &args.action {
        TrustAction::Settings { path } => settings(cwd, env, path.as_deref()),
        TrustAction::List => list(env),
        TrustAction::Revoke { path } => revoke(cwd, env, path),
    }
}

/// Where this invocation's `trust.json` lives, for a message that says
/// which file was written. The placeholder is only reachable when no home
/// directory resolves at all, in which case every write below has already
/// failed with its own error.
fn store_path_label(env: &HashMap<String, String>) -> String {
    TrustStore::path(env)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<no resolvable trust.json>".to_string())
}

fn settings(cwd: &Path, env: &HashMap<String, String>, explicit: Option<&Path>) -> ExitCode {
    let target = match conway::config::trust::resolve_settings_trust_target(cwd, env, explicit) {
        Ok(target) => target,
        Err(e) => {
            diag::error(e.to_string());
            return ExitCode::Usage;
        }
    };

    // Read ONCE, print what was read, record consent for those same bytes
    // -- see this module's own "Review, then record" doc.
    let contents = match std::fs::read_to_string(&target) {
        Ok(contents) => contents,
        Err(e) => {
            diag::error(format!("cannot read {}: {e}", target.display()));
            return ExitCode::Usage;
        }
    };

    let store = TrustStore::load(env);
    let status = store.settings_status(&target, &contents);

    println!("{}", target.display());
    println!("----");
    for line in contents.lines() {
        println!("  {line}");
    }
    println!("----");

    if status == TrustStatus::Unchanged {
        println!(
            "already trusted -- these exact bytes are already a recorded decision in {}; \
             nothing written.",
            store_path_label(env)
        );
    } else {
        // `Changed` and `New` are both "not trusted right now" and both
        // write the same record; they differ only in what to SAY, because
        // "this file changed since you trusted it" is a materially
        // different thing to have just consented to than "trusting this for
        // the first time" (`conway::config::trust::TrustStatus`'s own doc).
        if status == TrustStatus::Changed {
            println!(
                "note: this file was trusted before and has been EDITED since -- what you are \
                 consenting to above is not what you consented to last time."
            );
        }
        if let Err(e) = TrustStore::trust_settings_bytes(env, &target, &contents) {
            diag::error(format!(
                "could not record a trust decision for {}: {e}",
                target.display()
            ));
            return ExitCode::AgentFailed;
        }
        println!(
            "trusted {} -- recorded in {}.",
            target.display(),
            store_path_label(env)
        );
    }
    println!(
        "This covers exactly the bytes above: any later edit re-arms the refusal. \
         `conway trust revoke {}` withdraws it.",
        target.display()
    );

    // Ground truth, not a claim: re-run the very gate that was refusing,
    // against the same cwd, and report what it now says. Only meaningful
    // for the walk-discovered target -- an explicit `--path` elsewhere on
    // disk says nothing about whether THIS directory is unblocked, so
    // claiming either way would be a lie.
    if explicit.is_none() {
        match conway::config::trust::guard_untrusted_project_settings(cwd, env) {
            Ok(()) => println!("conway will now start in {}.", cwd.display()),
            Err(e) => {
                // Not expected: the target came from the same walk this gate
                // performs, and was just recorded under that same spelling.
                // Reported rather than swallowed, because the alternative is
                // an operator who was told "trusted" and still cannot start.
                diag::error(format!(
                    "the decision was recorded, but the gate still refuses: {e}"
                ));
                return ExitCode::AgentFailed;
            }
        }
    }
    ExitCode::Completed
}

fn list(env: &HashMap<String, String>) -> ExitCode {
    let store = TrustStore::load(env);
    let entries = store.entries();
    let label = store_path_label(env);
    if entries.is_empty() {
        // Also the answer when trust.json is missing, corrupt, or
        // group-writable: `TrustStore::load` fails closed to an empty store
        // in all three cases (that function's own doc), and an empty store
        // genuinely trusts nothing, so "no decisions recorded" is the
        // honest report either way.
        println!("no trust decisions recorded ({label})");
        return ExitCode::Completed;
    }
    println!("{label}");
    for entry in entries {
        let state = match entry.status {
            Some(TrustStatus::Unchanged) => "ok",
            Some(TrustStatus::Changed) => "EDITED since trusted -- not trusted now",
            // Unreachable: a row exists only because a record does.
            Some(TrustStatus::New) => "no record",
            None => "unreadable or gone",
        };
        println!(
            "{:<11}  {}  [{state}]  (trusted {})",
            entry.kind.as_str(),
            entry.path.display(),
            entry.trusted_at,
        );
    }
    ExitCode::Completed
}

fn revoke(cwd: &Path, env: &HashMap<String, String>, path: &Path) -> ExitCode {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };

    // Match against the spellings ACTUALLY recorded rather than trusting the
    // operator to reproduce one: `TrustStore` keys on a path's `Display`
    // string, so `./.conway/settings.json` and `.conway/settings.json` are
    // different keys for the same file. A revoke that silently matched
    // nothing because of a spelling difference would leave a decision in
    // place that the operator believes they withdrew -- the worst possible
    // failure for this particular command.
    let store = TrustStore::load(env);
    let resolve = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let wanted = resolve(&absolute);
    let mut keys: Vec<PathBuf> = store
        .entries()
        .into_iter()
        .map(|entry| entry.path)
        .filter(|recorded| *recorded == absolute || resolve(recorded) == wanted)
        .collect();
    keys.sort();
    keys.dedup();

    if keys.is_empty() {
        diag::error(format!(
            "nothing is recorded for {} -- `conway trust list` shows every decision \
             this trust.json holds",
            absolute.display()
        ));
        return ExitCode::Usage;
    }

    for key in keys {
        // One call per key clears BOTH kinds for that path
        // (`TrustStore::revoke`'s own doc).
        match TrustStore::revoke(env, &key) {
            Ok(revoked) => {
                let kinds: Vec<&str> = revoked.kinds().into_iter().map(|k| k.as_str()).collect();
                println!("revoked {} ({})", key.display(), kinds.join(", "));
            }
            Err(e) => {
                diag::error(format!("could not revoke {}: {e}", key.display()));
                return ExitCode::AgentFailed;
            }
        }
    }
    ExitCode::Completed
}
