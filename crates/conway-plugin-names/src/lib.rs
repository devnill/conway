//! `conway.names` -- operator-chosen, renameable names for agents (board
//! item `01M0TV5BSE98S16SFYECG9G9WP`, decision
//! `01M0TV3ZZBDKSSV7MD0FW3FSY7`).
//!
//! ## What this is for, in the operator's own words
//!
//! > "When working with agents, it's often useful to steer them
//! > individually. And in a large agentry, it's hard to tell which one is
//! > assigned which work... I find it very useful to be able to name a
//! > session so that I can quickly jump to it when I want to provide
//! > steering."
//!
//! A name is an **addressing affordance**, not a readability improvement.
//! It exists so `/steer scout check the merge` works -- you name the three
//! agents you are actually steering, never the forty in the tree. That is
//! why nothing here auto-generates a name for every spawn: the `/agents`
//! panel's own short id column (`conway_cli`'s `panel_agent_id`) already
//! solved uniqueness, and a screen full of invented tokens beside it would
//! be a trade, not an improvement.
//!
//! ## Why the trait lives HERE and not in `conway-core`
//!
//! The governing decision requires ZERO core change: core never learns the
//! word "name". `conway-cli` already depends on the first-party plugin
//! crates in order to install them, so [`AgentNames`] can be defined beside
//! its own implementation and named directly by the CLI. That keeps the
//! coupling between plugin and host a **compiled interface rather than a
//! file format** -- the distinction that separates this from the rejected
//! "plugin writes a file, the CLI reads it" shape. This tree already ran
//! that experiment: `conway_core::ports::memory_store`'s own module doc
//! records that a plugin marking sessions via `SessionMeta.labels`, with no
//! port and no compiler standing between the two readers, had to be
//! replaced by a typed port.
//!
//! A core `AnnotationStore` port was ALSO rejected, on §8.5: one consumer,
//! written by the port's author, in the same change. `MemoryStore` sits in
//! core because memory is a core-ish concern; naming is not.
//!
//! ## Where the store lives, and its key
//!
//! One flat JSON file, [`STORE_FILE_NAME`], in the SAME directory
//! `settings.json` and the TUI's input `history` file already live in --
//! `~/.conway/`, or `$CONWAY_CONFIG_DIR` when that is set (see
//! [`default_store_path`]). Names follow the operator, not the checkout,
//! for exactly the reason `conway::config::discovery::history_file_path`
//! gives for history.
//!
//! **Keyed by the bare `AgentId`, with no project or session partition.**
//! An `AgentId` is a ULID: unique across every project, every session, and
//! every process, forever. A per-project subdirectory (the shape
//! `conway::config::discovery::session_root` uses for session logs) would
//! therefore add a lookup key that carries no information the id does not
//! already determine. `conway_core::ports::memory_store`'s own "Scoping
//! (open question 1, decided): global, not per-project" argument is the
//! same one, made one level down, and it applies here unchanged: an
//! arbitrary CONTAINER boundary standing in for the thing that actually
//! varies. Session logs are partitioned by project because an operator
//! browsing them wants THIS checkout's history; nobody browses names --
//! they type one, and the resolver already narrows to the agents in the
//! live tree.
//!
//! **Disclosed cost.** A name persists across restart but does NOT travel
//! with a session file copied to another machine: making it travel needs a
//! record in the log, which means core, which the decision refused.
//!
//! ## Growth: entries persist until removed, and that is a decision
//!
//! Nothing here prunes. An entry survives the agent it names, and no TTL,
//! cap, or garbage sweep will remove it. Three reasons, stated rather than
//! left to be discovered:
//!
//! 1. This crate has no view of which agents still exist. It holds no
//!    `SessionStore`, and a `Command` cannot reach one (see
//!    `conway::plugin::Command`'s own doc). Any pruning rule it invented
//!    would be guessing.
//! 2. A finished agent is still addressable -- `/tree` lists terminal
//!    nodes, sessions are resumable -- so "the agent is gone" is not a fact
//!    this store could act on even if it knew it.
//! 3. Silently deleting an annotation the operator typed is worse than a
//!    file that grows by one short line per deliberate `/conway.names.
//!    rename`.
//!
//! What makes that honest rather than merely convenient is that removal is
//! first-class on day one ([`AgentNames::remove`], `/conway.names.unname`)
//! and the whole store is visible (`/conway.names.list`). The predecessor
//! this crate learns from -- the label-based memory curator -- had neither,
//! and its own port doc calls its bounded-by-construction cap "the growth
//! problem wearing a virtue's clothes". A cap here would be the same
//! mistake.
//!
//! ## Duplicate names are allowed; resolution reports the ambiguity
//!
//! [`AgentNames::set`] never refuses a name because another agent already
//! has it. Uniqueness is only meaningful among the agents on screen RIGHT
//! NOW, and this crate cannot see them -- refusing at write time would mean
//! `/conway.names.rename scout` failing because six months ago, in another
//! project, something else was called `scout`. The host's own
//! `resolve_agent` already knows how to report an ambiguous id prefix by
//! naming every candidate; a duplicate name takes the SAME path and the
//! same message shape, so there is one ambiguity story to learn, not two.
//! `/conway.names.rename` does say so at the time, as a non-fatal notice.
//!
//! ## What the host does, and what it does not
//!
//! `conway-cli` does three small things, each falling back to exactly
//! today's behaviour when this plugin is not installed: it holds the
//! [`AgentNames`] `Arc`, renders a name in the `/agents` panel where one
//! exists, and lets `resolve_agent` accept a name. Uninstalled, the store
//! is never opened, no row changes, and `resolve_agent` sees `None`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use conway::plugin::{
    async_trait, Command, CommandCtx, CommandOutcome, CommandSpec, Plugin, PluginDescription,
    PluginManifest,
};
use conway::AgentId;
use serde::{Deserialize, Serialize};

