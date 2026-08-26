//! `conway-plugin-statusline` (board item `01M0X500861X9035QJEA82F94K`): a
//! first-party plugin that runs an operator-configured command on a
//! bounded refresh cadence and pushes its stdout as a
//! [`conway::plugin::PluginStatusContribution`] -- the migration home for a
//! Claude Code `statusLine.type`/`statusLine.command` pair, which conway's
//! own status line (a closed ten-variant vocabulary,
//! `crates/conway-cli/src/tui/view/status.rs`) cannot express by design.
//!
//! **The operator ruling this item implements (2026-08-25):** "settings
//! migrate only where they fit conway's philosophy; a plugin is the right
//! home where one exists." A shell-out status line is exactly that case --
//! opinionated, spawns a process on a UI cadence, and unarguably not core.
//!
//! # Why this crate is worth more than the migration
//!
//! It is the **second consumer of the status-contribution seam, and the
//! first that is not a permission guard** -- `docs/vision/
//! DESIGN-plugin-dependencies.md` §8's third falsifier warns that if one
//! provider and one consumer are all that ever appear, the seam was
//! overbuilt. It also exercises the **push** half
//! ([`conway::plugin::PluginStatusContribution`]) rather than the pull
//! half, which that design's §7c flags as an open question: does one
//! mechanism serve both push and pull?
//!
//! **The concrete answer this crate found, stated here because it is the
//! most load-bearing thing in this crate:** see
//! [`StatusLinePlugin`]'s own [`Plugin::status_contributions`] impl, below,
//! for the exact mechanism, and this module's "What this proves, and what
//! it does not" section for the full argument. In short: the
//! WIRE half of the push mechanism (a plugin computing a fresh value on its
//! own cadence, non-blockingly readable at any time) works and is proven
//! end to end by this crate's own tests. The HOST half does not, YET, read
//! it more than once -- `conway::Conway::plugin_status_contributions`'s own
//! doc states plainly that its stored snapshot is "collected at
//! session-open... and frozen thereafter for the life of the process,"
//! and that "a genuinely live per-session poll... is a separate and larger
//! piece of work, deliberately not built by this wiring." A plugin that
//! refreshes every five seconds is therefore invisible to the TUI after
//! its first (and only) read -- which, depending on how much work
//! `ConwayBuilder::build` does between constructing this plugin and
//! reading its snapshot, may not even have completed by the time it is
//! read. **This is the concrete finding for §7c**: `PluginStatusContribution`
//! the TYPE is expressive enough for this use case (it already has
//! everything a status-line command produces: a value, a
//! success/failure verdict, a reason string on failure); what is missing is
//! entirely on the HOST side -- a live per-session poll, or the pull half
//! §7c already names as the alternative. The type did not need to grow a
//! field; the host needs to grow a poll loop.
//!
//! # Cadence -- the hazard this crate exists to get right
//!
//! A status line redraws constantly; spawning a process per redraw is
//! unacceptable. This crate DECOUPLES process-spawn cadence from render
//! cadence entirely: [`StatusLinePlugin::new`] starts one background task
//! (mirroring the forwarding-task shape `ConwayBuilder::build` already uses
//! for `Plugin::observe_sink`, `crates/conway/src/builder.rs`) that loops
//! run-then-sleep for the life of the plugin, regardless of how many times
//! -- zero, one, or (once a future live poll exists) many -- anything calls
//! [`Plugin::status_contributions`]. Reading the status is always an
//! `Arc<Mutex<_>>` lock-and-clone against the LATEST cached result; it never
//! spawns anything itself.
//!
//! **Worst-case process spawns per minute: 60.** [`MIN_REFRESH_INTERVAL_MS`]
//! (1000ms) is an enforced floor -- [`StatusLineSpec::clamped_refresh_interval_ms`]
//! is the only path the background loop's sleep duration is computed
//! through, so no operator-configured value, however small, can push the
//! cadence past one spawn per second. The default
//! ([`DEFAULT_REFRESH_INTERVAL_MS`], 5000ms) is 12 spawns/minute. This bound
//! holds independent of the configured command's own running time: a run
//! that takes `timeout_ms` to fail is followed by a full `sleep`, not a
//! shorter one, so a slow OR hung command can only ever make the cadence
//! SLOWER than the floor, never faster. See
//! `background_loop_never_starts_a_run_before_the_previous_ones_full_cycle_elapsed`
//! (this crate's own test suite) for the timing proof.
//!
//! # A slow command must never stall the UI
//!
//! **Posture reused from `crates/conway-core/src/ports/plugin.rs`,
//! [`Plugin::observe_sink`]'s own doc**, the identical "lossy-with-notice"
//! discipline: *"a slow plugin falls behind... The sink itself pushes to a
//! bounded queue and drops+warns on overflow, so the host turn NEVER
//! blocks on a slow plugin read loop."* This crate applies the same shape
//! to a synchronous read instead of a queue: `run_once` bounds a single
//! command with `tokio::time::timeout(spec.timeout_ms, ...)` and the
//! spawned child is `kill_on_drop(true)`, so a timed-out run's process is
//! reclaimed rather than orphaned; [`Plugin::status_contributions`] itself
//! never touches the subprocess machinery at all -- it only ever reads the
//! last value the background task finished computing, so a command that is
//! CURRENTLY mid-run (however slow) cannot make a caller of
//! `status_contributions()` wait even one extra millisecond. A 3-second
//! command shows a stale (or absent, on the very first run) value; it
//! never freezes anything reading this plugin.
//!
//! # A failing command must be visible
//!
//! `run_once` never returns an empty string on failure --
//! `ResultStatus::Failed { error }` names the concrete reason (spawn
//! failure, non-zero exit plus the first line of stderr, a timeout, or "no
//! output"), and the SAME text is also carried in
//! [`PluginStatusContribution::value`] so the status line's own
//! `key: value` rendering (`view/status.rs`'s `contributions_ladder`,
//! read-only from this crate's side) shows the reason inline, not merely a
//! failed badge with no explanation.
//!
//! # It must not displace the permission-mode field
//!
//! This crate produces ordinary [`conway::plugin::PluginStatusContribution`]
//! values through the ordinary `Plugin::status_contributions` surface --
//! the SAME data shape and the SAME host-side storage
//! (`AppState::plugin_status_contributions`) every other status-contributing
//! plugin uses. The non-displacement guarantee is therefore a property of
//! the RENDER path, not of this crate: `view/status.rs`'s `drop_priority`
//! ranks `StatusLineField::Contributions` (9) strictly below
//! `StatusLineField::Mode` (10) -- the give-up loop in `status_line_spans`
//! degrades every contribution to its own empty floor before `mode` is
//! ever asked to give up a single column -- and `mode_ladder`'s own top
//! rung never returns an empty `Vec`. That mechanism, and its existing
//! test `plugin_contributions_never_displace_the_forced_in_mode_field`
//! (`view/status.rs`), already cover every `PluginStatusContribution`
//! source uniformly, this crate's own included -- there is nothing for
//! this crate to add or override to preserve it, and nothing in this crate
//! reads or writes `view/status.rs` (out of scope for this item; read
//! only, per this item's own file-ownership fence).
//!
//! # Trust posture
//!
//! `[tui.status_line_command].command` runs an operator-named command with the
//! operator's own process privileges -- no sandboxing, no digest check, no
//! confirmation prompt. This is the IDENTICAL footing
//! `[hooks].rules[].command` already has
//! (`crates/conway/src/config/schema.rs`'s own `HookEntry::command` doc):
//! an operator who would not paste an unfamiliar command into `[hooks]`
//! should not paste one into `[tui.status_line_command].command` either. Stated
//! plainly in `docs/plugins/statusline.md`, not merely implied by this
//! doc comment.
//!
//! # What this proves, and what it does not
//!
//! Proven, end to end, by this crate's own tests: a configured command's
//! output reaches [`Plugin::status_contributions`]; a failing/absent
//! command reaches it as `Failed` with a legible reason, never an empty
//! string; a slow command never blocks a concurrent read; the background
//! loop's cadence is bounded regardless of configuration. NOT proven,
//! because it cannot be from inside this repository's parallel-wave lane
//! fence (this writer runs no `cargo build`/`test`/`clippy` -- a build lane
//! verifies the wave): that the real `conway` TUI binary, driven
//! interactively, shows a live-refreshing value on screen. Given the
//! one-shot-snapshot host behavior described above, driving the real
//! binary would show this plugin's value ONLY if its first background
//! refresh happens to complete before `ConwayBuilder::build` reads the
//! snapshot -- plausible for a fast command (`git branch`, `hostname`),
//! essentially never for anything slower. That gap is the concrete
//! evidence for the §7c finding above, not a defect this crate could fix
//! by itself -- fixing it means adding a live poll to the TUI's own render
//! loop (`crates/conway-cli/src/tui/`, out of this item's file ownership).

