//! `conway sessions {list,show,tree,export,name,unname,label,unlabel}`:
//! pure formatters over `Conway::sessions`/`Conway::resume`/
//! `SessionHandle::transcript` -- no method here reads a session store
//! `<session-id>.jsonl` file directly, everything goes through the
//! `conway` facade. `name`/`unname` are the one exception with any disk
//! access of their own: they read and write
//! `crate::session_names::NamesStore`'s sidecar (`session-names.json`,
//! beside the session files but never one of them) -- see that module's
//! own doc for why a name lives there and not in a session record.
//! `label`/`unlabel` (board item `01M1WVKVSDXHB68J66VZ9HE8B3`) are NOT a
//! second exception of that kind: a label lives IN `SessionMeta` (the
//! session's own header, unlike a name), so `label`/`unlabel` write
//! through `Conway::add_label`/`remove_label` -> `SessionStore::
//! add_label`/`remove_label` -- the facade, like every other subcommand
//! in this file.
//!
//! # Why a subcommand pair, not a `--name` creation flag
//!
//! Naming a session is not part of *creating* one -- it is furniture hung
//! on a session that already exists (INTENT.md §7b), and a session's own
//! id is available the moment creation finishes (a one-shot run's
//! `--output-format json` output already prints it as `transcript_ref`;
//! see `docs/sessions.md`'s own worked example). `sessions name <id>
//! <name>` covers "name it right after creating it" and "rename it later"
//! with the exact same call, so a second, narrower surface (a root `--name`
//! flag, usable only on the arms of `--session`/`--resume`/`--fork-from`
//! that create rather than reattach) would be one more flag, one more
//! combination to validate against `--resume`, for a capability this one
//! subcommand pair already covers completely. Smallest honest surface: one
//! mechanism, not two.

use std::io::Write as _;
use std::path::PathBuf;

use clap::{Args, Subcommand};
use conway::{Conway, LogRecord, SessionFilter, SessionId, SessionMeta, SubagentMode};

use crate::commands::fmt;
use crate::diag;
use crate::exit::ExitCode;
use crate::session_names::{self, NamesStore};

#[derive(Args, Debug)]
pub struct SessionsArgs {
    #[command(subcommand)]
    pub action: SessionsAction,
}

