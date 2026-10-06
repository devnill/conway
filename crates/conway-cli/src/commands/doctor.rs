//! `conway doctor [--json]` (board item `01M1YVXNPTBNH18ZEWFM18F00T`): runs
//! the checks a first-week operator ends up performing by hand one error
//! message at a time -- an unknown model's context window, a plugin server
//! that cannot start, a tool missing from `PATH`, a settings key that does
//! nothing -- and reports each as `pass`/`warn`/`fail` with a one-line fix.
//!
//! # Every check reuses the real feature's own code path
//!
//! Never a parallel, hand-rolled probe that could drift from what a live
//! session actually does:
//!
//! - **`config.loads`** calls the identical `ConwayBuilder::discover`/
//!   `ConwayBuilder::from_config` a live `conway` invocation calls
//!   (`main.rs`'s own `build_conway`), so the exact error text (and, for
//!   the merge-time [`WarningCode`] variants this module surfaces below,
//!   the exact warning text) is whatever an operator's own terminal would
//!   show.
//! - **`backends.<id>.reachable`** calls [`classify_entry`] with
//!   [`ProbePolicy::All`] -- the SAME classifier `conway`'s own settings
//!   screen and guided-setup trigger use, run here under the "operator is
//!   looking right at it and expects live status" policy that module's own
//!   doc reserves for exactly this kind of on-demand call (never a startup
//!   path).
//! - **`routing.*`** reads the [`ConfigWarning`]s `conway::config::merge::
//!   validate` already computes at config-load time
//!   (`ChainEntryContextWindowUnknown`/`HeadroomExceedsContext`/
//!   `HeadroomConsumesLargeFractionOfContext`) -- the identical check a live
//!   build already performs, not a second resolver.
//! - **`agents.parse`** calls `conway::agents::load_agent_defs` against
//!   the project's own `.conway/agents` directory -- the same loader
//!   `ConwayBuilder::build` calls.
//! - **`plugins.*`** runs the identical tiered install pipeline `main.rs`'s
//!   own `build_conway` runs for every read-only dispatch target
//!   (`crate::first_party_plugins::install`, `crate::subprocess_plugins::
//!   install`, `crate::mcp_plugins::install_tolerant`, `crate::
//!   claude_compat_plugins::install`, then `ConwayBuilder::build` itself) --
//!   see `build_full_conway`'s own doc for the one deliberate difference
//!   (a `DenyAllGate`, matching every other read-only subcommand's own
//!   choice in `main.rs`). A degraded MCP/subprocess entry surfaces exactly
//!   the way it would in a live session: as a [`WarningCode::
//!   McpServerFailed`] [`ConfigWarning`] on `Conway::warnings()`, read back
//!   out here rather than re-derived.
//! - **`tools.confine`** calls `conway_plugin_confine::ConfinePlugin::new`
//!   -- the exact constructor `ConwayBuilder::build` calls when
//!   `"conway.confine"` is installed, which already refuses to construct
//!   without its OS sandbox primitive and already names it in its error.
//!
//! `tools.git` is the one check with no library function to call: the
//! plugin marketplace's own git invocation helper is private to
//! `conway-plugin-marketplace`. It is a plain `PATH` probe for the literal
//! program name (`"git"`) that helper resolves too, documented here as
//! exactly that rather than claimed as the same call.
//!
//! # What this deliberately does not do
//!
//! No `--fix`: every non-`pass` `Check` carries a `fix` string naming the
//! edit or command that would resolve it, never a mutation this binary
//! performs on the operator's behalf. No network beyond the backends
//! already configured (the marketplace/git check never reaches a remote
//! host; `classify_entry` only ever dials `base_url`s an operator already
//! wrote into `settings.json`). No telemetry: nothing this module computes
//! leaves the process except what `--json`/the text report print to stdout.
//!
//! # `--json`
//!
//! `{"checks":[{"id","status":"pass"|"warn"|"fail","summary","fix"},...],
//! "summary":{"pass":N,"warn":N,"fail":N}}` -- `fix` is `null` for a `pass`
//! check, a string otherwise. Documented again, for an operator reading
//! `docs/getting-started.md` rather than this source file, there.
//!
//! Exit code: non-zero **iff** at least one check is `fail` -- `warn` alone
//! exits 0, matching `routes explain`'s own "informational, not a reason to
//! fail a script" posture for a non-fatal finding.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use clap::Args;
use conway::backend_usability::{classify_entry, ProbePolicy, Undetermined, Unusable, Usability};
use conway::config::discovery::session_root;
use conway::config::{ConfigWarning, WarningCode};
use conway::{Conway, ConwayBuilder, FacadeError};
use serde_json::{json, Value};