use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use conway::plugin::{
    Plugin, PluginDescription, PluginManifest, PluginStatusContribution, ResultStatus, Tool,
};

/// This plugin's manifest id (`Plugin::manifest().id`). **Not** an id an
/// operator names in `[plugins].install` -- unlike `first_party_plugins::
/// bundle()`'s ten candidates, this plugin has no closed-candidate-set
/// entry to opt into by naming an id at all: the ENTIRE opt-in is writing
/// a non-empty `[tui.status_line_command].command`
/// (`crates/conway/src/config/schema.rs`'s `StatusLineCommandConfig`,
/// resolved by `crates/conway-cli/src/statusline_plugin.rs`) -- see that
/// config field's own doc for the full argument. This constant exists for
/// the same reason every other plugin's `PLUGIN_ID` does: a stable
/// identity for `Plugin::manifest().id`, and for a future plugin-browser
/// row to key on.
pub const PLUGIN_ID: &str = "conway.statusline";

/// The lowest [`StatusLineSpec::refresh_interval_ms`] the background loop
/// will ever actually sleep for, regardless of configuration --
/// [`StatusLineSpec::clamped_refresh_interval_ms`] enforces this floor
/// unconditionally. `1000` (one spawn per second) is the hard ceiling on
/// this crate's own worst-case cadence: 60 process spawns per minute, no
/// matter what an operator writes into `settings.json`. See this module's
/// own doc, "Cadence", for the full worst-case argument.
pub const MIN_REFRESH_INTERVAL_MS: u64 = 1000;