#[derive(Subcommand, Debug)]
pub enum SessionsAction {
    /// List known sessions.
    List {
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show one session's resolved transcript.
    Show {
        id: String,
        #[arg(long)]
        json: bool,
        /// Board item 01M1YVEJB6GAPST5YZET4KZZE2: print the cumulative
        /// diff of every path this session's agents edited/wrote, against
        /// the bytes each path had the first time this session touched it
        /// -- the headless counterpart of the TUI's `/diff` command (see
        /// `diff_snapshot`'s own doc for how the two share one
        /// reconstruction). Mutually exclusive in effect with `--json`
        /// (whichever is checked first wins; `show` never combines the two
        /// output shapes) -- matches how this subcommand already treats
        /// `--json` as an output-shape switch, not an additive flag.
        #[arg(long)]
        diff: bool,
    },
    /// Print a session's fork tree.
    Tree { id: String },
    /// Export a session's ancestry-resolved transcript as JSONL.
    Export {
        id: String,
        #[arg(long = "out")]
        out: Option<PathBuf>,
    },
    /// Attach an operator-chosen name to a session, or rename its existing
    /// one. `ID` accepts a session id or an existing name, exactly like
    /// `--session`/`--resume`. Refuses a `NAME` that parses as a valid ULID,
    /// and refuses a `NAME` already bound to a different session, naming
    /// which one holds it -- never a silent overwrite. This is the whole
    /// naming surface -- see this module's own doc comment for why there is
    /// no separate creation-time flag.
    Name { id: String, name: String },
    /// Remove whichever name is bound to a session -- `ID` accepts a
    /// session id or the name itself. The session and its transcript are
    /// entirely unaffected: this only removes an entry from the separate
    /// name table.
    Unname { id: String },
    /// Attach `LABEL` to a session -- `ID` accepts a session id or an
    /// existing name, exactly like `name`/`unname`. Unlike a name, a
    /// session may carry any number of labels, and re-attaching one it
    /// already carries is a no-op success, not a refusal. `sessions list
    /// --label LABEL`/`conway.discover`'s `label` parameter match against
    /// labels attached this way.
    Label { id: String, label: String },
    /// Remove `LABEL` from a session -- `ID` accepts a session id or an
    /// existing name. Removing a label the session does not carry is a
    /// no-op success, not a refusal (unlike `unname` on an unbound name).
    Unlabel { id: String, label: String },
}

pub async fn run(args: &SessionsArgs, conway: &Conway) -> conway::Result<ExitCode> {
    match &args.action {
        SessionsAction::List { limit, label, json } => {
            list(conway, *limit, label.clone(), *json).await
        }
        SessionsAction::Show { id, json, diff } => show(conway, id, *json, *diff).await,
        SessionsAction::Tree { id } => tree(conway, id).await,
        SessionsAction::Export { id, out } => export(conway, id, out.clone()).await,
        SessionsAction::Name { id, name: new_name } => name(conway, id, new_name).await,
        SessionsAction::Unname { id } => unname(conway, id).await,
        SessionsAction::Label {
            id,
            label: the_label,
        } => label(conway, id, the_label).await,
        SessionsAction::Unlabel { id, label } => unlabel(conway, id, label).await,
    }
}

/// Loads this `Conway`'s session-names sidecar, reporting a usage error
/// (exit 2) on an unreadable/corrupt sidecar rather than propagating an
/// I/O error as an `AgentFailed` -- matches every other failure mode in
/// this file: nothing here reaches an agent, so `AgentFailed` is never the
/// right classification.
fn load_names(conway: &Conway) -> Result<NamesStore, ExitCode> {
    NamesStore::load(&session_names::session_root(conway)).map_err(|e| {
        diag::error(e.to_string());
        ExitCode::Usage
    })
}

/// Resolves a CLI-supplied `id-or-name` token (`session_names::resolve`: a
/// full ULID used directly, any other string looked up by name), reporting
/// a usage error (exit 2) rather than propagating a parse failure through
/// `main`'s `ExitCode::from_error` -- every failure mode reachable before a
/// real session lookup happens is a usage error, never an agent failure.
fn resolve_session_ref(id: &str, names: &NamesStore) -> Result<SessionId, ExitCode> {
    session_names::resolve(id, names).map_err(|e| {
        diag::error(e.to_string());
        ExitCode::Usage
    })
}

/// Looks up `id` via `Conway::resume`, collapsing "not found" (or any other
/// resume failure) to a usage error rather than the `AgentFailed` a raw
/// `FacadeError::Store` would map to through `from_error` -- matches
/// `show <unknown-id>`/`tree <unknown-id>`/`export <unknown-id>`'s shared
/// "exits 2 with empty stdout" contract.
async fn resume_or_usage_error(
    conway: &Conway,
    id: &str,
    sid: SessionId,
) -> Result<conway::SessionHandle, ExitCode> {
    conway.resume(sid).await.map_err(|e| {
        diag::error(format!("unknown session {id}: {e}"));
        ExitCode::Usage
    })
}

/// The primitive that created a session (fork and spawn are distinct
/// and must never be blurred into one label) -- `"fork"` or `"spawn"`,
/// matching `SubagentMode`'s own `snake_case` serde rendering.
fn mode_str(mode: SubagentMode) -> &'static str {
    match mode {
        SubagentMode::Fork => "fork",
        SubagentMode::Spawn => "spawn",
    }
}

/// `fmt::id_short` stays here deliberately (board item
/// `01M0V03FQGJ8C375QJDD75YH41`): this cell names ONE already-known parent
/// as annotation, not a token the operator is meant to distinguish among
/// several visible rows or paste elsewhere -- the same "one thing, not a
/// choice" case the TUI panel's `short_agent_id` carves out for its status
/// line and hop labels (`crates/conway-cli/src/tui/view/agents.rs`). If
/// that parent needs to be addressed directly, its own row's `ID` cell
/// prints it in full.
fn origin_cell(meta: &SessionMeta) -> String {
    match &meta.origin {
        Some(origin) => format!(
            "{}@{} {}",
            mode_str(origin.mode),
            origin.at_seq,
            fmt::id_short(origin.parent)
        ),
        None => String::new(),
    }
}

fn origin_json(meta: &SessionMeta) -> serde_json::Value {
    match &meta.origin {
        Some(origin) => serde_json::json!({
            "parent": origin.parent.to_string(),
            "at_seq": origin.at_seq.0,
            "mode": mode_str(origin.mode),
        }),
        None => serde_json::Value::Null,
    }
}

/// The maximum length, in CHARACTERS (never bytes -- see
/// [`truncate_chars_with_ellipsis`]), an [`auto_title`] returns. Chosen to
/// fit comfortably in a terminal row alongside `ID`/`CREATED`/`ROLE`/
/// `ORIGIN` without wrapping on an ordinary-width terminal; not tied to
/// any format-level limit, so raising or lowering it later is a pure
/// display decision with no compatibility concern.
const AUTO_TITLE_MAX_CHARS: usize = 60;

