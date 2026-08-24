//! `conway sessions {list,show,tree,export,name,unname}`: pure formatters
//! over `Conway::sessions`/`Conway::resume`/`SessionHandle::transcript` --
//! no method here reads a session store `<session-id>.jsonl` file
//! directly, everything goes through the `conway` facade. `name`/`unname`
//! are the one exception with any disk access of their own: they read and
//! write `crate::session_names::NamesStore`'s sidecar
//! (`session-names.json`, beside the session files but never one of them)
//! -- see that module's own doc for why a name lives there and not in a
//! session record.
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
}

pub async fn run(args: &SessionsArgs, conway: &Conway) -> conway::Result<ExitCode> {
    match &args.action {
        SessionsAction::List { limit, label, json } => {
            list(conway, *limit, label.clone(), *json).await
        }
        SessionsAction::Show { id, json } => show(conway, id, *json).await,
        SessionsAction::Tree { id } => tree(conway, id).await,
        SessionsAction::Export { id, out } => export(conway, id, out.clone()).await,
        SessionsAction::Name { id, name: new_name } => name(conway, id, new_name).await,
        SessionsAction::Unname { id } => unname(conway, id).await,
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

/// The `NAME` cell is blank for an unnamed session (`names.name_of` returns
/// `None`) -- never a synthesized placeholder like `-` or `<unnamed>`, per
/// this item's acceptance criteria.
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
fn session_row(meta: &SessionMeta, names: &NamesStore) -> Vec<String> {
    vec![
        meta.id.to_string(),
        names.name_of(meta.id).unwrap_or_default().to_string(),
        fmt::ts(meta.created),
        meta.role
            .as_ref()
            .map(|r| r.to_string())
            .unwrap_or_default(),
        origin_cell(meta),
    ]
}

fn session_json(meta: &SessionMeta, names: &NamesStore) -> serde_json::Value {
    serde_json::json!({
        "id": meta.id.to_string(),
        "name": names.name_of(meta.id),
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

    if json {
        let arr: Vec<_> = sessions.iter().map(|m| session_json(m, &names)).collect();
        println!(
            "{}",
            serde_json::to_string(&arr).expect("session list always serializes")
        );
    } else {
        let rows = sessions.iter().map(|m| session_row(m, &names)).collect();
        print!(
            "{}",
            fmt::table(&["ID", "NAME", "CREATED", "ROLE", "ORIGIN"], rows)
        );
    }
    let _ = std::io::stdout().flush();
    Ok(ExitCode::Completed)
}

async fn show(conway: &Conway, id: &str, json: bool) -> conway::Result<ExitCode> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use conway::AgentId;

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
        let names = NamesStore::default();
        let row_a = session_row(&meta_with_id(SHARED_PREFIX_ID_A), &names);
        let row_b = session_row(&meta_with_id(SHARED_PREFIX_ID_B), &names);

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
}
