//! `conway doctor` against the real compiled binary (board item
//! `01M1YVXNPTBNH18ZEWFM18F00T`).
//!
//! **Isolation matches `trust_cli.rs`'s own shape, deliberately.**
//! `CONWAY_CONFIG_DIR` points at a throwaway directory standing in for
//! `~/.conway`, and every project lives in its own `TempDir` -- a bare
//! `HOME` override does not isolate conway's own config discovery (it
//! keeps walking past it looking for `~/.conway`); `CONWAY_CONFIG_DIR` is
//! the seam that actually does. `CONWAY_LOCAL_PROBE_BASE_URL` is pointed at
//! a dead port too, so the unrelated local-server auto-detect probe
//! `build_conway`'s own guided-setup trigger would otherwise perform can
//! never flake against whatever happens to be listening on a developer's
//! or CI runner's real `127.0.0.1:11434` -- doctor's own dispatch never
//! reaches that trigger (it returns before `build_conway` runs at all), so
//! this is defensive, not load-bearing, exactly like `trust_cli.rs`'s
//! identical env var.
//!
//! **Deliberately does NOT use `tests/common`'s own `command`/`Fixture`
//! helpers** -- those template a config that already names a working mock
//! backend, which is the opposite of what most of these tests want (a
//! fixture with a SPECIFIC, deliberate problem). Builds its own minimal
//! `Project`, on the identical footing `trust_cli.rs` already established
//! for the same reason.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

/// A project directory (for its `.conway/agents`/`.conway/models.json`),
/// plus a separate config directory standing in for `~/.conway` that
/// actually carries `settings.json` -- see [`Project::with_settings`]'s own
/// doc for why the fixture document lives there, not under the project.
struct Project {
    project: TempDir,
    config: TempDir,
}

impl Project {
    fn with_settings(contents: &str) -> Self {
        let project = tempfile::tempdir().expect("tempdir");
        let conf_dir = project.path().join(".conway");
        std::fs::create_dir_all(&conf_dir).expect("create .conway");
        let config = tempfile::tempdir().expect("tempdir");
        // `settings.json` is written to the directory standing in for
        // `~/.conway` (`CONWAY_CONFIG_DIR`), NOT `<project>/.conway/` -- a
        // walk-discovered PROJECT settings.json is gated behind an explicit
        // `conway trust project` consent (`config::merge::
        // merged_document_impl`'s own `ProjectLayerTrust::Gated`, read back
        // here as `WarningCode::UntrustedProjectConfigIgnored`), while the
        // operator's own USER layer is never gated (that same function's
        // unconditional first branch). Every fixture in this file wants its
        // settings actually APPLIED so the checks below can observe them --
        // `trust_cli.rs`'s own tests are the ones that exercise the gate
        // itself. Writing here instead of `conf_dir` previously made every
        // settings-dependent check below silently see the baked-in empty
        // default document (caught by the check never appearing in the
        // report at all, not by a wrong value -- see this item's
        // verification report for the full root-cause writeup).
        std::fs::write(config.path().join("settings.json"), contents).expect("write settings.json");
        // `config::merge::validate`'s per-chain-entry context-window check
        // (`WarningCode::ChainEntryContextWindowUnknown`) only runs AT ALL
        // when `models.json` already names at least one model -- an empty
        // metadata table short-circuits the whole loop, silently, before it
        // ever looks at a chain entry (that check's own module comment:
        // "every chain entry of every role" is gated behind `if !metadata.
        // models.is_empty()`). Naming one UNRELATED model here, in every
        // fixture, is what lets `doctor_reports_unknown_chain_window_as_
        // warn_with_a_fix_line` below observe the warning for a chain entry
        // this file never also names -- harmless for every other test here,
        // since a role with an empty chain (the baked-in floor) has no
        // chain entry for the per-entry loop to ever reach regardless.
        std::fs::write(
            conf_dir.join("models.json"),
            serde_json::json!({
                "models": {
                    "unrelated/filler-model": {
                        "max_context_tokens": 128_000,
                        "tool_calling": "streaming",
                        "reasoning": false,
                        "reliability_tier": "verified"
                    }
                }
            })
            .to_string(),
        )
        .expect("write models.json");
        Project { project, config }
    }

    /// The project root as the SUBPROCESS will see it -- canonicalized, for
    /// the identical reason `trust_cli.rs::Project::root` is (`tempfile`'s
    /// own path does not resolve the `/var` -> `/private/var` symlink
    /// `std::env::current_dir()` inside the subprocess already has).
    fn root(&self) -> PathBuf {
        self.project
            .path()
            .canonicalize()
            .unwrap_or_else(|_| self.project.path().to_path_buf())
    }

