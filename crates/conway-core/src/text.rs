//! Text sanitization shared across crates.
//!
//! The single home for the **replace-semantics** control-character sanitizer:
//! every Unicode control character (`Cc`: `\x00`-`\x1F`, `\x7F`, and the C1
//! controls `\x80`-`\x9F`) **plus** a fixed, explicit set of additional
//! bidirectional-control/format (`Cf`) and line/paragraph-separator (`Zl`/
//! `Zp`) characters (see [`is_laundered_char`] for the exact list) is
//! rewritten to [`SANITIZED_CONTROL_PLACEHOLDER`]. This is the one function
//! the runtime's `rendered` seam (`conway_runtime::tools::runner::
//! sanitize_rendered`) and the permission gate's laundering-recognition
//! (`permission_pattern::contains_shell_metacharacters`) both depend on, so
//! the two can no longer drift apart.
//!
//! ## Why also Cf/Zl/Zp, not just Cc
//!
//! A bidirectional-override character (e.g. `U+202E RIGHT-TO-LEFT OVERRIDE`)
//! or a line/paragraph separator (`U+2028`/`U+2029`) is not `Cc` --
//! `char::is_control` returns `false` for every one of them -- but each can
//! change how a command's bytes are *displayed* (visual reordering, an
//! apparent extra line break) without changing what `/bin/bash -c` actually
//! executes (`tui/app/shell_cmd.rs` runs `arguments.command` verbatim). Left
//! unsanitized, a permission prompt could show the operator a command whose
//! visual order or line structure genuinely differs from the bytes bash
//! runs. [`is_laundered_char`]'s table closes that gap; see its own doc for
//! why it is a fixed table rather than a general Unicode-category query.
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
//!
//! ## One table, not a second list
//!
//! [`is_laundered_char`] is `pub` and is the ONLY place the additional Cf/Zl/
//! Zp set is spelled out. `conway-cli`'s `session_markdown::sanitize` (which
//! deliberately keeps `\n` -- see that function's own doc for why a static
//! export cannot) and `tui::view::mod::permission_command_lines`'s per-line
//! path both reach this same function (the latter via [`sanitize_control_
//! chars`] directly, the former via the re-exported predicate through the
//! `conway` facade), rather than re-deriving their own copy of the table.

/// The character a sanitized string carries in place of a control byte.
///
/// This is the single source of truth for what `sanitize_control_chars`
/// produces AND for what `permission_pattern::contains_shell_metacharacters`
/// treats as a metacharacter: the two must agree, or a sanitized string stops
/// being recognizable as laundered. Both reference THIS constant, so they
/// cannot drift.
pub const SANITIZED_CONTROL_PLACEHOLDER: char = '\u{FFFD}';

/// Replaces every character [`is_laundered_char`] flags with
/// [`SANITIZED_CONTROL_PLACEHOLDER`].
///
/// Applied to text derived from untrusted (model-/attacker-influenced)
/// input before it flows into model context or the operator's TUI, so a raw
/// ANSI escape sequence (`\x1b[...`), a smuggled newline, a bidirectional
/// override, or any other character [`is_laundered_char`] names cannot
/// reach a terminal as a live control/format byte. Never panics: the worst
/// case is one replacement character per input char.
///
/// This is the *shared* sanitizer; the runtime's `sanitize_rendered` and the
/// permission-pattern test fixtures both delegate here. See the module doc
/// for why a `filter` variant is deliberately NOT provided here.
pub fn sanitize_control_chars(input: &str) -> String {
    input
        .chars()
        .map(|c| {
            if is_laundered_char(c) {
                SANITIZED_CONTROL_PLACEHOLDER
            } else {
                c
            }
        })
        .collect()
}

