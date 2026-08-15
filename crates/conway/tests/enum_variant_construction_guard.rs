//! Enforcement for: a
//! declared-behavior enum variant must either be constructed by production
//! code somewhere in this tree, or be named on the allowlist below with a
//! reason (and, where one exists, a).
//!
//! ## An inert variant is not automatically a defect
//!
//! Declaring a variant before anything constructs it is a legitimate,
//! ordinary thing to do -- forward compatibility for a `#[non_exhaustive]`
//! vocabulary, wire/serialization stability for an older peer parsing a
//! newer message, or a deliberate seam (: expose it, let policy live
//! outside). This guard does not forbid that. **What it forbids is a
//! MISMATCH between what a variant's own doc comment promises and what a
//! reader would get if they declared it today.** `TruncationPolicy::Artifact`
//! (, since removed rather than
//! wired -- see `ALLOWLIST`'s comment for that entry) was not a problem
//! because it was unconstructed; it was a problem because its doc comment
//! described a behavior ("spill to an Artifact, keep a pointer") that a tool
//! declaring it would not actually receive -- the doc claimed a promise
//! nothing kept. An allowlisted entry's own doc comment must instead say
//! plainly that it is not yet implemented (checked below), which is the
//! cheap, high-value half of this guard.
//!
//! So there are three honest answers to a variant this guard flags, not two:
//! wire it (construct it somewhere in production), remove it (the vocabulary
//! entry was speculative and nothing needs it), or allowlist it as a
//! deliberate forward declaration -- which requires disclosing that status
//! in the variant's own doc comment, not just in this file.
//!
//! ## Inclusion criterion -- which enums this guard covers
//!
//! This file does not walk the workspace hunting for "behavior enums":
//! deciding whether an enum's documentation promises runtime behavior is a
//! judgment call, not a mechanical property, and rules out bringing in
//! a real parser to approximate one -- that would be exactly the kind of
//! static-analysis framework warns against building to serve one
//! guard. Instead [`WATCHED_ENUMS`] names its enums explicitly, and each
//! entry carries the one-line reason it qualifies under this criterion:
//!
//! **An enum belongs on this list iff a reader who has never touched its
//! implementation could predict, from the variant doc comments alone, what
//! the runtime DOES when that variant is the active one.** That is a
//! caller-visible behavior contract, not a plain data shape -- compare
//! `ToolCategory`, whose variants are labels with no doc claiming an effect,
//! and which is deliberately not on this list.
//!
//! When you add or touch a vocabulary enum that meets this bar, add it to
//! [`WATCHED_ENUMS`] in the same change. The criterion above is what a
//! reviewer checks a proposed addition against; it is not a promise this
//! file enforces on itself ( forbids the parser that would take to
//! verify "does this doc comment describe behavior" mechanically).
//!
//! ## How a variant is recognized as "constructed"
//!
//! For each watched enum, [`extract_variants`] parses `EnumName { ... }`
//! out of its declaration file (brace-depth text scanning -- no `syn`, per
//!) to get the variant list and each variant's own doc comment. Then,
//! for each variant, this guard searches a **production corpus** built from
//! every `crates/*/src/**/*.rs` file for a whole-word occurrence of
//! `EnumName::Variant` that is a genuine value-producing expression, not a
//! pattern.
//!
//! That distinction is the whole point: a `match`/`if let` arm that merely
//! READS a variant (`LogRecord::ContextMask { .. } => ...`, or `if let
//! CacheMode::ExplicitBreakpoints { .. } = &profile.cache`) contains the
//! exact same text a real construction would, so naive substring matching
//! cannot tell them apart -- and getting this wrong is exactly how
//! `LogRecord::ContextMask` would slip past silently (`resolver.rs` matches
//! it in `apply_context_mask` and would satisfy a naive "does the text
//! appear anywhere outside the declaration" check, despite nothing ever
//! appending one). [`is_pattern_position`] classifies an occurrence as a
//! pattern (excluded from "constructed" evidence) when it is immediately
//! preceded by `let`/`if let`/`while let`, or when scanning forward past its
//! own optional payload (and, transitively, past any `|`-joined or-pattern
//! alternatives -- see `LogRecord::seq`'s multi-variant arm) lands on `=>`
//! before anything else. Everything else -- a struct literal, a `const`
//! initializer, a match arm's RHS, a function's tail expression -- counts as
//! a construction. This is deliberately a heuristic, not a parser ();
//! its known gaps are stated in [`is_pattern_position`]'s own doc, and none
//! of them are exercised by this tree's current source (verified below and
//! by hand while writing this guard).
//!
//! ## The `#[cfg(test)]`-inside-`src/` problem
//!
//! Most unit tests in this tree live in `mod tests { ... }` blocks inside
//! the same source file as the code under test, so a naive scan of
//! `crates/*/src/**/*.rs` would count test-only constructions as production
//! ones and pass vacuously -- exactly the failure mode this guard exists to
//! avoid. [`strip_cfg_test_items`] removes every `#[cfg(test)]`-attributed
//! item's own text (its brace-matched block, or up through the next `;` for
//! a bare item like `#[cfg(test)] mod tests;` or `#[cfg(test)] use ...;`)
//! from a file's text before it enters the corpus, and it does this
//! per-occurrence rather than "everything after the first `#[cfg(test)]`
//! wins" -- `crates/conway-cli/src/tui/gate.rs` has two separate
//! `#[cfg(test)]`-gated methods in the MIDDLE of the file, with real
//! production code (this guard's own evidence for `PermissionScope::Session`
//! among others) following both. `crates/conway-cli/src/oneshot.rs`'s
//! existing `source_never_references_prompting_gate` test takes the
//! "everything after the first `#[cfg(test)]`" shortcut, which is only safe
//! there because that file's sole `#[cfg(test)]` block is the last thing in
//! it; `gate.rs` is the tree's own proof that shortcut does not generalize,
//! which is why this guard does not take it.
//!
//! A second, easier-to-miss form of the same problem: a source file can be
//! ENTIRELY test-only by virtue of how its parent module declares it --
//! `crates/conway-cli/src/tui/mod.rs` has `#[cfg(test)] pub(crate) mod
//! test_support;`, so `tui/test_support.rs` never compiles into production
//! code, yet a plain filesystem walk of `src/` would still read its
//! contents and (wrongly) treat anything it constructs as production
//! evidence. [`excluded_whole_files`] finds every bare (`;`-terminated)
//! `#[cfg(test)] mod NAME;` declaration across the tree first, resolves it
//! to `NAME.rs` or `NAME/mod.rs` next to the declaring file, and drops that
//! file from the corpus entirely. `unit_tests_do_not_leak_into_the_corpus`
//! below exercises both forms directly.
//!
//! Comments (`//`, `///`, `//!`) are also stripped, line-by-line, before
//! searching -- otherwise a doc comment or explanatory comment that merely
//! NAMES a variant (this file's own doc comment above does exactly that)
//! would count as a construction. This guard strips whole COMMENT LINES
//! (a trimmed line starting with `//`), not trailing same-line comments;
//! verified by hand that no production source in this tree currently puts
//! a trailing `// ... EnumName::Variant ...` comment after code on the same
//! line for any watched enum, so this is not a gap today, but a future
//! trailing comment of that shape would need `strip_full_line_comments`
//! taught to handle it.
//!
//! ## What a failure says
//!
//! [`report_unconstructed`] names the enum, the variant, and all three
//! options above, plus the exact allowlist-entry shape to add if the answer
//! is "deliberate forward declaration" -- so the next person to hit this
//! does not reach for the allowlist reflexively just because it is the
//! shortest option to type.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// One enum this guard covers, and why it meets the inclusion criterion
/// stated in this file's module doc.
struct WatchedEnum {
    /// The enum's name exactly as written in source.
    name: &'static str,
    /// Workspace-relative path to the file declaring it.
    decl_file: &'static str,
    /// Why this enum's variants are a caller-visible behavior contract, not
    /// plain data -- the inclusion criterion, applied.
    why: &'static str,
}

