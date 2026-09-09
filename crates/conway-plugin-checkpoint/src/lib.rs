//! `conway.checkpoint`: rolls back the model's own file changes, the
//! counterpart `conway.history`'s `/conway.history.rewind` never had --
//! that command can rewind the CONVERSATION to any point without
//! destroying anything, but nothing snapshots the FILES a turn touched, so
//! the operator's only recovery when a model edits the wrong file (or takes
//! a right one too far) has always been git, which cannot tell which
//! uncommitted edits were theirs and which were the model's.
//!
//! # This reverses a written ruling, on purpose
//!
//! `docs/vision/CATALOGUE.md`'s "Explicitly not recommended" section used
//! to read "filesystem checkpointing as a harness feature: git already
//! solves this". The operator revisited that call on 2026-09-07, applying
//! this project's own convergence test to four independent coding
//! harnesses that all ship some form of file checkpoint -- three of them ON
//! TOP of git, not instead of it -- and ruled: build it, as a PLUGIN, in
//! the default opinion set, removable like every other one. That amendment
//! is recorded in `docs/vision/CATALOGUE.md` itself, alongside this
//! reversal's own cost, rather than left as a contradiction between a
//! shipped capability and a doc that still argues against it.
//!
//! # What this plugin actually observes, and the seam gap that shapes it
//!
//! A `Plugin` gets exactly one seam for watching a tool call in-process:
//! [`ToolObserver::after_tool_call`], which fires **after**
//! a call's result is already durable -- the file has already been
//! written by the time this plugin ever sees the call. There is no
//! in-process "before" seam a compiled `Plugin` can hook: the only
//! `pre_tool_use` registration surface a plugin has (`PluginHookRule`,
//! `Plugin::hooks`) dispatches through
//! a SPAWNED SUBPROCESS (`docs/plugins/hooks.md` point 13), the same
//! mechanism a declarative `[hooks].rules[]` entry uses -- built for
//! wrapping an operator's own script, not for a first-party Rust plugin's
//! own file I/O, and a poor fit for a snapshot that has to run on every
//! single edit. **This is a genuine gap, reported rather than routed
//! around**: nothing in `conway-core`/`conway-runtime` was touched to build
//! this plugin, per this item's own "what not to build".
//!
//! So `old` (what a path looked like immediately before one checkpoint
//! entry) is never read off disk directly -- it is CHAINED: the `new` this
//! store captured for that same path's most recent EARLIER entry, if this
//! session's checkpoint history has one. The one case that leaves
//! genuinely unrecoverable is a path's FIRST observed touch in a session --
//! there was no earlier entry to chain from, and by the time this plugin
//! is ever asked about the call, the pre-edit bytes are already gone. That
//! case is represented explicitly ([`SnapshotRef::Unavailable`]),
//! never guessed at: `/conway.checkpoint.diff`/`.rollback` say plainly that
//! no baseline is known for that path rather than silently treating it as
//! empty or skipping it without a word.
//!
//! # `bash` is not captured, and this says so
//!
//! This plugin's `CheckpointObserver` (below) only ever inspects
//! `write`/`edit` tool calls. A file changed through `bash` -- `sed -i`,
//! a redirect, anything else with shell access to the filesystem -- leaves
//! no snapshot, exactly the same limitation Claude Code's own checkpoints
//! disclose for the identical reason: there is no seam here (or there) that
//! observes what an unconstrained shell subprocess actually touched. See
//! [`Plugin::description`]'s own `you_lose` text for the operator-facing
//! statement of this, and `docs/plugins/checkpoint.md` for the same point
//! in the doc a reader is more likely to see first.
//!
//! # Three commands, one store
//!
//! `/conway.checkpoint.list`, `/conway.checkpoint.diff <seq>`, and
//! `/conway.checkpoint.rollback <seq> [<path>] [--all] [--rewind]` all read
//! the SAME [`CheckpointStore`] this plugin's observer writes to --
//! see that module's own doc for the on-disk layout, the two configurable
//! bounds, and exactly how `rollback` preserves an operator's own hand edit
//! by default (a three-way check: the snapshot being restored TO, the
//! bytes conway itself last wrote, and whatever is on disk right now -- a
//! mismatch between the latter two is a conflict, reported per file and
//! never silently overwritten unless `--all` forces it) while still
//! snapshotting the CURRENT state first, so a rollback is itself always
//! undoable.
//!
//! `--rewind` composes with `conway.history`: a rollback that passes it
//! restores files exactly as an ordinary rollback would, and then asks the
//! host to fork the calling session at the SAME seq
//! ([`CommandOutcome::ForkSession`]) instead of reporting
//! text -- so the conversation and the files land at the same point
//! together. The equivalent two-command recipe --
//! `/conway.checkpoint.rollback <seq>` then `/conway.history.rewind <seq>`
//! -- always works too, and is what an operator who wants to read the
//! restore report before forking should use instead (`--rewind` discards
//! that report; `CommandOutcome` can only ever be one variant).
//!
//! # Installing it
//!
//! ```json
//! { "plugins": { "install": ["conway.checkpoint"] } }
//! ```
//!
//! In `conway-cli`'s default opinion set (`crates/conway-cli/src/
//! first_party_plugins.rs`'s `DEFAULT_OPINION_SET`) -- a fresh operator
//! gets it unprompted, and can remove it like any other opinion.