use crate::exit::ExitCode;

#[derive(Args, Debug)]
pub struct DoctorArgs {
    /// Emit the report as one JSON object on stdout instead of the
    /// human-readable listing -- see this module's own doc for the schema.
    #[arg(long)]
    pub json: bool,
}

/// One check's outcome. The four fields below are exactly `--json`'s wire
/// shape (this module's own doc) -- nothing here is renderer-only state.
#[derive(Debug, Clone)]
struct Check {
    id: String,
    status: CheckStatus,
    summary: String,
    fix: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

impl CheckStatus {
    fn as_str(self) -> &'static str {
        match self {
            CheckStatus::Pass => "pass",
            CheckStatus::Warn => "warn",
            CheckStatus::Fail => "fail",
        }
    }
}

impl Check {
    fn pass(id: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            status: CheckStatus::Pass,
            summary: summary.into(),
            fix: None,
        }
    }

    fn warn(id: impl Into<String>, summary: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            status: CheckStatus::Warn,
            summary: summary.into(),
            fix: Some(fix.into()),
        }
    }

    fn fail(id: impl Into<String>, summary: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            status: CheckStatus::Fail,
            summary: summary.into(),
            fix: Some(fix.into()),
        }
    }
}

/// Runs every check and prints the report (text or `--json`, per
/// `args.json`) to stdout. Returns the process exit code: [`ExitCode::
/// AgentFailed`] iff at least one check is `fail`, [`ExitCode::Completed`]
/// otherwise (a `warn`-only report still exits 0 -- this module's own doc).
///
/// `config_path`/`root` are `cli.config`/`cli.root`, threaded straight from
/// `main.rs`'s own dispatch, before `build_conway` ever runs -- doctor's
/// whole point is diagnosing the configuration that call would otherwise
/// fail (or warn) on, so it never waits for a successfully built `Conway`
/// the way `sessions`/`routes` do.
pub async fn run(
    args: &DoctorArgs,
    env: &HashMap<String, String>,
    config_path: Option<&Path>,
    root: Option<&Path>,
) -> ExitCode {
    let mut checks = Vec::new();

    let builder = match load_builder(config_path) {
        Ok(builder) => builder,
        Err(error) => {
            checks.push(Check::fail(
                "config.loads",
                error.to_string(),
                "edit the file and key named in the error above, then re-run `conway doctor`",
            ));
            render(args.json, &checks);
            return exit_code_for(&checks);
        }
    };
    checks.push(Check::pass(
        "config.loads",
        "settings.json (and every layer above it) parsed cleanly",
    ));

    let config = builder.config().clone();
    let load_time_warnings = builder.warnings().len();
    let mut saw_routing_warning = false;
    for (index, warning) in builder.warnings().iter().enumerate() {
        if is_routing_warning(warning.code) {
            saw_routing_warning = true;
        }
        checks.push(check_for_warning(warning, index));
    }
    if !saw_routing_warning {
        checks.push(Check::pass(
            "routing.chain_windows",
            "every role's chain entries resolve a context window from known model metadata \
             (no chain-entry-unknown or headroom warning at config-load time)",
        ));
    }

    for (id, entry) in &config.backends {
        checks.push(backend_reachable_check(
            id,
            &classify_entry(
                entry,
                env,
                ProbePolicy::All,
                conway::backend_usability::DEFAULT_PROBE_TIMEOUT,
            )
            .await,
        ));
        checks.push(backend_inert_keys_check(id, entry));
    }

    checks.push(agents_check(&config.cwd));
    checks.push(project_key_check(&config, env));
    checks.push(git_on_path_check());
    checks.push(confine_binary_check(&config));

    match build_full_conway(builder, env, root).await {
        Ok(conway) => {
            let new_warnings = &conway.warnings()[load_time_warnings..];
            for (offset, warning) in new_warnings.iter().enumerate() {
                checks.push(check_for_warning(warning, load_time_warnings + offset));
            }
            checks.push(Check::pass(
                "plugins.build",
                "every configured plugin and backend constructed, and every MCP/subprocess \
                 plugin completed its handshake within its configured startup budget",
            ));
        }
        Err(failure) => {
            checks.push(Check::fail(
                failure.stage,
                failure.error.to_string(),
                "fix the error named above, then re-run `conway doctor` -- routing/context-\
                 window checks are skipped until this stage succeeds",
            ));
        }
    }

    render(args.json, &checks);
    exit_code_for(&checks)
}