    fn agents_dir(&self) -> PathBuf {
        self.root().join(".conway").join("agents")
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(assert_cmd::cargo::cargo_bin("conway"))
            .current_dir(self.root())
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

/// Nothing ever listens on port 1 (a privileged, reserved port no server
/// binds), so a loopback connect to it answers `ECONNREFUSED` immediately
/// -- a deterministic `Unusable::EndpointRefused`, never the timeout-shaped
/// `Undetermined::EndpointUnreachable` a merely-unreachable host would
/// produce. The same trick `tests/trust_cli.rs`/`tests/common/mod.rs`
/// already use for an identical "the mock refuses" need.
const DEAD_PORT: &str = "http://127.0.0.1:1/v1";

/// A minimal, otherwise-valid settings document naming one backend whose
/// `base_url` is [`DEAD_PORT`] and which also carries one key
/// (`base_ur1`) this schema's `BackendEntry` never reads -- one fixture,
/// three deliberately observable problems: a definite reachability
/// failure, an inert/unrecognized key, and (since `some-model` is named by
/// no `models.json` entry anywhere) an unknown chain-entry context window.
fn flaky_backend_settings() -> String {
    serde_json::json!({
        "default_role": "default",
        "roles": { "default": { "chain": ["flaky/some-model"] } },
        "backends": {
            "flaky": {
                "kind": "openai-compat",
                "base_url": DEAD_PORT,
                "base_ur1": "a typo this schema never reads"
            }
        }
    })
    .to_string()
}

/// **`config.loads`, the `fail` half.** A typo'd key (`limits.max_step`,
/// the exact reproduction `crates/conway/tests/fixtures/config/
/// unknown_key.json` already pins for `deny_unknown_fields`) makes the
/// whole config fail to load -- `conway doctor` reports that as its own
/// named `fail` check (never refusing to run outright) and the real serde
/// error, naming the typo'd key, is the check's summary.
#[test]
fn doctor_reports_config_load_failure_as_fail_and_exits_nonzero() {
    let project = Project::with_settings(
        r#"{"default_role":"coder","roles":{"coder":{"chain":["anthropic/claude-sonnet-4-6"]}},"backends":{"anthropic":{"kind":"anthropic"}},"limits":{"max_step":10}}"#,
    );
    let out = project.run(&["doctor"]);
    assert!(
        !out.status.success(),
        "a config-load failure must exit nonzero: {}",
        stdout(&out)
    );
    let text = stdout(&out);
    assert!(text.contains("[FAIL] config.loads"), "{text}");
    assert!(
        text.contains("max_step"),
        "the real serde error should name the typo'd key: {text}"
    );
}

/// **`backends.<id>.reachable`, the `fail` half.** A dead port is a
/// definite, real `Unusable::EndpointRefused`, reported `fail` with a fix
/// line naming the exact remedy.
#[test]
fn doctor_reports_unreachable_backend_as_fail_with_a_fix_line() {
    let project = Project::with_settings(&flaky_backend_settings());
    let out = project.run(&["doctor"]);
    assert!(!out.status.success(), "{}", stdout(&out));
    let text = stdout(&out);
    assert!(text.contains("[FAIL] backends.flaky.reachable"), "{text}");
    assert!(
        text.contains("fix: start the server listening at"),
        "{text}"
    );
}

/// **`backends.<id>.inert_keys`, the `warn` half.** `base_ur1` lands in
/// `BackendEntry::extra` silently (no `deny_unknown_fields` on that
/// struct); this check is what surfaces it, naming the exact key.
#[test]
fn doctor_reports_inert_backend_key_as_warn_with_a_fix_line() {
    let project = Project::with_settings(&flaky_backend_settings());
    let out = project.run(&["doctor"]);
    let text = stdout(&out);
    assert!(text.contains("[WARN] backends.flaky.inert_keys"), "{text}");
    assert!(text.contains("base_ur1"), "{text}");
    assert!(text.contains("fix:"), "{text}");
}

/// **`routing.chain_window_unknown`, the `warn` half.** `flaky/some-model`
/// is named by no `models.json` entry anywhere, so `config::merge::
/// validate`'s own `WarningCode::ChainEntryContextWindowUnknown` fires at
/// config-load time -- read back here, never re-derived.
#[test]
fn doctor_reports_unknown_chain_window_as_warn_with_a_fix_line() {
    let project = Project::with_settings(&flaky_backend_settings());
    let out = project.run(&["doctor"]);
    let text = stdout(&out);
    assert!(
        text.contains("[WARN] routing.chain_window_unknown"),
        "{text}"
    );
    assert!(
        text.contains("fix: add this backend/model pair to models.json"),
        "{text}"
    );
}

/// **`agents.parse`, the `fail` half.** A `.conway/agents/*.md` file with
/// no `name:` key fails `conway::agents::load_agent_defs`'s real loader
/// with `"missing required field 'name'"`; this check names the exact file
/// too (pulled from the error's own structured `path` field, since
/// `FacadeError::AgentDef`'s `Display` renders `message` alone).
#[test]
fn doctor_reports_malformed_agent_definition_as_fail_naming_the_file() {
    let project = Project::with_settings("{}");
    std::fs::create_dir_all(project.agents_dir()).expect("create agents dir");
    let broken = project.agents_dir().join("broken.md");
    std::fs::write(
        &broken,
        "---\ndescription: no name here\n---\nDo the thing.\n",
    )
    .expect("write broken agent def");

    let out = project.run(&["doctor"]);
    assert!(!out.status.success(), "{}", stdout(&out));
    let text = stdout(&out);
    assert!(text.contains("[FAIL] agents.parse"), "{text}");
    assert!(
        text.contains(&broken.display().to_string()),
        "the file must be named in the report: {text}"
    );
    assert!(text.contains("missing required field 'name'"), "{text}");
}

/// **The clean report, `exit 0`.** A completely empty, otherwise-valid
/// settings document (`{}`) produces only `pass`/`warn` checks -- no
/// backend, no agents directory, and nothing malformed to find.
#[test]
fn doctor_exit_code_is_zero_when_every_check_is_pass_or_warn() {
    let project = Project::with_settings("{}");
    let out = project.run(&["doctor"]);
    assert!(
        out.status.success(),
        "an empty, otherwise-valid config must report pass/warn only: stdout={} stderr={}",
        stdout(&out),
        stderr(&out)
    );
}

/// **`backends.<id>.reachable`, the `pass` half, over a REAL loopback
/// listener.** `classify_entry`'s own probe is a bare TCP connect (that
/// module's own doc), so a `wiremock::MockServer` -- already a dev-only
/// dependency of this crate -- makes an honest, real "something answers
/// here" target with no mock expectations registered at all. The
/// injectable seam this whole check relies on is exactly `backends.<id>.
/// base_url`, an ordinary config value -- no network beyond what the
/// fixture itself stands up.
#[tokio::test]
async fn doctor_reports_a_live_backend_as_pass_via_a_real_loopback_listener() {
    let server = wiremock::MockServer::start().await;
    let settings = serde_json::json!({
        "default_role": "default",
        "roles": { "default": { "chain": ["live/some-model"] } },
        "backends": { "live": { "kind": "openai-compat", "base_url": server.uri() } }
    })
    .to_string();
    let project = Project::with_settings(&settings);

    let out = project.run(&["doctor"]);
    let text = stdout(&out);
    assert!(text.contains("[PASS] backends.live.reachable"), "{text}");
}

/// **The `--json` shape** this module's own doc (`docs/scripting.md`'s
/// "`conway doctor`" section) documents: `checks` is an array of
/// `{id,status,summary,fix}` with `status` one of `"pass"`/`"warn"`/
/// `"fail"` and `fix` `null` iff `status == "pass"`, and `summary` is a
/// top-level `{pass,warn,fail}` tally -- read back as `serde_json::Value`
/// rather than matched against the printed string, so field order can
/// never break this test. The fixture's one dead-port backend also pins
/// the exit-code contract: `--json`'s exit code still agrees with whether
/// any check is `fail`.
#[test]
fn doctor_json_report_has_the_documented_top_level_shape() {
    let project = Project::with_settings(&flaky_backend_settings());
    let out = project.run(&["doctor", "--json"]);
    let text = stdout(&out);
    let report: serde_json::Value = serde_json::from_str(text.trim())
        .unwrap_or_else(|e| panic!("doctor --json must print one JSON object, got {text:?}: {e}"));

    let checks = report["checks"].as_array().expect("checks array");
    assert!(!checks.is_empty());
    let mut saw_fail = false;
    for check in checks {
        let status = check["status"].as_str().expect("status string");
        assert!(
            ["pass", "warn", "fail"].contains(&status),
            "unexpected status {status}"
        );
        assert!(check["id"].is_string(), "{check}");
        assert!(check["summary"].is_string(), "{check}");
        if status == "pass" {
            assert!(
                check["fix"].is_null(),
                "a pass check must carry fix: null, got {check}"
            );
        } else {
            assert!(
                check["fix"].is_string(),
                "a non-pass check must carry a fix string, got {check}"
            );
            if status == "fail" {
                saw_fail = true;
            }
        }
    }
    assert!(report["summary"]["pass"].is_u64());
    assert!(report["summary"]["warn"].is_u64());
    assert!(report["summary"]["fail"].is_u64());
    assert!(
        saw_fail,
        "this fixture's dead-port backend must produce at least one fail check"
    );
    assert!(
        !out.status.success(),
        "--json's exit code must still be nonzero when any check is fail"
    );
}