/// The default refresh interval when `[tui.status_line_command]` sets none: 12
/// spawns/minute, chosen the same way `crates/conway/src/config/schema.rs`'s
/// `default_hook_timeout_ms` (5000ms) was -- long enough that an
/// operator's status line does not become the dominant source of process
/// churn on their machine, short enough that a value like a git branch or a
/// clock reads as "current" rather than obviously frozen.
pub const DEFAULT_REFRESH_INTERVAL_MS: u64 = 5000;

/// The default per-run timeout when `[tui.status_line_command]` sets none.
/// Deliberately shorter than [`conway::plugin::DEFAULT_TIMEOUT_MS`]
/// (5000ms, the shared default for a hook callout / a subprocess plugin
/// call): those are one-shot, operator-INITIATED calls an agent turn is
/// already waiting on; this is a background probe nothing is waiting on,
/// running repeatedly, so a shorter ceiling keeps a hung command from
/// eating most of its own refresh interval before the next run even has a
/// chance to start.
pub const DEFAULT_TIMEOUT_MS: u64 = 2000;

/// The default [`PluginStatusContribution::key`] this plugin files its
/// result under when `[tui.status_line_command]` names none.
pub const DEFAULT_KEY: &str = "statusline";

/// This plugin's own configuration -- what `[tui.status_line_command]`
/// (`crates/conway/src/config/schema.rs`'s `StatusLineCommandConfig`)
/// converts into, at the one site that performs that conversion
/// (`crates/conway-cli/src/statusline_plugin.rs::install`), mirroring how
/// `conway_plugin_subprocess::SubprocessPluginSpec` is a plugin-crate-owned
/// type distinct from its own config-schema counterpart
/// (`crates/conway-cli/src/subprocess_plugins.rs`).
///
/// **Off by construction when [`Self::command`] is empty** (the [`Default`]
/// value, and the state of `[tui.status_line_command]` when an operator writes
/// nothing at all): [`StatusLinePlugin::new`] starts no background task at
/// all in that case, so `crates/conway-cli/src/statusline_plugin.rs::install`
/// does not even attach the plugin -- zero process spawns, zero
/// contributions, zero `with_plugin` calls, matching the "nothing in this
/// tier runs unasked" rule every `[plugins]` sub-config in
/// `crates/conway/src/config/schema.rs` already states for itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusLineSpec {
    /// The command to run, argv-shaped (program, then its arguments) --
    /// never a single shell string, the same shape and reasoning
    /// `crates/conway/src/config/schema.rs`'s `HookEntry::command` doc
    /// gives: no shell-quoting ambiguity in config. An operator migrating a
    /// Claude Code `statusLine.command` shell one-liner wraps it
    /// explicitly, e.g. `["sh", "-c", "git branch --show-current"]` --
    /// `docs/plugins/statusline.md` states this conversion.
    pub command: Vec<String>,
    /// The [`PluginStatusContribution::key`] this plugin's result is filed
    /// under -- lets an operator running more than one status-contributing
    /// plugin distinguish them on the rendered line.
    pub key: String,
    /// Milliseconds between the end of one run and the start of the next --
    /// see [`Self::clamped_refresh_interval_ms`] for the enforced floor.
    pub refresh_interval_ms: u64,
    /// Milliseconds a single run is allowed before `run_once` gives up on
    /// it and reports `Failed`. Independent of `refresh_interval_ms`: a run
    /// that hits this ceiling still waits out the FULL refresh interval
    /// before the next attempt (see this crate's own module doc, "Cadence"
    /// -- a hung command can only slow the loop down, never speed it up).
    pub timeout_ms: u64,
}