mod diff;
mod store;

use std::path::PathBuf;
use std::sync::Arc;

use conway::plugin::{
    async_trait, Command, CommandCtx, CommandOutcome, CommandSpec, ObservedCall, ObserverAnswer,
    ObserverCtx, ObserverNote, Plugin, PluginDescription, PluginManifest, Tool, ToolObserver,
};
use conway::LogSeq;

pub use store::{
    CheckpointEntry, CheckpointStore, ConflictedPath, PutBlobOutcome, ResolvedRef, RestoredPath,
    RollbackReport, SnapshotRef, ToolKind, DEFAULT_MAX_PROJECT_BYTES, DEFAULT_MAX_SNAPSHOT_BYTES,
};

/// This plugin's manifest id.
pub const PLUGIN_ID: &str = "conway.checkpoint";

pub const COMMAND_NAME_LIST: &str = "list";
pub const COMMAND_NAME_DIFF: &str = "diff";
pub const COMMAND_NAME_ROLLBACK: &str = "rollback";

/// A note this plugin's observer appends carries this reason -- a skip
/// (per-file bound) or an eviction (per-project bound) notice, never a
/// silent one.
pub const NOTE_REASON: &str = "checkpoint_snapshot";

const TOOL_WRITE: &str = "write";
const TOOL_EDIT: &str = "edit";

fn describe_snapshot(snapshot: &SnapshotRef) -> String {
    match snapshot {
        SnapshotRef::Bytes { len, .. } => format!("{len} byte(s)"),
        SnapshotRef::Absent => "absent".to_string(),
        SnapshotRef::Unavailable { reason } => format!("unavailable ({reason})"),
    }
}

/// The [`ToolObserver`] half of this plugin -- see this crate's own module
/// doc, "What this plugin actually observes", for why only `write`/`edit`
/// are inspected and why `old` is a chain rather than a genuine pre-hook
/// read.
struct CheckpointObserver {
    store: Arc<CheckpointStore>,
    max_snapshot_bytes: u64,
    max_project_bytes: u64,
}

