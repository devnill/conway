//! `conway trust project|list|revoke` (board item
//! `01M2TTWSQ53CDWB9VRGSX05XNQ`, the review/record shape updated to cover
//! BOTH project files by board item `01M3TJQGJHFFPWE2YYN60WN1XB`): the
//! headless half of consent for a project's `.conway/settings.json` AND
//! `.conway/permissions.json` -- the same `conway::config::trust::
//! TrustStore` writer the rest of the tree already uses, and the same
//! `<user settings dir>/trust.json` file, reachable from a script or a
//! terminal with no TUI in sight. Deliberately the shape
//! `crate::commands::plugin`'s own module doc already sets for `conway
//! plugin list|install|remove`.
//!
//! # One trust act covers both files (board item `01M3TJQGJHFFPWE2YYN60WN1XB`)
//!
//! `conway trust project` is ONE operator act that reviews and records
//! consent for whichever of the project's `settings.json`/`permissions.json`
//! actually exist, in one output -- not two commands an operator has to
//! remember to run separately (the exact gap the "DOGFOOD 3" evidence this
//! item's own spec records: `conway trust settings` left `permissions.json`
//! unaffected, so `/trust permissions` was STILL needed afterward). Each
//! file still gets its own, independent `TrustedRecord` underneath
//! (`conway::config::trust`'s own "Global vs project"/"A second kind" docs:
//! two different files, two different authorities, two different digests),
//! this command just performs both writes from one invocation. `settings`
//! is kept as an alias for the prior spelling -- scripts that already call
//! `conway trust settings` keep working, and gain the permissions.json half
//! for free.
//!
//! # `01M3TJQGJHFFPWE2YYN60WN1XB`'s ruling: ignore, not refuse
//!
//! `conway::config::trust::guard_untrusted_project_settings` USED to refuse
//! to start conway outright when an untrusted project `settings.json` was
//! reachable from the cwd (board item `01M2TTWSQ53CDWB9VRGSX05XNQ`'s own
//! "defect this closes" section, preserved below for history). The
//! operator's own ruling on THIS item concluded that remedy was worse than
//! the harm: friction severe enough that an untrusted `.conway/` is now
//! IGNORED instead -- its files are never applied, conway starts anyway,
//! and a persistent, impossible-to-miss notice names them and this exact
//! command. See `conway::config::trust::UntrustedProjectSettings`'s own doc
//! for the ruling's full wording.
//!
//! ## (History) The defect `01M2TTWSQ53CDWB9VRGSX05XNQ` closed
//!
//! Before that item, the refusal named two remedies: the TUI's `/trust
//! settings` command, and the `TrustStore::trust_settings` Rust API. The TUI
//! is the thing that had just refused to start, so it could not be reached;
//! "write an embedder" is not an operator remedy; and `/trust settings` did
//! not actually exist (`crate::tui::commands` parses `/trust permissions`,
//! and nothing else). This module is what made the remedy reachable; it
//! stays reachable (and necessary, to apply a project's own config at all)
//! now that the condition it answers is "ignored" rather than "refused."
//!
//! # Why this runs before `build_conway`
//!
//! `main.rs` dispatches `Command::Trust` at its own entry point, ahead of
//! the single `build_conway` call every other subcommand goes through --
//! see that call site's own comment for why this still matters even though
//! an untrusted project config no longer fails `build_conway` on its own
//! (a SYNTAX error in either file still does, and `conway trust` must stay
//! reachable then too). Nothing here needs a `Conway`: the trust store is a
//! standalone file keyed on paths, resolved from the process cwd and
//! environment alone.
//!
//! # What this deliberately does NOT do
//!
//! - **No auto-trust, and no blanket flag.** There is no
//!   `--trust-project-settings` a script could set once and forget. Every
//!   invocation names the files found by the same walk the ignore-check
//!   itself uses (or an explicit `--path` for the settings half) and
//!   records a decision about each file's current BYTES. An edit afterwards
//!   re-arms the notice, by design (`conway::config::trust`'s own "trust
//!   subject" doc).
//! - **No degraded TUI.** Consent is never collected by starting the thing
//!   whose config is in question.
//!
//! # Review, then record -- in that order, in one output
//!
//! `project` prints the full contents of each file it finds before it
//! writes anything, and records consent for THOSE bytes (via
//! `conway::config::trust::TrustStore::trust_settings_bytes`/[`TrustStore::
//! trust`], so the bytes shown and the bytes recorded cannot drift apart if
//! a file is rewritten in between). Recording trust does not APPLY the
//! config -- the next `conway` invocation does -- so an operator who reads
//! the printed contents and dislikes them has `conway trust revoke <path>`
//! before anything the file says has ever been acted on. That is the same
//! "restart to apply" separation `conway plugin install` already relies on,
//! and it is why this command needs no interactive confirmation prompt of
//! its own: typing the command IS the explicit act of consent.

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
    /// Review this project's .conway/settings.json AND .conway/permissions.json
    /// (whichever exist) and record consent for exactly the bytes each
    /// holds right now, in one act.
    #[command(alias = "settings")]
    Project {
        /// Trust this file instead of the settings.json found by walking up
        /// from the current directory. Only changes which SETTINGS file is
        /// reviewed -- the project's permissions.json (if any) is still
        /// found by the ordinary walk.
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
/// performs reaches the same files the ignore-check itself would. `env` is
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
        TrustAction::Project { path } => project(cwd, env, path.as_deref()),
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

/// `conway trust project` (board item `01M3TJQGJHFFPWE2YYN60WN1XB`): the
/// one operator act that reviews and records consent for BOTH of a
/// project's `settings.json` and `permissions.json`, whichever exist on
/// disk -- see this module's own "One trust act covers both files" doc.
fn project(cwd: &Path, env: &HashMap<String, String>, explicit: Option<&Path>) -> ExitCode {
    let settings_target = conway::config::trust::resolve_settings_trust_target(cwd, env, explicit);
    // The project-scope candidate is always `permission_file_paths`'s FIRST
    // entry (that function's own doc) -- a candidate PATH, not necessarily
    // an existing file, so `is_file()` is what tells us whether there is
    // anything here to review at all.
    let permissions_target = conway::config::discovery::permission_file_paths(cwd, env)
        .into_iter()
        .next()
        .filter(|p| p.is_file());

    let settings_target = settings_target.ok();
    if settings_target.is_none() && permissions_target.is_none() {
        diag::error(format!(
            "no project .conway/settings.json or permissions.json is reachable from {} -- \
             nothing to trust here (pass --path <file> to name a settings.json explicitly)",
            cwd.display()
        ));
        return ExitCode::Usage;
    }

    let mut trusted_any = false;
    if let Some(target) = &settings_target {
        match trust_one(env, target, FileKind::Settings) {
            Ok(()) => trusted_any = true,
            Err(code) => return code,
        }
    }
    if let Some(target) = &permissions_target {
        match trust_one(env, target, FileKind::Permissions) {
            Ok(()) => trusted_any = true,
            Err(code) => return code,
        }
    }
    debug_assert!(trusted_any, "at least one target was reviewed above");

    println!(
        "This covers exactly the bytes shown above: any later edit re-arms the notice. \
         `conway trust revoke <path>` withdraws a decision."
    );
    ExitCode::Completed
}

/// Which `TrustStore` kind a [`trust_one`] call is reviewing -- a tiny enum
/// rather than a bare `bool` so the call sites in [`project`] stay
/// self-documenting, and so it reads the right half of [`TrustStore`]
/// (`settings_status`/`trust_settings_bytes` vs. `status`/`trust_bytes`)
/// for the file actually being shown.
enum FileKind {
    Settings,
    Permissions,
}

/// Reviews (prints) and records consent for ONE file -- the shared body
/// `project` calls once per candidate that actually exists. Read ONCE,
/// printed, then trusted for those same bytes (`conway::config::trust`'s
/// own "Review, then record" doc) -- never a second read between showing
/// the operator something and recording consent for it.
fn trust_one(env: &HashMap<String, String>, target: &Path, kind: FileKind) -> Result<(), ExitCode> {
    let contents = std::fs::read_to_string(target).map_err(|e| {
        diag::error(format!("cannot read {}: {e}", target.display()));
        ExitCode::Usage
    })?;

    let store = TrustStore::load(env);
    let status = match kind {
        FileKind::Settings => store.settings_status(target, &contents),
        FileKind::Permissions => store.status(target, &contents),
    };

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
        return Ok(());
    }
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
    let write_result = match kind {
        FileKind::Settings => TrustStore::trust_settings_bytes(env, target, &contents),
        FileKind::Permissions => TrustStore::trust_bytes(env, target, &contents),
    };
    if let Err(e) = write_result {
        diag::error(format!(
            "could not record a trust decision for {}: {e}",
            target.display()
        ));
        return Err(ExitCode::AgentFailed);
    }
    println!(
        "trusted {} -- recorded in {}.",
        target.display(),
        store_path_label(env)
    );
    Ok(())
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
