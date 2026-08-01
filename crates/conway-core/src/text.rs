//! Text sanitization shared across crates.
//!
//! The single home for the **replace-semantics** control-character sanitizer:
//! every Unicode control character (`Cc`: `\x00`-`\x1F`, `\x7F`, and the C1
//! controls `\x80`-`\x9F`) is rewritten to [`SANITIZED_CONTROL_PLACEHOLDER`].
//! This is the one function the runtime's `rendered` seam
//! (`conway_runtime::tools::runner::sanitize_rendered`) and the permission
//! gate's laundering-recognition (`permission_pattern::contains_shell_
//! metacharacters`) both depend on, so the two can no longer drift apart.
//!
//! ## Why replace, not filter
//!
//! Replacing preserves **one output character per input character**, which
//! the security property depends on: a control character laundered into the
//! placeholder is still EVIDENCE the gate recognizes
//! ([`SANITIZED_CONTROL_PLACEHOLDER`] is itself a metacharacter -- see
//! `conway_core::permission_pattern::contains_shell_metacharacters`). A filter
//! that DROPPED control bytes would erase that evidence entirely, reopening
//! the v0.5.0 laundering hole this crate's own gate exists to close.
//!
//! Filtering is correct *only* where the consumer measures **display width**
//! rather than token structure (see `conway-cli`'s `tui::view::header`:
//! `sticky_prompt_text`), and that site deliberately does NOT call this
//! function -- see its own comment for why.

/// The character a sanitized string carries in place of a control byte.
///
/// This is the single source of truth for what `sanitize_control_chars`
/// produces AND for what `permission_pattern::contains_shell_metacharacters`
/// treats as a metacharacter: the two must agree, or a sanitized string stops
/// being recognizable as laundered. Both reference THIS constant, so they
/// cannot drift.
pub const SANITIZED_CONTROL_PLACEHOLDER: char = '\u{FFFD}';

/// Replaces every Unicode control character with [`SANITIZED_CONTROL_PLACEHOLDER`].
///
/// P-10: applied to text derived from untrusted (model-/attacker-influenced)
/// input before it flows into model context or the operator's TUI, so a raw
/// ANSI escape sequence (`\x1b[...`), a smuggled newline, or any other `Cc`
/// byte cannot reach a terminal as a live control byte. Never panics: the
/// worst case is one replacement character per input char.
///
/// This is the *shared* sanitizer; the runtime's `sanitize_rendered` and the
/// permission-pattern test fixtures both delegate here. See the module doc
/// for why a `filter` variant is deliberately NOT provided here.
pub fn sanitize_control_chars(input: &str) -> String {
    input
        .chars()
        .map(|c| if c.is_control() { SANITIZED_CONTROL_PLACEHOLDER } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_text_is_unchanged() {
        assert_eq!(sanitize_control_chars("git status --short"), "git status --short");
    }

    #[test]
    fn ansi_escapes_are_replaced_not_dropped() {
        let sanitized = sanitize_control_chars("git status\x1b[31m; rm -rf /\x1b[0m");
        // Replace, not filter: the ESC bytes become U+FFFD rather than
        // vanishing, preserving one output char per input char.
        assert_eq!(
            sanitized,
            "git status\u{FFFD}[31m; rm -rf /\u{FFFD}[0m"
        );
        assert!(sanitized.chars().all(|c| !c.is_control()));
    }

    #[test]
    fn other_control_bytes_are_replaced() {
        for raw in ["a\0b", "a\nb", "a\rb", "a\tb", "a\x07b", "a\x7fb"] {
            let sanitized = sanitize_control_chars(raw);
            assert!(
                sanitized.chars().all(|c| !c.is_control()),
                "{raw:?} -> {sanitized:?}"
            );
            assert!(
                sanitized.contains('\u{FFFD}'),
                "{raw:?} -> {sanitized:?}: the control char must be EVIDENCE (replaced), not erased"
            );
        }
    }
}