//! `conway trust settings|list|revoke` against the real compiled binary --
//! board item `01M2TTWSQ53CDWB9VRGSX05XNQ`.
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
/// and carrying one observable value. Byte-identical in shape to the fixture
/// `conway::config::trust`'s own gate unit tests use, so a failure here is
/// about the CLI surface and never about a malformed config.
const PROJECT_SETTINGS: &str = r#"{"limits":{"max_steps":7}}"#;

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
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// **P-15: fails against HEAD.** An operator with an untrusted project
/// `settings.json` reaches a working conway using only the shell they are
/// already in -- no Rust, no deleting the file, no prior knowledge of
/// `trust.json`. Against HEAD step 2 has nothing to call: `conway trust` is
/// not a built-in subcommand, so it falls through clap's
/// `external_subcommand` into `dispatch` -- which is only reached AFTER
/// `build_conway`, i.e. after the very refusal being cleared. There is no
/// ordering of HEAD's commands that gets past step 1.
#[test]
fn an_untrusted_project_settings_can_be_trusted_from_a_blocked_shell() {
    let project = Project::new();

    // 1. Blocked: conway refuses to start at all.
    let blocked = project.run(&["plugin", "list"]);
    assert!(
        !blocked.status.success(),
        "an untrusted project settings.json must still refuse; stdout: {}",
        stdout(&blocked)
    );
    assert!(
        stderr(&blocked).contains("untrusted project settings.json"),
        "expected the consent refusal, got stderr: {}",
        stderr(&blocked)
    );

    // 2. The remedy, typed at that same blocked shell.
    let trusted = project.run(&["trust", "settings"]);
    assert!(
        trusted.status.success(),
        "`conway trust settings` must work where conway itself refuses to start; stderr: {}",
        stderr(&trusted)
    );

    // 3. Unblocked, with the project config actually in play.
    let after = project.run(&["plugin", "list"]);
    assert!(
        after.status.success(),
        "after consent conway must start; stderr: {}",
        stderr(&after)
    );
    assert!(
        stdout(&after).lines().any(|l| l.starts_with('[')),
        "the real plugin listing must come back, got stdout: {}",
        stdout(&after)
    );

    // The decision landed in the fixture's own trust.json, under the exact
    // path the guard gates -- never a developer's real ~/.conway.
    let recorded = std::fs::read_to_string(project.trust_json()).expect("trust.json was written");
    assert!(
        recorded.contains("settings_files"),
        "the settings kind must be what was recorded: {recorded}"
    );
    assert!(
        recorded.contains(&project.settings_path().display().to_string()),
        "trust.json must key on the gated path: {recorded}"
    );
}

/// Acceptance 2: the refusal names a remedy reachable from a shell where
/// conway refuses to start, and no longer names the two that were not.
/// **Fails against HEAD**, which named the TUI's `/trust settings` (a
/// command that does not exist -- the TUI parses `/trust permissions`) and
/// `TrustStore::trust_settings`, a Rust API.
#[test]
fn the_refusal_names_a_remedy_an_operator_can_actually_run() {
    let project = Project::new();
    let message = stderr(&project.run(&["plugin", "list"]));

    assert!(
        message.contains("conway trust settings --path"),
        "the refusal must name the headless consent command: {message}"
    );
    assert!(
        message.contains(&format!("rm {}", project.settings_path().display())),
        "the refusal must say the file can be removed to proceed: {message}"
    );
    // BREAK-THE-GUARD: the unreachable advice is gone, not merely joined.
    assert!(
        !message.contains("TrustStore::trust_settings"),
        "a Rust API is not an operator remedy: {message}"
    );
    assert!(
        !message.contains("the TUI's"),
        "the TUI is what just refused to start: {message}"
    );
}

/// `conway trust settings` is review-then-record in one output: the bytes
/// being consented to are printed before anything is written.
#[test]
fn trust_settings_prints_the_file_it_is_about_to_trust() {
    let project = Project::new();
    let out = project.run(&["trust", "settings"]);

    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let printed = stdout(&out);
    assert!(
        printed.contains("max_steps"),
        "the operator must see the contents being consented to: {printed}"
    );
    assert!(
        printed.contains(&project.settings_path().display().to_string()),
        "and the exact file they name: {printed}"
    );
    assert!(
        printed.contains("conway trust revoke"),
        "and how to undo it: {printed}"
    );
}

