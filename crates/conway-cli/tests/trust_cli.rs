//! `conway trust project|list|revoke` against the real compiled binary --
//! board item `01M3TJQGJHFFPWE2YYN60WN1XB`, superseding board item
//! `01M2TTWSQ53CDWB9VRGSX05XNQ`'s "refuse outright" contract this file used
//! to pin.
//!
//! **RULING (2026-09-30): an untrusted project `.conway/` no longer stops
//! conway from starting.** Its files are IGNORED -- never applied, never
//! silently skipped -- and conway starts anyway, with a notice naming the
//! file and the command that applies it. This file's tests prove exactly
//! that contract against the real binary, not the old "refuses to start"
//! one.
//!
//! **Deliberately does NOT use `tests/common`'s own `command` helper.** That
//! helper passes `--config <fixture>`, and `--config` bypasses the ancestor
//! walk entirely (`conway::ConwayBuilder::from_config` never calls the
//! consent gate -- `conway::config::trust`'s own "Also out of scope:
//! `--config <path>`" doc). The entire defect under test only exists on the
//! no-`--config` branch, so every invocation here builds its own `Command`
//! and lets `ConwayBuilder::discover` run for real.
//!
//! Isolation is the same shape `common::command` uses and is load-bearing
//! twice over: `CONWAY_CONFIG_DIR` points at a throwaway directory, so the
//! `trust.json` these tests write is the fixture's own and never a
//! developer's real `~/.conway/trust.json`, and `CONWAY_LOCAL_PROBE_BASE_URL`
//! points at a dead port so `backend_usability::classify_fleet`'s startup
//! probe cannot reach a local model server that happens to be running.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

/// A project directory holding an untrusted `.conway/settings.json`, plus a
/// separate, empty config directory standing in for `~/.conway`.
struct Project {
    project: TempDir,
    config: TempDir,
}

/// The project settings document every test here starts from: valid, minimal,
/// and carrying one observable value a real config load WOULD reflect if
/// (and only if) this file's content were applied: an otherwise-unknown
/// role named `probe`. `conway routes explain probe` answers "unknown role"
/// when this layer was ignored and does not when it was applied -- see
/// `settings_is_applied_or_not` below for exactly how that is read.
const PROJECT_SETTINGS: &str = r#"{"roles":{"probe":{"chain":["ollama/does-not-matter"]}}}"#;

impl Project {
    fn new() -> Self {
        Self::with_settings(PROJECT_SETTINGS)
    }

    fn with_settings(contents: &str) -> Self {
        let project = tempfile::tempdir().expect("tempdir");
        let conf_dir = project.path().join(".conway");
        std::fs::create_dir_all(&conf_dir).expect("create .conway");
        std::fs::write(conf_dir.join("settings.json"), contents).expect("write settings.json");
        Project {
            project,
            config: tempfile::tempdir().expect("tempdir"),
        }
    }

    /// The project root as the SUBPROCESS will see it. `std::env::current_dir`
    /// resolves symlinks (`getcwd(3)`) and `tempfile`'s own path does not --
    /// on macOS `$TMPDIR` lives under `/var/folders`, itself a symlink into
    /// `/private/var/folders` -- so a test asserting on a path the binary
    /// printed has to compare against the resolved spelling, exactly as
    /// `common::session_dir` already documents for the session root.
    fn root(&self) -> PathBuf {
        self.project
            .path()
            .canonicalize()
            .unwrap_or_else(|_| self.project.path().to_path_buf())
    }

    fn settings_path(&self) -> PathBuf {
        self.root().join(".conway").join("settings.json")
    }

    fn permissions_path(&self) -> PathBuf {
        self.root().join(".conway").join("permissions.json")
    }

    fn trust_json(&self) -> PathBuf {
        self.config.path().join("trust.json")
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_in(self.project.path(), args)
    }

    fn run_in(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(assert_cmd::cargo::cargo_bin("conway"))
            .current_dir(cwd)
            .env("CONWAY_CONFIG_DIR", self.config.path())
            .env("CONWAY_LOCAL_PROBE_BASE_URL", "http://127.0.0.1:1/v1")
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .output()
            .expect("run conway binary")
    }

