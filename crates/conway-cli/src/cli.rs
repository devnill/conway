//! The complete `conway` clap command surface.
//!
//! Declared once, here, for the whole module: every later work item
//! (earlier work.. earlier work) reads flags off this `Cli`/`Command` but never adds a
//! new field to them, so the four downstream tracks (one-shot, TUI,
//! subcommands, continuity flags) can proceed without contending for this
//! file. `--session`/`--resume`/`--fork-from` are declared here (so
//! `--help` is complete from the first commit) even though they are only
//! wired up behaviorally by earlier work -- their `conflicts_with_all` triple is
//! also set up now, since that is the one piece of their contract that
//! belongs to the flag *declaration* rather than the flag's runtime effect.

use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand, ValueEnum};

use crate::commands::routes::RoutesArgs;
use crate::commands::sessions::SessionsArgs;

/// Adding a flag here? It does **not** reach a running `conway` through
/// `conway::config::merge::CliOverrides` — that struct is an embedder-facing
/// config-override API `conway-cli` deliberately does not construct
/// (a settled design decision; see that struct's own doc comment for why
/// routing this crate's flags through it would be actively breaking, not
/// merely unwired). This crate reads its own fields off `Cli` directly and
/// wires them by hand: `oneshot::build_gate`/`oneshot::resolve_session` for
/// one-shot mode, and the TUI's own equivalent construction path. A new flag
/// needs a matching read in whichever of those paths it's meant to affect.
#[derive(Parser, Debug)]
#[command(name = "conway", version, about = "The conway agent harness CLI")]
pub struct Cli {
    /// One-shot mode: run `PROMPT` and exit. With no value, the prompt is
    /// read from stdin instead. With a value AND piped (non-terminal)
    /// stdin, both are sent: `PROMPT` is the directive, the piped text is
    /// the data it operates on, joined directive-first -- see
    /// `oneshot::read_prompt`'s own doc for the exact precedence (`conway
    /// -p "what broke?" < error.log` sends the model both). Absent
    /// entirely => interactive TUI.
    #[arg(
        short = 'p',
        long = "print",
        value_name = "PROMPT",
        num_args = 0..=1,
        default_missing_value = ""
    )]
    pub print: Option<String>,

    /// How one-shot output is shaped: `text` for model output alone, `json`
    /// for one object at the end, `jsonl` for one event per line as it
    /// happens. Ignored by the TUI.
    #[arg(long, value_enum, default_value = "text")]
    pub output_format: OutputFormat,

    /// Tools the agent may call, by exact name, comma-separated. One-shot
    /// mode cannot ask an operator for permission, so it fails closed:
    /// leaving this empty denies every tool rather than allowing them all.
    #[arg(long, value_delimiter = ',')]
    pub allowed_tools: Vec<String>,

    /// Tools to refuse regardless of `--allowed-tools`, comma-separated.
    /// Denial always wins.
    #[arg(long, value_delimiter = ',')]
    pub deny_tools: Vec<String>,

    /// One-shot mode's tool gate: `allowlist` honours `--allowed-tools`,
    /// `deny` refuses every tool outright. There is no prompting variant
    /// here because a non-interactive run has nobody to ask.
    #[arg(long, value_enum, default_value = "allowlist")]
    pub permission_mode: PermissionMode,

    /// Run the root agent under this role instead of the configured
    /// `default_role`. A role is an alias resolved to a model chain by
    /// `roles.<alias>` in settings -- see `conway routes explain <role>`.
    #[arg(long)]
    pub role_override: Option<String>,

    /// Pin every turn to one model, bypassing the role's chain entirely.
    /// Spelled `<backend-id>/<model>`, matching what `routes explain` prints.
    #[arg(long)]
    pub model: Option<String>,

    /// Run as this named agent definition (`.conway/agents/<name>.md`,
    /// `conway::agents::load_agent_defs`) instead of the bare, no-persona
    /// default -- its `system_prompt`, `role`, `model`, and `tools`
    /// selector all apply, each still overridable by its own flag
    /// (`--role-override`, `--model`, `--system-prompt`/
    /// `--append-system-prompt`). An unknown name is a usage error naming
    /// the directory searched, not a silent no-op. Not supported with
    /// `--resume` (a resumed session's agent definition is fixed by the
    /// session it continues); combines cleanly with `--fork-from`.
    #[arg(long, value_name = "NAME")]
    pub agent: Option<String>,

    /// Replace the effective system prompt outright. With `--agent`
    /// absent, this is what stops a one-shot run from being the built-in
    /// coding agent at all: the run gets exactly this text (and no other
    /// framing) as its system prompt. Combine with `--append-system-prompt`
    /// to add more after it. Not supported with `--resume`/`--fork-from`
    /// (a usage error): a continued session's system prompt is fixed by
    /// the session it continues, not by this invocation.
    #[arg(long, value_name = "TEXT")]
    pub system_prompt: Option<String>,

    /// Append to the effective system prompt: `--agent`'s own (when
    /// `--system-prompt` is absent and `--agent` is given), `--system-
    /// prompt`'s text (when both are given), or -- with neither -- this
    /// becomes the entire system prompt by itself. Same `--resume`/
    /// `--fork-from` restriction as `--system-prompt`.
    #[arg(long, value_name = "TEXT")]
    pub append_system_prompt: Option<String>,

    /// Ceiling on agent turns (steps) this run may take before it is
    /// stopped with `ResultStatus::BudgetExceeded`. Absent: the configured
    /// `[limits].max_steps` (default 40). Not supported with `--resume`/
    /// `--fork-from` in this release (a usage error) -- neither facade path
    /// accepts a caller-supplied budget override yet.
    #[arg(long, value_name = "N")]
    pub max_turns: Option<u32>,

    /// Ceiling on total tokens spent this run may take. Absent: the
    /// configured `[limits].max_tokens` (`0` there means unlimited). Same
    /// `--resume`/`--fork-from` restriction as `--max-turns`.
    #[arg(long, value_name = "N")]
    pub max_tokens: Option<u32>,

    /// Wall-clock ceiling, in seconds, counted from the moment this run
    /// starts. Absent: the configured `[limits].deadline_secs` (`0` there
    /// means no deadline). Same `--resume`/`--fork-from` restriction as
    /// `--max-turns`.
    #[arg(long, value_name = "SECONDS")]
    pub max_seconds: Option<u64>,

    /// Use (creating if new) a specific session id.
    #[arg(long, conflicts_with_all = ["resume", "fork_from"])]
    pub session: Option<String>,

    /// Reattach to a persisted session and continue its transcript.
    #[arg(long, conflicts_with_all = ["session", "fork_from"])]
    pub resume: Option<String>,

    /// Branch a new session from `<session-id>[@<seq>]`.
    #[arg(long, value_name = "SID[@SEQ]", conflicts_with_all = ["session", "resume"])]
    pub fork_from: Option<String>,

    /// Settings file to use as the project layer, instead of discovering the
    /// nearest `.conway/settings.json` by walking up from `--cwd`. It does
    /// not replace your user-scoped settings, which still apply underneath
    /// (precedence: defaults, then user, then this, then env, then flags).
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Where the agent WORKS -- the process's (and the root agent's own)
    /// working directory. This is NOT a security boundary: it never limits
    /// what a tool call can reach, only where a relative path starts from.
    /// See `--root` for the setting that actually confines the agent.
    #[arg(long, value_name = "DIR")]
    pub cwd: Option<PathBuf>,

    /// Where the agent is ALLOWED TO REACH -- confines the root agent (and,
    /// by inheritance, every subagent it forks or spawns) to this directory:
    /// any tool call whose path argument resolves outside it is denied
    /// before the operator's permission gate is ever consulted. This IS the
    /// security boundary; `--cwd` is not one (see that flag's own help).
    /// Omitted (the default): the root agent is unconfined, exactly as
    /// every invocation before this flag existed.
    #[arg(long, value_name = "DIR")]
    pub root: Option<PathBuf>,

    #[arg(short, long, action = ArgAction::Count)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Inspect persisted sessions.
    Sessions(SessionsArgs),
    /// Inspect routing decisions.
    Routes(RoutesArgs),
    /// Anything that is not one of the built-in subcommands above falls
    /// through here instead of failing to parse -- clap's own
    /// `external_subcommand` idiom (the same shape `cargo` uses to dispatch
    /// `cargo foo` to a `cargo-foo` binary). `commands::plugin::run`
    /// resolves the first token against every installed plugin's own
    /// `conway::plugin::Plugin::commands()`, namespaced
    /// `<plugin-id>.<command-name>` -- the identical full-name scheme the
    /// TUI's `/`-prefixed dispatch already uses
    /// (`tui::commands::CommandRegistry::build`), reused rather than
    /// reinvented. An unresolved name is a usage error, not a silent
    /// no-op: this variant existing does not mean anything typed here is
    /// accepted, only that resolution is deferred to plugin lookup instead
    /// of clap's static set.
    #[command(external_subcommand)]
    External(Vec<String>),
}

/// `--output-format`: how one-shot mode renders the event stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
    Jsonl,
}

/// `--permission-mode`: how one-shot mode's tool gate is built. Distinct
/// from `conway::config`'s own `permissions.mode` (which additionally has a
/// `Prompt` variant meaningful only to the TUI/an embedder) -- one-shot mode
/// never prompts (notes), so this CLI-facing enum only has the two
/// variants a non-interactive run can actually use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum PermissionMode {
    Allowlist,
    Deny,
}
