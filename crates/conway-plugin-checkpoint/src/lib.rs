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
//! # What this plugin observes, and how the first-edit gap closed
//!
//! A `Plugin` originally got exactly one seam for watching a tool call
//! in-process: [`ToolObserver::after_tool_call`], which fires **after** a
//! call's result is already durable -- the file has already been written by
//! the time this plugin sees the call. At the time this plugin first
//! shipped, that was a genuine gap: the only `pre_tool_use` registration
//! surface a plugin has (`PluginHookRule`, `Plugin::hooks`) dispatches
//! through a SPAWNED SUBPROCESS (`docs/plugins/hooks.md` point 13), the
//! same mechanism a declarative `[hooks].rules[]` entry uses -- built for
//! wrapping an operator's own script, not for a first-party Rust plugin's
//! own file I/O, and a poor fit for a snapshot that has to run on every
//! single edit. That gap was reported rather than routed around (this
//! plugin's own original "what not to build"), and board item
//! `01M20RYAK1T1DK7XWX431FFCYQ` is the follow-up that closed it:
//! [`ToolObserver::before_tool_call`] now runs after the permission
//! decision resolves and before the write/edit actually happens, so
//! `CheckpointObserver` (below) can read a path's real bytes at the one
//! moment they are still the ORIGINAL ones.
//!
//! `old` (what a path looked like immediately before one checkpoint entry)
//! therefore has two sources now, tried in this order:
//!
//! 1. CHAINED: the `new` this store captured for that same path's most
//!    recent EARLIER entry, if this session's checkpoint history has one.
//!    Still the normal case from a path's SECOND touch onward, and still
//!    correct there -- `before_tool_call` does not re-read the path in that
//!    case (see `CheckpointObserver::before_tool_call`'s own doc).
//! 2. REAL: for a path's FIRST observed touch in a session, the bytes
//!    `before_tool_call` read directly off disk before the write ran.
//!
//! Only when NEITHER is available (no pre-call seam ran for this call, or
//! it could not read the path, e.g. a permissions error) does an entry fall
//! back to [`SnapshotRef::Unavailable`] -- still represented explicitly,
//! never guessed at, for that genuinely-unrecoverable remainder.
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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use conway::plugin::{
    async_trait, Command, CommandCtx, CommandOutcome, CommandSpec, ObservedCall, ObserverAnswer,
    ObserverCtx, ObserverNote, PendingCall, Plugin, PluginDescription, PluginManifest, Tool,
    ToolObserver,
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
/// doc, "What this plugin observes, and how the first-edit gap closed", for
/// why only `write`/`edit` are inspected and for the two sources `old` now
/// has.
struct CheckpointObserver {
    store: Arc<CheckpointStore>,
    max_snapshot_bytes: u64,
    max_project_bytes: u64,
    /// A path's real pre-write bytes, captured by `before_tool_call` for a
    /// path this session has not touched yet, keyed by `call_id` and
    /// consumed by the matching `after_tool_call` for that SAME call.
    /// `call_id` (not `path`) is the key because it is what ties one
    /// observer's pre-call sighting of a call to its own post-call sighting
    /// of the SAME call (`PendingCall::call_id`/`ObservedCall::call_id`
    /// share the one id).
    pending_baseline: Mutex<HashMap<String, SnapshotRef>>,
}

impl CheckpointObserver {
    /// `write`/`edit` map to a [`ToolKind`]; every other tool (`bash`
    /// included -- this crate's own module doc, "`bash` is not captured")
    /// is `None`. Shared by both halves of [`ToolObserver`] so the tool
    /// filter cannot drift between them.
    fn tool_kind(tool: &str) -> Option<ToolKind> {
        match tool {
            TOOL_WRITE => Some(ToolKind::Write),
            TOOL_EDIT => Some(ToolKind::Edit),
            _ => None,
        }
    }
}

#[async_trait]
impl ToolObserver for CheckpointObserver {
    /// Reads a path's TRUE pre-write bytes, while they are still true --
    /// see this crate's own module doc for why this is the seam that makes
    /// that possible at all. Only does the read for a path's FIRST touch
    /// this session ([`CheckpointStore::latest_for_path`] returns `None`):
    /// from the second touch onward, `after_tool_call`'s own chain already
    /// recovers the true baseline (the prior entry's `new`), so reading
    /// here again would only duplicate that work for no benefit.
    ///
    /// This is observation, not a gate: nothing here can refuse, alter, or
    /// delay the call (`ToolObserver::before_tool_call`'s own doc) -- a
    /// read failure just means `after_tool_call` falls back to its
    /// pre-existing [`SnapshotRef::Unavailable`] behavior for this path.
    async fn before_tool_call(&self, _ctx: &ObserverCtx, call: &PendingCall) {
        if Self::tool_kind(call.tool.as_str()).is_none() {
            return;
        }
        let Some(raw_path) = call.arguments.get("path").and_then(|v| v.as_str()) else {
            return;
        };
        let path = self.store.resolve(raw_path);
        let session = call.session.to_string();
        if !matches!(self.store.latest_for_path(&session, &path), Ok(None)) {
            // Either a prior entry already exists for this path (the chain
            // already has the true baseline), or the read itself failed --
            // either way, nothing to capture here.
            return;
        }
        let baseline = match std::fs::read(&path) {
            Ok(bytes) => self
                .store
                .capture(&bytes, self.max_snapshot_bytes, self.max_project_bytes)
                .ok()
                .map(|(snapshot, _notice)| snapshot),
            // A positive fact, not a guess: this store read the filesystem
            // itself, synchronously, before the write ran -- the same
            // justification `SnapshotRef::Absent`'s own doc gives for a
            // rollback's `old`/`new`.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Some(SnapshotRef::Absent),
            Err(_) => None,
        };
        if let Some(snapshot) = baseline {
            self.pending_baseline
                .lock()
                .expect("pending_baseline lock poisoned")
                .insert(call.call_id.clone(), snapshot);
        }
    }

    async fn after_tool_call(&self, _ctx: &ObserverCtx, call: &ObservedCall) -> ObserverAnswer {
        // A failed call never mutated the filesystem -- nothing to
        // snapshot (and the write/edit tools' own `path_args` contract
        // gives no guarantee `arguments["path"]` is even meaningful on an
        // error path).
        if call.is_error {
            return ObserverAnswer::default();
        }
        let Some(tool) = Self::tool_kind(call.tool.as_str()) else {
            // Every other tool, `bash` included -- see this crate's own
            // module doc, "`bash` is not captured, and this says so".
            return ObserverAnswer::default();
        };
        let Some(raw_path) = call.arguments.get("path").and_then(|v| v.as_str()) else {
            return ObserverAnswer::default();
        };
        let path = self.store.resolve(raw_path);
        let session = call.session.to_string();
        // Consumed (removed), not just read: a stale entry left behind
        // under this `call_id` could never be read again anyway (call ids
        // are not reused), so this also keeps the map from growing
        // unbounded over a long session.
        let pre_captured = self
            .pending_baseline
            .lock()
            .expect("pending_baseline lock poisoned")
            .remove(&call.call_id);
        let recorded = self.store.record_observed(
            &session,
            call.result_seq.0,
            &path,
            tool,
            pre_captured,
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
///
/// # The empty listing says whose emptiness it is
///
/// Board item `01M2TWC242P96Z3JDXWC9F3R5E`. This store is keyed by session
/// id, and conway writes one session per AGENT, so "no snapshots" is always
/// a statement about ONE agent's record -- never about the project. Typed
/// as a bare shell subcommand (`conway conway.checkpoint.list`) with no
/// `--session`, it is a statement about a session conway minted for the
/// duration of the command, which has necessarily recorded nothing.
///
/// The old wording ("no snapshots recorded yet for this session") was true
/// every time and still misled, because the reader supplying "this session"
/// meant the one they had just killed. So the empty arm now names the
/// session id it is answering for and names the flag that addresses a
/// different one -- the whole difference between "there is nothing to roll
/// back" and "you asked the wrong session".
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
                format!("conway.checkpoint: no snapshots recorded yet for session {session}"),
                "(that is this ONE session's record. conway keeps one session per agent, so a \
                 delegated worker's writes are under its own session id: `conway sessions list` \
                 names them, and `conway conway.checkpoint.list --session <id-or-name>` reads \
                 one of them.)"
                    .to_string(),
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
///
/// # Direction, and why both sides are labelled
///
/// This is a preview of the ROLLBACK, not a replay of the write that
/// produced the snapshot: the `-` side is what is on disk right now and
/// the `+` side is what `rollback <seq>` would leave behind. That is the
/// orientation `diff -u <before> <after>` has for the operation the
/// operator is about to run, and the one the summary string and
/// `docs/plugins/checkpoint.md` both promise.
///
/// It rendered the other way round until board item
/// `01M2V5ZXJMMVFF0MVHYZ4HXAT8`, which matters more than a cosmetic slip:
/// this is the last thing an operator reads before deciding to destroy the
/// current contents of a file, so a reversed preview tells them rollback
/// will produce exactly what it is about to discard. Because both headers
/// carried the same bare path, nothing on screen disambiguated the
/// direction either -- hence the `(current)` / `(after rollback to seq N)`
/// labels below, which make the direction legible without reading this
/// source.
///
/// # A conflicted path gets a different label, not the same preview
///
/// A plain `/conway.checkpoint.rollback <seq>` REFUSES a path whose bytes on
/// disk right now disagree with what conway itself last wrote there (a hand
/// edit since) -- `rollback`'s own three-way check, read-only here via
/// [`CheckpointStore::hand_edit_conflict`]. Before this doc's own item, this
/// command rendered that path's diff exactly like any other, labelled
/// `(after rollback to seq N)` -- which is a lie for a conflicted path: an
/// unforced `rollback <seq>` would not produce that content at all, it would
/// refuse the path outright and leave the hand edit in place. A conflicted
/// path now gets an explicit notice line first, and its diff (still shown,
/// since the content IS what `--all` would produce) is labelled
/// accordingly -- never presented as what the plain command does.
struct DiffCommand {
    store: Arc<CheckpointStore>,
}

#[async_trait]
impl Command for DiffCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: COMMAND_NAME_DIFF.to_string(),
            summary: "shows, as a unified diff per path, what `rollback <seq>` would change: \
                      `-` is the file as it stands now, `+` is what the rollback would restore \
                      -- e.g. `/conway.checkpoint.diff 42`"
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
                    Some(
                        "(this path did not exist before this checkpoint -- rolling back \
                         DELETES it)",
                    ),
                ),
                ResolvedRef::Missing(reason) => {
                    lines.push(format!("{}: {reason}", entry.path));
                    continue;
                }
            };
            let current_bytes = std::fs::read(&entry.path).unwrap_or_default();
            // Is this a path a PLAIN `rollback <seq>` would actually refuse?
            // Same question `rollback` itself asks (`CheckpointStore::
            // hand_edit_conflict`'s own doc) -- never a second, separately
            // worded guess at the same rule.
            let conflict = match self
                .store
                .hand_edit_conflict(&session, Path::new(&entry.path))
            {
                Ok(conflict) => conflict,
                Err(err) => {
                    lines.push(format!(
                        "{}: error checking for a hand edit: {err}",
                        entry.path
                    ));
                    continue;
                }
            };
            if conflict {
                lines.push(format!(
                    "{}: rollback will not touch this file -- a hand edit was made since \
                     conway.checkpoint's last recorded write; `--all` would discard it and \
                     restore the content below",
                    entry.path
                ));
            }
            // The `-` side is the CURRENT file and the `+` side is the
            // rollback target -- see this command's own doc comment for
            // why that direction, and not its inverse, is the one a
            // preview of `rollback <seq>` has to render.
            let old_text = String::from_utf8_lossy(&current_bytes);
            let new_text = String::from_utf8_lossy(&target_bytes);
            let old_label = format!("{} (current)", entry.path);
            let new_label = if conflict {
                format!(
                    "{} (after `rollback {seq} --all` -- a plain rollback refuses this path)",
                    entry.path
                )
            } else {
                format!("{} (after rollback to seq {seq})", entry.path)
            };
            let mut rendered = diff::unified_diff(&old_label, &new_label, &old_text, &new_text);
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
///
/// # A conflict is reported as a refusal, not a success
///
/// When `report.conflicts` is non-empty (at least one touched path kept its
/// hand edit rather than being restored), this returns [`CommandOutcome::
/// Error`], never [`CommandOutcome::Output`] -- even when OTHER paths in the
/// same invocation restored cleanly. Two call sites read this outcome, and
/// both needed the same fix:
///
/// - **Headless** (`conway conway.checkpoint.rollback ...`,
///   `conway_cli::commands::plugin::run`): `CommandOutcome::Output` always
///   exits 0; a scripted caller that rolled back a path with a surviving
///   hand edit saw a success and moved on, the exact lie board item
///   `01M3SJBNF2KZRWA5P5SSF9B868`'s own evidence names. `CommandOutcome::
///   Error` maps to a non-zero exit there, with no change needed to that
///   call site at all.
/// - **The TUI**: `CommandOutcome::Error` is what reads as a refusal rather
///   than an ordinary notice.
///
/// `--rewind` is therefore never honored when a conflict occurred: forking
/// the conversation at `seq` while a conflicted path was left UNrestored
/// would claim the files reached that point together with the conversation,
/// when they did not. The two-command recipe
/// (`/conway.checkpoint.rollback <seq>` then, once every conflict is
/// resolved, `/conway.history.rewind <seq>`) still works, and is now the
/// only way to compose the two over a conflicted path at all -- `--all` or
/// a narrower `<path>` retry makes the rollback itself conflict-free first.
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
                "{}: rollback refused -- a hand edit was made since conway.checkpoint's last \
                 recorded write; pass --all to force (this discards that edit)",
                conflicted.path
            ));
        }
        for (path, reason) in &report.skipped {
            lines.push(format!("{path}: {reason}"));
        }

        if !report.conflicts.is_empty() {
            // A refused rollback is a refusal -- see this struct's own doc,
            // "A conflict is reported as a refusal, not a success", for why
            // this is `Error` (never `Output`, never honoring `--rewind`)
            // even when other paths in this same invocation DID restore.
            return CommandOutcome::Error(lines.join("\n"));
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
    /// relative operator-typed `<path>` resolves against, and -- the OLD,
    /// pre-item-`01M3SJBNF2KZRWA5P5SSF9B868` default, still here for an
    /// embedder with no env-aware resolution of its own -- the parent of
    /// this plugin's shadow store, at `<cwd>/.conway/checkpoints`. **The
    /// real `conway-cli` build does not call this**: it calls [`Self::
    /// with_root`] instead, with a location resolved by `conway::config::
    /// discovery::checkpoint_store_root` (env-aware: the operator's own
    /// config directory, keyed by the enclosing git root, never inside the
    /// project) -- see that function's own doc for why an in-project
    /// store dirtied `git status` and split one project's undo history
    /// across however many directories it was launched from. Uses
    /// [`DEFAULT_MAX_SNAPSHOT_BYTES`]/[`DEFAULT_MAX_PROJECT_BYTES`]; see
    /// [`Self::with_bounds`] for a caller that wants different bounds.
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
        Self::with_root_and_bounds(root, cwd, max_snapshot_bytes, max_project_bytes)
    }

    /// Like [`Self::new`], but `root` -- where the shadow store itself
    /// lives -- is resolved by the CALLER rather than derived from `cwd`.
    /// `conway-cli`'s own production wiring (`first_party_plugins::
    /// checkpoint_plugin`) is the one real caller: it resolves `root` via
    /// `conway::config::discovery::checkpoint_store_root(cwd, env)`, which
    /// is why this constructor -- not [`Self::new`] -- is what the shipped
    /// binary actually installs. Uses [`DEFAULT_MAX_SNAPSHOT_BYTES`]/
    /// [`DEFAULT_MAX_PROJECT_BYTES`]; see [`Self::with_root_and_bounds`]
    /// for explicit bounds.
    pub fn with_root(root: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Self::with_root_and_bounds(
            root,
            cwd,
            DEFAULT_MAX_SNAPSHOT_BYTES,
            DEFAULT_MAX_PROJECT_BYTES,
        )
    }

    /// [`Self::with_root`] with explicit bounds -- the fullest constructor,
    /// every other one on this type delegates to it.
    ///
    /// Exactly one thing happens here, beyond opening the store, best-effort
    /// and disclosed rather than silently assumed: **a one-time migration
    /// off the OLD in-project location** (`<cwd>/.conway/checkpoints`), when
    /// `root` itself is a DIFFERENT path and nothing exists at `root` yet. A
    /// plain `fs::rename` -- cheap, and correct for the overwhelmingly
    /// common case (the old and new locations are on the same filesystem,
    /// which they are whenever `CONWAY_CONFIG_DIR`/the operator's home
    /// directory and the project live on the same disk). If that rename
    /// fails for any reason -- most plausibly a cross-device move, e.g. a
    /// project on a network mount with a local home directory -- the old
    /// store is left exactly where it is, undiscovered by this or any later
    /// `CheckpointPlugin` construction: this plugin's own shadow store is
    /// transient undo data, not something worth a second, more elaborate
    /// migration path (a dual-root reader, a background copy) over. An
    /// operator in that position keeps old snapshots reachable the old way
    /// (reading `<cwd>/.conway/checkpoints` directly, or constructing a
    /// `CheckpointStore` against it by hand) and gets new ones at the new
    /// location from here on. Best-effort (`let _ =`) for the identical
    /// reason this plugin's observer never fails a tool call over a local
    /// filesystem problem -- a permissions error here should degrade to "no
    /// migration," never to a `CheckpointPlugin` that fails to construct at
    /// all. This step only ever touches disk when a legacy store is
    /// actually there to move (`migrate_legacy_in_project_store`'s own
    /// early-return doc) -- a fresh project triggers no write here.
    ///
    /// Construction itself never creates `root`, and never writes its
    /// self-ignoring `.gitignore` (`*`) -- both are deferred to
    /// `CheckpointStore::put_blob`/`append_index`, the store's own first
    /// real write, via `CheckpointStore::ensure_root`. That is what makes a
    /// `CheckpointPlugin` constructed but never written through (`conway
    /// doctor`'s own case, among every other read-only dispatch target)
    /// create nothing at all.
    pub fn with_root_and_bounds(
        root: impl Into<PathBuf>,
        cwd: impl Into<PathBuf>,
        max_snapshot_bytes: u64,
        max_project_bytes: u64,
    ) -> Self {
        let cwd = cwd.into();
        let root = root.into();
        Self::migrate_legacy_in_project_store(&cwd, &root);
        Self {
            store: Arc::new(CheckpointStore::new(root, cwd)),
            max_snapshot_bytes,
            max_project_bytes,
        }
    }

    /// See [`Self::with_root_and_bounds`]'s own doc, point 1, for the full
    /// contract -- this is just the mechanism: a plain, best-effort
    /// `fs::rename` of `<cwd>/.conway/checkpoints` onto `root`, run only
    /// when the two paths actually differ and `root` does not already
    /// exist (never clobbers a store that is already live at the new
    /// location, e.g. on this plugin's SECOND construction in the same
    /// project).
    fn migrate_legacy_in_project_store(cwd: &Path, root: &Path) {
        let legacy = cwd.join(".conway").join("checkpoints");
        if legacy == root || root.exists() || !legacy.exists() {
            return;
        }
        if let Some(parent) = root.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::rename(&legacy, root);
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
                       carry for the identical reason"
                .to_string(),
            costs: format!(
                "disk under {}, bounded to {} bytes per snapshotted file and {} bytes total per \
                 project -- both a skip and an eviction get a visible notice rather than \
                 growing past the bound silently",
                self.store.root().display(),
                self.max_snapshot_bytes,
                self.max_project_bytes
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
            pending_baseline: Mutex::new(HashMap::new()),
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

    /// The pre-call sibling of [`call`] -- SAME `call_id` for the same
    /// `seq`, so a test that calls both against the same `seq` exercises
    /// one observer's pre-call and post-call sighting of one real call,
    /// exactly as the runtime pairs them (`PendingCall::call_id`/
    /// `ObservedCall::call_id`).
    fn pending(session: SessionId, tool: &str, path: &str, seq: u64) -> PendingCall {
        PendingCall {
            agent_id: AgentId::new(),
            session,
            call_id: format!("c{seq}"),
            tool: ToolName::new(tool),
            arguments: serde_json::json!({ "path": path }),
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

    /// Board item `01M20RYAK1T1DK7XWX431FFCYQ`: the first-edit limitation
    /// is retired now that `before_tool_call` closes it -- `you_lose` must
    /// no longer claim a first edit cannot be rolled back, while the
    /// (still-true) `bash` limitation stays (covered by the test above).
    #[test]
    fn description_no_longer_discloses_a_first_edit_limit() {
        let plugin = CheckpointPlugin::new(std::env::temp_dir());
        let description = plugin.description();
        let lower = description.you_lose.to_lowercase();
        assert!(
            !lower.contains("first"),
            "the first-edit limitation must be retired: {:?}",
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

    /// Item `01M3SJBNF2KZRWA5P5SSF9B868`, point 3: `with_root` resolves to
    /// `root`, not `<cwd>/.conway/checkpoints` -- `description()`'s own
    /// `costs` string names the REAL directory, so this doubles as the
    /// regression guard for that string too.
    #[test]
    fn with_root_uses_the_given_root_not_cwd_dot_conway_checkpoints() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("elsewhere").join("store");
        let plugin = CheckpointPlugin::with_root(&root, dir.path());
        assert_eq!(plugin.store().root(), root);
        assert!(
            !dir.path().join(".conway").join("checkpoints").exists(),
            "with_root must never also create the old in-project location"
        );
        assert!(
            plugin
                .description()
                .costs
                .contains(&root.display().to_string()),
            "the operator-facing costs string must name the REAL root: {:?}",
            plugin.description().costs
        );
    }

    /// Board item `01M488BT2JE9ZMNANPWCBG5QSQ`: `with_root` alone -- no
    /// write through the store -- must create NOTHING, so a read-only
    /// caller (`conway doctor`, among every other read-only dispatch
    /// target) that constructs a `CheckpointPlugin` but never calls
    /// `record_observed`/`rollback` leaves no trace on disk.
    #[test]
    fn with_root_alone_creates_nothing_on_disk() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("store");
        let _plugin = CheckpointPlugin::with_root(&root, dir.path());
        assert!(
            !root.exists(),
            "construction alone must never create the store root"
        );
    }

    /// Point 3's self-ignoring `.gitignore` backstop -- written into `root`
    /// regardless of where `root` resolves, so even the rare in-project
    /// fallback (`checkpoint_store_root`'s own doc: no home directory
    /// discoverable) cannot dirty `git status`. Board item
    /// `01M488BT2JE9ZMNANPWCBG5QSQ`: deferred to the store's first real
    /// write (construction alone creates nothing -- the test above), so
    /// this one drives an actual write before checking for it.
    #[test]
    fn with_root_writes_a_self_ignoring_gitignore_on_its_first_real_write() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("store");
        let plugin = CheckpointPlugin::with_root(&root, dir.path());
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "v1").unwrap();
        plugin
            .store()
            .record_observed(
                "sess-gitignore",
                1,
                &path,
                ToolKind::Write,
                None,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();
        let gitignore = std::fs::read_to_string(root.join(".gitignore"))
            .expect("the first real write must write a .gitignore into the store root");
        assert_eq!(gitignore, "*\n");
    }

    /// The `.gitignore` is seeded once: an operator's edit to it survives
    /// every later write the store makes.
    #[test]
    fn an_edited_gitignore_survives_later_writes() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("store");
        let plugin = CheckpointPlugin::with_root(&root, dir.path());
        let path = dir.path().join("f.txt");
        let record = |turn: u64| {
            plugin
                .store()
                .record_observed(
                    "sess-gitignore-edit",
                    turn,
                    &path,
                    ToolKind::Write,
                    None,
                    DEFAULT_MAX_SNAPSHOT_BYTES,
                    DEFAULT_MAX_PROJECT_BYTES,
                )
                .unwrap();
        };
        std::fs::write(&path, "v1").unwrap();
        record(1);
        std::fs::write(root.join(".gitignore"), "*\n!keep.txt\n").unwrap();
        std::fs::write(&path, "v2").unwrap();
        record(2);
        assert_eq!(
            std::fs::read_to_string(root.join(".gitignore")).unwrap(),
            "*\n!keep.txt\n"
        );
    }

    /// Point 3's migration contract: an existing in-project store
    /// (`<cwd>/.conway/checkpoints`, what every `CheckpointPlugin::new`
    /// caller used before this item) is moved, once, to wherever `with_root`
    /// resolves -- so a session's already-recorded snapshots are not
    /// stranded by the relocation.
    #[test]
    fn with_root_migrates_an_existing_in_project_store_once() {
        let dir = TempDir::new().unwrap();
        let legacy_root = dir.path().join(".conway").join("checkpoints");
        let old_plugin = CheckpointPlugin::new(dir.path());
        assert_eq!(old_plugin.store().root(), legacy_root);
        let session = "sess-migrated";
        let target = dir.path().join("f.txt");
        std::fs::write(&target, "v1").unwrap();
        old_plugin
            .store()
            .record_observed(
                session,
                1,
                &target,
                ToolKind::Write,
                None,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();
        assert!(legacy_root.join("sessions").join(session).exists());

        let new_root = dir.path().join("relocated").join("store");
        let migrated_plugin = CheckpointPlugin::with_root(&new_root, dir.path());
        assert!(
            !legacy_root.exists(),
            "the legacy store must be MOVED, not copied -- nothing left behind at the old \
             location on a successful rename"
        );
        let entries = migrated_plugin
            .store()
            .entries(session)
            .expect("read the migrated session's entries");
        assert_eq!(
            entries.len(),
            1,
            "the migrated session's own snapshot must still be there, at the new location"
        );
    }

    /// The other half of the migration contract: a SECOND `with_root` call
    /// against a project that already has a live store at the new location
    /// must never overwrite it with whatever the legacy directory still
    /// holds (e.g. from before an earlier migration already ran).
    #[test]
    fn with_root_never_clobbers_an_existing_store_at_the_new_location() {
        let dir = TempDir::new().unwrap();
        let new_root = dir.path().join("relocated").join("store");

        // A session already recorded at the NEW location.
        let first = CheckpointPlugin::with_root(&new_root, dir.path());
        let target = dir.path().join("f.txt");
        std::fs::write(&target, "already-at-new-location").unwrap();
        first
            .store()
            .record_observed(
                "sess-new",
                1,
                &target,
                ToolKind::Write,
                None,
                DEFAULT_MAX_SNAPSHOT_BYTES,
                DEFAULT_MAX_PROJECT_BYTES,
            )
            .unwrap();

        // A STALE legacy directory also exists (e.g. left behind by a
        // process that never got to migrate, or recreated after the fact).
        let legacy_root = dir.path().join(".conway").join("checkpoints");
        std::fs::create_dir_all(legacy_root.join("sessions").join("sess-legacy")).unwrap();

        let second = CheckpointPlugin::with_root(&new_root, dir.path());
        assert!(
            second
                .store()
                .entries("sess-new")
                .expect("read sess-new")
                .iter()
                .any(|e| e.path.ends_with("f.txt")),
            "the already-migrated session must survive a second construction untouched"
        );
        assert!(
            legacy_root.exists(),
            "a stale legacy directory must be left alone once the new location is already live"
        );
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

    /// `before_tool_call` gets the SAME tool filter as `after_tool_call` --
    /// a `bash` call must never populate `pending_baseline`, or a later
    /// unrelated call to a `write`/`edit` sharing the same `call_id` space
    /// (never happens in practice, but this is the seam that would let a
    /// stray entry leak) could pick it up by accident.
    #[tokio::test]
    async fn before_tool_call_ignores_bash_and_every_other_tool() {
        let dir = TempDir::new().unwrap();
        let plugin = CheckpointPlugin::new(dir.path());
        std::fs::write(dir.path().join("f.txt"), "hello").unwrap();
        let session = SessionId::new();
        let observers = plugin.observers();
        let observer = &observers[0];
        observer
            .before_tool_call(&observer_ctx(), &pending(session, "bash", "f.txt", 1))
            .await;
        std::fs::write(dir.path().join("f.txt"), "changed").unwrap();
        observer
            .after_tool_call(&observer_ctx(), &call(session, "bash", "f.txt", 1))
            .await;
        assert!(plugin
            .store()
            .entries(&session.to_string())
            .unwrap()
            .is_empty());
    }

    /// Board item `01M20RYAK1T1DK7XWX431FFCYQ`, ACCEPTANCE 1: a file's
    /// FIRST edit in a session can now be rolled back to its TRUE original
    /// bytes. Drives `before_tool_call` and `after_tool_call` in the exact
    /// order the runtime calls them (`ToolRunner::execute_one`): pre-call
    /// seam while the path still holds its original bytes, then the write
    /// itself, then the post-call seam -- so this exercises the real
    /// two-hook contract, not just `CheckpointStore::record_observed`
    /// directly (that is `store::tests`' own job).
    #[tokio::test]
    async fn before_tool_call_lets_a_first_edit_roll_back_to_its_true_original_bytes() {
        let dir = TempDir::new().unwrap();
        let plugin = CheckpointPlugin::new(dir.path());
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "original").unwrap();
        let session = SessionId::new();
        let observers = plugin.observers();
        let observer = &observers[0];

        observer
            .before_tool_call(&observer_ctx(), &pending(session, "write", "f.txt", 1))
            .await;
        // The write itself, exactly as `ToolRunner::execute_one` would run
        // it between calling `before_tool_call` and `after_tool_call`.
        std::fs::write(&path, "changed").unwrap();
        observer
            .after_tool_call(&observer_ctx(), &call(session, "write", "f.txt", 1))
            .await;

        let entries = plugin.store().entries(&session.to_string()).unwrap();
        assert_eq!(entries.len(), 1);
        let ResolvedRef::Bytes(old_bytes) = plugin.store().resolve_ref(&entries[0].old).unwrap()
        else {
            panic!(
                "expected a resolvable baseline for a FIRST edit, got {:?}",
                entries[0].old
            );
        };
        assert_eq!(
            old_bytes, b"original",
            "the pre-call seam must have captured the TRUE pre-write bytes, not the post-write \
             ones a chain-only implementation would be stuck with"
        );

        // End to end through the real command surface: `rollback 1`
        // restores the true original, not a skip.
        let commands = plugin.commands();
        let rollback_cmd = &commands[2];
        let outcome = rollback_cmd.invoke(ctx_for(session, "1")).await;
        match outcome {
            CommandOutcome::Output(lines) => {
                assert!(
                    lines.iter().any(|l| l.contains("restored")),
                    "expected a restore, not a skip: {lines:?}"
                );
            }
            other => panic!("expected Output, got {other:?}"),
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
    }

    /// P-15's own bar: the test above alone cannot prove the PRE-CALL seam
    /// is what produced the resolvable baseline, rather than some other
    /// change to `record_observed`'s fallback. This is the contrast: the
    /// identical scenario, MINUS the `before_tool_call` call, must still
    /// fall back to `Unavailable` exactly as it did before this item --
    /// proving the seam above, not a changed default, is what did it.
    #[tokio::test]
    async fn without_before_tool_call_a_first_edit_still_falls_back_to_unavailable() {
        let dir = TempDir::new().unwrap();
        let plugin = CheckpointPlugin::new(dir.path());
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "original").unwrap();
        let session = SessionId::new();
        let observers = plugin.observers();
        let observer = &observers[0];

        // No `before_tool_call` call at all -- the pre-item shape.
        std::fs::write(&path, "changed").unwrap();
        observer
            .after_tool_call(&observer_ctx(), &call(session, "write", "f.txt", 1))
            .await;

        let entries = plugin.store().entries(&session.to_string()).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(
            matches!(entries[0].old, SnapshotRef::Unavailable { .. }),
            "without before_tool_call, a first touch must still fall back to Unavailable: {:?}",
            entries[0].old
        );
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

        // Deliberately NOT `v1`/`v2`/`v3`: the fixture content has to make
        // the two sides of the diff distinguishable, or an assertion on it
        // passes whichever way round the diff renders -- which is exactly
        // how the inversion board item `01M2V5ZXJMMVFF0MVHYZ4HXAT8` fixed
        // survived this test.
        for (seq, content) in [
            (1u64, "OLDEST-BYTES"),
            (2, "MIDDLE-BYTES"),
            (3, "NEWEST-BYTES"),
        ] {
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
                let rendered: Vec<&str> = joined.lines().collect();
                // `diff 2` previews `rollback 2`: on disk now is
                // `NEWEST-BYTES`, and rolling back to the state before seq
                // 2 restores `OLDEST-BYTES`. So the `-` side is the
                // CURRENT bytes and the `+` side is the restored ones.
                // Asserting both the presence of each expected side AND
                // the absence of its inverse is what makes a flip fail
                // here rather than pass silently.
                assert!(
                    rendered.contains(&"-NEWEST-BYTES"),
                    "the `-` side must be the current file: {joined}"
                );
                assert!(
                    rendered.contains(&"+OLDEST-BYTES"),
                    "the `+` side must be what rollback would restore: {joined}"
                );
                assert!(
                    !rendered.contains(&"+NEWEST-BYTES") && !rendered.contains(&"-OLDEST-BYTES"),
                    "the diff is rendered backwards -- this previews the ROLLBACK, not the \
                     write that made the snapshot: {joined}"
                );
                // And the direction is legible on screen, not only to a
                // reader of this source: identical headers on both sides
                // disambiguate nothing.
                assert!(
                    joined.contains("(current)"),
                    "the `---` side must be labelled: {joined}"
                );
                assert!(
                    joined.contains("(after rollback to seq 2)"),
                    "the `+++` side must name the rollback it previews: {joined}"
                );
            }
            other => panic!("expected Output, got {other:?}"),
        }

        let rollback_cmd = &commands[2];
        let rollback_out = rollback_cmd.invoke(ctx_for(session, "2")).await;
        assert!(matches!(rollback_out, CommandOutcome::Output(_)));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "OLDEST-BYTES",
            "the rollback must land on exactly the bytes `diff` put on its `+` side"
        );

        // A hand edit, bypassing conway. `rollback 2` targets the SAME
        // entry (seq 2) as above, whose baseline IS known -- the seq to
        // reuse here must have a known baseline, or this would hit the
        // "no baseline known" skip path instead of the conflict path (see
        // `store::tests::rollback_of_a_path_created_by_write_deletes_it_
        // when_restoring_to_absent` for that distinct case).
        std::fs::write(&path, "hand-edited").unwrap();
        let conflict_out = rollback_cmd.invoke(ctx_for(session, "2")).await;
        match conflict_out {
            // Item `01M3SJBNF2KZRWA5P5SSF9B868`: a conflict is a refusal,
            // never `Output` -- see `RollbackCommand`'s own doc, "A
            // conflict is reported as a refusal, not a success".
            CommandOutcome::Error(message) => {
                assert!(message.contains("hand edit"), "{message}");
                assert!(message.contains("refused"), "{message}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "hand-edited",
            "an unforced rollback must never overwrite a hand edit"
        );

        // `--all` forces past the conflict.
        rollback_cmd.invoke(ctx_for(session, "2 --all")).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "OLDEST-BYTES");

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

    /// Item `01M3SJBNF2KZRWA5P5SSF9B868`: `--rewind` must never fork past a
    /// conflict -- a refused rollback stays refused, not silently folded
    /// into a conversation fork that claims the files reached `seq` when a
    /// hand edit actually kept one of them exactly where it was.
    #[tokio::test]
    async fn rollback_with_rewind_refuses_rather_than_forking_past_a_conflict() {
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
        std::fs::write(&path, "hand-edited").unwrap();

        let commands = plugin.commands();
        let rollback_cmd = &commands[2];
        let outcome = rollback_cmd.invoke(ctx_for(session, "2 --rewind")).await;
        match outcome {
            CommandOutcome::Error(message) => {
                assert!(message.contains("refused"), "{message}");
            }
            other => panic!("expected Error (a refusal), got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "hand-edited",
            "a refused rollback must never write the file, --rewind or not"
        );
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

    /// The other direction-sensitive case: a path the model CREATED has no
    /// prior content, so rolling back deletes it. Previewing that rollback
    /// must show the file's current lines going away (`-`), with nothing
    /// arriving -- the exact inverse of what "show me what this write did"
    /// would render, and the case where reading the preview backwards is
    /// most dangerous (it would look like the file is about to be
    /// created).
    #[tokio::test]
    async fn diff_of_a_model_created_path_previews_its_deletion() {
        let dir = TempDir::new().unwrap();
        let plugin = CheckpointPlugin::new(dir.path());
        let path = dir.path().join("created.txt");
        let session = SessionId::new();
        let observers = plugin.observers();
        let observer = &observers[0];

        // The pre-call seam sees no file at all -> `SnapshotRef::Absent`.
        observer
            .before_tool_call(
                &observer_ctx(),
                &pending(session, "write", "created.txt", 1),
            )
            .await;
        std::fs::write(&path, "CREATED-BY-THE-MODEL").unwrap();
        observer
            .after_tool_call(&observer_ctx(), &call(session, "write", "created.txt", 1))
            .await;

        let commands = plugin.commands();
        let diff_cmd = &commands[1];
        let CommandOutcome::Output(lines) = diff_cmd.invoke(ctx_for(session, "1")).await else {
            panic!("expected Output");
        };
        let joined = lines.join("\n");
        let rendered: Vec<&str> = joined.lines().collect();
        assert!(
            rendered.contains(&"-CREATED-BY-THE-MODEL"),
            "rolling back a created path removes its content: {joined}"
        );
        assert!(
            !rendered.contains(&"+CREATED-BY-THE-MODEL"),
            "rendered backwards -- this previews the rollback, not the write: {joined}"
        );
        assert!(
            joined.contains("DELETES it"),
            "an absent baseline means the rollback deletes the path, and must say so: {joined}"
        );
    }

    /// Item `01M3SJBNF2KZRWA5P5SSF9B868`: a path a plain `rollback <seq>`
    /// would REFUSE (a hand edit since conway's last write) must not be
    /// previewed as an ordinary forthcoming restore -- `diff` must mark it
    /// as a conflict and label its own diff as what `--all` would produce,
    /// never what the plain command would.
    #[tokio::test]
    async fn diff_marks_a_conflicted_path_instead_of_presenting_a_forced_restore_as_plain() {
        let dir = TempDir::new().unwrap();
        let plugin = CheckpointPlugin::new(dir.path());
        let path = dir.path().join("f.txt");
        let session = SessionId::new();
        let observers = plugin.observers();
        let observer = &observers[0];

        for (seq, content) in [(1u64, "ORIGINAL-BYTES"), (2, "MODEL-WROTE-THIS")] {
            std::fs::write(&path, content).unwrap();
            observer
                .after_tool_call(&observer_ctx(), &call(session, "edit", "f.txt", seq))
                .await;
        }
        // The operator's own hand edit, bypassing conway -- a plain
        // `rollback 2` would now refuse this path entirely.
        std::fs::write(&path, "HAND-EDITED-BYTES").unwrap();

        let commands = plugin.commands();
        let diff_cmd = &commands[1];
        let CommandOutcome::Output(lines) = diff_cmd.invoke(ctx_for(session, "2")).await else {
            panic!("expected Output");
        };
        let joined = lines.join("\n");
        assert!(
            joined.contains("rollback will not touch this file"),
            "a conflicted path must be marked as one `rollback <seq>` would refuse, not \
             previewed as a plain restore: {joined}"
        );
        assert!(
            joined.contains("--all"),
            "the notice must name the way past the refusal: {joined}"
        );
        // Still useful: the forced-restore content is shown, but labelled
        // as what `--all` would do, never as the plain command's own
        // output.
        assert!(
            joined.contains("-HAND-EDITED-BYTES") && joined.contains("+ORIGINAL-BYTES"),
            "the diff itself must still show what --all would restore: {joined}"
        );
        assert!(
            !joined.contains("(after rollback to seq 2)"),
            "a conflicted path's diff must not carry the PLAIN rollback label -- that is \
             exactly the lie this item fixes: {joined}"
        );
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

    /// Board item `01M2TWC242P96Z3JDXWC9F3R5E`: an empty listing names the
    /// session it is answering for, and names the flag that addresses a
    /// different one. It used to say only "for this session",
    /// which an operator who had just killed a worker read as "there were
    /// no snapshots" -- the opposite of the truth.
    #[tokio::test]
    async fn an_empty_list_names_its_session_and_the_way_to_reach_another() {
        let dir = TempDir::new().unwrap();
        let plugin = CheckpointPlugin::new(dir.path());
        let commands = plugin.commands();
        let list_cmd = &commands[0];
        let session = SessionId::new();
        let outcome = list_cmd.invoke(ctx_for(session, "")).await;
        let CommandOutcome::Output(lines) = outcome else {
            panic!("an empty listing is Output, not Error");
        };
        let text = lines.join("\n");
        assert!(
            text.contains(&session.to_string()),
            "the empty listing must name the session it answered for: {text}"
        );
        assert!(
            text.contains("--session"),
            "the empty listing must name the flag that reaches another session: {text}"
        );
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
