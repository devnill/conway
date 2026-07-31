//! The complete `conway` clap command surface (WI-111).
//!
//! Declared once, here, for the whole module: every later work item
//! (WI-112..WI-117) reads flags off this `Cli`/`Command` but never adds a
//! new field to them, so the four downstream tracks (one-shot, TUI,
//! subcommands, continuity flags) can proceed without contending for this
//! file. `--session`/`--resume`/`--fork-from` are declared here (so
//! `--help` is complete from the first commit) even though they are only
//! wired up behaviorally by WI-117 -- their `conflicts_with_all` triple is
//! also set up now, since that is the one piece of their contract that
//! belongs to the flag *declaration* rather than the flag's runtime effect.

use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand, ValueEnum};

use crate::commands::routes::RoutesArgs;
use crate::commands::sessions::SessionsArgs;

#[derive(Parser, Debug)]
#[command(name = "conway", version, about = "The conway agent harness CLI")]
pub struct Cli {
    /// One-shot mode: run `PROMPT` (or, with no value, read the prompt from
    /// stdin) and exit. Absent entirely => interactive TUI.
    #[arg(
        short = 'p',
        long = "print",
        value_name = "PROMPT",
        num_args = 0..=1,
        default_missing_value = ""
    )]
    pub print: Option<String>,

    #[arg(long, value_enum, default_value = "text")]
    pub output_format: OutputFormat,

    #[arg(long, value_delimiter = ',')]
    pub allowed_tools: Vec<String>,

    #[arg(long, value_delimiter = ',')]
    pub deny_tools: Vec<String>,

    #[arg(long, value_enum, default_value = "allowlist")]
    pub permission_mode: PermissionMode,

    #[arg(long)]
    pub role_override: Option<String>,

    #[arg(long)]
    pub model: Option<String>,

    /// Use (creating if new) a specific session id.
    #[arg(long, conflicts_with_all = ["resume", "fork_from"])]
    pub session: Option<String>,

    /// Reattach to a persisted session and continue its transcript.
    #[arg(long, conflicts_with_all = ["session", "fork_from"])]
    pub resume: Option<String>,

    /// Branch a new session from `<session-id>[@<seq>]`.
    #[arg(long, value_name = "SID[@SEQ]", conflicts_with_all = ["session", "resume"])]
    pub fork_from: Option<String>,

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
/// never prompts (WI-112 notes), so this CLI-facing enum only has the two
/// variants a non-interactive run can actually use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum PermissionMode {
    Allowlist,
    Deny,
}