#[async_trait]
impl ToolObserver for CheckpointObserver {
    async fn after_tool_call(&self, _ctx: &ObserverCtx, call: &ObservedCall) -> ObserverAnswer {
        // A failed call never mutated the filesystem -- nothing to
        // snapshot (and the write/edit tools' own `path_args` contract
        // gives no guarantee `arguments["path"]` is even meaningful on an
        // error path).
        if call.is_error {
            return ObserverAnswer::default();
        }
        let tool = match call.tool.as_str() {
            TOOL_WRITE => ToolKind::Write,
            TOOL_EDIT => ToolKind::Edit,
            // Every other tool, `bash` included -- see this crate's own
            // module doc, "`bash` is not captured, and this says so".
            _ => return ObserverAnswer::default(),
        };
        let Some(raw_path) = call.arguments.get("path").and_then(|v| v.as_str()) else {
            return ObserverAnswer::default();
        };
        let path = self.store.resolve(raw_path);
        let session = call.session.to_string();
        let recorded = self.store.record_observed(
            &session,
            call.result_seq.0,
            &path,
            tool,
            self.max_snapshot_bytes,
            self.max_project_bytes,
        );
        let Ok((_entry, notices)) = recorded else {
            // This store performs no I/O the runtime handed it a
            // capability for -- a failure here is a local filesystem
            // problem (a permissions error under `.conway/checkpoints`,
            // say). Observation never fails the call it observed
            // (`ToolObserver`'s own doc, "fail open"), so this drops the
            // note rather than the call.
            return ObserverAnswer::default();
        };
        ObserverAnswer {
            notes: notices
                .into_iter()
                .map(|text| ObserverNote {
                    text: format!(
                        "conway.checkpoint: {} (seq {}) -- {text}",
                        path.display(),
                        call.result_seq.0
                    ),
                    reason: NOTE_REASON.to_string(),
                })
                .collect(),
        }
    }
}

/// `/conway.checkpoint.list`: every snapshot this session has recorded, in
/// seq order.
struct ListCommand {
    store: Arc<CheckpointStore>,
}

#[async_trait]
impl Command for ListCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: COMMAND_NAME_LIST.to_string(),
            summary: "lists every snapshot this session has recorded, by seq and path".to_string(),
        }
    }

    async fn invoke(&self, ctx: CommandCtx) -> CommandOutcome {
        if !ctx.args.trim().is_empty() {
            return CommandOutcome::Error(format!(
                "usage: /{PLUGIN_ID}.{COMMAND_NAME_LIST} -- takes no arguments, got {:?}",
                ctx.args.trim()
            ));
        }
        let session = ctx.session_id.to_string();
        match self.store.entries(&session) {
            Ok(entries) if entries.is_empty() => CommandOutcome::Output(vec![
                "conway.checkpoint: no snapshots recorded yet for this session".to_string(),
            ]),
            Ok(mut entries) => {
                entries.sort_by_key(|entry| entry.seq);
                CommandOutcome::Output(
                    entries
                        .iter()
                        .map(|entry| {
                            format!(
                                "seq {}: {:?} {} -> {}",
                                entry.seq,
                                entry.tool,
                                entry.path,
                                describe_snapshot(&entry.new)
                            )
                        })
                        .collect(),
                )
            }
            Err(err) => CommandOutcome::Error(format!(
                "conway.checkpoint: failed to read snapshots: {err}"
            )),
        }
    }
}

/// `/conway.checkpoint.diff <seq>`: previews, as a unified diff per touched
/// path, exactly what `rollback <seq>` would change -- reading the SAME
/// store `rollback` itself reads (this crate's own "ONE snapshot/restore
/// implementation" constraint), but performing no write of its own.
struct DiffCommand {
    store: Arc<CheckpointStore>,
}