/// Truncates `text` to at most `budget` CHARACTERS (not bytes), replacing
/// the tail with a single `…` sentinel once it doesn't fit whole -- never
/// panics on any input, including one where the byte offset `budget` would
/// land inside a multi-byte character. A raw `&text[..budget]` byte slice
/// is exactly the mistake that crashed the TUI's own args preview on a
/// multi-byte character straddling its limit (see
/// `crates/conway-cli/src/tui/view/transcript.rs`'s own
/// `truncate_chars_with_ellipsis`, which this mirrors exactly); this
/// module has no shared utils file to pull one copy from, so this is a
/// second, independently maintained copy for the same reason that one
/// documents its own duplication.
fn truncate_chars_with_ellipsis(text: &str, budget: usize) -> String {
    if text.chars().count() <= budget {
        return text.to_string();
    }
    if budget == 0 {
        return String::new();
    }
    if budget == 1 {
        return "…".to_string();
    }
    let mut truncated: String = text.chars().take(budget - 1).collect();
    truncated.push('…');
    truncated
}

/// Derives a DISPLAY title from a session's first user-turn text: its
/// first line (a multi-line prompt's second and later lines are dropped
/// entirely, never folded into the title), trimmed, then bounded to
/// [`AUTO_TITLE_MAX_CHARS`] characters. Deterministic and free -- no model
/// call, ever; this is the whole mechanism, not a stand-in for a smarter
/// one to add later.
///
/// **Display only.** The caller (`display_title`, below) never writes this
/// value anywhere; it exists purely to fill the `NAME` cell/`title` field
/// when no operator-chosen name exists. See [`load_names`]/`NamesStore`
/// for the one write path a real name has, which this function never
/// touches.
///
/// `pub(crate)`: `crate::tui::commands::LiveHost::resumable_sessions`
/// (bare `/resume`'s picker, board item `01M1YS550T52VR1VW8NMETXQS1`)
/// reuses this SAME pure step to format a row's own "first prompt" cell
/// from a `UserTurn` it has already read for another reason (see that
/// method's own doc for why it does not also call [`auto_title_for`] a
/// second time to get there).
pub(crate) fn auto_title(first_user_turn_text: &str) -> String {
    let first_line = first_user_turn_text.lines().next().unwrap_or("").trim();
    truncate_chars_with_ellipsis(first_line, AUTO_TITLE_MAX_CHARS)
}

/// Reads a session's own first `UserTurn` record and derives its
/// [`auto_title`] -- `None` when the session has no user turn yet (a
/// spawned child driven entirely by tool calls, or a session that errored
/// before its first prompt landed) or when it can no longer be resumed.
/// Uses `handle.transcript(handle.root())`, the identical ancestry-
/// resolved read `sessions show`/`sessions export` already perform, so a
/// forked child with no `UserTurn` of its own picks up the one it
/// inherited -- consistent with every other place this file treats "this
/// session's transcript" as the resolved one, not the bare on-disk file.
async fn auto_title_for(conway: &Conway, sid: SessionId) -> Option<String> {
    let handle = conway.resume(sid).await.ok()?;
    let records = handle.transcript(handle.root()).await.ok()?;
    records.iter().find_map(|record| match record {
        LogRecord::UserTurn { text, .. } => Some(auto_title(text)),
        _ => None,
    })
}

/// The display title `sessions list` shows: the operator-chosen name if
/// one is bound (`NamesStore::name_of`, unchanged), else the session's
/// [`auto_title_for`] result, else `None` (never a synthesized placeholder
/// -- an ordinary blank cell/`null` field, matching how an unnamed session
/// with no title has always rendered).
///
/// **Never writes `conway.names`.** This reads `names` but never calls
/// `NamesStore::set`; the only write path for a real name stays `sessions
/// name`, entirely untouched by this function.
///
/// `pub(crate)`: bare `/resume`'s picker (`crate::tui::commands::LiveHost::
/// resumable_sessions`, board item `01M1YS550T52VR1VW8NMETXQS1`) reuses this
/// exact function for each row's own title cell rather than deriving a
/// second opinion of what a session is called -- see that method's own doc.
pub(crate) async fn display_title(
    conway: &Conway,
    names: &NamesStore,
    meta: &SessionMeta,
) -> Option<String> {
    if let Some(name) = names.name_of(meta.id) {
        return Some(name.to_string());
    }
    auto_title_for(conway, meta.id).await
}