/// This plugin's manifest id: the string an operator names in
/// `[plugins].install` (`settings.json`).
pub const PLUGIN_ID: &str = "conway.names";

/// The bare name `RenameCommand` registers under -- reachable in the TUI as
/// `/conway.names.rename` (`conway_cli`'s `CommandRegistry::build` prefixes
/// a bare command name with its declaring plugin's manifest id).
pub const COMMAND_NAME_RENAME: &str = "rename";

/// The bare name `UnnameCommand` registers under -- `/conway.names.unname`.
pub const COMMAND_NAME_UNNAME: &str = "unname";

/// The bare name `ListCommand` registers under -- `/conway.names.list`.
pub const COMMAND_NAME_LIST: &str = "list";

/// The store file's name within the user config directory (module doc,
/// "Where the store lives, and its key").
pub const STORE_FILE_NAME: &str = "agent-names.json";

/// The longest name this plugin will store.
///
/// A display bound, not a storage cap: a name is rendered as ONE token in
/// an `/agents` panel row that already carries an indent, a status marker,
/// a short id, an `agent_def` label, and a recipe label. A 4000-character
/// name would not be a large name, it would be a broken row. Nothing about
/// the file format or the map cares how long a value is; this is the panel
/// speaking. (Contrast the module doc's refusal to cap the NUMBER of
/// entries -- that would be bounding growth, which is the mistake this
/// crate's predecessor made.)
pub const MAX_NAME_LEN: usize = 48;

/// Why a name was refused, or a store could not be opened or written.
#[derive(Debug, thiserror::Error)]
pub enum AgentNamesError {
    /// The store file could not be read, written, or replaced.
    #[error("agent-names store at {path}: {source}")]
    Io {
        /// The file involved.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// The store file exists but is not a document this crate wrote.
    ///
    /// Deliberately NOT "degrade to an empty store": the next
    /// [`AgentNames::set`] would then rewrite the file and destroy whatever
    /// was actually in it. See `conway_cli`'s `resolve_agent_names` for the
    /// fail-closed posture this drives, and `resolve_memory_store`'s own
    /// doc for the precedent it mirrors.
    #[error("agent-names store at {path} is not readable as this crate's format: {detail} -- fix or delete the file, or remove \"conway.names\" from [plugins].install to run without it")]
    Corrupt {
        /// The file involved.
        path: PathBuf,
        /// What `serde_json` said.
        detail: String,
    },
    /// The proposed name cannot be used. See [`validate_name`].
    #[error("{0}")]
    InvalidName(String),
}

/// Operator-chosen names for agents, ALONGSIDE their stable `AgentId` --
/// never instead of it.
///
/// Four methods, mirroring `conway::plugin::MemoryStore`'s
/// `put`/`get`/`list`/`remove`. `remove` ships day one: the label-based
/// predecessor's lack of removal is exactly what forced its rework.
///
/// **Synchronous, unlike `MemoryStore`.** The host's busiest reader is the
/// `/agents` panel's draw path, which is a plain `&AppState` render
/// function with no executor under it -- an `async` `get` could not be
/// called from there at all. Both implementations in this crate answer
/// `get`/`list` from an in-memory map and never touch the disk, so the
/// draw path stays allocation-cheap and I/O-free; only `set`/`remove`
/// write, and only in response to an operator typing a command.
///
/// **`set`/`remove` return `Result`, `get`/`list` cannot fail.** A rename
/// that silently failed to persist would report success now and be gone
/// after a restart -- exactly the failure this item exists to prevent
/// ("the name survives a restart"). Reads have no such hazard: the map is
/// already in memory.
pub trait AgentNames: Send + Sync + 'static {
    /// This agent's name, if the operator gave it one.
    fn get(&self, id: &AgentId) -> Option<String>;