/// [`ConwayBuilder::discover`]/[`ConwayBuilder::from_config`] -- the exact
/// two-armed choice `main.rs`'s own `build_conway` makes, reused verbatim
/// rather than restated, so `conway doctor --config <path>` and a live
/// `conway --config <path>` agree on which file was actually read.
fn load_builder(config_path: Option<&Path>) -> conway::Result<ConwayBuilder> {
    match config_path {
        Some(path) => ConwayBuilder::from_config(path),
        None => ConwayBuilder::discover(),
    }
}

/// One tiered-pipeline stage's name, paired with the [`FacadeError`] it
/// failed with -- `run`'s own `Err` arm renders this as a single `fail`
/// check named by `stage`.
struct PluginBuildFailure {
    stage: &'static str,
    error: FacadeError,
}

/// Runs the identical tiered install pipeline `main.rs`'s own
/// `build_conway` runs for every read-only dispatch target (`sessions`,
/// `routes`, `tools list`, `plugin list`/`install`/`remove`) -- see this
/// module's own top doc for the full list of which function each stage
/// calls. The one deliberate difference from `build_conway`: this always
/// supplies `conway::gates::DenyAllGate` (never `--root`'s own confinement
/// interacting with an interactive gate), matching the exact gate `main.rs`
/// already picks for those same read-only targets, since doctor never
/// starts an agent or dispatches a tool call either.
///
/// Returns the built [`Conway`] on success (so `run` can read `Conway::
/// warnings()` for the plugin/MCP-startup checks), or the first stage that
/// failed, named, with its real [`FacadeError`] -- never panics, never
/// retries.
async fn build_full_conway(
    builder: ConwayBuilder,
    env: &HashMap<String, String>,
    root: Option<&Path>,
) -> Result<Conway, PluginBuildFailure> {
    let builder = builder.with_default_hook_runner();
    let builder = builder.with_permission_gate(Arc::new(conway::gates::DenyAllGate));
    let builder = match root {
        Some(root) => builder.with_root(root),
        None => builder,
    };
    let builder = builder.with_builtin_plugins(conway::PluginSelection::All);
    let (builder, _memory_store, _agent_names, _skills_plugin) =
        crate::first_party_plugins::install(builder, env, None)
            .await
            .map_err(|error| PluginBuildFailure {
                stage: "plugins.first_party",
                error,
            })?;
    let builder = crate::subprocess_plugins::install(builder)
        .await
        .map_err(|error| PluginBuildFailure {
            stage: "plugins.subprocess",
            error,
        })?;
    let builder = crate::mcp_plugins::install_tolerant(builder).await;
    let builder = crate::claude_compat_plugins::install(builder)
        .await
        .map_err(|error| PluginBuildFailure {
            stage: "plugins.claude_compat",
            error,
        })?;
    builder.build().map_err(|error| PluginBuildFailure {
        stage: "plugins.build",
        error,
    })
}

