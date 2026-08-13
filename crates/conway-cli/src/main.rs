//! `conway`: the CLI binary (WI-111).
//!
//! `main` never uses `?`: every fallible step is matched explicitly and
//! converted to an [`conway_cli::exit::ExitCode`] via
//! [`conway_cli::exit::ExitCode::from_error`], so
//! there is exactly one place (the bottom of this file) that turns an
//! `ExitCode` into a process exit status.

use std::sync::Arc;

use clap::Parser;
use conway::{Conway, ConwayBuilder, PermissionGate};

use conway_cli::cli::{Cli, Command};
use conway_cli::exit::ExitCode;
use conway_cli::{commands, diag, first_party_plugins, oneshot, tui};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    // `Cli::parse()` (not `try_parse`) already implements the exact
    // contract the WI-111 criteria ask for: `--help`/`--version` print to
    // stdout and exit 0, every other parse error prints to stderr and
    // exits 2 -- clap's own `Error::exit()`, which this calls internally.
    let cli = Cli::parse();
    diag::set_verbosity(cli.verbose);
    install_tracing(cli.verbose);

    if let Some(dir) = &cli.cwd {
        if let Err(e) = std::env::set_current_dir(dir) {
            diag::error(format!("cannot set cwd to {}: {e}", dir.display()));
            return to_process_code(ExitCode::Usage);
        }
    }

    // **Disclosed, WI-112/WI-114 reconciliation -- widens this function and
    // `build_conway` beyond `dispatch`'s own match arms, which is the one
    // piece of this shared file each of those items' briefs asked to leave
    // alone.** `conway_runtime::runtime::Runtime` bakes its `PermissionGate`
    // in at construction and exposes no later swap point (confirmed by
    // reading `RuntimeDeps`/`Runtime::new`), so a gate built *after*
    // `Conway` already exists -- which is what every dispatch target other
    // than `tui`/`print` still does -- can never see a live tool call.
    // `tui` and one-shot (`print`) are the two targets whose own binding
    // notes require them to supply their own gate (CARRIED F-100-1: "the
    // TUI is the layer that wires the INTERACTIVE permission prompt
    // handler"; one-shot's own notes: "-p one-shot uses an ALLOW-LIST
    // gate ... this is the layer that wires the gate for non-interactive
    // use"), so each one's gate has to be built here, before the single
    // `build_conway` call below. This is also precisely the gap
    // `ConwayBuilder::build`'s own module doc flags as missing ("No
    // `with_prompt_handler` method exists ... flagged as a gap in this
    // item's own public surface -- the CLI or a future item should likely
    // add a way to supply a prompt handler") and that
    // `tests/cli_surface.rs`'s `MINIMAL_CONFIG` comment attributes to
    // "WI-112/114" by name. Every other dispatch target's behavior here is
    // byte-for-byte unchanged (`gate_override`/`tui_gate_rx` are both
    // `None`).
    let is_tui = cli.command.is_none() && cli.print.is_none();
    let (gate_override, tui_gate_rx): (
        Option<Arc<dyn PermissionGate>>,
        Option<tui::gate::GateReceiver>,
    ) = if is_tui {
        let (gate, rx) = tui::gate::TuiGate::channel();
        (Some(Arc::new(gate)), Some(rx))
    } else if cli.command.is_none() && cli.print.is_some() {
        let gate: Arc<dyn PermissionGate> = Arc::new(oneshot::build_gate(&cli));
        (Some(gate), None)
    } else {
        (None, None)
    };

    let conway = match build_conway(&cli, gate_override, is_tui) {
        Ok(conway) => conway,
        Err(e) => {
            diag::error(e.to_string());
            return to_process_code(ExitCode::from_error(&e));
        }
    };

    // Board item 01KZ803DJW8Y1H4FXTM8D3PYMY: `Conway::warnings()` (a real,
    // populated mechanism -- `config::merge::validate` pushes a
    // `WarningCode::HeadroomExceedsContext` when a role's effective headroom is
    // `>=` the smallest context window reachable through its chain) had zero
    // callers workspace-wide before this. Every non-interactive target
    // (`sessions`, `routes`, one-shot `-p`) shares this one choke point, so a
    // misconfiguration is visible on stderr no matter which of those the
    // operator runs -- exactly the same "one seam, every dispatch target" shape
    // `build_conway`'s own `[plugins].install` note already relies on. The
    // interactive TUI does NOT print here: once `tui::run` puts the terminal in
    // raw/alternate-screen mode, a stray stderr write lands on top of the drawn
    // UI rather than in the operator's scrollback, so the TUI instead renders
    // the same warnings into its own transcript (`tui::app::App::new`) where
    // they stay visible for the life of the session -- the two interactive
    // surfaces the every-mode-reachable rule asks about, each getting the shape
    // that actually reaches its user, not one carve-out covering only the
    // easier surface.
    if !is_tui {
        for warning in conway.warnings() {
            diag::warn(&warning.message);
        }
    }

    let result = dispatch(&cli, conway, tui_gate_rx).await;

    match result {
        Ok(code) => to_process_code(code),
        Err(e) => {
            diag::error(e.to_string());
            to_process_code(ExitCode::from_error(&e))
        }
    }
}