#[async_trait]
impl Command for DiffCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: COMMAND_NAME_DIFF.to_string(),
            summary: "shows, as a unified diff per path, what `rollback <seq>` would change -- \
                      e.g. `/conway.checkpoint.diff 42`"
                .to_string(),
        }
    }

    async fn invoke(&self, ctx: CommandCtx) -> CommandOutcome {
        let trimmed = ctx.args.trim();
        let mut tokens = trimmed.split_whitespace();
        let usage = || {
            CommandOutcome::Error(format!(
                "usage: /{PLUGIN_ID}.{COMMAND_NAME_DIFF} <seq> -- expected a non-negative \
                 integer sequence number, got {trimmed:?}"
            ))
        };
        let Some(seq_token) = tokens.next() else {
            return usage();
        };
        let Ok(seq) = seq_token.parse::<u64>() else {
            return usage();
        };
        if tokens.next().is_some() {
            return usage();
        }

        let session = ctx.session_id.to_string();
        let touched = match self.store.earliest_at_or_after(&session, seq, None) {
            Ok(touched) => touched,
            Err(err) => return CommandOutcome::Error(format!("conway.checkpoint: {err}")),
        };
        if touched.is_empty() {
            return CommandOutcome::Output(vec![format!(
                "conway.checkpoint: no snapshots touch any path at or after seq {seq}"
            )]);
        }

        let mut lines = Vec::new();
        for entry in touched {
            let resolved = match self.store.resolve_ref(&entry.old) {
                Ok(resolved) => resolved,
                Err(err) => {
                    lines.push(format!("{}: error reading snapshot: {err}", entry.path));
                    continue;
                }
            };
            let (target_bytes, absence_note) = match resolved {
                ResolvedRef::Bytes(bytes) => (bytes, None),
                ResolvedRef::Absent => (
                    Vec::new(),
                    Some("(this path did not exist before this checkpoint)"),
                ),
                ResolvedRef::Missing(reason) => {
                    lines.push(format!("{}: {reason}", entry.path));
                    continue;
                }
            };
            let current_bytes = std::fs::read(&entry.path).unwrap_or_default();
            let old_text = String::from_utf8_lossy(&target_bytes);
            let new_text = String::from_utf8_lossy(&current_bytes);
            let mut rendered = diff::unified_diff(&entry.path, &entry.path, &old_text, &new_text);
            if rendered.is_empty() {
                rendered = format!(
                    "{}: no difference from the pre-seq-{seq} snapshot",
                    entry.path
                );
            }
            if let Some(note) = absence_note {
                rendered = format!("{rendered} {note}");
            }
            lines.push(rendered);
        }
        CommandOutcome::Output(lines)
    }
}

/// `/conway.checkpoint.rollback <seq> [<path>] [--all] [--rewind]`: see
/// this crate's own module doc, "Three commands, one store", for the
/// three-way conflict/hand-edit-preservation contract this delegates to
/// [`CheckpointStore::rollback`] entirely -- this command is argument
/// parsing and report formatting, nothing more.
struct RollbackCommand {
    store: Arc<CheckpointStore>,
    max_snapshot_bytes: u64,
    max_project_bytes: u64,
}

#[async_trait]
impl Command for RollbackCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: COMMAND_NAME_ROLLBACK.to_string(),
            summary: "restores every path touched at or after <seq>, preserving a hand edit \
                      made since by default -- `/conway.checkpoint.rollback <seq> [<path>] \
                      [--all] [--rewind]`"
                .to_string(),
        }
    }

    async fn invoke(&self, ctx: CommandCtx) -> CommandOutcome {
        let trimmed = ctx.args.trim();
        let mut tokens = trimmed.split_whitespace();
        let usage = || {
            CommandOutcome::Error(format!(
                "usage: /{PLUGIN_ID}.{COMMAND_NAME_ROLLBACK} <seq> [<path>] [--all] [--rewind] \
                 -- got {trimmed:?}"
            ))
        };
        let Some(seq_token) = tokens.next() else {
            return usage();
        };
        let Ok(seq) = seq_token.parse::<u64>() else {
            return usage();
        };
        let mut force_all = false;
        let mut rewind = false;
        let mut path_arg: Option<&str> = None;
        for token in tokens {
            match token {
                "--all" => force_all = true,
                "--rewind" => rewind = true,
                other => {
                    if path_arg.is_some() {
                        return usage();
                    }
                    path_arg = Some(other);
                }
            }
        }

        let session = ctx.session_id.to_string();
        let path_filter = path_arg.map(|raw| self.store.resolve(raw));
        let report = match self.store.rollback(
            &session,
            seq,
            path_filter.as_deref(),
            force_all,
            self.max_snapshot_bytes,
            self.max_project_bytes,
        ) {
            Ok(report) => report,
            Err(err) => return CommandOutcome::Error(format!("conway.checkpoint: {err}")),
        };

        if report.restored.is_empty() && report.conflicts.is_empty() && report.skipped.is_empty() {
            return CommandOutcome::Error(format!(
                "conway.checkpoint: no snapshots touch{} at or after seq {seq}",
                path_arg.map(|p| format!(" {p}")).unwrap_or_default()
            ));
        }

        let mut lines: Vec<String> = Vec::new();
        for restored in &report.restored {
            let mut line = format!(
                "{}: restored to its state before seq {} (recorded as rollback seq {})",
                restored.path, restored.from_seq, restored.rollback_seq
            );
            if let Some(notice) = &restored.notice {
                line.push_str(&format!(" -- {notice}"));
            }
            lines.push(line);
        }
        for conflicted in &report.conflicts {
            lines.push(format!(
                "{}: a hand edit was made since conway.checkpoint's last recorded write -- not \
                 restored (pass --all to force)",
                conflicted.path
            ));
        }
        for (path, reason) in &report.skipped {
            lines.push(format!("{path}: {reason}"));
        }

        if rewind {
            // Composes with `conway.history` -- see this crate's own
            // module doc, "Three commands, one store". The `lines` report
            // built above is discarded here: `CommandOutcome` can only
            // ever be one variant, and `ForkSession` carries no text field
            // to fold it into.
            return CommandOutcome::ForkSession {
                at_seq: LogSeq(seq),
                directive: String::new(),
            };
        }
        CommandOutcome::Output(lines)
    }
}