    /// Names `id` (replacing any previous name for it), durably.
    ///
    /// Never refuses a name merely because another agent already has it --
    /// see this crate's module doc, "Duplicate names are allowed".
    /// [`AgentNamesError::InvalidName`] for a name [`validate_name`]
    /// rejects; [`AgentNamesError::Io`] if the store could not be written,
    /// in which case the in-memory map is left UNCHANGED too, so what a
    /// later `get` reports never disagrees with what is on disk.
    fn set(&self, id: &AgentId, name: &str) -> Result<(), AgentNamesError>;

    /// Forgets `id`'s name. Removing a name that was never set is a
    /// success, not an error -- the caller's intent ("this agent has no
    /// name") holds either way.
    fn remove(&self, id: &AgentId) -> Result<(), AgentNamesError>;

    /// Every stored `(id, name)` pair, ordered by id (which, ULIDs being
    /// lexicographically sortable by creation time, is oldest-first).
    fn list(&self) -> Vec<(AgentId, String)>;
}

/// Rejects a name that could never do its job.
///
/// - **Empty**, or containing **whitespace**: `resolve_agent` is reached
///   through commands like `/steer <agent> <text>`, whose argument split is
///   whitespace. A name with a space in it could be stored but never typed
///   at the agent it names, which is the entire purpose.
/// - **Longer than [`MAX_NAME_LEN`]**: see that constant.
/// - **A valid ULID**: the host tries a full `AgentId` parse BEFORE it
///   tries a name, so a name that is itself a well-formed id would be
///   shadowed by that branch and could never resolve to its own agent.
///   Refusing it is better than storing something unusable.
pub fn validate_name(name: &str) -> Result<(), AgentNamesError> {
    if name.is_empty() {
        return Err(AgentNamesError::InvalidName(
            "a name cannot be empty".to_string(),
        ));
    }
    if name.chars().any(char::is_whitespace) {
        return Err(AgentNamesError::InvalidName(format!(
            "a name cannot contain whitespace (got {name:?}) -- it has to be typeable as one \
             word after `/steer`, `/context`, `/fork @`"
        )));
    }
    if name.chars().count() > MAX_NAME_LEN {
        return Err(AgentNamesError::InvalidName(format!(
            "a name cannot be longer than {MAX_NAME_LEN} characters (got {})",
            name.chars().count()
        )));
    }
    if name.parse::<AgentId>().is_ok() {
        return Err(AgentNamesError::InvalidName(format!(
            "{name:?} is itself a valid agent id, so it would never resolve as a name -- the \
             host matches a full id first"
        )));
    }
    Ok(())
}

/// This plugin's default store path: [`STORE_FILE_NAME`] in the same
/// directory `settings.json` resolves into -- `$CONWAY_CONFIG_DIR` when set,
/// else `~/.conway/`.
///
/// Takes an explicit `env` map rather than reading `std::env` itself, for
/// the same reason `conway::config::discovery::user_config_path` does:
/// callers inject it, and tests stay hermetic and parallel-safe. **Every
/// test that constructs a real store must point this at a temporary
/// directory** -- writing into the operator's own `~/.conway/` from a test
/// run is the defect `crates/conway/tests/config_isolation_guard.rs` exists
/// to catch.
///
/// `None` when no home directory is discoverable AND `CONWAY_CONFIG_DIR` is
/// unset -- the same extreme edge case `user_config_path` itself returns
/// `None` for. The host treats that as "run without persisted names"
/// rather than as a startup failure.
pub fn default_store_path(env: &HashMap<String, String>) -> Option<PathBuf> {
    conway::config::discovery::user_config_path(env)
        .and_then(|settings| settings.parent().map(|dir| dir.join(STORE_FILE_NAME)))
}

/// The on-disk document.
///
/// A one-field object rather than a bare `{ "<id>": "<name>" }` map, so a
/// later field is an additive change to a document this crate already
/// knows how to parse instead of a format migration. `#[serde(default)]`
/// so a file written before such a field existed still loads.
#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreDocument {
    #[serde(default)]
    names: BTreeMap<String, String>,
}

