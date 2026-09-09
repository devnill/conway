//! Bare `/resume`'s own picker -- pure, state-free data/formatting helpers
//! factored out of `commands.rs`, following [`super::model_picker`]'s own
//! shape exactly (a data layer with no `AppState`/`Host` in sight, unit
//! -testable on its own): [`ResumableSessionRow`] is one row's worth of
//! already-resolved display data, [`format_rows`] turns a list of them into
//! the plain option strings [`crate::tui::form::PendingFormAsk`] carries,
//! and [`session_id_from_option`] is the inverse -- recovering the row's own
//! [`conway::SessionId`] from the exact string `Mode::UiForm` hands back on
//! `Enter` (`conway_plugin_ui::AskSelectAnswer::selected` is "never an
//! index", the identical contract [`super::model_picker`]'s own doc already
//! leans on).
//!
//! # Where each field comes from, and the one rule about titles
//!
//! [`ResumableSessionRow::title`] is always `commands::sessions::
//! display_title`'s own return value, verbatim -- the operator-bound name
//! if one exists, else the session's derived auto-title, else `None`. This
//! module holds no second opinion on what a session is called; see that
//! function's own doc for the one mechanism.
//!
//! [`ResumableSessionRow::first_prompt`], `last_activity`, and `seq_count`
//! are NOT read through `display_title`/`auto_title_for` a second time --
//! `commands::LiveHost::resumable_sessions` (the one production caller)
//! already has to resume the session and read its own transcript to learn
//! `last_activity`/`seq_count`, so it extracts the first `UserTurn`'s text
//! from that SAME read and formats it with `commands::sessions::
//! auto_title` directly (the pure, first-line-and-truncate step
//! `auto_title_for` itself calls) rather than paying for a second
//! `resume`+transcript round trip just to reuse the wrapper. The
//! *mechanism* -- first line, trimmed, character-bounded -- still has
//! exactly one definition; only the I/O that feeds it is done once, not
//! twice.

use chrono::{DateTime, Utc};
use conway::SessionId;

/// One row `bare /resume`'s picker shows, fully resolved -- no lazy
/// fields, no further I/O once built (`commands::LiveHost::
/// resumable_sessions` is the one place that assembles these).
#[derive(Debug, Clone, PartialEq)]
pub struct ResumableSessionRow {
    pub id: SessionId,
    /// `commands::sessions::display_title`'s own result, verbatim -- see
    /// this module's own doc for why nothing here re-derives it.
    pub title: Option<String>,
    /// The session's first `UserTurn`, formatted with `commands::sessions::
    /// auto_title` -- `None` when the session has no user turn yet (a
    /// spawned child driven entirely by tool calls) or could not be read.
    /// Shown even when `title` is an operator-chosen NAME, so the row still
    /// says what the session was actually about.
    pub first_prompt: Option<String>,
    /// The most recent record's own timestamp -- `None` only when the
    /// session's transcript could not be read at all (a store I/O
    /// failure), never a synthesized "now".
    pub last_activity: Option<DateTime<Utc>>,
    /// How many records the session's own resolved transcript holds.
    pub seq_count: usize,
    /// `SessionMeta::labels`, verbatim.
    pub labels: Vec<String>,
}

/// The marker `format_row` appends before a row's own id, and
/// [`session_id_from_option`] splits on to recover it. Deliberately NOT
/// reused anywhere in the human-readable half of a row (title/prompt text
/// is free-form operator/model text and could legitimately contain this
/// exact substring) -- [`session_id_from_option`] only ever looks at the
/// LAST occurrence (`str::rsplit_once`), so an incidental earlier match
/// inside free text can never be mistaken for the row's own trailing id.
const ID_MARKER: &str = " -- id:";