/// The `NAME` cell shows `title` -- the operator-chosen name if one is
/// bound, else the session's derived [`auto_title`], else blank -- never a
/// synthesized placeholder like `-` or `<unnamed>` for the fully-blank
/// case.
///
/// The `ID` cell is the **full** id, not `fmt::id_short`'s 8-character
/// truncation (board item `01M0V03FQGJ8C375QJDD75YH41`). Two sessions
/// created within about a second of each other share their first 8
/// characters, and this column is the row's own identity -- the token an
/// operator reads off the listing to paste into `--session`/`--resume`/
/// `--fork-from`. A truncated, colliding `ID` cell would make the listing
/// actively misleading rather than merely terse: two distinct rows would
/// display the identical value. Consistent with `session_json`, which
/// already emits `meta.id.to_string()` in full -- text and JSON now agree.
/// A `NAME` column exists for the human-friendly short handle; this column
/// is the durable reference (TREE-ID `01M0TNCAP1HH4YNC5K9753YG26`'s
/// ruling), and full ids trivially satisfy uniqueness with no prefix-
/// extension algorithm and no dependency on which page of the store is
/// being viewed.
fn session_row(meta: &SessionMeta, title: Option<&str>) -> Vec<String> {
    vec![
        meta.id.to_string(),
        title.unwrap_or_default().to_string(),
        fmt::ts(meta.created),
        meta.role
            .as_ref()
            .map(|r| r.to_string())
            .unwrap_or_default(),
        origin_cell(meta),
    ]
}

/// `"name"` stays the operator-bound name exactly as before (`null` when
/// unset -- an identity field, not a display one, unchanged in meaning and
/// never written by the title path below). `"title"` is the new field: the
/// same value `session_row`'s `NAME` cell shows, so a JSON consumer (a
/// future picker among them) gets both the durable identity and a
/// ready-to-display label in one row, without conflating the two.
fn session_json(meta: &SessionMeta, names: &NamesStore, title: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "id": meta.id.to_string(),
        "name": names.name_of(meta.id),
        "title": title,
        "created": fmt::ts(meta.created),
        "role": meta.role.as_ref().map(|r| r.to_string()),
        "origin": origin_json(meta),
    })
}

async fn list(
    conway: &Conway,
    limit: Option<usize>,
    label: Option<String>,
    json: bool,
) -> conway::Result<ExitCode> {
    let names = match load_names(conway) {
        Ok(names) => names,
        Err(code) => return Ok(code),
    };
    let filter = SessionFilter {
        limit,
        label,
        ..Default::default()
    };
    let sessions = conway.sessions(filter).await?;

    // One title per row, computed up front -- `auto_title_for` only does
    // real work (a `resume` + transcript read) for a row with no bound
    // name; a named row short-circuits in `display_title` before touching
    // the store at all.
    let mut titles = Vec::with_capacity(sessions.len());
    for meta in &sessions {
        titles.push(display_title(conway, &names, meta).await);
    }

    if json {
        let arr: Vec<_> = sessions
            .iter()
            .zip(&titles)
            .map(|(m, title)| session_json(m, &names, title.as_deref()))
            .collect();
        println!(
            "{}",
            serde_json::to_string(&arr).expect("session list always serializes")
        );
    } else {
        let rows = sessions
            .iter()
            .zip(&titles)
            .map(|(m, title)| session_row(m, title.as_deref()))
            .collect();
        print!(
            "{}",
            fmt::table(&["ID", "NAME", "CREATED", "ROLE", "ORIGIN"], rows)
        );
    }
    let _ = std::io::stdout().flush();
    Ok(ExitCode::Completed)
}

async fn show(conway: &Conway, id: &str, json: bool, diff: bool) -> conway::Result<ExitCode> {
    let names = match load_names(conway) {
        Ok(names) => names,
        Err(code) => return Ok(code),
    };
    let sid = match resolve_session_ref(id, &names) {
        Ok(sid) => sid,
        Err(code) => return Ok(code),
    };
    let handle = match resume_or_usage_error(conway, id, sid).await {
        Ok(handle) => handle,
        Err(code) => return Ok(code),
    };
    let records: Vec<LogRecord> = handle.transcript(handle.root()).await?;

    if diff {
        print_diff_snapshot(&records);
        let _ = std::io::stdout().flush();
        return Ok(ExitCode::Completed);
    }

    if json {
        for record in &records {
            println!(
                "{}",
                serde_json::to_string(record).expect("log record always serializes")
            );
        }
    } else {
        for record in &records {
            println!("--- {} seq={:?} ---", record.kind_str(), record.seq());
            println!("{record:#?}");
            println!();
        }
    }
    let _ = std::io::stdout().flush();
    Ok(ExitCode::Completed)
}