/// Whether `code` is one of the three [`WarningCode`] variants
/// `conway::config::merge::validate` raises about a role's chain/headroom --
/// the set `run` checks for before deciding whether to add a synthetic
/// `routing.chain_windows` pass check (its own call site's doc).
fn is_routing_warning(code: WarningCode) -> bool {
    matches!(
        code,
        WarningCode::ChainEntryContextWindowUnknown
            | WarningCode::HeadroomExceedsContext
            | WarningCode::HeadroomConsumesLargeFractionOfContext
    )
}

/// `true` for exactly [`WarningCode::McpServerFailed`]: a degraded MCP/
/// subprocess plugin is a real, named capability loss (the tool it would
/// have contributed never registers), not a merely informational notice --
/// every other [`WarningCode`] this module has ever observed is non-fatal
/// by the config loader's own classification and renders `warn`.
fn warning_status(code: WarningCode) -> CheckStatus {
    if matches!(code, WarningCode::McpServerFailed) {
        CheckStatus::Fail
    } else {
        CheckStatus::Warn
    }
}

/// A short, stable check-id prefix per [`WarningCode`] variant -- `run`
/// appends `.{index}` to keep two warnings of the same code from colliding
/// on one check id. The wildcard arm exists only for this `#[non_exhaustive]`
/// enum's forward compatibility; it is not expected to be exercised by this
/// module's own fixtures.
fn warning_check_id_prefix(code: WarningCode) -> &'static str {
    match code {
        WarningCode::ChainEntryContextWindowUnknown => "routing.chain_window_unknown",
        WarningCode::HeadroomExceedsContext => "routing.headroom_exceeds_context",
        WarningCode::HeadroomConsumesLargeFractionOfContext => "routing.headroom_large_fraction",
        WarningCode::PresentationConfigIgnored => "config.presentation_ignored",
        WarningCode::UntrustedProjectConfigIgnored => "config.untrusted_project_ignored",
        WarningCode::McpServerFailed => "plugins.mcp_server_failed",
        WarningCode::RootWithUnconfinableTool => "tools.root_unconfinable",
        WarningCode::NoFirstPartyPluginsInstalled => "plugins.none_installed",
        WarningCode::OptionalPluginDependencyMissing => "plugins.optional_dependency_missing",
        WarningCode::OptionalHostCapabilityMissing => "plugins.optional_host_capability_missing",
        _ => "config.warning",
    }
}

/// A short, canned fix per [`WarningCode`] variant, for the cases where
/// `message` itself (already shown as the check's `summary`) does not
/// already spell out the remedy. `None` for a code whose own `message` text
/// already names the fix (`McpServerFailed`/`RootWithUnconfinableTool`,
/// both of which already end with one) -- `check_for_warning` falls back to
/// a generic "see above" line rather than ever leaving `fix` unset, per this
/// module's own "every non-pass check carries a fix" rule.
fn warning_fix(code: WarningCode) -> Option<&'static str> {
    match code {
        WarningCode::ChainEntryContextWindowUnknown => Some(
            "add this backend/model pair to models.json with its real context window, or \
             accept the dialect-default floor routing falls back to",
        ),
        WarningCode::HeadroomExceedsContext
        | WarningCode::HeadroomConsumesLargeFractionOfContext => Some(
            "lower the role's headroom_tokens, or route it to a model with a larger \
                 context window",
        ),
        WarningCode::PresentationConfigIgnored => Some(
            "move [tui] settings into conway-cli's own presentation config file -- this \
             schema no longer reads them from settings.json",
        ),
        WarningCode::UntrustedProjectConfigIgnored => {
            Some("run `conway trust project` to review and apply this project's .conway/ files")
        }
        WarningCode::NoFirstPartyPluginsInstalled => Some("run `conway plugin install --defaults`"),
        _ => None,
    }
}

/// Builds the `Check` for one [`ConfigWarning`] -- `run`'s only call site,
/// pulled out so every warning (load-time or build-time) goes through the
/// same id/status/fix mapping.
fn check_for_warning(warning: &ConfigWarning, index: usize) -> Check {
    let id = format!("{}.{index}", warning_check_id_prefix(warning.code));
    let fix = warning_fix(warning.code).unwrap_or("see the message above for the exact fix");
    match warning_status(warning.code) {
        CheckStatus::Fail => Check::fail(id, warning.message.clone(), fix),
        _ => Check::warn(id, warning.message.clone(), fix),
    }
}

