//! `conway sessions {list,show,tree,export}` (WI-116): pure formatters over
//! `Conway::sessions`/`Conway::resume`/`SessionHandle::transcript` -- no
//! method here reads a session store file directly, everything goes
//! through the `conway` facade.

use std::io::Write as _;
use std::path::PathBuf;

use clap::{Args, Subcommand};
use conway::{Conway, LogRecord, SessionFilter, SessionId, SessionMeta};

use crate::commands::fmt;
use crate::diag;
use crate::exit::ExitCode;

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
}

pub async fn run(args: &SessionsArgs, conway: &Conway) -> conway::Result<ExitCode> {
    match &args.action {
        SessionsAction::List { limit, label, json } => {
            list(conway, *limit, label.clone(), *json).await
        }
        SessionsAction::Show { id, json } => show(conway, id, *json).await,
        SessionsAction::Tree { id } => tree(conway, id).await,
        SessionsAction::Export { id, out } => export(conway, id, out.clone()).await,
    }
}

/// Parses a CLI-supplied session id, reporting a usage error (exit 2)
/// rather than propagating a parse failure through `main`'s
/// `ExitCode::from_error` (which would map a bare `ConwayError::Parse` to
/// `AgentFailed`, 1) -- every failure mode reachable before a real session
/// lookup happens is a usage error, never an agent failure.
fn parse_session_id(id: &str) -> Result<SessionId, ExitCode> {
    id.parse::<SessionId>().map_err(|e| {
        diag::error(format!("invalid session id {id:?}: {e}"));
        ExitCode::Usage
    })
}

/// Looks up `id` via `Conway::resume`, collapsing "not found" (or any other
/// resume failure) to a usage error rather than the `AgentFailed` a raw
/// `ConwayError::Store` would map to through `from_error` -- matches
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

/// A `Debug`-derived enum's variant name, lower-cased -- every
/// `SessionStatus` variant (`Active`/`Completed`/`Failed`/`Cancelled`) is a
/// single word, so this matches the type's own `snake_case` serde rendering
/// without this crate needing to name `conway_core::log::SessionStatus`
/// directly (out of file scope: that type is not re-exported by the
/// `conway` facade).
fn status_str(status: &impl std::fmt::Debug) -> String {
    format!("{status:?}").to_lowercase()
}

fn origin_cell(meta: &SessionMeta) -> String {
    match &meta.origin {
        Some(origin) => format!("fork@{} {}", origin.at_seq, fmt::id_short(origin.parent)),
        None => String::new(),
    }
}

fn origin_json(meta: &SessionMeta) -> serde_json::Value {
    match &meta.origin {
        Some(origin) => serde_json::json!({
            "parent": origin.parent.to_string(),
            "at_seq": origin.at_seq.0,
        }),
        None => serde_json::Value::Null,
    }
}

fn session_row(meta: &SessionMeta) -> Vec<String> {
    vec![
        fmt::id_short(meta.id),
        fmt::ts(meta.created),
        meta.role
            .as_ref()
            .map(|r| r.to_string())
            .unwrap_or_default(),
        status_str(&meta.status),
        origin_cell(meta),
    ]
}

fn session_json(meta: &SessionMeta) -> serde_json::Value {
    serde_json::json!({
        "id": meta.id.to_string(),
        "created": fmt::ts(meta.created),
        "role": meta.role.as_ref().map(|r| r.to_string()),
        "status": status_str(&meta.status),
        "origin": origin_json(meta),
    })
}

async fn list(
    conway: &Conway,
    limit: Option<usize>,
    label: Option<String>,
    json: bool,
) -> conway::Result<ExitCode> {
    let filter = SessionFilter {
        limit,
        label,
        ..Default::default()
    };
    let sessions = conway.sessions(filter).await?;

    if json {
        let arr: Vec<_> = sessions.iter().map(session_json).collect();
        println!(
            "{}",
            serde_json::to_string(&arr).expect("session list always serializes")
        );
    } else {
        let rows = sessions.iter().map(session_row).collect();
        print!(
            "{}",
            fmt::table(&["ID", "CREATED", "ROLE", "STATUS", "ORIGIN"], rows)
        );
    }
    let _ = std::io::stdout().flush();
    Ok(ExitCode::Completed)
}

async fn show(conway: &Conway, id: &str, json: bool) -> conway::Result<ExitCode> {
    let sid = match parse_session_id(id) {
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
    let sid = match parse_session_id(id) {
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
    let label = |m: &SessionMeta| {
        format!(
            "{}  role={}  status={}",
            fmt::id_short(m.id),
            m.role
                .as_ref()
                .map(|r| r.to_string())
                .unwrap_or_else(|| "-".to_string()),
            status_str(&m.status),
        )
    };

    for line in fmt::tree_lines(&root_meta, |m| children_of(m.id), label) {
        println!("{line}");
    }
    let _ = std::io::stdout().flush();
    Ok(ExitCode::Completed)
}

async fn export(conway: &Conway, id: &str, out: Option<PathBuf>) -> conway::Result<ExitCode> {
    let sid = match parse_session_id(id) {
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