/// `conway trust list` reports what is trusted, and `conway trust revoke`
/// withdraws it -- after which the guard refuses again. The full loop, all
/// of it headless.
#[test]
fn list_shows_the_decision_and_revoke_withdraws_it() {
    let project = Project::new();
    assert!(project.run(&["trust", "settings"]).status.success());

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

    let after = project.run(&["plugin", "list"]);
    assert!(
        !after.status.success(),
        "after revocation the guard must refuse again; stdout: {}",
        stdout(&after)
    );
    assert!(
        stdout(&project.run(&["trust", "list"])).contains("no trust decisions recorded"),
        "the revoked row must be gone from the listing"
    );
}

/// **The guard is not weakened.** Consent is per-path AND per-bytes: an
/// edit after trusting re-arms the refusal, which is the whole point of
/// digest-scoped trust (`conway::config::trust`'s own "trust subject" doc)
/// and the case that matters most in practice -- a teammate's `git push`
/// changing a file you already approved.
#[test]
fn editing_a_trusted_project_settings_re_arms_the_refusal() {
    let project = Project::new();
    assert!(project.run(&["trust", "settings"]).status.success());
    assert!(project.run(&["plugin", "list"]).status.success());

    std::fs::write(project.settings_path(), r#"{"limits":{"max_steps":9999}}"#)
        .expect("rewrite the project settings.json");

    let after = project.run(&["plugin", "list"]);
    assert!(
        !after.status.success(),
        "an edit since consent must refuse again; stdout: {}",
        stdout(&after)
    );
    assert!(
        stderr(&after).contains("untrusted project settings.json"),
        "stderr: {}",
        stderr(&after)
    );

    // And re-consenting says plainly that this is not what was approved
    // before, rather than quietly re-recording.
    let re_trusted = project.run(&["trust", "settings"]);
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
}

/// **The guard is not weakened, part two.** There is no blanket flag a
/// script could set to skip consent -- nothing on the root command, and
/// nothing on `trust settings` itself beyond naming which file.
#[test]
fn there_is_no_blanket_trust_flag() {
    let project = Project::new();

    for args in [
        vec!["--help"],
        vec!["trust", "--help"],
        vec!["trust", "settings", "--help"],
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

    // And the refusal itself cannot be argued away: passing the flag a
    // script might guess is a parse error, not a bypass.
    let guessed = project.run(&["--trust-project-settings", "plugin", "list"]);
    assert!(
        !guessed.status.success(),
        "an invented bypass flag must not work; stdout: {}",
        stdout(&guessed)
    );
}

/// Nothing to consent to is a usage error naming what it looked for, not a
/// silent success that leaves an operator believing something was trusted.
#[test]
fn trust_settings_with_no_project_layer_is_a_usage_error() {
    let project = Project::new();
    // Run from the throwaway config dir, which has no `.conway/settings.json`
    // of its own and no ancestor that does.
    let out = project.run_in(project.config.path(), &["trust", "settings"]);

    assert_eq!(
        out.status.code(),
        Some(2),
        "expected a usage error; stdout: {} stderr: {}",
        stdout(&out),
        stderr(&out)
    );
    assert!(
        stderr(&out).contains("no project settings.json is reachable"),
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
/// on the key the guard looks up -- the silent-failure class this resolver
/// exists to close (`conway::config::trust::resolve_settings_trust_target`'s
/// own doc).
#[test]
fn an_explicit_relative_path_records_the_key_the_guard_gates() {
    let project = Project::new();

    let out = project.run(&["trust", "settings", "--path", ".conway/settings.json"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));

    let after = project.run(&["plugin", "list"]);
    assert!(
        after.status.success(),
        "a relative --path must clear the same gate; stderr: {}",
        stderr(&after)
    );
}
