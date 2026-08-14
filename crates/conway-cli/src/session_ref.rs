//! Parsing for the session-continuity flag values:
//! `--session`/`--resume` each take a bare `SessionId`; `--fork-from` takes
//! `<session-id>[@<seq>]`. This module owns the string -> typed-value step
//! only -- what `oneshot::run` does with a parsed value (resume, fork,
//! reject) lives there.
//!
//! `conway_core::ids::SessionId`/`LogSeq` are not depended on directly here
//! -- `conway-cli`'s `no_forbidden_deps` test restricts this crate
//! to the `conway` facade -- so every type below is the facade's own
//! re-export.

use std::fmt;

use conway::{LogSeq, SessionId};

/// A malformed `--session`/`--resume`/`--fork-from` value. Every variant's
/// message names the expected form `<session-id>[@<seq>]`, per this item's
/// criteria -- `--session`/`--resume` are the degenerate case with no `@seq`
/// half, so the same wording still applies (just never exercises it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    input: String,
    reason: &'static str,
}

impl ParseError {
    fn new(input: &str, reason: &'static str) -> Self {
        Self {
            input: input.to_string(),
            reason,
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid session reference {:?}: expected `<session-id>[@<seq>]` ({})",
            self.input, self.reason
        )
    }
}

impl std::error::Error for ParseError {}

/// Parses a bare `--session`/`--resume` value: a full `SessionId` (ULID),
/// no `@<seq>` suffix accepted. Per this item's binding notes, prefix
/// matching is an interactive affordance deliberately not offered
/// here -- scripts driving `-p` need an unambiguous input.
pub fn parse_sid(s: &str) -> Result<SessionId, ParseError> {
    s.parse::<SessionId>()
        .map_err(|_| ParseError::new(s, "not a valid session id (ULID)"))
}

/// Parses a `--fork-from` value: `<session-id>` or `<session-id>@<seq>`.
/// `<seq>` is a [`LogSeq`] in D-11's LOCAL units (the target session's own
/// sequence numbering, not any ancestor's) -- `oneshot::run` passes it
/// straight through to `Conway::fork_from` unchanged.
pub fn parse_fork_ref(s: &str) -> Result<(SessionId, Option<LogSeq>), ParseError> {
    match s.split_once('@') {
        None => Ok((parse_sid(s)?, None)),
        Some((sid_part, seq_part)) => {
            // Name the FULL ref the user typed in the error, not the bare
            // sid substring: `--fork-from @142` should
            // report `@142`, not an empty string.
            let sid = sid_part
                .parse::<SessionId>()
                .map_err(|_| ParseError::new(s, "not a valid session id (ULID)"))?;
            if seq_part.is_empty() {
                return Err(ParseError::new(s, "empty `@<seq>` suffix"));
            }
            let seq: LogSeq = seq_part
                .parse()
                .map_err(|_| ParseError::new(s, "`@<seq>` is not a valid non-negative integer"))?;
            Ok((sid, Some(seq)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A canonical example ULID (the one from the ULID spec itself) --
    // any syntactically valid 26-char Crockford-base32 ULID would do.
    const SID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

    #[test]
    fn bare_session_id() {
        let (sid, seq) = parse_fork_ref(SID).expect("valid bare sid");
        assert_eq!(sid.to_string(), SID);
        assert_eq!(seq, None);
    }

    #[test]
    fn session_id_with_seq() {
        let (sid, seq) = parse_fork_ref(&format!("{SID}@142")).expect("valid sid@seq");
        assert_eq!(sid.to_string(), SID);
        assert_eq!(seq, Some(LogSeq(142)));
    }

    #[test]
    fn empty_seq_suffix_is_an_error() {
        let err = parse_fork_ref(&format!("{SID}@")).expect_err("empty @seq must be rejected");
        assert!(err.to_string().contains("<session-id>[@<seq>]"));
    }

    #[test]
    fn missing_session_id_is_an_error() {
        let err = parse_fork_ref("@142").expect_err("missing sid half must be rejected");
        assert!(err.to_string().contains("<session-id>[@<seq>]"));
    }

    #[test]
    fn non_numeric_seq_is_an_error() {
        let err =
            parse_fork_ref(&format!("{SID}@abc")).expect_err("non-numeric seq must be rejected");
        assert!(err.to_string().contains("<session-id>[@<seq>]"));
    }

    #[test]
    fn invalid_ulid_is_an_error() {
        let err = parse_fork_ref("not-a-ulid").expect_err("garbage sid must be rejected");
        assert!(err.to_string().contains("<session-id>[@<seq>]"));
    }

    #[test]
    fn invalid_ulid_with_seq_is_an_error() {
        let err = parse_fork_ref("not-a-ulid@1").expect_err("garbage sid must be rejected");
        assert!(err.to_string().contains("<session-id>[@<seq>]"));
    }

    #[test]
    fn parse_sid_rejects_non_ulid() {
        assert!(parse_sid("garbage").is_err());
    }

    #[test]
    fn parse_sid_accepts_a_valid_ulid() {
        assert_eq!(parse_sid(SID).unwrap().to_string(), SID);
    }
}
