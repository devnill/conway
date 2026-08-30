//! Typed, non-panicking failure modes for reading an untrusted Claude Code
//! plugin directory: untrusted input is range-checked at the boundary,
//! never trusted to be well-formed.

use std::path::PathBuf;

/// Every way [`crate::discover`] can fail. Never a panic: a plugin directory
/// is third-party content, and a malformed file in it is an ORDINARY,
/// expected outcome, not an invariant violation.
#[derive(Debug, thiserror::Error)]
pub enum ClaudeCompatError {
    /// The path handed to [`crate::discover`] does not exist, or exists but
    /// is not a directory -- checked once, up front, rather than surfacing
    /// as a confusing "file not found" from whichever sub-file this crate
    /// tries to read first.
    #[error("'{0}' does not exist or is not a directory")]
    NotADirectory(PathBuf),
    /// A file this crate needed to read could not be opened/read (permission
    /// error, broken symlink, etc.) -- named by path, never silently
    /// skipped.
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// `path` is not valid UTF-8. Every file this crate reads is treated as
    /// text (JSON or Markdown); an operator's plugin directory containing a
    /// non-UTF-8 file under one of the paths this crate looks at is a named,
    /// typed failure, not a lossy re-encode.
    #[error("{path}: not valid UTF-8")]
    NotUtf8 { path: PathBuf },
    /// `path` exceeds the byte limit this crate applies to any single file
    /// read from an untrusted plugin directory -- an untrusted
    /// plugin directory's declared size is not trusted either (P-10:
    /// "absurd sizes" is named explicitly as a threat this boundary must
    /// range-check).
    #[error("{path}: {len} bytes exceeds the {limit}-byte read limit this crate applies to untrusted plugin content")]
    TooLarge { path: PathBuf, len: u64, limit: u64 },
    /// `path` parsed as neither valid JSON nor the specific shape this
    /// crate expected of it (e.g. `.mcp.json` present but not a JSON
    /// object). Named by path and the underlying `serde_json` message,
    /// never silently treated as "no such declarations".
    #[error("{path}: malformed JSON: {source}")]
    MalformedJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}