/// Board item 01M1YVEJB6GAPST5YZET4KZZE2: prints the SAME cumulative diff
/// the TUI's `/diff` shows (`tui/commands.rs::render_diff_snapshot`) --
/// both fold an ordered sequence of successful `edit`/`write` touches
/// through the identical `crate::diff::cumulative_diffs`, this one built
/// from a completed session's own `records` rather than the live
/// `AppState::transcript`. A session with nothing touched yet prints one
/// honest line rather than nothing at all, matching the TUI's own empty
/// state.
fn print_diff_snapshot(records: &[LogRecord]) {
    let touches = diff_touches_from_records(records);
    let diffs = crate::diff::cumulative_diffs(&touches);
    if diffs.is_empty() {
        println!("no files edited or written yet in this session");
        return;
    }
    for (path, diff_text) in diffs {
        println!("## {path}");
        print!("{diff_text}");
    }
}

/// Walks `records` in order, pairing each `edit`/`write`
/// `ContentBlock::ToolUse` (carried on a `LogRecord::Assistant`'s own
/// `content`) with its matching `LogRecord::ToolResultRecord` by
/// `call_id`, and extracts a [`crate::diff::FileTouch`] for every call
/// that SUCCEEDED (`!result.is_error`) -- a failed call (e.g. `old_string
/// not found`) never touched the file, so it contributes nothing. A
/// `ToolUse` with no matching `ToolResultRecord` later in the log (the
/// session ended mid-call) is silently dropped, the same "never panics on
/// an incomplete/untrusted log" posture every other reader in this crate
/// takes.
fn diff_touches_from_records(records: &[LogRecord]) -> Vec<crate::diff::FileTouch> {
    use conway::plugin::ContentBlock;

    let mut pending: std::collections::HashMap<String, (String, serde_json::Value)> =
        std::collections::HashMap::new();
    let mut touches = Vec::new();
    for record in records {
        match record {
            LogRecord::Assistant { content, .. } => {
                for block in content {
                    if let ContentBlock::ToolUse {
                        call_id,
                        name,
                        arguments,
                    } = block
                    {
                        pending.insert(call_id.clone(), (name.to_string(), arguments.clone()));
                    }
                }
            }
            LogRecord::ToolResultRecord { result, .. } => {
                if !result.is_error {
                    if let Some((name, args)) = pending.get(&result.call_id) {
                        if let Some(touch) = crate::diff::file_touch_from_args(name, args) {
                            touches.push(touch);
                        }
                    }
                }
                pending.remove(&result.call_id);
            }
            _ => {}
        }
    }
    touches
}

async fn tree(conway: &Conway, id: &str) -> conway::Result<ExitCode> {
    let names = match load_names(conway) {
        Ok(names) => names,
        Err(code) => return Ok(code),
    };
    let sid = match resolve_session_ref(id, &names) {
        Ok(sid) => sid,
        Err(code) => return Ok(code),
    };
    // `include_ephemeral: true` -- the explicitly-named target must resolve
    // by direct id even when it is itself ephemeral (matching `show`/
    // `export`, which resolve via `Conway::resume` -> a direct `store.meta`
    // lookup, not the default-filtered catalog); see
    // `SessionHandle::resolve_agent_session`'s identical rationale
    // (session_handle.rs): a direct id lookup is an identity check, not a
    // catalog browse. Descendant traversal below still excludes ephemeral
    // children via `children_of`'s own `!m.ephemeral` filter, so only this
    // top-level target resolution is widened.
    let all = conway
        .sessions(SessionFilter {
            include_ephemeral: true,
            ..Default::default()
        })
        .await?;
    let Some(root_meta) = all.iter().find(|m| m.id == sid).cloned() else {
        diag::error(format!("unknown session {id}"));
        return Ok(ExitCode::Usage);
    };

    let children_of = |parent: SessionId| -> Vec<SessionMeta> {
        all.iter()
            .filter(|m| m.origin.as_ref().map(|o| o.parent) == Some(parent) && !m.ephemeral)
            .cloned()
            .collect()
    };
    // Full id, not `fmt::id_short` (board item `01M0V03FQGJ8C375QJDD75YH41`):
    // a tree prints several sibling rows together, the same "operator is
    // choosing between visible rows" shape `session_row`'s own doc gives
    // for `sessions list`'s `ID` column, so the same fix applies -- and for
    // the identical reason, truncating here would let two children forked
    // moments apart render the same label.
    let label = |m: &SessionMeta| {
        format!(
            "{}  role={}",
            m.id,
            m.role
                .as_ref()
                .map(|r| r.to_string())
                .unwrap_or_else(|| "-".to_string()),
        )
    };

    for line in fmt::tree_lines(&root_meta, |m| children_of(m.id), label) {
        println!("{line}");
    }
    let _ = std::io::stdout().flush();
    Ok(ExitCode::Completed)
}

