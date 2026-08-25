//! Compiled-binary regression test for board item
//! `01M0VV6CVSZM4XH8J4G6EBV5E3`: `CONWAY_CONFIG_DIR` relocates the *user*
//! config layer, but before this item the *project* layer's own upward
//! walk (`conway::config::discovery::discover`) knew nothing of that
//! variable and could still reach `~/.conway/settings.json` from any `cwd`
//! beneath `$HOME` -- outranking the isolated layer the operator believed
//! they had switched to, since `project` beats `user` in the five-source
//! precedence order regardless of what `CONWAY_CONFIG_DIR` names. This cost
//! the operator two live provider calls on real credentials on 2026-08-25.
//!
//! Deliberately NOT reusing `tests/common/mod.rs`'s `command`/`Fixture`
//! harness: that harness always passes `--config <fixture>`, which sets
//! `LoadOptions.explicit_path` and bypasses `discover` entirely (see
//! `merge::merged_document_impl`'s own body -- the project layer is
//! `explicit_path.or_else(discover)`, so a fixture that always supplies the
//! former never exercises the latter). This defect lives specifically in
//! the `explicit_path`-absent path, so this file builds its own `Command`
//! with no `--config` flag at all, driving `main.rs`'s
//! `ConwayBuilder::discover()` branch directly -- the same branch a real,
//! ordinary invocation with no `--config` flag takes.
//!
//! **No real `$HOME`, no live network, ever.** "Simulated home" below is an
//! ordinary [`tempfile::TempDir`] this process created and owns, standing
//! in for the operator's real home directory -- never the real one, never
//! read or written to. `[backends]` is never populated in either fixture
//! config (the empty-object baked-in default, `default_document`'s own
//! `"backends": {}`, plus an empty `roles.default.chain`, both already
//! satisfy `merge::validate`'s checks 1-2 with nothing further to declare),
//! so `sessions list` -- the read-only subcommand this test drives -- never
//! dials anything: no backend id is ever named in a role chain for it to
//! resolve, let alone connect to.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Declares exactly one `[backends]` entry, pointed at `port` on
/// `127.0.0.1` -- see [`IsolatedConfigDir::write`]'s own doc for why one is
/// required at all and why it is never actually dialed. Shared by both
/// fixture writers below so the two settings.json bodies differ ONLY in the
/// field this test's assertions key off of (`session.root`), not in
/// unrelated shape.
fn backend_json(port: u16) -> String {
    format!(
        r#"{{
  "backends": {{
    "dead": {{ "kind": "openai-compat", "base_url": "http://127.0.0.1:{port}/v1", "dialect": "openai" }}
  }}
}}"#
    )
}

/// The dead-port constant every test that does not itself verify liveness
/// uses: nothing on this machine listens on TCP port 9 (`discard`, RFC 863,
/// never bound by an ordinary process) -- the same port
/// `subcommands.rs::static_fixture` already uses for the identical reason.
const DEAD_PORT: u16 = 9;

/// A directory this test owns end to end, standing in for `$CONWAY_CONFIG_DIR`
/// -- the ISOLATED destination an operator setting that variable believes
/// every layer of config now reads from.
struct IsolatedConfigDir {
    dir: tempfile::TempDir,
}

