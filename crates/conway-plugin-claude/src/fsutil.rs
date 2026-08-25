//! Bounded, typed-error file reads for content this crate treats as
//! untrusted (P-10). Every read in this crate goes through
//! [`read_bounded`] rather than a bare `std::fs::read_to_string` -- the one
//! choke point that enforces the size cap and turns an I/O/UTF-8 failure
//! into a [`crate::ClaudeCompatError`] instead of a panic or a silent lossy
//! re-encode.
//!
//! **No path-escape guard, and that is a deliberate, stated scope
//! boundary, not an oversight.** Every path this crate ever reads is built
//! by JOINING a literal, hard-coded relative fragment this crate itself
//! chooses (`.claude-plugin/plugin.json`, `.mcp.json`, `hooks/hooks.json`,
//! `commands/*.md`, `skills/*/SKILL.md`, `agents/*.md`) onto the directory
//! the operator named -- never a path *read out of* the untrusted content
//! itself. There is no data-driven path for a malicious `.mcp.json` to name
//! and have this crate open. The directory itself sits on the identical
//! trust footing `[plugins].subprocess`/`[plugins].mcp` already establish
//! (`docs/plugins/trust-and-security.md`): an operator who points conway at
//! a directory is trusting whatever is IN it to the same degree as naming a
//! command in `settings.json` directly.

use std::path::Path;

use crate::error::ClaudeCompatError;

/// The largest single file this crate will read from an untrusted plugin
/// directory. 1 MiB comfortably fits any real `plugin.json`/`.mcp.json`/
/// `hooks.json` (these are hand-authored declarations, not data dumps) while
/// refusing to let a directory with an absurdly large file (P-10's own
/// named threat) run this process out of memory on a single `fs::read`.
pub(crate) const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Reads `path` as UTF-8 text, refusing anything over [`MAX_FILE_BYTES`] and
/// turning every failure mode (missing, unreadable, oversized, not-UTF-8)
/// into a typed [`ClaudeCompatError`] -- never a panic, per this crate's own
/// P-10 boundary.
pub(crate) fn read_bounded(path: &Path) -> Result<String, ClaudeCompatError> {
    let metadata = std::fs::metadata(path).map_err(|source| ClaudeCompatError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.len() > MAX_FILE_BYTES {
        return Err(ClaudeCompatError::TooLarge {
            path: path.to_path_buf(),
            len: metadata.len(),
            limit: MAX_FILE_BYTES,
        });
    }
    let bytes = std::fs::read(path).map_err(|source| ClaudeCompatError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    String::from_utf8(bytes).map_err(|_| ClaudeCompatError::NotUtf8 {
        path: path.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_ordinary_small_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f.json");
        std::fs::write(&path, "hello").unwrap();
        assert_eq!(read_bounded(&path).unwrap(), "hello");
    }

    #[test]
    fn refuses_a_file_over_the_size_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big.json");
        std::fs::write(&path, vec![b'x'; (MAX_FILE_BYTES + 1) as usize]).unwrap();
        let err = read_bounded(&path).unwrap_err();
        assert!(matches!(err, ClaudeCompatError::TooLarge { .. }), "{err:?}");
    }

    #[test]
    fn a_missing_file_is_a_typed_io_error_not_a_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nope.json");
        let err = read_bounded(&path).unwrap_err();
        assert!(matches!(err, ClaudeCompatError::Io { .. }), "{err:?}");
    }

    #[test]
    fn non_utf8_content_is_a_typed_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bad.json");
        std::fs::write(&path, [0xff, 0xfe, 0xfd]).unwrap();
        let err = read_bounded(&path).unwrap_err();
        assert!(matches!(err, ClaudeCompatError::NotUtf8 { .. }), "{err:?}");
    }
}