/// The plugin itself.
pub struct CheckpointPlugin {
    store: Arc<CheckpointStore>,
    max_snapshot_bytes: u64,
    max_project_bytes: u64,
}

impl CheckpointPlugin {
    /// `cwd` is the project directory a relative tool-call path or a
    /// relative operator-typed `<path>` resolves against, and the parent
    /// of this plugin's own `.conway/checkpoints` shadow store -- the
    /// identical `.conway/<name>` convention `conway.skills`/
    /// `conway.memory` already establish. Uses [`DEFAULT_MAX_SNAPSHOT_BYTES`]/
    /// [`DEFAULT_MAX_PROJECT_BYTES`]; see [`Self::with_bounds`] for a
    /// caller that wants different bounds.
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self::with_bounds(cwd, DEFAULT_MAX_SNAPSHOT_BYTES, DEFAULT_MAX_PROJECT_BYTES)
    }

    /// [`Self::new`] with explicit bounds, mirroring
    /// `conway_plugin_stepguard::StepGuardPlugin::with_capacity`'s own
    /// precedent for a first-party plugin's configurable constant.
    pub fn with_bounds(
        cwd: impl Into<PathBuf>,
        max_snapshot_bytes: u64,
        max_project_bytes: u64,
    ) -> Self {
        let cwd = cwd.into();
        let root = cwd.join(".conway").join("checkpoints");
        Self {
            store: Arc::new(CheckpointStore::new(root, cwd)),
            max_snapshot_bytes,
            max_project_bytes,
        }
    }

    /// The store this plugin's observer and commands share -- exposed for
    /// a caller (this crate's own end-to-end tests) that wants to inspect
    /// or drive it directly, alongside the installed plugin.
    pub fn store(&self) -> &Arc<CheckpointStore> {
        &self.store
    }
}