/// Config is loaded through `ConwayBuilder::discover()` when `--config` is
/// absent, `from_config(path)` when present (module notes). `gate`, when
/// `Some`, overrides `permissions`-derived gate selection via
/// `ConwayBuilder::with_permission_gate` -- see this file's `main` for why
/// (WI-114 reconciliation) and which dispatch targets ever pass one.
///
/// `--root` (board item 01KYTMH9JX21CGSE2Y6E2KP8SJ) is applied here, via
/// `ConwayBuilder::with_root`, whenever `cli.root` is `Some` -- absent, this
/// `Conway`'s root agent (and every session it starts) stays `Unconfined`,
/// exactly as before this flag existed. Resolved relative to the process's
/// OWN working directory, same as `--cwd` immediately above in `main`
/// (`std::env::set_current_dir` has already run by the time this is
/// called), NOT re-resolved against `--cwd`'s value again here -- clap
/// itself never joins two path flags together, and neither does this
/// function.
///
/// `is_tui` (bash opt-in board item: bash ships on by default and cannot be
/// declined) selects which built-in plugins `build()` registers.
/// Every non-interactive CLI target (`sessions`, `routes`, one-shot `-p`)
/// keeps this crate's pre-item behavior UNCHANGED -- every built-in,
/// `conway.shell`/bash included, is always registered
/// (`PluginSelection::All`) exactly as `ConwayBuilder::build`'s own
/// pre-item default did; one-shot's `--allowed-tools` allow-list (default:
/// empty -- see `oneshot::build_gate`) is, and always was, the thing that
/// actually keeps bash from running unattended, not registration. The
/// interactive TUI is the one CLI target that now defers to the
/// config-derived selection instead (`ConwayConfig::tools.builtin_plugins`,
/// default: every built-in except bash) -- an operator turns bash on for
/// the TUI by adding `"conway.shell"` to that `settings.json` array (see
/// `docs/interactive.md`).
fn build_conway(
    cli: &Cli,
    gate: Option<Arc<dyn PermissionGate>>,
    is_tui: bool,
) -> conway::Result<Conway> {
    let builder = match &cli.config {
        Some(path) => ConwayBuilder::from_config(path)?,
        None => ConwayBuilder::discover()?,
    };
    // Board item 01KZVTTP492R3BDY33FAGYWDNW: unconditional, not gated on
    // whether the loaded config's `[hooks].rules` is non-empty -- injecting
    // a runner that never gets consulted (no `pre_tool_use` rules present)
    // costs nothing, and it is one fewer branch than checking first. Without
    // this call a `pre_tool_use` rule in an operator's `settings.json`
    // parses, validates, and is silently never dispatched -- see
    // `conway::config::schema::HooksConfig`'s own doc for the full
    // disclosure this call resolves.
    let builder = builder.with_default_hook_runner();
    let builder = match gate {
        Some(gate) => builder.with_permission_gate(gate),
        None => builder,
    };
    let builder = match &cli.root {
        Some(root) => builder.with_root(root.clone()),
        None => builder,
    };
    let builder = if is_tui {
        builder
    } else {
        builder.with_builtin_plugins(conway::PluginSelection::All)
    };
    // First-party plugin tier (board item 01KZDC3JQ7W4DY1MG6MBCVB2DV):
    // `[plugins].install` names ids against the small bundle this binary
    // links (`first_party_plugins::bundle`) -- every dispatch target
    // (TUI, one-shot `-p`, `sessions`, `routes`) shares this single
    // `build_conway` choke point, so all four see the same installed set
    // from the same config, with no target-specific carve-out the way
    // `is_tui`'s built-in selection above has one.
    //
    // Board item 01KZHF270T3W8GZ7NM6DSNQ4MM: `wanted` is not `[plugins].
    // install` alone -- `first_party_plugins::wanted_ids` unions it with
    // `[plugins].default_backends` (default: both provider-adapter dialect
    // kind ids) BEFORE `install` resolves anything, which is what makes a
    // default install reach a model with no `[plugins]` section in
    // `settings.json` at all. Every dispatch target sees this union from
    // the SAME choke point, so the property holds for the TUI and every
    // one-shot/subcommand invocation identically.
    let plugins_config = &builder.config().plugins;
    let wanted =
        first_party_plugins::wanted_ids(&plugins_config.install, &plugins_config.default_backends);
    let builder = first_party_plugins::install(builder, &wanted)?;
    builder.build()
}