async fn export(conway: &Conway, id: &str, out: Option<PathBuf>) -> conway::Result<ExitCode> {
    let names = match load_names(conway) {
        Ok(names) => names,
        Err(code) => return Ok(code),
    };
    let sid = match resolve_session_ref(id, &names) {
        Ok(sid) => sid,
        Err(code) => return Ok(code),
    };
    let handle = match resume_or_usage_error(conway, id, sid).await {
        Ok(handle) => handle,
        Err(code) => return Ok(code),
    };
    let records: Vec<LogRecord> = handle.transcript(handle.root()).await?;

    let mut buf = String::new();
    for record in &records {
        buf.push_str(&serde_json::to_string(record).expect("log record always serializes"));
        buf.push('\n');
    }

    match out {
        Some(path) => {
            std::fs::write(&path, buf)?;
        }
        None => {
            print!("{buf}");
            let _ = std::io::stdout().flush();
        }
    }
    Ok(ExitCode::Completed)
}

/// `sessions name <id-or-name> <name>`. Confirms `id` names a real session
/// before writing anything -- a typo'd id must not create a name entry
/// that resolves nowhere useful -- then delegates the actual bind (ULID-
/// shape refusal, collision refusal, idempotent re-bind, and rename-by-
/// moving-the-one-name-a-session-carries) entirely to `NamesStore::set`.
async fn name(conway: &Conway, id: &str, new_name: &str) -> conway::Result<ExitCode> {
    let mut names = match load_names(conway) {
        Ok(names) => names,
        Err(code) => return Ok(code),
    };
    let sid = match resolve_session_ref(id, &names) {
        Ok(sid) => sid,
        Err(code) => return Ok(code),
    };
    if conway.resume(sid).await.is_err() {
        diag::error(format!("unknown session {id}"));
        return Ok(ExitCode::Usage);
    }
    match names.set(new_name, sid) {
        Ok(()) => {
            println!("{sid}  {new_name}");
            let _ = std::io::stdout().flush();
            Ok(ExitCode::Completed)
        }
        Err(e) => {
            diag::error(e.to_string());
            Ok(ExitCode::Usage)
        }
    }
}

/// `sessions unname <id-or-name>`. Removes whichever name is bound to the
/// resolved session -- the session's own record is never touched, only
/// this entry in the separate name table.
async fn unname(conway: &Conway, id: &str) -> conway::Result<ExitCode> {
    let mut names = match load_names(conway) {
        Ok(names) => names,
        Err(code) => return Ok(code),
    };
    let sid = match resolve_session_ref(id, &names) {
        Ok(sid) => sid,
        Err(code) => return Ok(code),
    };
    match names.unset(&sid.to_string()) {
        Ok(()) => Ok(ExitCode::Completed),
        Err(e) => {
            diag::error(e.to_string());
            Ok(ExitCode::Usage)
        }
    }
}

/// `sessions label <id-or-name> <label>`. Resolves `id` the same way every
/// other subcommand in this file does (the names sidecar, then `sessions
/// show|tree|export`'s own "unknown session is a usage error" contract),
/// then writes through the facade -- `Conway::add_label` ->
/// `SessionStore::add_label` -- the ONE mechanism this item introduces
/// (board item `01M1WVKVSDXHB68J66VZ9HE8B3`); this function itself never
/// touches a session file, exactly as `name`/`unname` never touch one
/// either, just a different underlying store this time.
async fn label(conway: &Conway, id: &str, label: &str) -> conway::Result<ExitCode> {
    let names = match load_names(conway) {
        Ok(names) => names,
        Err(code) => return Ok(code),
    };
    let sid = match resolve_session_ref(id, &names) {
        Ok(sid) => sid,
        Err(code) => return Ok(code),
    };
    match conway.add_label(sid, label).await {
        Ok(()) => {
            println!("{sid}  {label}");
            let _ = std::io::stdout().flush();
            Ok(ExitCode::Completed)
        }
        Err(e) => {
            diag::error(format!("unknown session {id}: {e}"));
            Ok(ExitCode::Usage)
        }
    }
}