impl Default for StatusLineSpec {
    /// The all-off default: no command, so [`StatusLinePlugin::new`] starts
    /// no background task (see this type's own doc). The other three
    /// fields carry sensible values regardless, so a caller that sets only
    /// `command` gets a working spec with no further tuning required.
    fn default() -> Self {
        Self {
            command: Vec::new(),
            key: DEFAULT_KEY.to_string(),
            refresh_interval_ms: DEFAULT_REFRESH_INTERVAL_MS,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

impl StatusLineSpec {
    /// [`Self::refresh_interval_ms`], floored at [`MIN_REFRESH_INTERVAL_MS`]
    /// -- the ONLY place the background loop's sleep duration is computed,
    /// so no configured value, however small (including `0`), can push
    /// this plugin's worst-case cadence past one spawn per second. See this
    /// crate's own module doc, "Cadence", for the worst-case-per-minute
    /// arithmetic this floor makes provable rather than merely documented.
    pub fn clamped_refresh_interval_ms(&self) -> u64 {
        self.refresh_interval_ms.max(MIN_REFRESH_INTERVAL_MS)
    }
}

/// The plugin's own latest-result cache: `None` until the background loop's
/// first run completes (or forever, when [`StatusLineSpec::command`] is
/// empty and no loop was ever started), `Some(_)` thereafter, always
/// holding the MOST RECENT run's outcome regardless of how many runs have
/// happened since. A plain [`std::sync::Mutex`], not a channel: exactly one
/// writer (the background loop) and any number of readers
/// ([`Plugin::status_contributions`] callers) each want "the current
/// value", never a queue of past ones -- a channel would answer a question
/// nothing here asks.
type Shared = Arc<Mutex<Option<PluginStatusContribution>>>;

/// A plugin that runs [`StatusLineSpec::command`] on a bounded background
/// cadence and answers [`Plugin::status_contributions`] with whatever that
/// loop most recently produced -- see this crate's own module doc for the
/// full cadence/slow-command/trust argument this type exists to satisfy.
pub struct StatusLinePlugin {
    spec: StatusLineSpec,
    state: Shared,
}

impl StatusLinePlugin {
    /// Constructs the plugin and, when [`StatusLineSpec::command`] is
    /// non-empty, starts its background refresh loop immediately --
    /// mirroring `ConwayBuilder::build`'s own observe-sink forwarding-task
    /// shape (`crates/conway/src/builder.rs`): spawned on the CURRENT tokio
    /// runtime if one is reachable (`tokio::runtime::Handle::try_current`),
    /// silently started as "never refreshes" if this constructor is called
    /// outside one -- the identical "advertising a point means the host
    /// speaks it, not that the host requires it" degrade
    /// `crates/conway-core/src/ports/plugin.rs`'s own `Plugin::observe_sink`
    /// doc describes for its own forwarding task, applied here to a plugin
    /// author's OWN background work rather than the host's. This makes
    /// `StatusLinePlugin::new` safe to call from a synchronous, no-runtime
    /// context too (a library embedder constructing plugins before ever
    /// entering `#[tokio::main]`, or any other read-only candidate scan
    /// that only inspects `Plugin::manifest`/`Plugin::description`) -- it
    /// degrades to inert there rather than panicking (`tokio::spawn`
    /// outside a runtime panics; this constructor never calls it in that
    /// case).
    pub fn new(spec: StatusLineSpec) -> Self {
        let state: Shared = Arc::new(Mutex::new(None));
        if !spec.command.is_empty() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let loop_spec = spec.clone();
                let loop_state = state.clone();
                handle.spawn(refresh_loop(loop_spec, loop_state));
            }
        }
        Self { spec, state }
    }

    /// This plugin's own configuration, as constructed -- an introspection
    /// seam for an embedder or a future plugin-browser row that wants to
    /// show what this plugin is actually configured to run (the command,
    /// the effective floored cadence via
    /// [`StatusLineSpec::clamped_refresh_interval_ms`]), without threading
    /// a second copy of the spec through separately.
    pub fn spec(&self) -> &StatusLineSpec {
        &self.spec
    }
}

impl Plugin for StatusLinePlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PLUGIN_ID.to_string(),
            // Versioned WITH the workspace -- see this crate's own
            // Cargo.toml doc comment.
            version: env!("CARGO_PKG_VERSION").to_string(),
            tools: vec![],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    /// Contributes no tool -- this plugin's whole surface is
    /// [`Plugin::status_contributions`].
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![]
    }

    fn description(&self) -> PluginDescription {
        PluginDescription {
            summary: "runs an operator-configured command on a bounded refresh cadence and \
                      shows its output on the status line"
                .to_string(),
            you_get: "the status line's `plugins` field shows this command's latest output, or \
                      a visible `failed` badge with a legible reason when it errors or times \
                      out"
            .to_string(),
            you_lose: "nothing -- absent a `[tui.status_line_command].command`, this plugin is \
                       installed but completely inert"
                .to_string(),
            costs: "one operator-named command, run with the operator's own privileges on the \
                    SAME trust footing as [hooks].rules[].command -- no sandboxing, no digest \
                    check -- at most one spawn per second (see this crate's own module doc, \
                    \"Cadence\")"
                .to_string(),
        }
    }

    /// A point-in-time read of the background loop's latest result --
    /// never spawns, never blocks on the subprocess, returns instantly (a
    /// single `Mutex::lock`). See this crate's own module doc, "A slow
    /// command must never stall the UI", for the posture this mirrors and
    /// where it was reused from. Empty (`vec![]`) until the first run
    /// completes, or forever when no command is configured (matching the
    /// default `Vec::new()` [`Plugin::status_contributions`] itself
    /// returns) -- the exact "typically empty at real session start"
    /// framing `conway::Conway::plugin_status_contributions`'s own doc
    /// already establishes for the host-side snapshot this feeds.
    fn status_contributions(&self) -> Vec<PluginStatusContribution> {
        match self.state.lock() {
            Ok(guard) => guard.clone().into_iter().collect(),
            // A poisoned lock (the background task panicked mid-write) is
            // treated the same as "nothing yet" rather than propagating the
            // panic to a caller that has no way to react to it --
            // `Plugin::status_contributions` has no `Result` in its
            // signature, and an observer-class read degrading to empty on
            // an internal fault is the same posture every other
            // zero-cost-default method in this trait already takes for its
            // own absent-data case.
            Err(_) => Vec::new(),
        }
    }
}