/// If `command.is_some()` -> `commands::{sessions,routes}::run`; else if
/// `print.is_some()` -> `oneshot::run`; else -> `tui::run` (module notes).
/// `tui_gate_rx` is `Some` exactly when the `None` (tui) arm below is the
/// one taken -- see `main`'s comment.
async fn dispatch(
    cli: &Cli,
    conway: Conway,
    tui_gate_rx: Option<tui::gate::GateReceiver>,
) -> conway::Result<ExitCode> {
    match &cli.command {
        Some(Command::Sessions(args)) => commands::sessions::run(args, &conway).await,
        Some(Command::Routes(args)) => commands::routes::run(args, &conway).await,
        None if cli.print.is_some() => oneshot::run(cli, conway).await,
        None => {
            let gate_rx = tui_gate_rx.expect("tui_gate_rx is constructed whenever is_tui is true");
            tui::run(cli, conway, gate_rx).await
        }
    }
}

/// Installs a `tracing_subscriber::fmt` subscriber to stderr when `-v`/`-vv`
/// is passed (WI-121). Without this, `diag::set_verbosity` had no effect on
/// the `tracing::{trace,info,warn,error}!` calls already scattered through
/// conway-runtime/-backends/-session/conway (agent-loop failures, backend
/// health-breaker trips, probe warnings): nothing installs a subscriber, so
/// those events went nowhere and `-vv` produced no output at all.
///
/// `-v` surfaces `info` and above (routing/health notices); `-vv` (or more)
/// also surfaces `trace`. Scoped to this workspace's own crates so `-v`
/// doesn't also turn on dependency-level tracing noise (reqwest/hyper/tokio
/// emit nothing here today, but nothing stops a future dependency from
/// wiring in `tracing`). `RUST_LOG`, when set, overrides this entirely.
///
/// Writes to stderr only, via `with_writer` -- `-p` one-shot mode's stdout
/// purity contract (stdout carries only model output) must hold regardless
/// of verbosity.
fn install_tracing(verbose: u8) {
    if verbose == 0 {
        return;
    }
    let level = if verbose >= 2 { "trace" } else { "info" };
    let directive = format!(
        "warn,conway={level},conway_core={level},conway_plugin_backends={level},\
         conway_plugin_routing={level},conway_tools={level},conway_session={level},\
         conway_runtime={level},conway_cli={level}"
    );
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(directive));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn to_process_code(code: ExitCode) -> std::process::ExitCode {
    std::process::ExitCode::from(code.code() as u8)
}

#[cfg(test)]
mod tracing_target_tests {
    //! Board item 01KZFC43J1J06BM4CCWKCKHSNV names `install_tracing`'s
    //! per-crate target list as a place a rename can silently stop matching
    //! with no compile error (it's a plain `&str`, not a dependency): this
    //! module verifies `conway_plugin_routing` is actually a live,
    //! filter-matched target by observing REAL `tracing_subscriber::
    //! EnvFilter` behavior -- constructing the exact directive
    //! `install_tracing` builds and checking what it lets through -- rather
    //! than asserting on the literal source string.
    //!
    //! **Finding, disclosed rather than hidden:** `EnvFilter`'s per-target
    //! directive matching is an unanchored string prefix, not a `::`-
    //! segment-boundary match (a documented `tracing-subscriber` quirk).
    //! Every crate name in this list literally starts with `"conway"`, so
    //! the very first clause, `conway={level}`, already prefix-matches
    //! `conway_plugin_routing`, `conway_plugin_backends`, `conway_cli`, and (had it
    //! never been renamed) the pre-rename crate name too -- an event on
    //! that OLD target (the `conway-routing` crate's module-path spelling,
    //! not written out verbatim here since this crate no longer exists --
    //! see `.design/philosophy-debt.md` entry 5) was verified, empirically,
    //! to still pass this exact directive string even with every trace of
    //! that name gone from the source. The specific `conway_plugin_routing={level}`
    //! clause this item's spec worried would "silently stop matching after
    //! the rename, with NO compile error" is therefore, in practice,
    //! redundant with the catch-all `conway={level}` clause that already
    //! precedes it -- the concrete risk named never actually materializes
    //! for THIS crate list, because every name in it shares that one
    //! prefix. The tests below still verify the real, user-facing property
    //! that matters (`-v`/`-vv` genuinely surface `conway_plugin_routing`
    //! events, at the right level, and genuinely exclude a
    //! non-`conway`-prefixed dependency crate) rather than a claim about
    //! the specific clause that the mechanism does not actually support.
    //!
    //! `install_tracing` itself calls `.init()`, which installs a
    //! process-global subscriber exactly once; calling it twice across
    //! multiple tests in one process would panic. These tests therefore
    //! reconstruct the same directive-building logic against a SCOPED
    //! subscriber (`tracing::subscriber::with_default`, not `.init()`), so
    //! each test gets its own filter without touching global state -- never
    //! calling `install_tracing` itself.