    /// `conway routes explain probe` -- `Ok(true)` when the `probe` role
    /// IS configured (an observable only `PROJECT_SETTINGS`' own content
    /// could have produced), `Ok(false)` when it reports "unknown role"
    /// (the role was never seen at all -- the settings layer was ignored).
    fn probe_role_is_configured(&self) -> bool {
        let out = self.run(&["routes", "explain", "probe"]);
        let err = stderr(&out);
        assert!(
            !err.contains("unknown role") || !out.status.success(),
            "an 'unknown role' message must come with a failing exit code: {err}"
        );
        !err.contains("unknown role")
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// **The headline property.** An untrusted project `settings.json` no
/// longer stops conway from starting: the real binary runs, its own
/// content is NOT applied (the `probe` role it declares is invisible to
/// `routes explain`), and the ignoring is announced on stderr, naming the
/// file and the remedy. **Break-the-guard**: a stub that silently applied
/// the untrusted file anyway would still pass every assertion except
/// `probe_role_is_configured()` -- that is the one line a silent-apply
/// regression trips.
#[test]
fn an_untrusted_project_settings_no_longer_blocks_startup_but_is_not_applied() {
    let project = Project::new();

    let out = project.run(&["plugin", "list"]);
    assert!(
        out.status.success(),
        "conway must start even with an untrusted project settings.json; stderr: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("project config ignored"),
        "the ignoring must be announced on stderr, got: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains(&project.settings_path().display().to_string()),
        "the notice must name the exact file: {}",
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("conway trust project"),
        "the notice must name the command that applies it: {}",
        stderr(&out)
    );

    assert!(
        !project.probe_role_is_configured(),
        "an untrusted project settings.json's content must NOT be applied"
    );
}

/// `conway trust project` is the one act that applies both of a project's
/// files on the next start -- settings.json's `probe` role becomes
/// visible, and (separately) a `permissions.json` placed alongside it is
/// also recorded as trusted in the same invocation.
#[test]
fn conway_trust_project_applies_both_files_on_the_next_start() {
    let project = Project::new();
    std::fs::write(project.permissions_path(), r#"{"allow":["read:*"]}"#)
        .expect("write permissions.json");

    assert!(
        !project.probe_role_is_configured(),
        "sanity: untrusted, not yet applied"
    );

    let trusted = project.run(&["trust", "project"]);
    assert!(
        trusted.status.success(),
        "`conway trust project` must succeed; stderr: {}",
        stderr(&trusted)
    );
    let printed = stdout(&trusted);
    assert!(
        printed.contains(&project.settings_path().display().to_string()),
        "must review settings.json: {printed}"
    );
    assert!(
        printed.contains(&project.permissions_path().display().to_string()),
        "must ALSO review permissions.json, in the same invocation: {printed}"
    );

    assert!(
        project.probe_role_is_configured(),
        "after one `conway trust project`, the settings.json layer must apply"
    );

    let recorded = std::fs::read_to_string(project.trust_json()).expect("trust.json was written");
    assert!(
        recorded.contains("settings_files") && recorded.contains("permission_files"),
        "both kinds must be recorded by one invocation: {recorded}"
    );
}

/// `conway trust settings` is kept as an alias of `project`, for a script
/// written before this item.
#[test]
fn conway_trust_settings_is_an_alias_of_project() {
    let project = Project::new();
    let out = project.run(&["trust", "settings"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(project.probe_role_is_configured());
}

/// **The guard is not weakened.** Consent is per-path AND per-bytes: an
/// edit after trusting re-arms the ignore-notice, which is the whole point
/// of digest-scoped trust (`conway::config::trust`'s own "trust subject"
/// doc) and the case that matters most in practice -- a teammate's `git
/// push` changing a file you already approved.
#[test]
fn editing_a_trusted_project_settings_re_arms_the_ignore_notice() {
    let project = Project::new();
    assert!(project.run(&["trust", "project"]).status.success());
    assert!(project.probe_role_is_configured());

    std::fs::write(
        project.settings_path(),
        r#"{"roles":{"probe":{"chain":["ollama/different"]},"other":{"chain":["ollama/x"]}}}"#,
    )
    .expect("rewrite the project settings.json");

    let after = project.run(&["plugin", "list"]);
    assert!(
        after.status.success(),
        "an edit since consent must still start conway, just ignore the new bytes; stdout: {}",
        stdout(&after)
    );
    assert!(
        stderr(&after).contains("project config ignored") && stderr(&after).contains("EDITED"),
        "stderr must disclose an edit since trusted, not the first-time wording: {}",
        stderr(&after)
    );
    assert!(
        !project.probe_role_is_configured(),
        "the EDITED file's content must not apply until re-trusted"
    );

    // And re-consenting says plainly that this is not what was approved
    // before, rather than quietly re-recording.
    let re_trusted = project.run(&["trust", "project"]);
    assert!(
        re_trusted.status.success(),
        "stderr: {}",
        stderr(&re_trusted)
    );
    assert!(
        stdout(&re_trusted).contains("EDITED since"),
        "re-consent must disclose that the bytes changed: {}",
        stdout(&re_trusted)
    );
    assert!(project.probe_role_is_configured());
}

/// `conway trust list` reports what is trusted, and `conway trust revoke`
/// withdraws it -- after which the ignore-notice fires again (never a hard
/// refusal).
#[test]
fn list_shows_the_decision_and_revoke_withdraws_it() {
    let project = Project::new();
    assert!(project.run(&["trust", "project"]).status.success());

    let listed = project.run(&["trust", "list"]);
    assert!(listed.status.success(), "stderr: {}", stderr(&listed));
    let rows = stdout(&listed);
    assert!(
        rows.contains(&project.settings_path().display().to_string()),
        "the listing must name the trusted file: {rows}"
    );
    assert!(
        rows.contains("settings"),
        "and which kind it was trusted as: {rows}"
    );

    let path = project.settings_path().display().to_string();
    let revoked = project.run(&["trust", "revoke", &path]);
    assert!(revoked.status.success(), "stderr: {}", stderr(&revoked));

    assert!(
        !project.probe_role_is_configured(),
        "after revocation the file must be ignored again"
    );
    let after = project.run(&["plugin", "list"]);
    assert!(
        after.status.success(),
        "revocation must NOT stop conway from starting; stdout: {}",
        stdout(&after)
    );
    assert!(
        stderr(&after).contains("project config ignored"),
        "stderr: {}",
        stderr(&after)
    );
}

/// **There is no blanket flag.** Nothing on the root command, and nothing
/// on `trust project` itself beyond naming which file.
#[test]
fn there_is_no_blanket_trust_flag() {
    let project = Project::new();

    for args in [
        vec!["--help"],
        vec!["trust", "--help"],
        vec!["trust", "project", "--help"],
    ] {
        let help = stdout(&project.run_in(project.config.path(), &args));
        for forbidden in [
            "--trust-project-settings",
            "--trust-all",
            "--no-trust",
            "--yes",
        ] {
            assert!(
                !help.contains(forbidden),
                "`conway {}` must not offer {forbidden}: {help}",
                args.join(" ")
            );
        }
    }

    // And conway never refuses to start regardless -- there is nothing left
    // to "bypass."
    let guessed = project.run(&["--trust-project-settings", "plugin", "list"]);
    assert!(
        !guessed.status.success(),
        "an invented bypass flag must not parse; stdout: {}",
        stdout(&guessed)
    );
}

/// Nothing to consent to is a usage error naming what it looked for, not a
/// silent success that leaves an operator believing something was trusted.
#[test]
fn trust_project_with_no_project_layer_is_a_usage_error() {
    let project = Project::new();
    // Run from the throwaway config dir, which has no `.conway/settings.json`
    // of its own and no ancestor that does, and no `.conway/permissions.json`
    // either.
    let out = project.run_in(project.config.path(), &["trust", "project"]);

    assert_eq!(
        out.status.code(),
        Some(2),
        "expected a usage error; stdout: {} stderr: {}",
        stdout(&out),
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("nothing to trust"),
        "stderr: {}",
        stderr(&out)
    );
    assert!(
        !project.trust_json().exists(),
        "a failed resolve must not write a trust.json"
    );
}

/// Revoking a path that was never trusted is a usage error, not a success
/// message -- a typo'd path must never read as a completed revocation.
#[test]
fn revoking_an_unrecorded_path_is_a_usage_error() {
    let project = Project::new();
    let path = project.settings_path().display().to_string();

    let out = project.run(&["trust", "revoke", &path]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "stdout: {} stderr: {}",
        stdout(&out),
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("nothing is recorded for"),
        "stderr: {}",
        stderr(&out)
    );
}

/// `--path` names one file explicitly, and a relative spelling still lands
/// on the key the ignore-check looks up -- the silent-failure class this
/// resolver exists to close
/// (`conway::config::trust::resolve_settings_trust_target`'s own doc).
#[test]
fn an_explicit_relative_path_records_the_key_the_check_gates() {
    let project = Project::new();

    let out = project.run(&["trust", "project", "--path", ".conway/settings.json"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));

    assert!(
        project.probe_role_is_configured(),
        "a relative --path must clear the same check"
    );
}