/// The durable [`AgentNames`]: one JSON file, one in-memory map.
///
/// **Reads never touch the disk.** The file is read exactly once, by
/// [`FsAgentNames::open`]; every later `get`/`list` answers from the map.
/// That is what lets the `/agents` panel call `get` once per row per frame
/// without an I/O budget.
///
/// **Writes are whole-file and atomic.** `set`/`remove` rewrite the entire
/// document to a `.tmp` sibling and `rename` it over the real path -- the
/// same shape `conway_cli`'s `tui::history::save` and `conway-session`'s
/// `SessionIndex::persist_full` already use, so a crash mid-write leaves
/// either the complete old file or the complete new one, never a truncated
/// one. Whole-file is right at this size: the document is one short line
/// per name an operator deliberately typed.
#[derive(Debug)]
pub struct FsAgentNames {
    path: PathBuf,
    names: RwLock<BTreeMap<AgentId, String>>,
}

impl FsAgentNames {
    /// Opens (or adopts as empty) the store at `path`.
    ///
    /// A **missing** file is an empty store, not an error -- the ordinary
    /// first run. A file that exists but does not parse is
    /// [`AgentNamesError::Corrupt`]: see that variant's own doc for why
    /// this refuses to degrade quietly.
    ///
    /// An entry whose key is not a valid `AgentId` is DROPPED, with no
    /// error: such a key names nothing that could ever exist, so it can
    /// never be displayed or resolved, and keeping bytes this crate cannot
    /// interpret would only mean writing them back out again.
    ///
    /// Synchronous, unlike `conway::memory::FsMemoryStore::open`. There is
    /// one small file to read, once, at startup, on a path
    /// (`conway_cli`'s `build_conway`) that is already doing synchronous
    /// config discovery around it -- an async bridge here would be
    /// ceremony with nothing behind it.
    pub fn open(path: PathBuf) -> Result<Self, AgentNamesError> {
        let names = match std::fs::read_to_string(&path) {
            Ok(contents) if contents.trim().is_empty() => BTreeMap::new(),
            Ok(contents) => {
                let doc: StoreDocument =
                    serde_json::from_str(&contents).map_err(|e| AgentNamesError::Corrupt {
                        path: path.clone(),
                        detail: e.to_string(),
                    })?;
                doc.names
                    .into_iter()
                    .filter_map(|(k, v)| k.parse::<AgentId>().ok().map(|id| (id, v)))
                    .collect()
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => {
                return Err(AgentNamesError::Io {
                    path: path.clone(),
                    source: e,
                })
            }
        };
        Ok(Self {
            path,
            names: RwLock::new(names),
        })
    }

    /// The file this store reads and writes -- for a host that wants to
    /// name it in an error message.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Serializes `names` and replaces the store file with it, atomically.
    fn persist(&self, names: &BTreeMap<AgentId, String>) -> Result<(), AgentNamesError> {
        let doc = StoreDocument {
            names: names
                .iter()
                .map(|(id, name)| (id.to_string(), name.clone()))
                .collect(),
        };
        // A `BTreeMap<String, String>` cannot fail to serialize; the
        // `map_err` is exhaustiveness, not a reachable fallback.
        let body = serde_json::to_string_pretty(&doc).map_err(|e| AgentNamesError::Corrupt {
            path: self.path.clone(),
            detail: e.to_string(),
        })?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| AgentNamesError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, body).map_err(|e| AgentNamesError::Io {
            path: tmp.clone(),
            source: e,
        })?;
        std::fs::rename(&tmp, &self.path).map_err(|e| AgentNamesError::Io {
            path: self.path.clone(),
            source: e,
        })
    }
}

impl AgentNames for FsAgentNames {
    fn get(&self, id: &AgentId) -> Option<String> {
        // A poisoned lock means another thread panicked mid-write. The
        // panic is the bug; refusing to draw a row over it would be a
        // second one, so read through the poison (the map is a plain
        // `BTreeMap` -- a panicking `insert` cannot leave it torn).
        let names = self.names.read().unwrap_or_else(|e| e.into_inner());
        names.get(id).cloned()
    }

    fn set(&self, id: &AgentId, name: &str) -> Result<(), AgentNamesError> {
        validate_name(name)?;
        let mut names = self.names.write().unwrap_or_else(|e| e.into_inner());
        let previous = names.insert(*id, name.to_string());
        if let Err(e) = self.persist(&names) {
            // Roll back, so `get` never reports a name the next process
            // will not find (this trait's own `set` doc).
            match previous {
                Some(old) => names.insert(*id, old),
                None => names.remove(id),
            };
            return Err(e);
        }
        Ok(())
    }

