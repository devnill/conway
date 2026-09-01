//! Shared, PERMISSIVE frontmatter splitting for a Claude Code markdown file
//! (`commands/*.md`, `skills/*/SKILL.md`, `agents/*.md`) -- pulled out so
//! [`crate::skills`] and [`crate::agents`] (board item
//! `01M1DG5TTF6NHW2RXJRZ8ZPE7K`) share ONE split algorithm rather than each
//! inventing its own, per that item's own explicit instruction ("the agent
//! work must reuse whatever frontmatter-translation layer skills
//! establishes rather than inventing a second one"). [`crate::commands`]
//! keeps its own private copy (pre-dating this module, already its own
//! well-tested surface) -- this module is not a retroactive refactor of
//! that crate-internal code, only the shared base for the two NEW
//! translated kinds this item adds.
//!
//! Deliberately more permissive than `conway::skills::split_frontmatter`/
//! `conway::agents::split_frontmatter` (this crate's own top doc, question
//! 2: foreign frontmatter is parsed permissively, not `deny_unknown_
//! fields`-strict): a file with NO `---` block at all is ordinary here
//! (Claude Code frontmatter is optional on some file kinds), returned as
//! `Ok((None, content))`, never an error -- a caller for a KIND that
//! requires frontmatter (skills, agents) checks for `None` itself and
//! refuses/reports accordingly, rather than this shared function guessing
//! at a per-kind requirement it should not own.

/// Splits `content` into an optional YAML frontmatter block and the
/// remaining body. `Err` only for a file that OPENS a `---` block and never
/// closes it -- that shape is unambiguously broken, not merely
/// frontmatter-free.
pub(crate) fn split_frontmatter(content: &str) -> Result<(Option<&str>, &str), &'static str> {
    let content = content.strip_prefix('\u{FEFF}').unwrap_or(content);
    let Some(after_open) = content
        .strip_prefix("---\r\n")
        .or_else(|| content.strip_prefix("---\n"))
    else {
        return Ok((None, content));
    };

    let mut pos = 0usize;
    loop {
        if pos >= after_open.len() {
            return Err("unterminated frontmatter: no closing `---` delimiter found");
        }
        let rest = &after_open[pos..];
        let line_len = rest.find('\n').map(|i| i + 1).unwrap_or(rest.len());
        let line_no_nl = rest[..line_len].trim_end_matches(['\n', '\r']);
        if line_no_nl.trim_end() == "---" {
            let yaml_src = &after_open[..pos];
            let body = &after_open[pos + line_len..];
            return Ok((Some(yaml_src), body));
        }
        pos += line_len;
    }
}

/// Identical algorithm to `conway::skills::normalize_body`/`conway::
/// agents::normalize_body` (duplicated for the identical reason those
/// modules' own docs already give one another: this crate does not depend
/// on `conway`, the facade, in production code) -- strips a single leading
/// `\n`/`\r\n` then `trim_end()`s. Internal whitespace (indentation) is left
/// untouched.
pub(crate) fn normalize_body(raw: &str) -> String {
    let stripped = raw
        .strip_prefix("\r\n")
        .or_else(|| raw.strip_prefix('\n'))
        .unwrap_or(raw);
    stripped.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_frontmatter_is_ok_none() {
        let (fm, body) = split_frontmatter("just a body\n").unwrap();
        assert!(fm.is_none());
        assert_eq!(body, "just a body\n");
    }

    #[test]
    fn a_well_formed_block_splits_cleanly() {
        let (fm, body) = split_frontmatter("---\nkey: value\n---\nbody text\n").unwrap();
        assert_eq!(fm, Some("key: value\n"));
        assert_eq!(body, "body text\n");
    }

    #[test]
    fn unterminated_frontmatter_is_an_error() {
        let err = split_frontmatter("---\nkey: value\nno closing delimiter\n").unwrap_err();
        assert!(err.contains("unterminated"));
    }

    #[test]
    fn bom_is_stripped_before_looking_for_the_delimiter() {
        let (fm, body) = split_frontmatter("\u{FEFF}---\nkey: value\n---\nbody\n").unwrap();
        assert_eq!(fm, Some("key: value\n"));
        assert_eq!(body, "body\n");
    }

    #[test]
    fn normalize_body_strips_one_leading_newline_and_trailing_whitespace() {
        assert_eq!(normalize_body("\nbody\n\n"), "body");
        assert_eq!(normalize_body("\r\nbody\r\n"), "body");
        assert_eq!(
            normalize_body("line one\n\n  indented\n"),
            "line one\n\n  indented"
        );
    }
}