const WATCHED_ENUMS: &[WatchedEnum] = &[
    WatchedEnum {
        name: "TruncationPolicy",
        decl_file: "crates/conway-core/src/content.rs",
        why: "each variant's doc names a distinct on-overflow behavior \
              (keep the head, keep the tail, keep both ends, ...) \
              that a tool author picks specifically for its effect",
    },
    WatchedEnum {
        name: "CacheMode",
        decl_file: "crates/conway-core/src/capabilities.rs",
        why: "each variant names a distinct prompt-cache protocol a \
              backend adapter must speak differently to honor",
    },
    WatchedEnum {
        name: "PathArgs",
        decl_file: "crates/conway-core/src/ports/plugin.rs",
        why: "each variant changes whether/how root-containment checking \
              applies to a tool call -- a security-relevant behavior, not \
              a data label",
    },
    WatchedEnum {
        name: "RenderKind",
        decl_file: "crates/conway-core/src/ports/plugin.rs",
        why: "each variant changes whether the metacharacter chaining \
              gate applies to a tool's pattern grants",
    },
    WatchedEnum {
        name: "LogRecord",
        decl_file: "crates/conway-core/src/log.rs",
        why: "each variant is a distinct kind of append-only log entry \
              with its own replay/assembly behavior downstream",
    },
    WatchedEnum {
        name: "PermissionScope",
        decl_file: "crates/conway-core/src/agent.rs",
        why: "each variant changes how broadly an AllowAlways grant \
              applies to future calls -- a security-relevant behavior",
    },
    WatchedEnum {
        name: "PermissionMode",
        decl_file: "crates/conway-core/src/permission_mode.rs",
        why: "each variant is a distinct operator-facing policy for how \
              much the gate asks before a tool runs",
    },
    WatchedEnum {
        name: "Select",
        decl_file: "crates/conway-core/src/permission_pattern.rs",
        why: "each variant changes WHICH tools a structured rule matches \
              (named tools, a wildcard, a whole category) -- a \
              security-relevant behavior, not a data label",
    },
    WatchedEnum {
        name: "When",
        decl_file: "crates/conway-core/src/permission_pattern.rs",
        why: "each variant changes WHEN a structured rule applies (always, \
              a command prefix, a path boundary) -- including the \
              paths_under confinement semantics",
    },
    WatchedEnum {
        name: "Then",
        decl_file: "crates/conway-core/src/permission_pattern.rs",
        why: "each variant changes what a matching rule DOES (grant, \
              refuse, ask) -- the allow/deny asymmetry and trust gate \
              hinge on it",
    },
];