    fn remove(&self, id: &AgentId) -> Result<(), AgentNamesError> {
        let mut names = self.names.write().unwrap_or_else(|e| e.into_inner());
        let Some(previous) = names.remove(id) else {
            // Nothing stored: a no-op success, and NO write -- rewriting an
            // unchanged file is a way to lose it for no reason.
            return Ok(());
        };
        if let Err(e) = self.persist(&names) {
            names.insert(*id, previous);
            return Err(e);
        }
        Ok(())
    }

    fn list(&self) -> Vec<(AgentId, String)> {
        let names = self.names.read().unwrap_or_else(|e| e.into_inner());
        names.iter().map(|(id, name)| (*id, name.clone())).collect()
    }
}

/// A non-durable [`AgentNames`] for a host that has not opted into
/// `conway.names` and for embedders/tests that want no file at all.
///
/// The exact counterpart of `conway_plugin_memory::InMemoryMemoryStore`,
/// and it exists for the same reason: `conway_cli`'s `bundle` constructs a
/// `conway.names` candidate unconditionally (selection is
/// `install_selected`'s job, not `bundle`'s), so its dependency has to be
/// constructible with no I/O for an operator who never asked for it.
#[derive(Debug, Default)]
pub struct InMemoryAgentNames {
    names: RwLock<BTreeMap<AgentId, String>>,
}

impl InMemoryAgentNames {
    /// A fresh, empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl AgentNames for InMemoryAgentNames {
    fn get(&self, id: &AgentId) -> Option<String> {
        let names = self.names.read().unwrap_or_else(|e| e.into_inner());
        names.get(id).cloned()
    }

    fn set(&self, id: &AgentId, name: &str) -> Result<(), AgentNamesError> {
        validate_name(name)?;
        let mut names = self.names.write().unwrap_or_else(|e| e.into_inner());
        names.insert(*id, name.to_string());
        Ok(())
    }

    fn remove(&self, id: &AgentId) -> Result<(), AgentNamesError> {
        let mut names = self.names.write().unwrap_or_else(|e| e.into_inner());
        names.remove(id);
        Ok(())
    }

    fn list(&self) -> Vec<(AgentId, String)> {
        let names = self.names.read().unwrap_or_else(|e| e.into_inner());
        names.iter().map(|(id, name)| (*id, name.clone())).collect()
    }
}

/// Which agent a command was aimed at, and what is left of its arguments.
///
/// `CommandCtx` carries `focused_agent` but no way to resolve an arbitrary
/// agent REFERENCE -- a plugin command cannot see the agent tree, so it
/// cannot expand the `/agents` panel's short id the way `resolve_agent`
/// can (widening `CommandCtx` would mean editing `conway-core`, which this
/// item forbids). So the target is either implicit (the focused agent,
/// the common case: focus a row, name it) or a FULL agent id, which
/// `/tree` prints in full for exactly this kind of use.
fn split_target(ctx: &CommandCtx, tokens: &[&str]) -> (AgentId, usize) {
    match tokens.first().map(|first| first.parse::<AgentId>()) {
        Some(Ok(id)) => (id, 1),
        _ => (ctx.focused_agent, 0),
    }
}

/// `/conway.names.rename [<agent-id>] <name>`.
///
/// With one token, names the FOCUSED agent; with a leading full agent id
/// (as `/tree` prints it), names that agent instead. Anything else is a
/// named [`CommandOutcome::Error`] carrying the usage line, never a panic
/// -- the same discipline `conway_plugin_history`'s commands follow.
struct RenameCommand {
    names: Arc<dyn AgentNames>,
}

fn rename_usage(got: &str) -> CommandOutcome {
    CommandOutcome::Error(format!(
        "usage: /{PLUGIN_ID}.{COMMAND_NAME_RENAME} [<agent-id>] <name> -- a single-word name for \
         the focused agent, or a FULL agent id (as `/tree` prints it) followed by one, got \
         {got:?}"
    ))
}