impl IsolatedConfigDir {
    /// Writes a minimal `settings.json` at this directory's root:
    /// `default_document`'s own baked-in defaults (`roles.default.chain:
    /// []`, `default_role: "default"`) already pass every hard-error
    /// validation check with nothing added, EXCEPT `ConwayBuilder::build`'s
    /// own separate "at least one backend configured" check (unrelated to
    /// `[roles]`/routing -- it fires on an empty `[backends]` table
    /// regardless of whether any role's chain would ever reference one), so
    /// this fixture declares exactly one backend entry, pointed at a dead
    /// local port (`http://127.0.0.1:9`) per this file's own safety
    /// constraint: nothing here is ever dialed anyway, since `roles.default
    /// .chain` stays empty and `sessions list` never dispatches a turn.
    fn write(port: u16) -> Self {
        let dir = tempfile::tempdir().expect("tempdir for isolated CONWAY_CONFIG_DIR");
        std::fs::write(dir.path().join("settings.json"), backend_json(port))
            .expect("write settings.json");
        Self { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// A directory this test owns end to end, standing in for the operator's
/// real `$HOME` -- with a `.conway/settings.json` at its root, exactly the
/// shape the real `~/.conway/settings.json` has on the machine that lost two
/// live provider calls to this defect. `session.root` is set to an absolute,
/// obviously-marked path (`POISONED-SESSIONS`) so this test can tell,
/// after the run, whether the "home" config's project-layer read ever won:
/// if it did, the binary creates that exact directory when `sessions list`
/// opens its session store; if isolation held, it never does.
struct SimulatedHome {
    dir: tempfile::TempDir,
}

impl SimulatedHome {
    fn write(port: u16) -> Self {
        let dir = tempfile::tempdir().expect("tempdir for simulated $HOME");
        let conf_dir = dir.path().join(".conway");
        std::fs::create_dir_all(&conf_dir).expect("create simulated $HOME/.conway");
        let poisoned_root = dir.path().join("POISONED-SESSIONS");
        let mut body: serde_json::Value =
            serde_json::from_str(&backend_json(port)).expect("parse backend_json");
        body["session"] = serde_json::json!({ "root": poisoned_root.to_string_lossy() });
        std::fs::write(
            conf_dir.join("settings.json"),
            serde_json::to_vec_pretty(&body).expect("serialize poisoned settings.json"),
        )
        .expect("write simulated $HOME/.conway/settings.json");
        Self { dir }
    }

    fn poisoned_sessions_root(&self) -> PathBuf {
        self.dir.path().join("POISONED-SESSIONS")
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// A working directory nested two levels beneath this simulated home's
    /// root, with no `.conway/` of its own -- exactly the ordinary shape of
    /// "an operator working somewhere under their home directory," which is
    /// what makes `discover`'s upward walk from here reach
    /// `<home>/.conway/settings.json` at all (the walk's own doc: nearest
    /// match wins, and there IS no nearer one).
    fn nested_cwd(&self) -> PathBuf {
        let cwd = self.dir.path().join("work").join("project");
        std::fs::create_dir_all(&cwd).expect("create nested cwd under simulated $HOME");
        cwd
    }
}

/// Builds the real compiled `conway` binary's `Command`, deliberately with
/// NO `--config` flag (see this file's own module doc for why), `cwd` set to
/// `home`'s nested project directory, and `CONWAY_CONFIG_DIR` pointed at
/// `isolated`. `sessions list` is a read-only subcommand that never dials a
/// backend (see `config_warnings.rs`/`subcommands.rs`'s own precedent for
/// this exact command standing in for "any non-interactive dispatch
/// target").
fn run_sessions_list(isolated: &IsolatedConfigDir, home: &SimulatedHome) -> std::process::Output {
    Command::new(assert_cmd::cargo::cargo_bin("conway"))
        .current_dir(home.nested_cwd())
        .env("CONWAY_CONFIG_DIR", isolated.path())
        // `HOME` (and `USERPROFILE` for a Windows-hosted `directories` build)
        // point at `home`'s own temp directory, NOT this developer machine's
        // real one -- `directories::BaseDirs::home_dir()` is what
        // `discovery::home_settings_path`/`user_config_path`'s fallback
        // branch both consult, and it is what makes this test a faithful
        // stand-in for "cwd under the operator's real $HOME": the subprocess
        // must genuinely believe `home.dir` IS its home directory, the same
        // way an operator's real shell session does, for the walk from
        // `home.nested_cwd()` to reach `<home>/.conway/settings.json` for
        // the same structural reason it would on a real machine.
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .arg("sessions")
        .arg("list")
        .output()
        .expect("run conway binary")
}

/// **ACCEPTANCE 2.** Before the fix, this reproduces the defect: the
/// unbounded project-discovery walk from a `cwd` beneath the simulated
/// `$HOME` reaches `<home>/.conway/settings.json`, and -- because `project`
/// outranks `user` in the five-source merge order -- its `session.root`
/// (`POISONED-SESSIONS`) wins over the isolated `CONWAY_CONFIG_DIR` layer's
/// own central-default resolution. `sessions list` then creates exactly
/// that directory when it opens its (empty) session store. After the fix,
/// `discover` excludes that candidate (`project_discovery_exclusions`), the
/// central default resolves against the ISOLATED `CONWAY_CONFIG_DIR`
/// instead, and the poisoned directory is never created.
#[test]
fn cli_with_conway_config_dir_set_never_reads_a_settings_json_discovered_under_a_simulated_home() {
    let isolated = IsolatedConfigDir::write(DEAD_PORT);
    let home = SimulatedHome::write(DEAD_PORT);

    let out = run_sessions_list(&isolated, &home);
    assert!(
        out.status.success(),
        "sessions list should succeed regardless of which config layer won; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let poisoned_root = home.poisoned_sessions_root();
    assert!(
        !poisoned_root.exists(),
        "isolation defeated: the binary created {} -- the session root named \
         by <simulated $HOME>/.conway/settings.json, discovered as a *project* \
         config even though CONWAY_CONFIG_DIR was set to an isolated directory. \
         This is board item 01M0VV6CVSZM4XH8J4G6EBV5E3's own defect, reproduced.",
        poisoned_root.display()
    );
}

/// BREAK-THE-GUARD, the same shape `config_warnings.rs`'s `a_healthy_
/// headroom_prints_no_warning` uses: proves the isolated `CONWAY_CONFIG_DIR`
/// layer is not merely "not poisoned" but is genuinely THE layer that
/// resolved -- the central-default session root actually lands inside the
/// isolated directory, not in some third, unaccounted-for place (e.g. the
/// real developer machine's own `~/.conway`, which would also make the
/// assertion above pass for the wrong reason).
#[test]
fn cli_with_conway_config_dir_set_resolves_the_session_root_inside_the_isolated_dir() {
    let isolated = IsolatedConfigDir::write(DEAD_PORT);
    let home = SimulatedHome::write(DEAD_PORT);

    let out = run_sessions_list(&isolated, &home);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Mirrors `conway::config::discovery::session_root`'s own central-default
    // computation: `<CONWAY_CONFIG_DIR>/sessions/<project-key(cwd)>`.
    let nested_cwd = home
        .nested_cwd()
        .canonicalize()
        .unwrap_or_else(|_| home.nested_cwd());
    let mut env = std::collections::HashMap::new();
    env.insert(
        "CONWAY_CONFIG_DIR".to_string(),
        isolated.path().to_string_lossy().into_owned(),
    );
    let expected_root = conway::config::discovery::session_root(&nested_cwd, None, &env);

    assert!(
        expected_root.exists(),
        "expected the central-default session root {} (inside the isolated \
         CONWAY_CONFIG_DIR) to have been created by `sessions list`, but it was not \
         -- the config that actually resolved came from neither the isolated \
         layer nor the poisoned one",
        expected_root.display()
    );
}

/// **Safety verification, not merely code-path reasoning.** Both fixture
/// configs' one `[backends]` entry points at an ephemeral TCP listener THIS
/// TEST binds and never accepts on -- rather than a bare dead port, so this
/// test can assert, directly, that zero connection attempts reached it: not
/// "no error occurred" (a dead port and a refused connection look the same
/// from the caller's side, and either could theoretically hide a real dial
/// this suite failed to configure away), but "nothing tried to connect to
/// the one address this run's config could have dialed at all." This is the
/// per-item safety constraint ("do not run the compiled binary in a way
/// that could reach a real provider") demonstrated, not assumed --
/// `sessions list` is read-only and both fixtures' `roles.default.chain` is
/// empty, so no code path in `ConwayBuilder::build`/dispatch ever names this
/// backend id to begin with; this test is the belt to that suspenders.
#[test]
fn cli_run_never_attempts_a_connection_to_the_fixtures_own_backend_address() {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral guard listener");
    listener
        .set_nonblocking(true)
        .expect("set guard listener non-blocking");
    let port = listener
        .local_addr()
        .expect("guard listener local addr")
        .port();

    let isolated = IsolatedConfigDir::write(port);
    let home = SimulatedHome::write(port);

    let out = run_sessions_list(&isolated, &home);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    match listener.accept() {
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(e) => panic!("unexpected guard-listener error: {e}"),
        Ok((_stream, addr)) => panic!(
            "the compiled binary connected to {addr}, the fixture's own backend \
             address -- a read-only `sessions list` invocation must never dial a \
             backend at all"
        ),
    }
}