/// A watched enum's variant that is deliberately allowlisted: nothing in
/// this tree's production code constructs it yet, and that is a considered
/// forward declaration, not an oversight.
///
/// Being on this list is not an escape hatch. It is the register the
/// operator asked for: an entry here converts an invisible trap (a
/// doc comment promising behavior nothing delivers) into a visible,
/// reviewable, honestly-labeled seam.
struct Allowlisted {
    enum_name: &'static str,
    variant: &'static str,
    /// Why this variant is allowed to have no producer. Stated in full,
    /// because this file is the only place a reader can learn it.
    reason: &'static str,
}

/// The literal substring an allowlisted variant's own doc comment must
/// contain (case-insensitively), so a reader of the enum's declaration --
/// who will never see this test file -- learns the same thing this guard
/// enforces: the variant is declared but does not yet do what it says.
const NOT_YET_IMPLEMENTED_MARKER: &str = "not yet implemented";

const ALLOWLIST: &[Allowlisted] = &[
    // `TruncationPolicy::Artifact`, previously allowlisted here, was
    // triaged by: REMOVED rather than
    // wired. Spill-to-file is a workload-specific opinion (where to spill,
    // when, retention, preview shape) that puts in a hook or plugin,
    // not in core's `TruncationPolicy`. The variant is gone, so its
    // allowlist entry is gone too.
    // `LogRecord::ContextMask`, previously allowlisted here ("nothing
    // appends one -- there is no tool or operator surface that can mask a
    // record"), was triaged by board item 01KZY8QRAVVVKCRBZ6HAEGW3GG
    // (`/checkout` and a reachable `ContextMask`): WIRED, not removed.
    // `conway::Conway::mask_record` (`crates/conway/src/conway.rs`) is a
    // real production construction site, reached through
    // `conway_plugin_history`'s `/conway.history.mask` command via
    // `CommandOutcome::MaskRecord` -- so this guard now finds it
    // constructed and the entry is gone. See that variant's own doc
    // (`crates/conway-core/src/log.rs`) for what changed and what did not
    // (still fork-prefix-only, per that item's own scope decision).
    //
    // -- Newly flagged by THIS guard (
    // itself), reported in that item's completion rather than fixed --
    // acceptance explicitly says "any newly-flagged variant is reported,
    // do not fix them in this item". None of these two has its own
    // dedicated yet; each `board_item` below says so honestly
    // rather than inventing one. Filing dedicated items is a stated
    // follow-up, not silently expanded scope of this item.
    //
    // `LogRecord::ToolCallRecord`, once allowlisted here, was triaged by
    //:
    // removed rather than wired, since `ContentBlock::ToolUse` inside the
    // preceding `Assistant` record is the durable shape and nothing else
    // consumes a standalone tool-call record. The variant is gone, so its
    // allowlist entry is gone too.
    //
    // `PermissionScope::Agent` and `PermissionScope::AgentSubtree`, also
    // allowlisted here from this guard's first run, were triaged by the
    // grant-prompt scope item (: WIRE,
    // do not remove): both are now constructed in production -- the TUI
    // prompt's `s` scope key (`conway-cli/src/tui/input.rs`) and the
    // facade's scoped grant methods -- so their entries are gone too.
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/conway/../.. is the workspace root")
        .to_path_buf()
}