/// The background loop [`StatusLinePlugin::new`] starts: run, publish,
/// sleep, repeat -- forever, for the life of the plugin (which is the life
/// of the process: `Arc<dyn Plugin>` is held by the host's
/// `PluginRegistry`/`RuntimeDeps` for the whole session, mirroring every
/// other forwarding task `ConwayBuilder::build` spawns alongside a
/// plugin). The sleep uses [`StatusLineSpec::clamped_refresh_interval_ms`],
/// not the raw configured value -- see this crate's own module doc,
/// "Cadence", for why this is the one enforcement point the worst-case
/// bound depends on.
async fn refresh_loop(spec: StatusLineSpec, state: Shared) {
    let sleep_for = Duration::from_millis(spec.clamped_refresh_interval_ms());
    loop {
        let contribution = run_once(&spec).await;
        if let Ok(mut guard) = state.lock() {
            *guard = Some(contribution);
        }
        tokio::time::sleep(sleep_for).await;
    }
}

/// Runs [`StatusLineSpec::command`] exactly once, bounded by
/// `timeout_ms`, and turns the outcome into one
/// [`PluginStatusContribution`] -- never an empty string on failure (this
/// crate's own module doc, "A failing command must be visible"). Returns
/// `Completed` with the first line of stdout, trimmed, on a zero exit with
/// non-empty output; `Failed` with a legible `error`/`value` in every other
/// case: spawn failure, a non-zero exit (carrying the exit code and the
/// first line of stderr when present), a timeout, an unreadable output
/// stream, or a zero exit with no output at all (silently rendering
/// nothing would be indistinguishable from "not yet refreshed", the exact
/// ambiguity `Failed` exists to avoid).
///
/// **Never called directly by [`Plugin::status_contributions`].** Only
/// `refresh_loop` calls this -- see `Shared`'s own doc for why the read
/// path and the run path are structurally separate.
async fn run_once(spec: &StatusLineSpec) -> PluginStatusContribution {
    let Some((program, args)) = spec.command.split_first() else {
        // `StatusLinePlugin::new` never starts this loop when `command` is
        // empty, so this arm is unreachable in practice; a defensive
        // `Failed` (rather than a panic) keeps that guarantee from being
        // load-bearing for this function's own correctness too.
        return failed(spec, "no command configured".to_string());
    };

    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Ensures a timed-out run's child is reclaimed rather than
        // orphaned: dropping the `tokio::time::timeout` future below on
        // expiry drops the `Child` too, and `kill_on_drop(true)` is what
        // makes that drop actually terminate the process instead of
        // merely closing this crate's handle to it. Known limitation,
        // disclosed rather than silently accepted: this kills the
        // immediate child only, not any grandchildren a shell command
        // spawns of its own accord (the heavier `conway::plugin::
        // kill_group`/`ChildSession` process-group machinery exists for
        // exactly that case, but is shaped for a persistent RPC child, not
        // a fire-and-forget probe -- see `docs/plugins/statusline.md`).
        .kill_on_drop(true);

    let child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return failed(spec, format!("failed to spawn {program:?}: {e}")),
    };

    let timeout = Duration::from_millis(spec.timeout_ms);
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Err(_) => failed(spec, format!("timed out after {}ms", spec.timeout_ms)),
        Ok(Err(e)) => failed(spec, format!("failed to read output: {e}")),
        Ok(Ok(output)) => {
            if !output.status.success() {
                let code = output
                    .status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".to_string());
                let stderr_line = first_line(&output.stderr);
                let reason = if stderr_line.is_empty() {
                    format!("exit {code}")
                } else {
                    format!("exit {code}: {stderr_line}")
                };
                return failed(spec, reason);
            }
            let line = first_line(&output.stdout);
            if line.is_empty() {
                return failed(spec, "produced no output".to_string());
            }
            PluginStatusContribution {
                key: spec.key.clone(),
                status: ResultStatus::Completed,
                value: line,
            }
        }
    }
}