/// `backends.<id>.reachable` -- reads [`classify_entry`]'s real classifier
/// output, never re-derives it. [`Usability::Usable`] is `pass`;
/// [`Usability::Unusable`] (a definite failure) is `fail`;
/// [`Usability::Undetermined`] (a probe timeout, no credential declared for
/// a `kind` this module does not know, etc.) is `warn` -- never `fail`,
/// matching that type's own "never treat this as a failure" doc.
fn backend_reachable_check(id: &str, usability: &Usability) -> Check {
    let check_id = format!("backends.{id}.reachable");
    match usability {
        Usability::Usable => Check::pass(check_id, format!("{id}: reachable")),
        Usability::Unusable(reason) => Check::fail(
            check_id,
            format!("{id}: {reason}"),
            unusable_fix(reason, id),
        ),
        Usability::Undetermined(reason) => Check::warn(
            check_id,
            format!("{id}: {reason}"),
            undetermined_fix(reason, id),
        ),
    }
}

fn unusable_fix(reason: &Unusable, id: &str) -> String {
    match reason {
        Unusable::CredentialVariableUnset { variable } => {
            format!("export {variable} with backends.{id}'s real credential, then re-run")
        }
        Unusable::EndpointRefused { base_url } => {
            format!("start the server listening at {base_url}, or fix backends.{id}.base_url")
        }
    }
}

fn undetermined_fix(reason: &Undetermined, id: &str) -> String {
    match reason {
        Undetermined::NoCredentialDeclared { .. } => format!(
            "if backends.{id} needs a credential, set api_key or api_key_env; a local server \
             needing none is fine as-is"
        ),
        Undetermined::EndpointUnreachable { base_url } => format!(
            "{base_url} did not answer within the probe window -- retry, or check whether the \
             server is slow to start"
        ),
        Undetermined::NotProbed => format!(
            "backends.{id} declares a credential but is not local, so this was not dialed -- \
             run `conway routes explain <role>` after a real turn to confirm it actually works"
        ),
        Undetermined::UnparseableEndpoint { base_url } => format!(
            "backends.{id}.base_url ('{base_url}') is not a URL this check can read a host and \
             port from -- verify it by hand"
        ),
    }
}

/// `backends.<id>.inert_keys` -- `BackendEntry::extra`'s own catch-all for
/// everything the schema does not read, named directly rather than
/// re-detected: a typo'd field name (`base_ur1`) lands here silently
/// (`BackendEntry`'s own doc, "Kind-specific keys: the catch-all shape"),
/// and this is the one place that surfaces it.
fn backend_inert_keys_check(id: &str, entry: &conway::config::schema::BackendEntry) -> Check {
    let check_id = format!("backends.{id}.inert_keys");
    if entry.extra.is_empty() {
        return Check::pass(
            check_id,
            format!("backends.{id} carries no unrecognized keys"),
        );
    }
    let keys: Vec<&str> = entry.extra.keys().map(String::as_str).collect();
    Check::warn(
        check_id,
        format!(
            "backends.{id} carries key(s) this schema never reads: {}",
            keys.join(", ")
        ),
        "if these were meant to configure kind/api_key/api_key_env/base_url/dialect/\
         stream_tools/local, check the spelling; a third-party kind's own keys are expected \
         here and safe to leave",
    )
}