/// Every `crates/*/src/**/*.rs` file, workspace-relative walk order not
/// guaranteed.
fn all_crate_source_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let crates_dir = root.join("crates");
    for entry in std::fs::read_dir(&crates_dir).expect("read crates/") {
        let entry = entry.expect("crates/ dir entry");
        let src_dir = entry.path().join("src");
        if src_dir.is_dir() {
            collect_rs_files(&src_dir, &mut files);
        }
    }
    files
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}")) {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Finds every bare `[pub[(crate)]] mod NAME;` declaration immediately
/// preceded by `#[cfg(test)]` across `files`, and resolves each to the file
/// it names (`NAME.rs` or `NAME/mod.rs`, next to the declaring file). Those
/// files are entirely test-only despite living under `src/` and must not
/// enter the production corpus.
fn excluded_whole_files(files: &[PathBuf]) -> BTreeSet<PathBuf> {
    let mut excluded = BTreeSet::new();
    for file in files {
        let text = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("read {file:?}: {e}"));
        let marker = "#[cfg(test)]";
        let mut rest = text.as_str();
        while let Some(idx) = rest.find(marker) {
            let after = &rest[idx + marker.len()..];
            if let Some(name) = bare_mod_decl_name(after) {
                let dir = file.parent().expect("file has a parent dir");
                let as_file = dir.join(format!("{name}.rs"));
                let as_dir_mod = dir.join(name).join("mod.rs");
                if as_file.is_file() {
                    excluded.insert(as_file);
                } else if as_dir_mod.is_file() {
                    excluded.insert(as_dir_mod);
                }
            }
            rest = after;
        }
    }
    excluded
}

/// Recognizes `[pub[(crate)]] mod IDENT;` at the start of `s` (after
/// trimming leading whitespace/visibility keywords), returning `IDENT`.
/// Returns `None` for an inline `mod IDENT { .. }` (its content never left
/// the file the brace-matched `#[cfg(test)]` stripping already handles) or
/// for anything else following the attribute.
fn bare_mod_decl_name(s: &str) -> Option<&str> {
    let mut s = s.trim_start();
    loop {
        if let Some(rest) = s.strip_prefix("pub(crate)") {
            s = rest.trim_start();
            continue;
        }
        if let Some(rest) = s.strip_prefix("pub") {
            s = rest.trim_start();
            continue;
        }
        break;
    }
    let s = s.strip_prefix("mod")?;
    match s.chars().next() {
        Some(c) if c.is_alphanumeric() || c == '_' => return None, // e.g. `module`
        _ => {}
    }
    let s = s.trim_start();
    let end = s.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
    let name = &s[..end];
    if name.is_empty() {
        return None;
    }
    if s[end..].trim_start().starts_with(';') {
        Some(name)
    } else {
        None
    }
}

/// Removes every `#[cfg(test)]`-attributed item's text from `src`: the
/// attribute itself plus everything through the matching close of its
/// brace-delimited body (`mod`/`fn`/`impl`/...), or through the next `;`
/// for a bare item (`#[cfg(test)] mod tests;`, `#[cfg(test)] use ...;`).
/// Handles as many occurrences as the file has, each independently, so
/// production code that follows an earlier `#[cfg(test)]` item in the same
/// file (e.g. `crates/conway-cli/src/tui/gate.rs`) is preserved.
fn strip_cfg_test_items(src: &str) -> String {
    let marker = "#[cfg(test)]";
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    loop {
        match rest.find(marker) {
            None => {
                out.push_str(rest);
                break;
            }
            Some(idx) => {
                out.push_str(&rest[..idx]);
                let after = &rest[idx + marker.len()..];
                let mut end = after.len();
                for (i, ch) in after.char_indices() {
                    if ch == '{' {
                        end = match find_matching_close(&after[i + 1..], '{', '}') {
                            Some(close) => i + 1 + close + 1,
                            None => after.len(),
                        };
                        break;
                    }
                    if ch == ';' {
                        end = i + 1;
                        break;
                    }
                }
                rest = &after[end..];
            }
        }
    }
    out
}