#[async_trait]
impl Command for RenameCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: COMMAND_NAME_RENAME.to_string(),
            summary: "names an agent so you can steer it by that name, e.g. \
                      `/conway.names.rename scout` (the focused agent) or \
                      `/conway.names.rename <agent-id> scout` -- the name persists across \
                      restarts and never replaces the agent's id"
                .to_string(),
        }
    }

    async fn invoke(&self, ctx: CommandCtx) -> CommandOutcome {
        let trimmed = ctx.args.trim();
        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        let (target, consumed) = split_target(&ctx, &tokens);
        let rest = &tokens[consumed..];
        let [name] = rest else {
            return rename_usage(trimmed);
        };
        // Duplicate detection BEFORE the write, so the notice describes the
        // state the operator is creating rather than the one they created
        // (this crate's module doc: allowed, but never silent).
        let also_named: Vec<AgentId> = self
            .names
            .list()
            .into_iter()
            .filter(|(id, existing)| existing == name && id != &target)
            .map(|(id, _)| id)
            .collect();
        match self.names.set(&target, name) {
            Ok(()) => {
                let mut lines = vec![format!("named {target} `{name}`")];
                if !also_named.is_empty() {
                    lines.push(format!(
                        "note: {} other agent(s) already answer to `{name}` ({}) -- steering by \
                         that name will report an ambiguity if more than one of them is in this \
                         session's tree",
                        also_named.len(),
                        also_named
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                CommandOutcome::Output(lines)
            }
            Err(e) => CommandOutcome::Error(format!("could not name {target}: {e}")),
        }
    }
}

/// `/conway.names.unname [<agent-id>]`: forgets a name. With no argument,
/// the FOCUSED agent -- see [`split_target`] for why a target is either
/// implicit or a full id.
struct UnnameCommand {
    names: Arc<dyn AgentNames>,
}

#[async_trait]
impl Command for UnnameCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: COMMAND_NAME_UNNAME.to_string(),
            summary: "forgets an agent's name -- `/conway.names.unname` for the focused agent, \
                      or `/conway.names.unname <agent-id>`; the agent itself and its id are \
                      untouched"
                .to_string(),
        }
    }

    async fn invoke(&self, ctx: CommandCtx) -> CommandOutcome {
        let trimmed = ctx.args.trim();
        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        let usage = || {
            CommandOutcome::Error(format!(
                "usage: /{PLUGIN_ID}.{COMMAND_NAME_UNNAME} [<agent-id>] -- no argument for the \
                 focused agent, or a FULL agent id (as `/tree` prints it), got {trimmed:?}"
            ))
        };
        let target = match tokens.as_slice() {
            [] => ctx.focused_agent,
            [only] => match only.parse::<AgentId>() {
                Ok(id) => id,
                Err(_) => return usage(),
            },
            _ => return usage(),
        };
        let previous = self.names.get(&target);
        match self.names.remove(&target) {
            Ok(()) => CommandOutcome::Output(vec![match previous {
                Some(name) => format!("{target} is no longer `{name}`"),
                None => format!("{target} had no name"),
            }]),
            Err(e) => CommandOutcome::Error(format!("could not unname {target}: {e}")),
        }
    }
}

/// `/conway.names.list`: every stored name, oldest first.
///
/// The store's visibility surface -- this crate's module doc rests its
/// "entries persist until removed" decision on the operator being able to
/// SEE what has accumulated and remove it, so listing is not a nicety.
/// Includes names for agents from other sessions and other projects,
/// because the store is flat and global; that is the disclosed cost of the
/// key choice, and hiding it would be worse than showing it.
struct ListCommand {
    names: Arc<dyn AgentNames>,
}

#[async_trait]
impl Command for ListCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: COMMAND_NAME_LIST.to_string(),
            summary: "lists every agent name stored, across every session and project -- the \
                      whole store, so nothing accumulates out of sight"
                .to_string(),
        }
    }

    async fn invoke(&self, _ctx: CommandCtx) -> CommandOutcome {
        let stored = self.names.list();
        if stored.is_empty() {
            return CommandOutcome::Output(vec!["no agents are named".to_string()]);
        }
        CommandOutcome::Output(
            stored
                .into_iter()
                .map(|(id, name)| format!("{id}  {name}"))
                .collect(),
        )
    }
}

/// The plugin itself: three commands over one [`AgentNames`].
///
/// Constructed with the store rather than opening one, mirroring
/// `conway_plugin_memory::MemoryPlugin::new` exactly -- `conway-cli`
/// resolves ONE store per process and threads the same `Arc` here and to
/// its own readers, so the plugin's writes and the panel's reads can never
/// be two independent, unsynchronized views of one file.
pub struct NamesPlugin {
    names: Arc<dyn AgentNames>,
}

impl NamesPlugin {
    /// Wraps `names`. The host owns the store's lifetime and identity.
    pub fn new(names: Arc<dyn AgentNames>) -> Self {
        Self { names }
    }

    /// The store this plugin writes through -- for a host that constructed
    /// the plugin and wants the same `Arc` back without keeping its own
    /// copy.
    pub fn names(&self) -> Arc<dyn AgentNames> {
        self.names.clone()
    }
}