/// `agents.parse` -- calls `conway::agents::load_agent_defs` directly
/// against `<cwd>/.conway/agents`, the same directory and loader
/// `ConwayBuilder::build` reads. A malformed file's own error (including a
/// missing `name:` key) already names the file; this check never re-parses
/// it to add that detail a second way.
fn agents_check(cwd: &Path) -> Check {
    let agents_dir = cwd.join(".conway").join("agents");
    match conway::agents::load_agent_defs(&agents_dir) {
        Ok(defs) => Check::pass(
            "agents.parse",
            format!(
                "{} agent definition(s) parsed from {}",
                defs.len(),
                agents_dir.display()
            ),
        ),
        // `FacadeError::AgentDef`'s own `#[error("{message}")]` deliberately
        // formats `message` alone (every other `FacadeError` variant that
        // carries a `path` does the same, `error.rs`'s own doc) -- the file
        // is named here, explicitly, from the error's own structured
        // `path` field, rather than lost the moment this collapses to a
        // bare `.to_string()`.
        Err(FacadeError::AgentDef { path, message }) => Check::fail(
            "agents.parse",
            format!("{}: {message}", path.display()),
            "fix the file named above (a common cause: a missing `name:` key in its \
             frontmatter), then re-run `conway doctor`",
        ),
        Err(error) => Check::fail(
            "agents.parse",
            error.to_string(),
            "fix the file named above (a common cause: a missing `name:` key in its \
             frontmatter), then re-run `conway doctor`",
        ),
    }
}

/// `session.project_key` -- calls [`session_root`] directly, the same
/// resolver `config::merge::load_impl` uses for `[session].root`'s
/// effective value, so this always names the directory a real session
/// launched from this `cwd` would actually write to.
fn project_key_check(
    config: &conway::config::ConwayConfig,
    env: &HashMap<String, String>,
) -> Check {
    let resolved = session_root(&config.cwd, config.session.root.as_deref(), env);
    let summary = match &config.session.root {
        Some(_) => format!(
            "session.root is set explicitly; sessions and board data resolve under {}",
            resolved.display()
        ),
        None => {
            let key = resolved
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("?");
            format!(
                "project key `{key}`; sessions and board data resolve under {}",
                resolved.display()
            )
        }
    };
    Check::pass("session.project_key", summary)
}

/// `tools.git` -- a plain `PATH` probe for the literal program name `git`,
/// since the plugin marketplace's own git-invocation helper
/// (`conway_plugin_marketplace::git_source`) is private to that crate. See
/// this module's own top doc for why this is disclosed as a probe rather
/// than claimed as the same reused call.
fn git_on_path_check() -> Check {
    let available = std::process::Command::new("git")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if available {
        Check::pass(
            "tools.git",
            "git is on PATH (the plugin marketplace's git-sourced installs need it)",
        )
    } else {
        Check::warn(
            "tools.git",
            "git was not found on PATH",
            "install git (e.g. `brew install git`, or your OS package manager) to use the \
             plugin marketplace's git-sourced installs",
        )
    }
}

/// `tools.confine` -- calls `conway_plugin_confine::ConfinePlugin::new`
/// directly, the exact constructor `ConwayBuilder::build` calls once
/// `"conway.confine"` is in `plugins.install`. `fail` only when that id is
/// actually configured (a genuine startup blocker); `warn` otherwise (the
/// sandbox primitive is simply not needed yet).
fn confine_binary_check(config: &conway::config::schema::ConwayConfig) -> Check {
    match conway_plugin_confine::ConfinePlugin::new() {
        Ok(_) => Check::pass(
            "tools.confine",
            "the OS containment primitive conway.confine depends on is present",
        ),
        Err(error) => {
            let installed = config
                .plugins
                .install
                .iter()
                .any(|id| id == conway_plugin_confine::PLUGIN_ID);
            let summary = error.to_string();
            if installed {
                Check::fail(
                    "tools.confine",
                    summary,
                    "install the sandbox primitive named above, or remove \"conway.confine\" \
                     from plugins.install",
                )
            } else {
                Check::warn(
                    "tools.confine",
                    summary,
                    "install the sandbox primitive named above before adding \"conway.confine\" \
                     to plugins.install",
                )
            }
        }
    }
}

fn tally(checks: &[Check]) -> (usize, usize, usize) {
    let mut pass = 0;
    let mut warn = 0;
    let mut fail = 0;
    for check in checks {
        match check.status {
            CheckStatus::Pass => pass += 1,
            CheckStatus::Warn => warn += 1,
            CheckStatus::Fail => fail += 1,
        }
    }
    (pass, warn, fail)
}