/// Blanks every whole-line comment (a trimmed line starting with `//`,
/// which covers `///` and `//!` too). See this file's module doc for why
/// line-granularity is sufficient for this tree today.
fn strip_full_line_comments(src: &str) -> String {
    src.lines()
        .map(|line| {
            if line.trim_start().starts_with("//") {
                ""
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Index (relative to `s`, which starts JUST AFTER the opening `open`) of
/// the matching `close`, tracking nested `open`/`close` pairs. Skips `//`
/// line comments while counting -- a doc comment illustrating JSON syntax
/// (`` `{`/`}` ``, as `RenderKind`'s does) must not desynchronize the
/// depth count.
fn find_matching_close(s: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 1i32;
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let ch = s[i..].chars().next().expect("valid utf8 boundary");
        if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += ch.len_utf8();
    }
    None
}

/// One variant of a watched enum, as parsed from its declaration.
struct Variant {
    name: String,
    /// This variant's own `///` doc comment lines, joined with spaces.
    doc: String,
}

/// Parses `pub enum {enum_name} { ... }` out of `source` (the enum's own
/// declaration file) via brace-depth text scanning -- no `syn`.
fn extract_variants(source: &str, enum_name: &str) -> Vec<Variant> {
    let needle = format!("enum {enum_name}");
    let decl_start = source
        .find(&needle)
        .unwrap_or_else(|| panic!("declaration `enum {enum_name}` not found"));
    let after = &source[decl_start + needle.len()..];
    let brace_rel = after
        .find('{')
        .unwrap_or_else(|| panic!("no `{{` found after `enum {enum_name}`"));
    let body_start = decl_start + needle.len() + brace_rel + 1;
    let close_rel = find_matching_close(&source[body_start..], '{', '}')
        .unwrap_or_else(|| panic!("unbalanced braces scanning `enum {enum_name}`"));
    let body = &source[body_start..body_start + close_rel];

    let mut variants = Vec::new();
    let mut depth = 0i32;
    let mut chunk_start = 0usize;
    let bytes = body.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        // Skip `//` line comments entirely -- a doc comment's own prose
        // (commas, parens) must not be mistaken for variant structure.
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        match bytes[i] {
            b'{' | b'(' => depth += 1,
            b'}' | b')' => depth -= 1,
            b',' if depth == 0 => {
                push_variant(&body[chunk_start..i], &mut variants);
                chunk_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    push_variant(&body[chunk_start..], &mut variants);
    variants
}

fn push_variant(chunk: &str, out: &mut Vec<Variant>) {
    let mut doc_lines = Vec::new();
    for line in chunk.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("#[") {
            continue;
        }
        if let Some(text) = line.strip_prefix("///") {
            doc_lines.push(text.trim().to_string());
            continue;
        }
        if line.starts_with("//") {
            continue;
        }
        let name: String = line
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            out.push(Variant {
                name,
                doc: doc_lines.join(" "),
            });
        }
        return;
    }
}

/// Every whole-word byte-range occurrence of `needle` in `haystack`.
fn find_word_occurrences(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0usize;
    while let Some(rel) = haystack[start..].find(needle) {
        let match_start = start + rel;
        let match_end = match_start + needle.len();
        let preceding_ok = haystack[..match_start]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        let following_ok = haystack[match_end..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        if preceding_ok && following_ok {
            out.push((match_start, match_end));
        }
        start = match_start + 1;
    }
    out
}

/// Bytes consumed from the start of `s` by an optional `{ ... }` or
/// `( ... )` payload immediately at (or after leading whitespace in) `s`'s
/// start. Zero if there is no such payload (a unit variant reference).
fn skip_payload(s: &str) -> usize {
    let trimmed = s.trim_start();
    let ws = s.len() - trimmed.len();
    if let Some(rest) = trimmed.strip_prefix('{') {
        if let Some(close) = find_matching_close(rest, '{', '}') {
            return ws + 1 + close + 1;
        }
    } else if let Some(rest) = trimmed.strip_prefix('(') {
        if let Some(close) = find_matching_close(rest, '(', ')') {
            return ws + 1 + close + 1;
        }
    }
    0
}

/// Bytes consumed from the start of `s` (which starts right after an
/// or-pattern's `|`) by one alternative: an identifier/path
/// (`Enum::Variant`-shaped) plus its optional payload.
fn skip_path_and_payload(s: &str) -> usize {
    let trimmed = s.trim_start();
    let ws = s.len() - trimmed.len();
    let path_len = trimmed
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
        .unwrap_or(trimmed.len());
    ws + path_len + skip_payload(&trimmed[path_len..])
}

/// Classifies the occurrence of a watched `EnumName::Variant` spanning
/// `corpus[match_start..match_end]` as a PATTERN position (a `match`/`if
/// let`/`while let` read, not a value-producing construction).
///
/// Two rules, applied to `corpus` (which has already had `#[cfg(test)]`
/// items and comments stripped -- byte offsets are relative to that
/// stripped text):
/// 1. **Backward**: immediately preceded (skipping whitespace and a
///    leading `&`) by the keyword `let` -- covers `let PATTERN = ...`,
///    `if let PATTERN = ...`, `while let PATTERN = ...`.
/// 2. **Forward**: after skipping this occurrence's own optional payload,
///    and then transitively skipping any `| next_alternative` or-pattern
///    continuations (`LogRecord::seq`'s multi-variant arm is exactly this
///    shape), the next significant token is `=>` -- a `match` arm's LHS.
///
/// **Stated gaps** (none exercised by this tree's current source, verified
/// by hand while writing this guard): a pattern nested inside another
/// pattern's payload (`if let Some(Enum::Variant { .. }) = x`) is not
/// recognized by rule 1, since the token immediately before the occurrence
/// is `Some(`, not `let`; and a tuple-of-patterns match arm
/// (`(Enum::Variant, Other) => ...`) is not recognized by rule 2, since the
/// first token after the occurrence's own payload is a bare `,` rather
/// than `|` or `=>`. Both would need a real parser to close in general;
/// rules that out for this guard.
fn is_pattern_position(corpus: &str, match_start: usize, match_end: usize) -> bool {
    let before = corpus[..match_start].trim_end();
    let before = before
        .strip_suffix('&')
        .map(str::trim_end)
        .unwrap_or(before);
    if let Some(stripped) = before.strip_suffix("let") {
        let ok_boundary = stripped
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        if ok_boundary {
            return true;
        }
    }

    let mut pos = match_end + skip_payload(&corpus[match_end..]);
    for _ in 0..64 {
        let tail = &corpus[pos..];
        let trimmed = tail.trim_start();
        if trimmed.starts_with("=>") {
            return true;
        }
        if let Some(after_bar) = trimmed.strip_prefix('|') {
            if after_bar.starts_with('|') {
                break; // `||` is boolean-or, not an or-pattern continuation.
            }
            let consumed_ws = tail.len() - trimmed.len();
            pos += consumed_ws + 1 + skip_path_and_payload(after_bar);
            continue;
        }
        break;
    }
    false
}

/// Whether at least one occurrence of `EnumName::Variant` in `corpus` is a
/// genuine construction (not a pattern).
fn variant_has_production_construction(corpus: &str, enum_name: &str, variant: &str) -> bool {
    let needle = format!("{enum_name}::{variant}");
    find_word_occurrences(corpus, &needle)
        .into_iter()
        .any(|(s, e)| !is_pattern_position(corpus, s, e))
}

/// Builds the production corpus: every `crates/*/src/**/*.rs` file except
/// those [`excluded_whole_files`] identifies as test-only-by-declaration,
/// with `#[cfg(test)]` items and comment lines stripped from what remains.
fn production_corpus(root: &Path) -> String {
    let files = all_crate_source_files(root);
    let excluded = excluded_whole_files(&files);
    let mut corpus = String::new();
    for file in &files {
        if excluded.contains(file) {
            continue;
        }
        let text = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("read {file:?}: {e}"));
        corpus.push_str(&strip_full_line_comments(&strip_cfg_test_items(&text)));
        corpus.push('\n');
    }
    corpus
}

fn report_unconstructed(enum_name: &str, variant: &Variant, why: &str) -> String {
    format!(
        "`{enum_name}::{variant_name}` is declared ({why}) but no production code (outside \
         `#[cfg(test)]`) constructs it, and it is not on this file's `ALLOWLIST`.\n\
         Pick one:\n\
         \x20 1. WIRE IT -- add the call site that produces this variant, if the behavior it \
         promises should exist now.\n\
         \x20 2. REMOVE IT -- if nothing needs this vocabulary entry, delete the variant.\n\
         \x20 3. ALLOWLIST IT -- if this is a deliberate forward declaration (forward \
         compatibility, wire stability, or a seam whose consumer lives outside this tree), add \
         an `Allowlisted {{ enum_name: \"{enum_name}\", variant: \"{variant_name}\", reason: \
         \"...\", board_item: \"...\" }}` entry to `ALLOWLIST` in this file AND update the \
         variant's own doc comment in `{enum_name}`'s declaration to say it is \"{marker}\" -- \
         a declaration may be inert, but it must not claim to work.",
        variant_name = variant.name,
        marker = NOT_YET_IMPLEMENTED_MARKER,
    )
}

#[test]
fn every_declared_behavior_variant_is_constructed_or_allowlisted() {
    let root = workspace_root();
    let corpus = production_corpus(&root);

    let mut failures = Vec::new();
    let mut seen_allowlist_entries = BTreeSet::new();

    for watched in WATCHED_ENUMS {
        let decl_path = root.join(watched.decl_file);
        let decl_source = std::fs::read_to_string(&decl_path)
            .unwrap_or_else(|e| panic!("read {decl_path:?}: {e}"));
        let variants = extract_variants(&decl_source, watched.name);
        assert!(
            !variants.is_empty(),
            "parsed zero variants for `{}` out of {:?} -- the parser or the enum's shape \
             changed; fix `extract_variants`",
            watched.name,
            watched.decl_file
        );

        for variant in &variants {
            let constructed =
                variant_has_production_construction(&corpus, watched.name, &variant.name);
            let allowlisted = ALLOWLIST
                .iter()
                .find(|a| a.enum_name == watched.name && a.variant == variant.name);

            match (constructed, allowlisted) {
                (true, None) => {} // normal: wired, not on the list.
                (true, Some(entry)) => {
                    failures.push(format!(
                        "`{}::{}` is on `ALLOWLIST` (reason: {:?}) but \
                         production code now constructs it -- remove the stale allowlist entry.",
                        watched.name, variant.name, entry.reason
                    ));
                }
                (false, None) => {
                    failures.push(report_unconstructed(watched.name, variant, watched.why));
                }
                (false, Some(entry)) => {
                    seen_allowlist_entries.insert((entry.enum_name, entry.variant));
                    let doc_lower = variant.doc.to_lowercase();
                    if !doc_lower.contains(NOT_YET_IMPLEMENTED_MARKER) {
                        failures.push(format!(
                            "`{}::{}` is allowlisted but its own doc comment \
                             in `{}` does not say \"{}\" -- a reader of the enum declaration who \
                             never sees this guard would still believe the doc's original \
                             claim. Update the variant's doc comment, not just this file's \
                             allowlist entry. Current doc: {:?}",
                            watched.name,
                            variant.name,
                            watched.decl_file,
                            NOT_YET_IMPLEMENTED_MARKER,
                            variant.doc
                        ));
                    }
                }
            }
        }
    }

    for entry in ALLOWLIST {
        if !seen_allowlist_entries.contains(&(entry.enum_name, entry.variant)) {
            failures.push(format!(
                "`ALLOWLIST` names `{}::{}`, which is not a variant this guard could find on \
                 `WATCHED_ENUMS` (or it turned out to already be constructed and was reported \
                 above) -- fix the typo or drop the stale entry.",
                entry.enum_name, entry.variant
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "\n\n{}\n",
        failures.join("\n\n---\n\n")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercises the `#[cfg(test)]` handling this guard depends on, in
    /// isolation from the real tree: a variant that appears ONLY inside a
    /// `#[cfg(test)] mod tests { ... }` block must NOT count as
    /// constructed, and one that also appears in real production code
    /// (even in the same file, even alongside test-only uses) must.
    #[test]
    fn cfg_test_blocks_are_excluded_from_the_production_corpus() {
        let src = r#"
pub fn make_none() -> Truncation {
    Truncation::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_ever_built_in_a_test() {
        let v = Truncation::Artifact;
        assert!(matches!(v, Truncation::Artifact));
    }
}
"#;
        let corpus = strip_full_line_comments(&strip_cfg_test_items(src));
        assert!(
            variant_has_production_construction(&corpus, "Truncation", "None"),
            "a real production construction must still be found once cfg(test) is stripped"
        );
        assert!(
            !variant_has_production_construction(&corpus, "Truncation", "Artifact"),
            "a variant built ONLY inside `#[cfg(test)] mod tests` must not count as constructed \
             -- this is the exact vacuous-pass failure mode a naive scan has"
        );
    }

    /// The other cfg(test) shape: `#[cfg(test)] mod name;` declares an
    /// entire SEPARATE FILE as test-only. A plain filesystem walk would
    /// still read that file's bytes; `excluded_whole_files` must catch it
    /// before those bytes ever enter the corpus, mirroring
    /// `crates/conway-cli/src/tui/mod.rs`'s real `test_support` module.
    #[test]
    fn a_bare_cfg_test_mod_declaration_excludes_the_whole_named_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let parent = dir.path().join("lib.rs");
        std::fs::write(
            &parent,
            "pub mod real;\n#[cfg(test)]\npub(crate) mod test_support;\n",
        )
        .unwrap();
        let child = dir.path().join("test_support.rs");
        std::fs::write(
            &child,
            "pub fn build() -> Truncation { Truncation::Artifact }\n",
        )
        .unwrap();

        let files = vec![parent, child.clone()];
        let excluded = excluded_whole_files(&files);
        assert!(
            excluded.contains(&child),
            "a bare `#[cfg(test)] mod test_support;` declaration must exclude \
             `test_support.rs` from the production corpus, got: {excluded:?}"
        );
    }

    /// A pattern that merely READS a variant (a `match`/`if let` arm) must
    /// not count as a construction -- this is `LogRecord::ContextMask`'s
    /// exact real shape: `resolver.rs` matches it, nothing builds one.
    #[test]
    fn match_and_if_let_patterns_are_not_constructions() {
        let src = r#"
fn seq(&self) -> Option<u64> {
    match self {
        Mode::A { .. } | Mode::B { .. } => Some(1),
        Mode::C => None,
    }
}

fn read(&self) {
    if let Mode::B { flag, .. } = self {
        let _ = flag;
    }
}
"#;
        let corpus = strip_full_line_comments(&strip_cfg_test_items(src));
        assert!(
            !variant_has_production_construction(&corpus, "Mode", "A"),
            "the first alternative of an or-pattern (not immediately before `=>`) must still \
             be recognized as a pattern, not a construction"
        );
        assert!(
            !variant_has_production_construction(&corpus, "Mode", "B"),
            "`Mode::B` appears only as an or-pattern alternative and an `if let` pattern -- \
             never constructed"
        );
        assert!(
            !variant_has_production_construction(&corpus, "Mode", "C"),
            "a standalone match arm pattern must not count as a construction"
        );
    }

    /// The mirror image, matching `CacheMode::ExplicitBreakpoints`'s real
    /// fix (`profile.rs`'s `to_cache_mode`): a match arm's RHS -- the same
    /// textual shape a pattern has, just on the other side of `=>` -- IS a
    /// construction.
    #[test]
    fn a_match_arms_right_hand_side_is_a_construction() {
        let src = r#"
fn to_cache_mode(spec: &Spec) -> CacheMode {
    match spec {
        Spec::Explicit { n } => CacheMode::ExplicitBreakpoints { n: *n },
        Spec::None => CacheMode::None,
    }
}
"#;
        let corpus = strip_full_line_comments(&strip_cfg_test_items(src));
        assert!(variant_has_production_construction(
            &corpus,
            "CacheMode",
            "ExplicitBreakpoints"
        ));
        assert!(variant_has_production_construction(
            &corpus,
            "CacheMode",
            "None"
        ));
    }

    #[test]
    fn extract_variants_parses_struct_and_unit_variants_with_docs() {
        let src = r#"
#[non_exhaustive]
pub enum TruncationPolicy {
    None,
    Head {
        max_bytes: u64,
    },
    /// Spill the full output to an Artifact, keep a pointer in context.
    Artifact,
}
"#;
        let variants = extract_variants(src, "TruncationPolicy");
        let names: Vec<&str> = variants.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["None", "Head", "Artifact"]);
        assert!(variants[2].doc.contains("Spill the full output"));
        assert!(variants[0].doc.is_empty());
    }

    /// The doc-comment honesty invariant, standalone: an allowlisted
    /// variant whose doc comment does not disclose its status is a guard
    /// failure distinct from "unconstructed", because the failure this
    /// invariant catches is a doc comment that still claims to work.
    #[test]
    fn allowlisted_variants_declared_in_this_tree_disclose_their_status_in_their_own_doc() {
        let root = workspace_root();
        for entry in ALLOWLIST {
            let watched = WATCHED_ENUMS
                .iter()
                .find(|w| w.name == entry.enum_name)
                .unwrap_or_else(|| panic!("allowlisted enum `{}` is not watched", entry.enum_name));
            let decl_source = std::fs::read_to_string(root.join(watched.decl_file)).unwrap();
            let variants = extract_variants(&decl_source, watched.name);
            let variant = variants
                .iter()
                .find(|v| v.name == entry.variant)
                .unwrap_or_else(|| panic!("`{}::{}` not found", entry.enum_name, entry.variant));
            assert!(
                variant
                    .doc
                    .to_lowercase()
                    .contains(NOT_YET_IMPLEMENTED_MARKER),
                "`{}::{}`'s doc comment must say \"{}\": {:?}",
                entry.enum_name,
                entry.variant,
                NOT_YET_IMPLEMENTED_MARKER,
                variant.doc
            );
        }
    }
}