/// The first non-empty-after-trim line of `bytes`, decoded lossily (a
/// status line has no business rejecting a command over invalid UTF-8 --
/// `String::from_utf8_lossy` degrades any invalid byte to `\u{FFFD}`
/// instead). A status line is one line; a multi-line command's output is
/// deliberately narrowed to its first line rather than joined or
/// truncated mid-word, matching `view/status.rs`'s own "every field is a
/// complete phrasing" discipline for whatever downstream renders this
/// plugin's `value`.
fn first_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Builds a `Failed` contribution carrying `reason` in both
/// `ResultStatus::Failed::error` and `PluginStatusContribution::value` --
/// the latter is what `view/status.rs`'s `contributions_ladder` actually
/// renders inline (`key: value`), so the failure reason is visible on the
/// status line itself, not only reachable by inspecting `status`.
fn failed(spec: &StatusLineSpec, reason: String) -> PluginStatusContribution {
    PluginStatusContribution {
        key: spec.key.clone(),
        status: ResultStatus::Failed {
            error: reason.clone(),
        },
        value: reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_id_matches_the_published_constant() {
        let plugin = StatusLinePlugin::new(StatusLineSpec::default());
        assert_eq!(plugin.manifest().id, PLUGIN_ID);
    }

    #[test]
    fn spec_accessor_returns_what_the_plugin_was_constructed_with() {
        let spec = StatusLineSpec {
            command: vec!["echo".to_string(), "hi".to_string()],
            key: "custom-key".to_string(),
            ..StatusLineSpec::default()
        };
        let expected = spec.clone();
        let plugin = StatusLinePlugin::new(spec);
        assert_eq!(plugin.spec(), &expected);
    }

    #[test]
    fn description_is_non_empty() {
        let plugin = StatusLinePlugin::new(StatusLineSpec::default());
        let description = plugin.description();
        assert!(!description.summary.is_empty());
        assert!(!description.you_get.is_empty());
        assert!(!description.you_lose.is_empty());
        assert!(!description.costs.is_empty());
    }

    #[test]
    fn contributes_no_tool() {
        let plugin = StatusLinePlugin::new(StatusLineSpec::default());
        assert!(plugin.tools().is_empty());
    }

    /// A default (no command) spec starts no background task -- proven
    /// negatively: even constructed INSIDE a running runtime (where
    /// `Handle::try_current()` would succeed), `status_contributions()`
    /// stays empty forever, because [`StatusLinePlugin::new`] never calls
    /// `handle.spawn` at all when `command` is empty.
    #[tokio::test]
    async fn an_unconfigured_plugin_never_spawns_and_stays_empty() {
        let plugin = StatusLinePlugin::new(StatusLineSpec::default());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            plugin.status_contributions().is_empty(),
            "a plugin with no configured command must contribute nothing, ever"
        );
    }

    /// Acceptance criterion 1 (this crate's own layer): a configured
    /// command's output reaches `Plugin::status_contributions()`, exactly
    /// once the background loop's first run has completed.
    #[tokio::test]
    async fn a_configured_commands_output_appears_in_status_contributions() {
        let spec = StatusLineSpec {
            command: vec!["sh".to_string(), "-c".to_string(), "echo hello".to_string()],
            refresh_interval_ms: MIN_REFRESH_INTERVAL_MS,
            ..StatusLineSpec::default()
        };
        let plugin = StatusLinePlugin::new(spec);

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let contributions = plugin.status_contributions();
            if !contributions.is_empty() {
                assert_eq!(contributions.len(), 1);
                assert_eq!(contributions[0].key, DEFAULT_KEY);
                assert_eq!(contributions[0].status, ResultStatus::Completed);
                assert_eq!(contributions[0].value, "hello");
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the background loop never produced a result within 5s"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Acceptance criterion 4: a failing command renders `Failed` with a
    /// legible reason, never a silent empty string.
    #[tokio::test]
    async fn a_nonzero_exit_renders_failed_with_a_legible_reason() {
        let spec = StatusLineSpec {
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo boom >&2; exit 3".to_string(),
            ],
            ..StatusLineSpec::default()
        };
        let contribution = run_once(&spec).await;
        assert_eq!(
            contribution.status,
            ResultStatus::Failed {
                error: "exit 3: boom".to_string()
            }
        );
        assert_eq!(contribution.value, "exit 3: boom");
    }

    /// The other half of acceptance criterion 4: a command that does not
    /// exist at all (never spawns) is `Failed`, not silently empty either.
    #[tokio::test]
    async fn a_missing_command_renders_failed_not_empty() {
        let spec = StatusLineSpec {
            command: vec!["conway-definitely-not-a-real-binary-xyz".to_string()],
            ..StatusLineSpec::default()
        };
        let contribution = run_once(&spec).await;
        match contribution.status {
            ResultStatus::Failed { ref error } => assert!(
                error.contains("failed to spawn"),
                "expected a spawn-failure reason, got: {error}"
            ),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(!contribution.value.is_empty());
    }

    /// A zero exit with no stdout at all is ALSO `Failed`, not `Completed`
    /// with an empty value -- an empty value would be visually
    /// indistinguishable from a healthy command that legitimately has
    /// nothing to say, which is the exact silent-success trap this item's
    /// own spec calls out by name.
    #[tokio::test]
    async fn a_successful_but_silent_command_is_still_failed_not_a_silent_completed() {
        let spec = StatusLineSpec {
            command: vec!["true".to_string()],
            ..StatusLineSpec::default()
        };
        let contribution = run_once(&spec).await;
        assert_ne!(contribution.status, ResultStatus::Completed);
        assert!(!contribution.value.is_empty());
    }

    /// Acceptance criterion 3, half one: a command that runs past its own
    /// `timeout_ms` is bounded -- `run_once` itself returns close to
    /// `timeout_ms`, never anywhere near the command's own full runtime.
    #[tokio::test]
    async fn a_slow_command_is_bounded_by_its_own_timeout_not_left_to_run() {
        let spec = StatusLineSpec {
            command: vec!["sh".to_string(), "-c".to_string(), "sleep 5".to_string()],
            timeout_ms: 100,
            ..StatusLineSpec::default()
        };
        let started = tokio::time::Instant::now();
        let contribution = run_once(&spec).await;
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "run_once must return close to its own timeout_ms (100ms), not wait out the \
             command's full 5s runtime: took {elapsed:?}"
        );
        match contribution.status {
            ResultStatus::Failed { ref error } => assert!(
                error.contains("timed out"),
                "expected a timeout reason, got: {error}"
            ),
            other => panic!("expected Failed (timeout), got {other:?}"),
        }
    }

    /// Acceptance criterion 3, half two -- the UI-facing half: while a slow
    /// command is genuinely in flight in the background loop,
    /// `status_contributions()` still returns instantly, reading only the
    /// cached value, never blocking on the in-flight run. This is the
    /// direct proof of this crate's own module doc's "A slow command must
    /// never stall the UI" claim.
    #[tokio::test]
    async fn status_contributions_never_blocks_while_a_slow_run_is_in_flight() {
        let spec = StatusLineSpec {
            command: vec!["sh".to_string(), "-c".to_string(), "sleep 5".to_string()],
            timeout_ms: 10_000,
            refresh_interval_ms: MIN_REFRESH_INTERVAL_MS,
            ..StatusLineSpec::default()
        };
        let plugin = StatusLinePlugin::new(spec);
        // Give the background loop time to actually start its (slow) run.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // A direct, synchronous call -- deliberately NOT wrapped in
        // `tokio::time::timeout`, which would only bound an async future
        // and say nothing about a call that blocks the calling thread
        // itself (exactly the failure mode this test exists to catch:
        // `status_contributions()` must never touch the subprocess
        // machinery at all). `started.elapsed()` is the real assertion.
        let started = tokio::time::Instant::now();
        let contributions = plugin.status_contributions();
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "status_contributions() took {:?} while a run was in flight -- it must be a \
             non-blocking cache read, not something that waits on the in-flight subprocess",
            started.elapsed()
        );
        // Nothing has completed yet (the run needs 5s, we only waited
        // 100ms), so the cache is still empty -- a stale/absent value, not
        // a stalled call.
        assert!(contributions.is_empty());
    }

    #[test]
    fn clamped_refresh_interval_floors_at_the_published_minimum() {
        let spec = StatusLineSpec {
            refresh_interval_ms: 1,
            ..StatusLineSpec::default()
        };
        assert_eq!(spec.clamped_refresh_interval_ms(), MIN_REFRESH_INTERVAL_MS);

        let spec = StatusLineSpec {
            refresh_interval_ms: 0,
            ..StatusLineSpec::default()
        };
        assert_eq!(spec.clamped_refresh_interval_ms(), MIN_REFRESH_INTERVAL_MS);

        let spec = StatusLineSpec {
            refresh_interval_ms: MIN_REFRESH_INTERVAL_MS * 10,
            ..StatusLineSpec::default()
        };
        assert_eq!(
            spec.clamped_refresh_interval_ms(),
            MIN_REFRESH_INTERVAL_MS * 10,
            "a value already at or above the floor is left unchanged"
        );
    }

    /// Acceptance criterion 2, the cadence bound itself, measured directly
    /// rather than inferred: each run appends one line to a scratch file
    /// (a real process spawn each time, going through the unmodified
    /// `run_once`/`refresh_loop` path), and after a fixed wall-clock
    /// window this test counts the lines. `MIN_REFRESH_INTERVAL_MS` (the
    /// floor every configured interval is clamped to) bounds how many
    /// spawns COULD have started in that window; a real spawn count at or
    /// under that bound, for a configured interval already AT the floor,
    /// is the executable form of this crate's own module doc's
    /// "60 spawns/minute worst case" claim.
    #[tokio::test(flavor = "multi_thread")]
    async fn background_loop_spawn_count_over_a_window_never_exceeds_the_floor_bound() {
        let path = std::env::temp_dir().join(format!(
            "conway-plugin-statusline-cadence-test-{}-{}.txt",
            std::process::id(),
            // A second disambiguator: two instances of this test could
            // theoretically race inside the same process across retries.
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        let spec = StatusLineSpec {
            command: vec![
                "sh".to_string(),
                "-c".to_string(),
                format!("echo x >> {}", path.display()),
            ],
            refresh_interval_ms: MIN_REFRESH_INTERVAL_MS,
            ..StatusLineSpec::default()
        };
        let _plugin = StatusLinePlugin::new(spec);

        // A window covering just under 3 floor-intervals: one spawn can
        // start immediately (t=0), then at most one more per elapsed
        // MIN_REFRESH_INTERVAL_MS -- so this window bounds the spawn count
        // at 3, never higher, however fast the machine running this test
        // is.
        let window = Duration::from_millis(MIN_REFRESH_INTERVAL_MS * 3 - 200);
        tokio::time::sleep(window).await;

        let spawn_count = std::fs::read_to_string(&path)
            .map(|s| s.lines().count())
            .unwrap_or(0);
        let _ = std::fs::remove_file(&path);

        assert!(
            spawn_count <= 3,
            "expected at most 3 spawns in a {window:?} window at the floor interval \
             ({MIN_REFRESH_INTERVAL_MS}ms), a violation of this crate's own documented \
             worst-case cadence bound (60/minute at the floor): got {spawn_count}"
        );
        assert!(
            spawn_count >= 1,
            "expected at least 1 spawn in a {window:?} window -- the loop must actually be \
             running, not merely bounded"
        );
    }
}