/// Formats one row as a single-line (pre-wrap) option string: title/auto
/// -title, the first prompt, last activity, message count, and any labels,
/// human-first, with the row's own id appended last via `ID_MARKER` --
/// present so [`session_id_from_option`] can recover it, not meant to be
/// the part an operator reads first. `Mode::UiForm`'s own renderer
/// (`view::draw_ui_form`) wraps a long option across multiple visual rows
/// rather than truncating it, so this never clips a long title/prompt
/// pair; it only chooses what belongs on the one logical row.
pub fn format_row(row: &ResumableSessionRow) -> String {
    let title = row.title.as_deref().unwrap_or("(untitled)");
    let prompt = row.first_prompt.as_deref().unwrap_or("(no prompt yet)");
    let activity = row
        .last_activity
        .map(crate::commands::fmt::ts)
        .unwrap_or_else(|| "never".to_string());
    let plural = if row.seq_count == 1 { "" } else { "s" };
    let labels = if row.labels.is_empty() {
        String::new()
    } else {
        format!("  [{}]", row.labels.join(", "))
    };
    format!(
        "{title} -- {prompt} -- {activity} -- {} record{plural}{labels}{ID_MARKER}{}",
        row.seq_count, row.id
    )
}

/// [`format_row`], applied to every row in order -- the exact `Vec<String>`
/// `conway_plugin_ui::AskSelectRequest::options` needs.
pub fn format_rows(rows: &[ResumableSessionRow]) -> Vec<String> {
    rows.iter().map(format_row).collect()
}

/// The inverse of [`format_row`]'s own `ID_MARKER` suffix: recovers the
/// [`SessionId`] a picker option was built from. `None` for any string that
/// does not end in a well-formed `ID_MARKER<ulid>` tail -- in particular,
/// for any option this module did not itself produce, so a caller can
/// never mistake a malformed/foreign string for a real row.
pub fn session_id_from_option(option: &str) -> Option<SessionId> {
    let (_, id) = option.rsplit_once(ID_MARKER)?;
    id.trim().parse().ok()
}