impl Plugin for NamesPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PLUGIN_ID.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            // No tools: this plugin's entire surface is the three TUI slash
            // commands below, plus the name the host renders and resolves.
            // A model has no business renaming agents -- naming is the
            // OPERATOR's addressing affordance (module doc).
            tools: vec![],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn description(&self) -> PluginDescription {
        PluginDescription {
            summary: "name agents so you can steer them by name".to_string(),
            you_get: format!(
                "3 commands: /{PLUGIN_ID}.{COMMAND_NAME_RENAME} (name an agent), \
                 /{PLUGIN_ID}.{COMMAND_NAME_UNNAME} (forget a name), \
                 /{PLUGIN_ID}.{COMMAND_NAME_LIST} (every name stored) -- plus the name shown in \
                 the /agents panel, and accepted anywhere an agent is named (/steer, /context, \
                 /fork @)"
            ),
            you_lose: "nothing -- an agent's id keeps working everywhere it worked before, and \
                       uninstalling leaves every surface exactly as it was"
                .to_string(),
            costs: format!(
                "one small JSON file ({STORE_FILE_NAME}) beside your settings.json; names \
                 persist across restarts but do not travel with a session log copied elsewhere"
            ),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn conway::plugin::Tool>> {
        Vec::new()
    }

    fn commands(&self) -> Vec<Arc<dyn Command>> {
        vec![
            Arc::new(RenameCommand {
                names: self.names.clone(),
            }),
            Arc::new(UnnameCommand {
                names: self.names.clone(),
            }),
            Arc::new(ListCommand {
                names: self.names.clone(),
            }),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(focused: AgentId, args: &str) -> CommandCtx {
        CommandCtx {
            focused_agent: focused,
            root_agent: focused,
            session_id: conway::SessionId::new(),
            args: args.to_string(),
        }
    }

    fn store() -> Arc<dyn AgentNames> {
        Arc::new(InMemoryAgentNames::new())
    }

    #[test]
    fn manifest_id_matches_the_published_constant() {
        assert_eq!(NamesPlugin::new(store()).manifest().id, PLUGIN_ID);
    }

    #[test]
    fn every_declared_command_is_registrable_by_the_hosts_own_rule() {
        // `CommandRegistry::build` refuses an empty or whitespace-bearing
        // bare name outright -- a command that could never be typed.
        for command in NamesPlugin::new(store()).commands() {
            let name = command.spec().name;
            assert!(!name.is_empty(), "empty command name");
            assert!(
                !name.chars().any(char::is_whitespace),
                "command name {name:?} has whitespace and could never be typed"
            );
        }
    }

    #[test]
    fn a_name_with_whitespace_is_refused_because_it_could_never_be_typed() {
        let err = validate_name("two words").expect_err("whitespace must be refused");
        assert!(
            matches!(err, AgentNamesError::InvalidName(_)),
            "expected InvalidName, got {err:?}"
        );
    }

    #[test]
    fn a_name_that_is_itself_a_valid_agent_id_is_refused() {
        let id = AgentId::new().to_string();
        let err = validate_name(&id).expect_err("a bare ULID must be refused as a name");
        assert!(
            err.to_string().contains("valid agent id"),
            "message should say why: {err}"
        );
    }

    #[test]
    fn an_over_long_name_is_refused_at_the_display_bound() {
        let ok = "n".repeat(MAX_NAME_LEN);
        validate_name(&ok).expect("exactly MAX_NAME_LEN is fine");
        let err = validate_name(&"n".repeat(MAX_NAME_LEN + 1)).expect_err("one over must fail");
        assert!(err.to_string().contains("longer than"), "{err}");
    }

    #[tokio::test]
    async fn rename_with_one_token_names_the_focused_agent() {
        let names = store();
        let plugin = NamesPlugin::new(names.clone());
        let focused = AgentId::new();
        let rename = &plugin.commands()[0];
        let outcome = rename.invoke(ctx(focused, "scout")).await;
        assert!(
            matches!(outcome, CommandOutcome::Output(_)),
            "expected Output, got {outcome:?}"
        );
        assert_eq!(names.get(&focused).as_deref(), Some("scout"));
    }

    #[tokio::test]
    async fn rename_with_a_leading_full_id_names_that_agent_not_the_focused_one() {
        let names = store();
        let plugin = NamesPlugin::new(names.clone());
        let focused = AgentId::new();
        let other = AgentId::new();
        let rename = &plugin.commands()[0];
        let outcome = rename.invoke(ctx(focused, &format!("{other} scout"))).await;
        assert!(matches!(outcome, CommandOutcome::Output(_)), "{outcome:?}");
        assert_eq!(names.get(&other).as_deref(), Some("scout"));
        assert_eq!(names.get(&focused), None, "the focused agent was not named");
    }

    #[tokio::test]
    async fn rename_reports_a_bad_argument_shape_as_an_error_not_a_panic() {
        let plugin = NamesPlugin::new(store());
        let rename = &plugin.commands()[0];
        for args in ["", "one two three", "two words here"] {
            let outcome = rename.invoke(ctx(AgentId::new(), args)).await;
            assert!(
                matches!(outcome, CommandOutcome::Error(_)),
                "args {args:?} should be a named error, got {outcome:?}"
            );
        }
    }

    #[tokio::test]
    async fn renaming_a_second_agent_to_the_same_name_succeeds_and_says_so() {
        let names = store();
        let plugin = NamesPlugin::new(names.clone());
        let first = AgentId::new();
        let second = AgentId::new();
        let rename = &plugin.commands()[0];
        rename.invoke(ctx(first, "scout")).await;
        let outcome = rename.invoke(ctx(second, "scout")).await;
        match outcome {
            CommandOutcome::Output(lines) => {
                assert_eq!(
                    lines.len(),
                    2,
                    "the duplicate must be disclosed, not silent: {lines:?}"
                );
                assert!(lines[1].contains("already answer to"), "{lines:?}");
            }
            other => panic!("a duplicate name is allowed, not refused: {other:?}"),
        }
        // Both keep the name -- resolution, not the store, reports the
        // ambiguity (module doc).
        assert_eq!(names.get(&first).as_deref(), Some("scout"));
        assert_eq!(names.get(&second).as_deref(), Some("scout"));
    }

    #[tokio::test]
    async fn unname_forgets_the_focused_agents_name_and_reports_the_old_one() {
        let names = store();
        let plugin = NamesPlugin::new(names.clone());
        let focused = AgentId::new();
        names.set(&focused, "scout").expect("set");
        let unname = &plugin.commands()[1];
        let outcome = unname.invoke(ctx(focused, "")).await;
        match outcome {
            CommandOutcome::Output(lines) => assert!(lines[0].contains("scout"), "{lines:?}"),
            other => panic!("expected Output, got {other:?}"),
        }
        assert_eq!(names.get(&focused), None);
    }

    #[tokio::test]
    async fn unname_of_an_unnamed_agent_is_a_success_that_says_nothing_was_stored() {
        let plugin = NamesPlugin::new(store());
        let unname = &plugin.commands()[1];
        match unname.invoke(ctx(AgentId::new(), "")).await {
            CommandOutcome::Output(lines) => assert!(lines[0].contains("had no name"), "{lines:?}"),
            other => panic!("expected Output, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unname_rejects_a_target_that_is_not_a_full_agent_id() {
        let plugin = NamesPlugin::new(store());
        let unname = &plugin.commands()[1];
        let outcome = unname.invoke(ctx(AgentId::new(), "scout")).await;
        assert!(
            matches!(outcome, CommandOutcome::Error(_)),
            "a short token is not a resolvable target here: {outcome:?}"
        );
    }

    #[tokio::test]
    async fn list_shows_every_stored_name_and_says_so_when_there_are_none() {
        let names = store();
        let plugin = NamesPlugin::new(names.clone());
        let list = &plugin.commands()[2];
        match list.invoke(ctx(AgentId::new(), "")).await {
            CommandOutcome::Output(lines) => assert_eq!(lines, vec!["no agents are named"]),
            other => panic!("expected Output, got {other:?}"),
        }
        let a = AgentId::new();
        names.set(&a, "scout").expect("set");
        match list.invoke(ctx(AgentId::new(), "")).await {
            CommandOutcome::Output(lines) => {
                assert_eq!(lines.len(), 1);
                assert!(lines[0].contains("scout") && lines[0].contains(&a.to_string()));
            }
            other => panic!("expected Output, got {other:?}"),
        }
    }

    #[test]
    fn in_memory_store_round_trips_set_get_remove_list() {
        let store = InMemoryAgentNames::new();
        let a = AgentId::new();
        assert_eq!(store.get(&a), None);
        store.set(&a, "scout").expect("set");
        assert_eq!(store.get(&a).as_deref(), Some("scout"));
        assert_eq!(store.list(), vec![(a, "scout".to_string())]);
        store.remove(&a).expect("remove");
        assert_eq!(store.get(&a), None);
        assert!(store.list().is_empty());
    }

    #[test]
    fn default_store_path_follows_conway_config_dir() {
        let env: HashMap<String, String> = [(
            "CONWAY_CONFIG_DIR".to_string(),
            "/tmp/not-a-real-dir".to_string(),
        )]
        .into_iter()
        .collect();
        assert_eq!(
            default_store_path(&env),
            Some(PathBuf::from("/tmp/not-a-real-dir").join(STORE_FILE_NAME)),
            "the store must land beside settings.json, wherever that is redirected to"
        );
    }
}