    use std::sync::{Arc, Mutex};

    /// Byte-for-byte the directive `install_tracing` constructs, kept in
    /// sync deliberately (not `pub(crate)`-shared, since `install_tracing`
    /// is a four-line function with no other caller): a drift between the
    /// two would be undetectable by Rust's own compiler (this is exactly
    /// the "plain string, not a dependency" risk this module exists to
    /// guard against some OTHER way), so every test below runs against
    /// THIS copy and is only as trustworthy as this copy staying identical
    /// to `install_tracing`'s own `format!` call -- both are four lines,
    /// side by side in this same file, for a reviewer to diff by eye.
    fn directive(verbose: u8) -> String {
        let level = if verbose >= 2 { "trace" } else { "info" };
        format!(
            "warn,conway={level},conway_core={level},conway_plugin_backends={level},\
             conway_plugin_routing={level},conway_tools={level},conway_session={level},\
             conway_runtime={level},conway_cli={level}"
        )
    }

    #[derive(Clone, Default)]
    struct CapturingWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for CapturingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("capture lock").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturingWriter {
        type Writer = CapturingWriter;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn captured_output(verbose: u8, run: impl FnOnce()) -> String {
        let writer = CapturingWriter::default();
        let filter = tracing_subscriber::EnvFilter::new(directive(verbose));
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(writer.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, run);
        let bytes = writer.0.lock().expect("capture lock").clone();
        String::from_utf8(bytes).expect("tracing output is utf8")
    }

    /// `-v` (verbose=1, "info" level): an `info!` event targeting
    /// `conway_plugin_routing` -- the crate's real module path once
    /// installed as a router plugin -- is actually let through the filter
    /// `install_tracing` builds, all the way to the writer. Observed,
    /// real filtered output, not a read of the source string.
    #[test]
    fn dash_v_lets_an_info_event_from_conway_plugin_routing_through() {
        let output = captured_output(1, || {
            tracing::info!(target: "conway_plugin_routing", "marker-info-conway-plugin-routing");
        });
        assert!(
            output.contains("marker-info-conway-plugin-routing"),
            "an info! event targeting conway_plugin_routing must pass `-v`'s filter (built from \
             install_tracing's own directive string), got captured output: {output:?}"
        );
    }

    /// The exclusion half of the same property (the doc comment's own
    /// stated purpose: "`-v` doesn't also turn on dependency-level tracing
    /// noise"): a target that does NOT start with `"conway"` at all --
    /// unlike every name actually listed, see this module's own top-of-file
    /// finding -- is filtered by the trailing catch-all `warn` clause, at
    /// the SAME info level this fixture would otherwise easily clear.
    #[test]
    fn dash_v_excludes_a_non_conway_prefixed_dependency_target() {
        let output = captured_output(1, || {
            tracing::info!(target: "reqwest", "marker-info-dependency-noise");
        });
        assert!(
            !output.contains("marker-info-dependency-noise"),
            "an info! event on a non-conway-prefixed target must be filtered out by the \
             trailing `warn` catch-all, got captured output: {output:?}"
        );
    }

    /// `-v` (verbose=1, "info" level) does NOT let a `trace!`-level event on
    /// `conway_plugin_routing` through -- proves the filter's LEVEL, not
    /// just its target list, is real: a directive string with the crate
    /// name spelled right but no level suffix (or the wrong syntax) would
    /// either fail to parse (falling back to `warn` only) or match every
    /// level, and either failure mode would flip this assertion.
    #[test]
    fn dash_v_does_not_let_a_trace_event_from_conway_plugin_routing_through() {
        let output = captured_output(1, || {
            tracing::trace!(target: "conway_plugin_routing", "marker-trace-conway-plugin-routing");
        });
        assert!(
            !output.contains("marker-trace-conway-plugin-routing"),
            "a trace! event targeting conway_plugin_routing must NOT pass `-v`'s (info-level) \
             filter, got captured output: {output:?}"
        );
    }

    /// `-vv` (verbose=2, "trace" level) DOES let that same trace! event
    /// through -- the positive control proving the assertion above can
    /// fail, and that `-vv` genuinely raises the level for this target.
    #[test]
    fn dash_vv_lets_a_trace_event_from_conway_plugin_routing_through() {
        let output = captured_output(2, || {
            tracing::trace!(target: "conway_plugin_routing", "marker-trace-conway-plugin-routing-vv");
        });
        assert!(
            output.contains("marker-trace-conway-plugin-routing-vv"),
            "a trace! event targeting conway_plugin_routing must pass `-vv`'s (trace-level) \
             filter, got captured output: {output:?}"
        );
    }
}