/// Exit non-zero **iff** at least one check is `fail` -- this module's own
/// top doc.
fn exit_code_for(checks: &[Check]) -> ExitCode {
    if checks.iter().any(|check| check.status == CheckStatus::Fail) {
        ExitCode::AgentFailed
    } else {
        ExitCode::Completed
    }
}

fn render(as_json: bool, checks: &[Check]) {
    if as_json {
        render_json(checks);
    } else {
        render_text(checks);
    }
}

fn render_text(checks: &[Check]) {
    for check in checks {
        let marker = match check.status {
            CheckStatus::Pass => "PASS",
            CheckStatus::Warn => "WARN",
            CheckStatus::Fail => "FAIL",
        };
        println!("[{marker}] {}: {}", check.id, check.summary);
        if let Some(fix) = &check.fix {
            println!("       fix: {fix}");
        }
    }
    let (pass, warn, fail) = tally(checks);
    println!("\n{pass} passed, {warn} warned, {fail} failed");
}

fn render_json(checks: &[Check]) {
    let checks_json: Vec<Value> = checks
        .iter()
        .map(|check| {
            json!({
                "id": check.id,
                "status": check.status.as_str(),
                "summary": check.summary,
                "fix": check.fix,
            })
        })
        .collect();
    let (pass, warn, fail) = tally(checks);
    let report = json!({
        "checks": checks_json,
        "summary": { "pass": pass, "warn": warn, "fail": fail },
    });
    println!(
        "{}",
        serde_json::to_string(&report).expect("doctor report always serializes")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warning(code: WarningCode, message: &str) -> ConfigWarning {
        ConfigWarning {
            code,
            message: message.to_string(),
        }
    }

    /// `check_for_warning` names [`WarningCode::ChainEntryContextWindowUnknown`]
    /// as a `warn`, carries the real message as `summary`, and never leaves
    /// `fix` empty.
    #[test]
    fn doctor_chain_entry_context_window_unknown_renders_as_warn_with_a_fix() {
        let w = warning(
            WarningCode::ChainEntryContextWindowUnknown,
            "role 'coder' chain entry 'local/glm' has no models.json entry",
        );
        let check = check_for_warning(&w, 0);
        assert_eq!(check.status, CheckStatus::Warn);
        assert_eq!(
            check.summary,
            "role 'coder' chain entry 'local/glm' has no models.json entry"
        );
        assert!(check.fix.is_some());
        assert!(check.id.starts_with("routing.chain_window_unknown"));
    }

    /// `WarningCode::McpServerFailed` is the one code `warning_status` maps
    /// to `fail`, not `warn` -- a degraded tool is a real capability loss,
    /// unlike every other load-time notice. Paired with the "every other
    /// code is warn" test below: together they prove `warning_status`
    /// branches on the code, not a constant.
    #[test]
    fn doctor_mcp_server_failed_renders_as_fail() {
        let w = warning(
            WarningCode::McpServerFailed,
            "[plugins].mcp entry 'acme': spawn failed -- starting WITHOUT this server's tools",
        );
        let check = check_for_warning(&w, 0);
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.id.starts_with("plugins.mcp_server_failed"));
    }

    /// The "every other code is warn, never fail" half of the pairing above.
    #[test]
    fn doctor_headroom_exceeds_context_renders_as_warn_not_fail() {
        let w = warning(
            WarningCode::HeadroomExceedsContext,
            "role 'default' headroom exceeds its smallest reachable context window",
        );
        let check = check_for_warning(&w, 0);
        assert_eq!(check.status, CheckStatus::Warn);
    }

    /// `backend_reachable_check` maps every [`Usability`] variant to its own
    /// status, and [`Usability::Undetermined`] is specifically NEVER `fail`
    /// -- the exact distinction `backend_usability`'s own doc states as its
    /// reason to exist.
    #[test]
    fn doctor_backend_usable_is_pass_unusable_is_fail_undetermined_is_warn() {
        let usable = backend_reachable_check("local", &Usability::Usable);
        assert_eq!(usable.status, CheckStatus::Pass);

        let unusable = backend_reachable_check(
            "hosted",
            &Usability::Unusable(Unusable::CredentialVariableUnset {
                variable: "KIMI_API_KEY".to_string(),
            }),
        );
        assert_eq!(unusable.status, CheckStatus::Fail);
        assert!(unusable.summary.contains("KIMI_API_KEY"));
        assert!(unusable.fix.unwrap().contains("KIMI_API_KEY"));

        let undetermined = backend_reachable_check(
            "slow",
            &Usability::Undetermined(Undetermined::EndpointUnreachable {
                base_url: "http://127.0.0.1:11434/v1".to_string(),
            }),
        );
        assert_eq!(undetermined.status, CheckStatus::Warn);
    }

    /// `backend_inert_keys_check` is `pass` for an entry with nothing
    /// unrecognized, and `warn` (never `fail` -- a third-party kind
    /// legitimately uses this map) when `extra` carries a key, naming it in
    /// `summary`. Paired: together they prove the check actually reads
    /// `extra` rather than always answering one way.
    #[test]
    fn doctor_backend_inert_keys_pass_when_empty_warn_when_present() {
        use conway::config::schema::BackendEntry;

        let clean = BackendEntry::default();
        let clean_check = backend_inert_keys_check("ok", &clean);
        assert_eq!(clean_check.status, CheckStatus::Pass);

        let mut dirty = BackendEntry::default();
        dirty
            .extra
            .insert("base_ur1".to_string(), serde_json::json!("typo"));
        let dirty_check = backend_inert_keys_check("typo", &dirty);
        assert_eq!(dirty_check.status, CheckStatus::Warn);
        assert!(dirty_check.summary.contains("base_ur1"));
    }

    /// `exit_code_for`/`tally`: any `fail` makes the whole report exit
    /// non-zero; a report with only `pass`/`warn` exits 0 -- the module's
    /// own documented contract, and the one JSON consumers rely on.
    #[test]
    fn doctor_exit_code_is_nonzero_iff_any_check_failed() {
        let clean = vec![Check::pass("a", "ok"), Check::warn("b", "meh", "fix b")];
        assert_eq!(exit_code_for(&clean), ExitCode::Completed);

        let dirty = vec![Check::pass("a", "ok"), Check::fail("c", "broken", "fix c")];
        assert_eq!(exit_code_for(&dirty), ExitCode::AgentFailed);
    }

    /// The `--json` schema this module's own doc documents: `checks` is an
    /// array of `{id,status,summary,fix}`, `fix` is `null` for a pass and a
    /// string otherwise, and `summary` is a stable top-level `pass`/`warn`/
    /// `fail` tally -- read back with `serde_json::Value` rather than
    /// asserting on the printed string, so field order can never break this
    /// test.
    #[test]
    fn doctor_json_report_has_the_documented_shape() {
        let checks = vec![
            Check::pass("a", "all good"),
            Check::warn("b", "hmm", "do this"),
            Check::fail("c", "broken", "do that"),
        ];
        let checks_json: Vec<Value> = checks
            .iter()
            .map(|check| {
                json!({
                    "id": check.id,
                    "status": check.status.as_str(),
                    "summary": check.summary,
                    "fix": check.fix,
                })
            })
            .collect();
        let (pass, warn, fail) = tally(&checks);
        let report = json!({
            "checks": checks_json,
            "summary": { "pass": pass, "warn": warn, "fail": fail },
        });

        assert_eq!(report["summary"]["pass"], 1);
        assert_eq!(report["summary"]["warn"], 1);
        assert_eq!(report["summary"]["fail"], 1);
        assert_eq!(report["checks"][0]["status"], "pass");
        assert!(report["checks"][0]["fix"].is_null());
        assert_eq!(report["checks"][1]["status"], "warn");
        assert_eq!(report["checks"][1]["fix"], "do this");
        assert_eq!(report["checks"][2]["status"], "fail");
        assert_eq!(report["checks"][2]["fix"], "do that");
    }
}