/// `sessions unlabel <id-or-name> <label>`. Removing a label the session
/// does not carry is a no-op success (`Conway::remove_label`'s own
/// idempotent contract) -- unlike `unname`, there is no "unknown target"
/// refusal to surface here, since a label is not a bijective binding a
/// caller could target incorrectly.
async fn unlabel(conway: &Conway, id: &str, label: &str) -> conway::Result<ExitCode> {
    let names = match load_names(conway) {
        Ok(names) => names,
        Err(code) => return Ok(code),
    };
    let sid = match resolve_session_ref(id, &names) {
        Ok(sid) => sid,
        Err(code) => return Ok(code),
    };
    match conway.remove_label(sid, label).await {
        Ok(()) => Ok(ExitCode::Completed),
        Err(e) => {
            diag::error(format!("unknown session {id}: {e}"));
            Ok(ExitCode::Usage)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway::backend::{ModelId, StopReason, Usage};
    use conway::plugin::ContentBlock;
    use conway::AgentId;
    use conway_core::ids::{BackendId, LogSeq};

    /// A minimal `LogRecord::Assistant` carrying exactly one
    /// `ContentBlock::ToolUse` -- the shape `diff_touches_from_records`
    /// reads its `(call_id, tool, arguments)` from.
    fn tool_use_record(call_id: &str, tool: &str, args: serde_json::Value) -> LogRecord {
        LogRecord::Assistant {
            seq: LogSeq(0),
            ts: chrono::Utc::now(),
            content: vec![ContentBlock::ToolUse {
                call_id: call_id.to_string(),
                name: conway::ToolName::new(tool),
                arguments: args,
            }],
            model: conway::ModelRef {
                backend: BackendId::new("test"),
                model: ModelId::new("test"),
            },
            route_reason: serde_json::json!({}),
            usage: Usage::default(),
            stop: StopReason::ToolUse,
        }
    }

    fn tool_result_record(call_id: &str, tool: &str, is_error: bool) -> LogRecord {
        LogRecord::ToolResultRecord {
            seq: LogSeq(1),
            ts: chrono::Utc::now(),
            result: conway_core::content::ToolResult {
                call_id: call_id.to_string(),
                tool: conway::ToolName::new(tool),
                blocks: Vec::new(),
                is_error,
                truncated: None,
            },
        }
    }

    /// Board item 01M1YVEJB6GAPST5YZET4KZZE2: a successful `edit`
    /// `ToolUse`/`ToolResultRecord` pair extracts one [`crate::diff::
    /// FileTouch`]; a FAILED one (matching this same call_id) extracts
    /// none -- nothing on disk actually changed.
    #[test]
    fn diff_touches_from_records_extracts_successful_calls_only() {
        let records = vec![
            tool_use_record(
                "tc_1",
                "edit",
                serde_json::json!({"path": "f.txt", "old_string": "a", "new_string": "b"}),
            ),
            tool_result_record("tc_1", "edit", false),
            tool_use_record(
                "tc_2",
                "edit",
                serde_json::json!({"path": "f.txt", "old_string": "x", "new_string": "y"}),
            ),
            tool_result_record("tc_2", "edit", true),
        ];

        let touches = diff_touches_from_records(&records);
        assert_eq!(touches.len(), 1, "{touches:?}");
        assert_eq!(touches[0].path, "f.txt");
    }

    /// A `bash`/non-`edit`/`write` call is never extracted, even on
    /// success -- there is no file-content change to fold.
    #[test]
    fn diff_touches_from_records_ignores_non_file_tools() {
        let records = vec![
            tool_use_record("tc_1", "bash", serde_json::json!({"command": "ls"})),
            tool_result_record("tc_1", "bash", false),
        ];
        assert!(diff_touches_from_records(&records).is_empty());
    }

    /// Two real ULIDs sharing their first 8 characters -- the exact shape
    /// two sessions created within about a second of each other produce
    /// (a ULID's leading characters encode the high bits of its
    /// millisecond timestamp). Built by hand rather than by racing
    /// `SessionId::new()` against the clock so the test is deterministic,
    /// not merely likely to hit the collision window.
    const SHARED_PREFIX_ID_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const SHARED_PREFIX_ID_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

    fn meta_with_id(id: &str) -> SessionMeta {
        SessionMeta {
            id: id.parse().expect("valid ULID literal"),
            agent_id: AgentId::new(),
            origin: None,
            agent_def: None,
            role: None,
            created: chrono::Utc::now(),
            cwd: PathBuf::from("/tmp"),
            labels: vec![],
            ephemeral: false,
            ask_origin: None,
            root: None,
            plugin_config: conway::plugin::PluginConfig::default(),
        }
    }

    /// Board item `01M0V03FQGJ8C375QJDD75YH41`, acceptance 4: two sessions
    /// created in rapid succession must be distinguishable in `sessions
    /// list`'s output. This is the assertion that fails against the
    /// pre-fix implementation -- `session_row` used to build the `ID` cell
    /// with `fmt::id_short(meta.id)` (a fixed first-8-characters
    /// truncation), which collapses `SHARED_PREFIX_ID_A`/`_B` to the
    /// identical string `"01ARZ3ND"`, making `row_a[0] == row_b[0]` and
    /// this assertion fail. Reverting `session_row`'s `ID` cell from
    /// `meta.id.to_string()` back to `fmt::id_short(meta.id)` (and nothing
    /// else in this diff) reproduces that failure -- the smallest check
    /// available to whoever reviews this without running `cargo test`.
    #[test]
    fn two_sessions_sharing_a_ulid_prefix_get_distinguishable_id_cells() {
        let row_a = session_row(&meta_with_id(SHARED_PREFIX_ID_A), None);
        let row_b = session_row(&meta_with_id(SHARED_PREFIX_ID_B), None);

        // Precondition: these two ids really do collide under the old
        // 8-char truncation, or the assertion below would prove nothing.
        assert_eq!(
            fmt::id_short(SHARED_PREFIX_ID_A),
            fmt::id_short(SHARED_PREFIX_ID_B)
        );

        assert_ne!(
            row_a[0], row_b[0],
            "two sessions from the same second must render distinct ID cells: {row_a:?} vs {row_b:?}"
        );
    }

    /// This item's own required check: a multi-line first prompt must
    /// produce a title that is the FIRST LINE ONLY, trimmed -- never the
    /// whole prompt folded together, and never a later line. Catches an
    /// implementation that uses the whole `first_user_turn_text` verbatim
    /// (e.g. `text.trim()` with no `.lines().next()`), which would fold
    /// every line into one long title instead of stopping at the first.
    #[test]
    fn auto_title_takes_only_the_first_line_of_a_multi_line_prompt() {
        let title = auto_title("Fix the login bug\n\nSteps to reproduce:\n1. ...");
        assert_eq!(title, "Fix the login bug");
    }

    /// Leading/trailing whitespace on the first line is trimmed before
    /// bounding -- a prompt typed with a leading space or trailing newline
    /// should not carry that whitespace into the displayed title.
    #[test]
    fn auto_title_trims_the_first_line() {
        assert_eq!(auto_title("  hello world  \nmore text"), "hello world");
    }

    /// The bound applies to a single line too: one long enough to exceed
    /// `AUTO_TITLE_MAX_CHARS` is truncated with the `…` sentinel, never
    /// left un-bounded. Catches an implementation with no length bound at
    /// all.
    #[test]
    fn auto_title_bounds_a_long_single_line() {
        let long_line = "x".repeat(AUTO_TITLE_MAX_CHARS + 20);
        let title = auto_title(&long_line);
        assert_eq!(title.chars().count(), AUTO_TITLE_MAX_CHARS);
        assert!(title.ends_with('…'));
    }

    /// This item's own required check: bounding must count CHARACTERS, not
    /// bytes. Built to reproduce the exact shape that crashed this
    /// codebase's TUI args preview (`crates/conway-cli/src/tui/view/
    /// transcript.rs`'s own field report): `AUTO_TITLE_MAX_CHARS - 1`
    /// ASCII bytes, then one 3-byte character (`—`), so a naive
    /// `&text[..AUTO_TITLE_MAX_CHARS]` BYTE slice lands inside it and
    /// panics (`byte index N is not a char boundary`). Catches a
    /// byte-index implementation directly -- this one neither panics nor
    /// corrupts the character.
    #[test]
    fn auto_title_bounds_on_a_char_boundary_not_mid_multibyte_char() {
        let prefix = "x".repeat(AUTO_TITLE_MAX_CHARS - 1);
        let first_line =
            format!("{prefix}—rest of a much longer prompt that keeps going well past the bound");
        assert!(
            first_line.len() > AUTO_TITLE_MAX_CHARS,
            "byte length must exceed the char bound for this to actually exercise the hazard"
        );

        let title = auto_title(&first_line);

        assert_eq!(title.chars().count(), AUTO_TITLE_MAX_CHARS);
        assert!(title.ends_with('…'));
    }

    /// [`truncate_chars_with_ellipsis`] in isolation, mirroring
    /// `view::transcript`'s own identical test for its copy of this
    /// function: a budget of 5 keeps exactly 5 characters (4 kept + 1
    /// sentinel), counted in characters, not bytes -- each `—` here is 3
    /// bytes, so a byte-counting implementation would keep far fewer.
    #[test]
    fn truncate_chars_with_ellipsis_counts_chars_not_bytes() {
        let text = "—".repeat(10);
        let truncated = truncate_chars_with_ellipsis(&text, 5);
        assert_eq!(truncated.chars().count(), 5, "4 kept chars + 1 sentinel");
        assert!(truncated.ends_with('…'));
    }
}