/// A `LogRecord`'s own `ts`, generically -- every variant but `Header`
/// carries one (`conway_core::log::LogRecord`'s own definition), so this is
/// a small, independent mirror of that shape rather than a shared
/// accessor (`conway::LogRecord::seq` is the one generic accessor the core
/// crate itself exposes; `ts` has no equivalent there today). `#[non_
/// exhaustive]`, so a future variant this has not been taught about yet
/// returns `None` rather than failing to compile -- the identical policy
/// `conway_session::discovery::record_text`'s own trailing wildcard uses
/// for the analogous "what does this record say" extraction.
///
/// Used by `commands::LiveHost::resumable_sessions` to find a session's own
/// `last_activity`: the LAST record with a `ts` in its (already
/// chronological) resolved transcript.
pub fn record_ts(record: &conway::LogRecord) -> Option<DateTime<Utc>> {
    use conway::LogRecord;
    match record {
        LogRecord::Header(_) => None,
        LogRecord::UserTurn { ts, .. }
        | LogRecord::Assistant { ts, .. }
        | LogRecord::ToolResultRecord { ts, .. }
        | LogRecord::ForkDirective { ts, .. }
        | LogRecord::ParentSteer { ts, .. }
        | LogRecord::SystemNote { ts, .. }
        | LogRecord::AgentResultRecord { ts, .. }
        | LogRecord::ChildResultRecord { ts, .. }
        | LogRecord::ContextReportRecord { ts, .. }
        | LogRecord::ContextMask { ts, .. }
        | LogRecord::ContextPathSet { ts, .. }
        | LogRecord::ContextPathNamed { ts, .. }
        | LogRecord::PermissionDecisionRecord { ts, .. } => Some(*ts),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: SessionId) -> ResumableSessionRow {
        ResumableSessionRow {
            id,
            title: Some("daily standup".to_string()),
            first_prompt: Some("let's plan the week".to_string()),
            last_activity: Some(
                DateTime::parse_from_rfc3339("2026-09-08T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            seq_count: 5,
            labels: vec!["urgent".to_string()],
        }
    }

    #[test]
    fn format_row_round_trips_through_session_id_from_option() {
        let id = SessionId::new();
        let text = format_row(&row(id));
        assert_eq!(
            session_id_from_option(&text),
            Some(id),
            "a row's own id must be recoverable from its own formatted option: {text:?}"
        );
    }

    #[test]
    fn format_row_shows_every_field_a_human_reads() {
        let id = SessionId::new();
        let text = format_row(&row(id));
        assert!(text.contains("daily standup"), "{text:?}");
        assert!(text.contains("let's plan the week"), "{text:?}");
        assert!(text.contains("2026-09-08"), "{text:?}");
        assert!(text.contains("5 records"), "{text:?}");
        assert!(text.contains("[urgent]"), "{text:?}");
    }

    /// PAIRING: an option this module never produced (no [`ID_MARKER`] at
    /// all) must not resolve to a phantom id -- proves
    /// [`session_id_from_option`] refuses a foreign string rather than
    /// parsing some arbitrary trailing token as if it were one.
    #[test]
    fn session_id_from_option_rejects_a_string_with_no_marker() {
        assert_eq!(session_id_from_option("just some free text"), None);
    }

    /// A title/prompt containing the marker substring itself (free-form
    /// operator text can say anything) must not confuse recovery -- only
    /// the LAST occurrence (the one `format_row` itself appended) is ever
    /// used, per [`ID_MARKER`]'s own doc.
    #[test]
    fn session_id_from_option_uses_the_last_marker_even_when_the_title_contains_one() {
        let id = SessionId::new();
        let mut row = row(id);
        row.title = Some(format!("weird title{ID_MARKER}not-an-id"));
        let text = format_row(&row);
        assert_eq!(session_id_from_option(&text), Some(id));
    }

    #[test]
    fn format_rows_formats_every_row_in_order() {
        let a = SessionId::new();
        let b = SessionId::new();
        let rows = vec![row(a), row(b)];
        let options = format_rows(&rows);
        assert_eq!(options.len(), 2);
        assert_eq!(session_id_from_option(&options[0]), Some(a));
        assert_eq!(session_id_from_option(&options[1]), Some(b));
    }

    #[test]
    fn record_ts_reads_an_ordinary_records_own_timestamp() {
        let ts = DateTime::parse_from_rfc3339("2026-09-08T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let record = conway::LogRecord::UserTurn {
            seq: conway::LogSeq(1),
            ts,
            text: "hello".to_string(),
            prov: conway::Provenance::UserPrompt,
        };
        assert_eq!(record_ts(&record), Some(ts));
    }

    /// PAIRING: a `Header` record (the one variant with no `ts` at all)
    /// must not panic and must not fabricate one -- proves `record_ts`
    /// actually checks the variant rather than always finding SOME
    /// timestamp-shaped field.
    #[test]
    fn record_ts_of_a_header_record_is_none() {
        let meta = conway::SessionMeta {
            id: SessionId::new(),
            agent_id: conway::AgentId::new(),
            origin: None,
            agent_def: None,
            role: None,
            created: Utc::now(),
            cwd: std::path::PathBuf::from("/tmp"),
            labels: vec![],
            ephemeral: false,
            ask_origin: None,
            root: None,
            plugin_config: Default::default(),
        };
        assert_eq!(record_ts(&conway::LogRecord::Header(meta)), None);
    }

    #[test]
    fn format_row_without_a_title_or_prompt_still_formats_something_readable() {
        let id = SessionId::new();
        let bare = ResumableSessionRow {
            id,
            title: None,
            first_prompt: None,
            last_activity: None,
            seq_count: 0,
            labels: Vec::new(),
        };
        let text = format_row(&bare);
        assert!(text.contains("(untitled)"));
        assert!(text.contains("(no prompt yet)"));
        assert!(text.contains("never"));
        assert!(text.contains("0 records"));
        assert_eq!(session_id_from_option(&text), Some(id));
    }
}