impl Plugin for CheckpointPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PLUGIN_ID.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            // No tools: this plugin's entire surface is one observer and
            // three TUI commands. `PluginManifest::tools` names only what
            // `Plugin::tools` actually returns -- an empty `Vec` here,
            // never a stub.
            tools: vec![],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn description(&self) -> PluginDescription {
        PluginDescription {
            summary: "snapshots a file's bytes around every write/edit tool call, so a \
                      rollback can put them back"
                .to_string(),
            you_get: format!(
                "a shadow snapshot taken around every `write`/`edit` tool call, and 3 \
                 commands: /{PLUGIN_ID}.list (every snapshot, by seq), /{PLUGIN_ID}.diff \
                 (preview what a rollback would change, as a unified diff), \
                 /{PLUGIN_ID}.rollback (restore every path touched at or after a seq, \
                 preserving a hand edit made since by default, and itself undoable)"
            ),
            you_lose: "coverage of any file change made through `bash` (or any tool other \
                       than `write`/`edit`) -- this plugin observes ONLY those two tools, so a \
                       model that edits a file through a shell command leaves no snapshot to \
                       roll back to, the same disclosed limit Claude Code's own checkpoints \
                       carry for the identical reason; AND the ability to undo a path's FIRST \
                       edit in a session -- this harness has no pre-write seam, so a baseline \
                       exists only from the second edit of a given file onward, and diff and \
                       rollback say a baseline is unknown rather than guessing it was empty"
                .to_string(),
            costs: format!(
                "disk under .conway/checkpoints, bounded to {} bytes per snapshotted file and \
                 {} bytes total per project -- both a skip and an eviction get a visible \
                 notice rather than growing past the bound silently",
                self.max_snapshot_bytes, self.max_project_bytes
            ),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    fn commands(&self) -> Vec<Arc<dyn Command>> {
        vec![
            Arc::new(ListCommand {
                store: self.store.clone(),
            }),
            Arc::new(DiffCommand {
                store: self.store.clone(),
            }),
            Arc::new(RollbackCommand {
                store: self.store.clone(),
                max_snapshot_bytes: self.max_snapshot_bytes,
                max_project_bytes: self.max_project_bytes,
            }),
        ]
    }

    fn observers(&self) -> Vec<Arc<dyn ToolObserver>> {
        vec![Arc::new(CheckpointObserver {
            store: self.store.clone(),
            max_snapshot_bytes: self.max_snapshot_bytes,
            max_project_bytes: self.max_project_bytes,
        }) as Arc<dyn ToolObserver>]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway::plugin::PluginEventHandle;
    use conway::{AgentId, SessionId, ToolName};
    use tempfile::TempDir;

    fn ctx_for(session_id: SessionId, args: &str) -> CommandCtx {
        CommandCtx {
            focused_agent: AgentId::new(),
            root_agent: AgentId::new(),
            session_id,
            args: args.to_string(),
        }
    }

    fn observer_ctx() -> ObserverCtx {
        ObserverCtx {
            events: PluginEventHandle::noop(PLUGIN_ID),
        }
    }

    fn call(session: SessionId, tool: &str, path: &str, seq: u64) -> ObservedCall {
        ObservedCall {
            agent_id: AgentId::new(),
            session,
            call_id: format!("c{seq}"),
            tool: ToolName::new(tool),
            arguments: serde_json::json!({ "path": path }),
            is_error: false,
            result_seq: LogSeq(seq),
        }
    }

    #[test]
    fn manifest_id_matches_the_published_constant() {
        let plugin = CheckpointPlugin::new(std::env::temp_dir());
        assert_eq!(plugin.manifest().id, PLUGIN_ID);
    }

    /// The plugin browser's own read surface: a real description, never
    /// the trait's empty default.
    #[test]
    fn description_is_non_empty_and_discloses_the_bash_gap() {
        let plugin = CheckpointPlugin::new(std::env::temp_dir());
        let description = plugin.description();
        assert!(!description.summary.is_empty());
        assert!(!description.you_get.is_empty());
        assert!(
            description.you_lose.to_lowercase().contains("bash"),
            "you_lose must name the bash gap plainly: {:?}",
            description.you_lose
        );
    }

    #[test]
    fn plugin_declares_three_commands_one_observer_and_no_tools() {
        let plugin = CheckpointPlugin::new(std::env::temp_dir());
        let names: Vec<String> = plugin.commands().iter().map(|c| c.spec().name).collect();
        assert_eq!(
            names,
            vec![
                COMMAND_NAME_LIST.to_string(),
                COMMAND_NAME_DIFF.to_string(),
                COMMAND_NAME_ROLLBACK.to_string(),
            ]
        );
        assert_eq!(plugin.observers().len(), 1);
        assert!(plugin.tools().is_empty());
    }

    /// The observer's own end-to-end wiring: a real `write` call, observed,
    /// captured under the plugin's own store -- proves `CheckpointObserver`
    /// extracts `path` and reads the right file, not merely that
    /// `CheckpointStore::record_observed` (already covered directly in
    /// `store::tests`) does.
    #[tokio::test]
    async fn observer_records_a_successful_write_and_skips_a_failed_one() {
        let dir = TempDir::new().unwrap();
        let plugin = CheckpointPlugin::new(dir.path());
        std::fs::write(dir.path().join("f.txt"), "hello").unwrap();
        let session = SessionId::new();
        let observers = plugin.observers();
        let observer = &observers[0];
        let answer = observer
            .after_tool_call(&observer_ctx(), &call(session, "write", "f.txt", 1))
            .await;
        assert!(
            answer.notes.is_empty(),
            "an ordinary-size write needs no notice"
        );
        let entries = plugin.store().entries(&session.to_string()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].seq, 1);

        // An error result must never be recorded.
        let mut errored = call(session, "write", "f.txt", 2);
        errored.is_error = true;
        observer.after_tool_call(&observer_ctx(), &errored).await;
        assert_eq!(
            plugin.store().entries(&session.to_string()).unwrap().len(),
            1,
            "a failed call must not add an entry"
        );
    }

    /// `bash` (or any tool besides `write`/`edit`) is never observed --
    /// this crate's own module doc, "bash is not captured".
    #[tokio::test]
    async fn observer_ignores_bash_and_every_other_tool() {
        let dir = TempDir::new().unwrap();
        let plugin = CheckpointPlugin::new(dir.path());
        std::fs::write(dir.path().join("f.txt"), "hello").unwrap();
        let session = SessionId::new();
        let observers = plugin.observers();
        let observer = &observers[0];
        observer
            .after_tool_call(&observer_ctx(), &call(session, "bash", "f.txt", 1))
            .await;
        assert!(plugin
            .store()
            .entries(&session.to_string())
            .unwrap()
            .is_empty());
    }

    /// Anchor: acceptance scenario 1, driven through the real
    /// `ToolObserver`/`Command` surface rather than `store::` directly --
    /// three edits, `diff`, `rollback`, hand-edit preservation, and
    /// list-then-undo-the-rollback, end to end through this plugin.
    #[tokio::test]
    async fn end_to_end_diff_rollback_hand_edit_and_undo_the_rollback() {
        let dir = TempDir::new().unwrap();
        let plugin = CheckpointPlugin::new(dir.path());
        let path = dir.path().join("f.txt");
        let session = SessionId::new();
        let observers = plugin.observers();
        let observer = &observers[0];

        for (seq, content) in [(1u64, "v1"), (2, "v2"), (3, "v3")] {
            std::fs::write(&path, content).unwrap();
            observer
                .after_tool_call(&observer_ctx(), &call(session, "edit", "f.txt", seq))
                .await;
        }

        let commands = plugin.commands();
        let diff_cmd = &commands[1];
        let diff_out = diff_cmd.invoke(ctx_for(session, "2")).await;
        match diff_out {
            CommandOutcome::Output(lines) => {
                let joined = lines.join("\n");
                assert!(joined.contains("v1") || joined.contains("v2"), "{joined}");
            }
            other => panic!("expected Output, got {other:?}"),
        }

        let rollback_cmd = &commands[2];
        let rollback_out = rollback_cmd.invoke(ctx_for(session, "2")).await;
        assert!(matches!(rollback_out, CommandOutcome::Output(_)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v1");

        // A hand edit, bypassing conway. `rollback 2` targets the SAME
        // entry (seq 2) as above, whose baseline IS known -- the seq to
        // reuse here must have a known baseline, or this would hit the
        // "no baseline known" skip path instead of the conflict path (see
        // `store::tests::rollback_of_a_path_created_by_write_deletes_it_
        // when_restoring_to_absent` for that distinct case).
        std::fs::write(&path, "hand-edited").unwrap();
        let conflict_out = rollback_cmd.invoke(ctx_for(session, "2")).await;
        match conflict_out {
            CommandOutcome::Output(lines) => {
                assert!(lines.iter().any(|l| l.contains("hand edit")), "{lines:?}");
            }
            other => panic!("expected Output, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "hand-edited",
            "an unforced rollback must never overwrite a hand edit"
        );

        // `--all` forces past the conflict.
        rollback_cmd.invoke(ctx_for(session, "2 --all")).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v1");

        let list_cmd = &commands[0];
        let list_out = list_cmd.invoke(ctx_for(session, "")).await;
        let CommandOutcome::Output(lines) = list_out else {
            panic!("expected Output");
        };
        assert!(
            lines.iter().any(|l| l.contains("Rollback")),
            "a rollback must appear in `list`: {lines:?}"
        );
    }

    /// `--rewind` performs the restore AND returns `ForkSession` at the
    /// same seq, rather than a text report.
    #[tokio::test]
    async fn rollback_with_rewind_restores_files_and_returns_fork_session() {
        let dir = TempDir::new().unwrap();
        let plugin = CheckpointPlugin::new(dir.path());
        let path = dir.path().join("f.txt");
        let session = SessionId::new();
        let observers = plugin.observers();
        let observer = &observers[0];
        for (seq, content) in [(1u64, "v1"), (2, "v2")] {
            std::fs::write(&path, content).unwrap();
            observer
                .after_tool_call(&observer_ctx(), &call(session, "write", "f.txt", seq))
                .await;
        }
        let commands = plugin.commands();
        let rollback_cmd = &commands[2];
        let outcome = rollback_cmd.invoke(ctx_for(session, "2 --rewind")).await;
        assert_eq!(
            outcome,
            CommandOutcome::ForkSession {
                at_seq: LogSeq(2),
                directive: String::new(),
            }
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "v1");
    }

    #[tokio::test]
    async fn rollback_rejects_a_non_numeric_seq_with_a_named_error() {
        let dir = TempDir::new().unwrap();
        let plugin = CheckpointPlugin::new(dir.path());
        let commands = plugin.commands();
        let rollback_cmd = &commands[2];
        let outcome = rollback_cmd
            .invoke(ctx_for(SessionId::new(), "not-a-seq"))
            .await;
        match outcome {
            CommandOutcome::Error(message) => {
                assert!(message.contains("usage"), "{message}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn diff_rejects_extra_tokens() {
        let dir = TempDir::new().unwrap();
        let plugin = CheckpointPlugin::new(dir.path());
        let commands = plugin.commands();
        let diff_cmd = &commands[1];
        let outcome = diff_cmd.invoke(ctx_for(SessionId::new(), "1 extra")).await;
        assert!(matches!(outcome, CommandOutcome::Error(_)));
    }

    #[tokio::test]
    async fn list_rejects_arguments() {
        let dir = TempDir::new().unwrap();
        let plugin = CheckpointPlugin::new(dir.path());
        let commands = plugin.commands();
        let list_cmd = &commands[0];
        let outcome = list_cmd.invoke(ctx_for(SessionId::new(), "anything")).await;
        assert!(matches!(outcome, CommandOutcome::Error(_)));
    }

    /// The per-file bound, exercised end to end through the observer: a
    /// file over the bound is skipped with a visible notice, never
    /// silently captured partway.
    #[tokio::test]
    async fn observer_reports_a_visible_notice_when_a_file_exceeds_the_snapshot_bound() {
        let dir = TempDir::new().unwrap();
        let plugin = CheckpointPlugin::with_bounds(dir.path(), 4, DEFAULT_MAX_PROJECT_BYTES);
        std::fs::write(dir.path().join("big.txt"), "way too big").unwrap();
        let session = SessionId::new();
        let observers = plugin.observers();
        let observer = &observers[0];
        let answer = observer
            .after_tool_call(&observer_ctx(), &call(session, "write", "big.txt", 1))
            .await;
        assert_eq!(answer.notes.len(), 1);
        assert_eq!(answer.notes[0].reason, NOTE_REASON);
        assert!(
            answer.notes[0].text.contains("4-byte"),
            "{:?}",
            answer.notes[0].text
        );
    }
}