/// Whether `c` is one of the characters [`sanitize_control_chars`] rewrites
/// to [`SANITIZED_CONTROL_PLACEHOLDER`]: every Unicode `Cc` control
/// character (`char::is_control`) **plus** a fixed, explicit superset of
/// additional bidirectional-control/format (`Cf`) and line/paragraph-
/// separator (`Zl`/`Zp`) characters that can change how text is *displayed*
/// without changing the bytes a shell actually executes.
///
/// `pub`, and the ONLY place this additional set is spelled out (see the
/// module doc, "One table, not a second list") -- every consumer that needs
/// the identical laundering rule (`conway-cli`'s `session_markdown::
/// sanitize`, which keeps `\n` unlike this function; `permission_command_
/// lines`'s per-line path, via [`sanitize_control_chars`] directly) reaches
/// this predicate rather than re-deriving its own copy.
///
/// **Deliberately a fixed range table, not a general Unicode-category
/// query: std's `char` has no `general_category()`/`is_format()` method,
/// and this crate does not add a dependency (`unicode-general-category` or
/// similar) for one predicate.** The table below is hand-picked from the
/// Unicode category assignments current as of Unicode 15 and is a
/// deliberate SUPERSET of `Cc`, covering (by category):
/// - **Cf (format):** `U+00AD` (soft hyphen), `U+061C` (Arabic letter
///   mark), `U+180E` (Mongolian vowel separator), `U+200B`-`U+200F`
///   (zero-width space/non-joiner/joiner and the LTR/RTL marks),
///   `U+202A`-`U+202E` (the bidi embedding/override controls),
///   `U+2060`-`U+2064` (word joiner and invisible operators), `U+2066`-
///   `U+206F` (the bidi isolate controls and deprecated format chars),
///   `U+FEFF` (zero-width no-break space / BOM), `U+FFF9`-`U+FFFB`
///   (interlinear annotation controls), and the "tag" block
///   `U+E0001`, `U+E0020`-`U+E007F`.
/// - **Zl/Zp (separator, line/paragraph):** `U+2028` (line separator),
///   `U+2029` (paragraph separator).
///
/// A future Unicode version adding a new character to one of these blocks
/// is covered automatically by the range bounds; a genuinely new block
/// would need a new arm here, same as any other hand-maintained table.
pub fn is_laundered_char(c: char) -> bool {
    c.is_control() || is_additional_laundered_char(c)
}

/// The additional, non-`Cc` half of [`is_laundered_char`]'s table -- see
/// that function's own doc for the full category breakdown.
fn is_additional_laundered_char(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_text_is_unchanged() {
        assert_eq!(
            sanitize_control_chars("git status --short"),
            "git status --short"
        );
    }

    #[test]
    fn ansi_escapes_are_replaced_not_dropped() {
        let sanitized = sanitize_control_chars("git status\x1b[31m; rm -rf /\x1b[0m");
        // Replace, not filter: the ESC bytes become U+FFFD rather than
        // vanishing, preserving one output char per input char.
        assert_eq!(sanitized, "git status\u{FFFD}[31m; rm -rf /\u{FFFD}[0m");
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

    /// A bidi-override command: `char::is_control` is `false` for
    /// `U+202E`, so before this item's fix it passed `sanitize_control_
    /// chars` unchanged, letting a permission prompt display a command
    /// whose visual order differs from the bytes bash actually runs.
    #[test]
    fn bidi_override_characters_are_replaced() {
        let raw = "echo safe\u{202E}fr- mr\u{2066}";
        assert!(!raw.chars().any(|c| c.is_control()), "fixture sanity");
        let sanitized = sanitize_control_chars(raw);
        assert_eq!(sanitized, "echo safe\u{FFFD}fr- mr\u{FFFD}");
        assert!(sanitized.contains('\u{FFFD}'));
    }

    /// `U+2028`/`U+2029` (Zl/Zp) are not `Cc` either, but a line/paragraph
    /// separator can split a single-line prompt into what looks like two
    /// lines without the command actually containing a real newline.
    #[test]
    fn line_and_paragraph_separators_are_replaced() {
        for raw in ["a\u{2028}b", "a\u{2029}b"] {
            let sanitized = sanitize_control_chars(raw);
            assert_eq!(sanitized, "a\u{FFFD}b", "{raw:?} -> {sanitized:?}");
        }
    }

    #[test]
    fn is_laundered_char_covers_every_documented_extra_range() {
        for c in [
            '\u{00AD}',
            '\u{061C}',
            '\u{180E}',
            '\u{200B}',
            '\u{200F}',
            '\u{2028}',
            '\u{2029}',
            '\u{202A}',
            '\u{202E}',
            '\u{2060}',
            '\u{2064}',
            '\u{2066}',
            '\u{206F}',
            '\u{FEFF}',
            '\u{FFF9}',
            '\u{FFFB}',
            '\u{E0001}',
            '\u{E0020}',
            '\u{E007F}',
        ] {
            assert!(
                is_laundered_char(c),
                "{c:?} (U+{:04X}) must be laundered",
                c as u32
            );
        }
        // A handful of ordinary, non-laundered chars stay themselves.
        for c in ['a', 'Z', '0', ' ', '-', '\u{00AC}', '\u{2025}', '\u{2070}'] {
            assert!(!is_laundered_char(c), "{c:?} must NOT be laundered");
        }
    }
}
