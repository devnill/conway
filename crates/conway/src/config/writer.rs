//! A config **writer** -- the missing half `crate::config::merge`'s own
//! module doc names ("a layered read with no writer outside test
//! fixtures"). [`set_plugin_installed`] is the first (and, as of this
//! item, only) mutation this crate performs against a real
//! `settings.json`: adding or removing one id from the top-level
//! `plugins.install` array, decision `01M0K8BAXJ6THVJAPK0JZ17VV6`'s
//! resolved user layer (`~/.conway/settings.json`, or
//! `$CONWAY_CONFIG_DIR/settings.json` -- see [`super::discovery::user_config_path`],
//! this module's own caller).
//!
//! # Why this is a hand-rolled text patch, not a parse/mutate/reserialize
//!
//! `settings.json` is a **hand-editable** file, layered under project/env/
//! CLI (`merge.rs`'s own five-source precedence) -- an operator's own
//! comments-as-keys (`"//": "..."`), unrelated top-level sections
//! (`backends`, `routing`, `roles`, ...), and whatever key ORDER they
//! happened to type are all things this writer must leave untouched. The
//! obvious approach -- parse into `serde_json::Value`, mutate one field,
//! `serde_json::to_string_pretty` the whole thing back out -- fails that
//! bar on two counts: `serde_json::Map` (without the `preserve_order`
//! Cargo feature, which this workspace does not enable -- pulling it in
//! would add `indexmap`/`ahash` as new transitive dependencies for a
//! writer whose own scope is ONE array, the exact "stop and report
//! instead" case dependency minimalism names) is a `BTreeMap`, so a full
//! reserialize silently ALPHABETIZES every top-level key; and a full
//! reserialize necessarily re-renders every value's own formatting/
//! whitespace, even ones this write never touched. The existing precedent
//! for a config-file *rewrite* in this crate
//! (`crate::permissions::rewrite_permission_file_removing`) accepts
//! exactly that cost for `permissions.json` -- a narrow, single-purpose
//! file with a `#[serde(deny_unknown_fields)]`-shaped struct behind it, so
//! there is no "unrelated key" to lose. `settings.json` is the opposite:
//! the operator's general, hand-maintained, many-sectioned config, so that
//! precedent's cost is not acceptable here.
//!
//! So this module never reserializes the document. It locates the byte
//! span of exactly the one thing being changed -- the `plugins.install`
//! array, or the `plugins`/`install` object member if either is missing --
//! via a small, purpose-built JSON scanner (`scan_object_members`/
//! `scan_array_elements`, below -- private to this module), and splices a
//! replacement into that span alone. Every byte outside the touched span -- including any
//! `"//"`-keyed comment convention, whatever section order the operator
//! chose, and the file's own indentation style where it can be detected --
//! survives untouched, because it is never re-emitted from a parsed
//! representation at all; it is copied verbatim from the original text on
//! either side of the splice.
//!
//! **This scanner is not a general JSON parser and does not try to be
//! one.** It never decodes an escape sequence (`\uXXXX`, `\"`, ...) into
//! its represented character -- a JSON key/string element this module
//! compares against a known ASCII literal (`"plugins"`, `"install"`, a
//! plugin id such as `"conway.memory"`) is compared against its RAW,
//! still-escaped source text. Every id this module is ever asked to
//! toggle is a bare dotted-lowercase manifest id
//! (`conway_core::ports::plugin::PluginManifest::id`) with no JSON
//! metacharacter in it, so raw comparison and decoded comparison always
//! agree for the inputs this module actually receives; a key or element
//! that legitimately needed escaping to hold `"plugins"`/`"install"`/a
//! plugin id verbatim is not a case any settings.json this codebase
//! produces or expects to see.
//!
//! # Safety posture: refuse rather than guess
//!
//! A file that does not parse as strict JSON at all is never touched --
//! [`set_plugin_installed`] validates the WHOLE document with
//! `serde_json::from_str` before attempting any edit and returns a named
//! [`crate::error::ConwayError::Config`] instead, mirroring
//! `rewrite_permission_file_removing`'s own "not valid JSON, refusing to
//! rewrite it blindly" posture. A goal state already holding (the id is
//! already present when turning a plugin ON, or already absent when
//! turning one OFF) performs no write at all -- so a toggle that is a
//! no-op can never even flip the file's mtime, let alone risk corrupting
//! it.

use std::path::Path;

use crate::error::{ConwayError, Result};

/// Backstop on the patch loop in [`set_plugin_installed`]. Each pass
/// strictly shrinks the document (or stops), so a real file converges in
/// one or two; this only bounds a pathological case rather than expressing
/// a supported limit.
const MAX_PATCH_PASSES: usize = 64;

/// Adds (`installed: true`) or removes (`installed: false`) `plugin_id`
/// from the top-level `plugins.install` array of the JSON document at
/// `path`, writing the result via a tmp-then-rename (the same durability
/// shape `crate::permissions::rewrite_permission_file_removing` already
/// uses for `permissions.json`) so a reader can never observe a
/// partially-written file.
///
/// Returns `Ok(true)` if a write happened, `Ok(false)` if the goal state
/// already held (no write -- see this module's own doc, "Safety posture").
///
/// A missing file is created (parent directories included) when
/// `installed` is `true`; when `installed` is `false` and the file does
/// not exist, there is nothing to remove, so this returns `Ok(false)`
/// with no filesystem write at all.
///
/// See this module's own doc for why this is a targeted text splice
/// rather than a parse-mutate-reserialize round trip, and for the safety
/// posture on a file that is not valid JSON.
pub fn set_plugin_installed(path: &Path, plugin_id: &str, installed: bool) -> Result<bool> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(ConwayError::Io(e)),
    };

    let new_text = if text.trim().is_empty() {
        // A missing OR whitespace-only file is treated identically: the
        // real config loader (`crate::config::merge::read_json_layer`)
        // cannot successfully parse whitespace-only content as JSON
        // either, so there is no existing document this writer could be
        // asked to "preserve" here -- an empty/absent settings.json is
        // exactly the state a fresh, minimal document strictly improves
        // on, never regresses.
        if !installed {
            return Ok(false);
        }
        fresh_document(plugin_id)
    } else {
        // Validate the WHOLE document parses as strict JSON before
        // touching anything -- refuse to rewrite blindly otherwise (this
        // module's own doc, "Safety posture").
        if let Err(e) = serde_json::from_str::<serde_json::Value>(&text) {
            return Err(ConwayError::Config {
                path: Some(path.to_path_buf()),
                message: format!(
                    "{} is not valid JSON, refusing to rewrite it blindly: {e}",
                    path.display()
                ),
            });
        }
        // Applied REPEATEDLY until the goal state holds, not once.
        //
        // A single pass removes a single array element, so a hand-edited
        // file listing the same id twice -- an ordinary copy-paste slip --
        // would keep the plugin installed after a toggle-off, while the UI
        // reported success and showed it as off. One pass is correct for
        // every other case and simply reports "nothing to do" on the second
        // iteration, so looping costs a re-scan and buys the guarantee that
        // the post-condition actually holds.
        //
        // The bound exists because this loop's termination depends on each
        // pass strictly shrinking the document; it is a backstop against a
        // future edit breaking that, never an expected limit -- two
        // iterations is the normal maximum for a single duplicate.
        let mut current = text.clone();
        let mut changed = false;
        for _ in 0..MAX_PATCH_PASSES {
            match patch_install_array(&current, plugin_id, installed) {
                Ok(Some(patched)) => {
                    current = patched;
                    changed = true;
                }
                Ok(None) => break,
                Err(msg) => {
                    return Err(ConwayError::Config {
                        path: Some(path.to_path_buf()),
                        message: format!("{}: {msg}", path.display()),
                    })
                }
            }
        }
        if !changed {
            return Ok(false);
        }
        current
    };

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // tmp-then-rename -- the same durability shape
    // `crate::permissions::rewrite_permission_file_removing` already uses,
    // so a reader (including this crate's own five-source `load`) can
    // never observe a partially-written settings.json.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &new_text)?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

/// A fresh, minimal document for the "no existing settings.json" case --
/// exactly the shape an operator hand-authoring a new file from scratch
/// would write.
fn fresh_document(plugin_id: &str) -> String {
    format!(
        "{{\n  \"plugins\": {{\n    \"install\": [\n      {}\n    ]\n  }}\n}}\n",
        json_string_literal(plugin_id)
    )
}

/// The targeted patch itself, over an already-validated-as-JSON `text`.
/// Returns `Ok(Some(new_text))` when a splice is needed, `Ok(None)` when
/// the goal state already holds, `Err(message)` when `text`'s shape at the
/// one path this function cares about (`plugins`/`plugins.install`) is not
/// what it must be to edit safely (e.g. `"plugins"` present but not an
/// object) -- named, never guessed past.
fn patch_install_array(
    text: &str,
    plugin_id: &str,
    installed: bool,
) -> std::result::Result<Option<String>, String> {
    let bytes = text.as_bytes();
    let root_open = skip_ws(bytes, 0);
    if bytes.get(root_open) != Some(&b'{') {
        return Err("the top-level JSON value must be an object".to_string());
    }
    let (root_members, root_close) = scan_object_members(text, root_open)?;

    // LAST match, not first. JSON permits duplicate object keys and
    // `serde_json` resolves them last-wins, so the last `"plugins"` block is
    // the one that actually governs the loaded config. Editing the first
    // would splice a block the loader discards: the write succeeds, the file
    // changes, and the effective configuration does not -- a silent no-op
    // that reports success, which is the failure mode this module exists to
    // avoid. Verified against `serde_json` directly rather than assumed.
    let Some(plugins_member) = root_members.iter().rev().find(|m| m.key == "plugins") else {
        if !installed {
            return Ok(None);
        }
        let value = format!("{{\"install\": [{}]}}", json_string_literal(plugin_id));
        return Ok(Some(insert_member(
            text,
            root_open,
            &root_members,
            root_close,
            "plugins",
            &value,
        )));
    };

    if bytes.get(plugins_member.value_start) != Some(&b'{') {
        return Err("\"plugins\" must be a JSON object".to_string());
    }
    let (plugins_members, plugins_close) = scan_object_members(text, plugins_member.value_start)?;

    // LAST match again, for the same last-wins reason as `"plugins"` above.
    let Some(install_member) = plugins_members.iter().rev().find(|m| m.key == "install") else {
        if !installed {
            return Ok(None);
        }
        let value = format!("[{}]", json_string_literal(plugin_id));
        return Ok(Some(insert_member(
            text,
            plugins_member.value_start,
            &plugins_members,
            plugins_close,
            "install",
            &value,
        )));
    };

    if bytes.get(install_member.value_start) != Some(&b'[') {
        return Err("\"plugins.install\" must be a JSON array".to_string());
    }
    let (elements, array_close) = scan_array_elements(text, install_member.value_start)?;

    let found_index = elements
        .iter()
        .position(|e| e.raw_string == Some(plugin_id));

    match (installed, found_index) {
        (true, Some(_)) => Ok(None),
        (false, None) => Ok(None),
        (true, None) => {
            let raw_value = json_string_literal(plugin_id);
            Ok(Some(insert_array_element(
                text,
                install_member.value_start,
                &elements,
                array_close,
                &raw_value,
            )))
        }
        (false, Some(idx)) => Ok(Some(remove_array_element(
            text,
            install_member.value_start,
            array_close,
            &elements,
            idx,
        ))),
    }
}

/// A JSON string or `serde_json::to_string`'s own escaping -- reused here
/// (`serde_json` is already a dependency; this borrows its string-escaping
/// logic rather than reimplementing it) so a plugin id containing a
/// character that genuinely needs escaping is still written correctly,
/// even though no first-party plugin id ever does.
fn json_string_literal(s: &str) -> String {
    serde_json::to_string(s).expect("a &str always serializes to a JSON string")
}

/// One member of a scanned JSON object -- `key` is the RAW (still-escaped)
/// text between the quotes, not decoded (see this module's own doc for
/// why that is safe for every comparison this module performs).
struct Member<'a> {
    key: &'a str,
    key_start: usize,
    colon_pos: usize,
    value_start: usize,
}

/// One element of a scanned JSON array. `raw_string` is `Some(raw inner
/// text)` when the element is a JSON string literal (every element this
/// module ever writes or removes is one), `None` for any other JSON value
/// (a number, object, ... an operator hand-added -- left untouched,
/// never matched, never a candidate for removal).
struct Elem<'a> {
    start: usize,
    end: usize,
    raw_string: Option<&'a str>,
}

fn skip_ws(bytes: &[u8], mut pos: usize) -> usize {
    while matches!(bytes.get(pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        pos += 1;
    }
    pos
}

/// Returns the byte offset right after the string literal beginning at
/// `bytes[pos]` (`bytes[pos] == b'"'`) -- never decodes, just finds the
/// end, correctly skipping an escaped quote (`\"`) so it does not
/// terminate the scan early. Every escape sequence (including `\uXXXX`) is
/// skipped two bytes at a time and the loop's own byte-by-byte advance
/// naturally walks over the remaining hex digits of a `\u` escape as
/// ordinary content bytes -- no special-casing needed since none of them
/// can equal `"` or `\`.
fn skip_string(bytes: &[u8], pos: usize) -> std::result::Result<usize, String> {
    let mut i = pos + 1;
    loop {
        match bytes.get(i) {
            None => return Err("unterminated string literal".to_string()),
            Some(b'"') => return Ok(i + 1),
            Some(b'\\') => {
                if bytes.get(i + 1).is_none() {
                    return Err("string literal ends mid-escape".to_string());
                }
                i += 2;
            }
            Some(_) => i += 1,
        }
    }
}

/// Skips one JSON value beginning at `bytes[pos]` (after leading
/// whitespace has already been skipped by the caller), returning the byte
/// offset right after it.
fn skip_value(bytes: &[u8], pos: usize) -> std::result::Result<usize, String> {
    match bytes.get(pos) {
        Some(b'"') => skip_string(bytes, pos),
        Some(b'{') | Some(b'[') => skip_bracketed(bytes, pos),
        Some(b't') => expect_literal(bytes, pos, b"true"),
        Some(b'f') => expect_literal(bytes, pos, b"false"),
        Some(b'n') => expect_literal(bytes, pos, b"null"),
        Some(c) if c.is_ascii_digit() || *c == b'-' => Ok(skip_number(bytes, pos)),
        _ => Err(format!("expected a JSON value at byte offset {pos}")),
    }
}

/// Depth-counts a `{...}`/`[...]` span starting at `bytes[pos]` (a `{` or
/// `[`) to its matching close, skipping over any string literal (so a
/// bracket character INSIDE a string is never mistaken for a structural
/// one). Depth-only, never bracket-TYPE-checked -- safe because the caller
/// (`set_plugin_installed`) already validated the whole document with
/// `serde_json::from_str` before this function is ever reached, so
/// mismatched bracket types cannot occur.
fn skip_bracketed(bytes: &[u8], pos: usize) -> std::result::Result<usize, String> {
    let mut depth: i32 = 0;
    let mut i = pos;
    loop {
        match bytes.get(i) {
            None => return Err("unexpected end of input inside a JSON structure".to_string()),
            Some(b'"') => {
                i = skip_string(bytes, i)?;
            }
            Some(b'{') | Some(b'[') => {
                depth += 1;
                i += 1;
            }
            Some(b'}') | Some(b']') => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return Ok(i);
                }
            }
            Some(_) => i += 1,
        }
    }
}

fn expect_literal(
    bytes: &[u8],
    pos: usize,
    lit: &'static [u8],
) -> std::result::Result<usize, String> {
    if bytes.len() >= pos + lit.len() && &bytes[pos..pos + lit.len()] == lit {
        Ok(pos + lit.len())
    } else {
        Err(format!("expected a JSON literal at byte offset {pos}"))
    }
}

fn skip_number(bytes: &[u8], pos: usize) -> usize {
    let mut i = pos;
    while matches!(
        bytes.get(i),
        Some(b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
    ) {
        i += 1;
    }
    i
}

/// Scans the direct (depth-1) members of the JSON object opening at
/// `bytes[open_brace]` (`== b'{'`), returning them in source order plus
/// the byte offset of the matching close brace. Never recurses into a
/// nested object/array's own members -- `skip_value` treats those as
/// opaque spans, exactly what a targeted, one-path patcher needs.
fn scan_object_members(
    text: &str,
    open_brace: usize,
) -> std::result::Result<(Vec<Member<'_>>, usize), String> {
    let bytes = text.as_bytes();
    let mut pos = skip_ws(bytes, open_brace + 1);
    let mut members = Vec::new();
    if bytes.get(pos) == Some(&b'}') {
        return Ok((members, pos));
    }
    loop {
        pos = skip_ws(bytes, pos);
        if bytes.get(pos) != Some(&b'"') {
            return Err(format!("expected an object key at byte offset {pos}"));
        }
        let key_start = pos;
        let key_end = skip_string(bytes, pos)?;
        let key = &text[key_start + 1..key_end - 1];
        pos = skip_ws(bytes, key_end);
        if bytes.get(pos) != Some(&b':') {
            return Err(format!(
                "expected ':' after an object key at byte offset {pos}"
            ));
        }
        let colon_pos = pos;
        pos = skip_ws(bytes, pos + 1);
        let value_start = pos;
        pos = skip_value(bytes, pos)?;
        members.push(Member {
            key,
            key_start,
            colon_pos,
            value_start,
        });
        pos = skip_ws(bytes, pos);
        match bytes.get(pos) {
            Some(b',') => pos += 1,
            Some(b'}') => return Ok((members, pos)),
            _ => return Err(format!("expected ',' or '}}' at byte offset {pos}")),
        }
    }
}

/// The array-shaped sibling of [`scan_object_members`]: direct elements of
/// the JSON array opening at `bytes[open_bracket]` (`== b'['`), in source
/// order, plus the byte offset of the matching close bracket.
fn scan_array_elements(
    text: &str,
    open_bracket: usize,
) -> std::result::Result<(Vec<Elem<'_>>, usize), String> {
    let bytes = text.as_bytes();
    let mut pos = skip_ws(bytes, open_bracket + 1);
    let mut elems = Vec::new();
    if bytes.get(pos) == Some(&b']') {
        return Ok((elems, pos));
    }
    loop {
        pos = skip_ws(bytes, pos);
        let start = pos;
        let raw_string = if bytes.get(pos) == Some(&b'"') {
            let end = skip_string(bytes, pos)?;
            let s = &text[start + 1..end - 1];
            pos = end;
            Some(s)
        } else {
            pos = skip_value(bytes, pos)?;
            None
        };
        let end = pos;
        elems.push(Elem {
            start,
            end,
            raw_string,
        });
        pos = skip_ws(bytes, pos);
        match bytes.get(pos) {
            Some(b',') => pos += 1,
            Some(b']') => return Ok((elems, pos)),
            _ => return Err(format!("expected ',' or ']' at byte offset {pos}")),
        }
    }
}

/// The whitespace (spaces/tabs only) immediately preceding `pos` on its
/// own line -- used to match a newly-inserted member/element's
/// indentation to whatever the surrounding document already uses.
fn line_leading_whitespace(text: &str, pos: usize) -> String {
    let line_start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
    text[line_start..pos]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Inserts a new `"key": raw_value` member as the FIRST member of the
/// object spanning `open_brace..=close_brace` (never the last -- avoids
/// ever needing to add/move a trailing comma on whatever member used to be
/// last), splicing only within that object's own span. Detects pretty
/// (newline + indent per member) vs. compact (single-line) style from the
/// FIRST existing member, if any; an EMPTY object is fully replaced with a
/// freshly two-space-indented `{ "key": raw_value }` (never a partial
/// splice into nothing, and still confined to exactly `open_brace..=
/// close_brace` -- there is no existing member formatting to lose either
/// way).
fn insert_member(
    text: &str,
    open_brace: usize,
    members: &[Member<'_>],
    close_brace: usize,
    key: &str,
    raw_value: &str,
) -> String {
    let key_lit = json_string_literal(key);
    if members.is_empty() {
        let outer_indent = line_leading_whitespace(text, open_brace);
        let inner_indent = format!("{outer_indent}  ");
        let body = format!("\n{inner_indent}{key_lit}: {raw_value}\n{outer_indent}");
        format!(
            "{}{{{}}}{}",
            &text[..open_brace],
            body,
            &text[close_brace + 1..]
        )
    } else {
        let first = &members[0];
        let pretty = text[open_brace + 1..first.key_start].contains('\n');
        let space_after_colon = text.as_bytes().get(first.colon_pos + 1) == Some(&b' ');
        let colon = if space_after_colon { ": " } else { ":" };
        let mut inserted = String::new();
        if pretty {
            inserted.push('\n');
            inserted.push_str(&line_leading_whitespace(text, first.key_start));
        }
        inserted.push_str(&key_lit);
        inserted.push_str(colon);
        inserted.push_str(raw_value);
        inserted.push(',');
        format!(
            "{}{}{}",
            &text[..open_brace + 1],
            inserted,
            &text[open_brace + 1..]
        )
    }
}

/// The array-element sibling of [`insert_member`]: appends `raw_value` as
/// the LAST element of the array spanning `open_bracket..=close_bracket`,
/// matching the existing elements' pretty/compact style (detected from the
/// first element, if any). An EMPTY array is fully replaced the same way
/// an empty object is above.
fn insert_array_element(
    text: &str,
    open_bracket: usize,
    elements: &[Elem<'_>],
    close_bracket: usize,
    raw_value: &str,
) -> String {
    if elements.is_empty() {
        let outer_indent = line_leading_whitespace(text, open_bracket);
        let inner_indent = format!("{outer_indent}  ");
        let body = format!("\n{inner_indent}{raw_value}\n{outer_indent}");
        format!(
            "{}[{}]{}",
            &text[..open_bracket],
            body,
            &text[close_bracket + 1..]
        )
    } else {
        let first = &elements[0];
        let pretty = text[open_bracket + 1..first.start].contains('\n');
        let last = elements.last().expect("checked non-empty above");
        let mut inserted = String::new();
        inserted.push(',');
        if pretty {
            inserted.push('\n');
            inserted.push_str(&line_leading_whitespace(text, first.start));
        }
        inserted.push_str(raw_value);
        format!("{}{}{}", &text[..last.end], inserted, &text[last.end..])
    }
}

/// Removes `elements[idx]` from the array spanning
/// `open_bracket..=close_bracket`, along with exactly the one separating
/// comma that becomes dangling -- the comma AFTER it when it is the first
/// of several elements, the comma BEFORE it otherwise. The sole remaining
/// element case collapses the whole array to a compact `[]` rather than
/// leaving stray whitespace behind. Every OTHER element's own span is
/// untouched byte-for-byte.
fn remove_array_element(
    text: &str,
    open_bracket: usize,
    close_bracket: usize,
    elements: &[Elem<'_>],
    idx: usize,
) -> String {
    if elements.len() == 1 {
        format!("{}[]{}", &text[..open_bracket], &text[close_bracket + 1..])
    } else if idx == 0 {
        format!(
            "{}{}",
            &text[..elements[0].start],
            &text[elements[1].start..]
        )
    } else {
        format!(
            "{}{}",
            &text[..elements[idx - 1].end],
            &text[elements[idx].end..]
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn tempfile_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "conway-config-writer-test-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn unique_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    /// Every test in this module writes into a fresh temp directory of its
    /// own, NEVER `~/.conway/` -- the hard constraint this whole item is
    /// bound by. `set_plugin_installed` itself never reads `HOME`/
    /// `CONWAY_CONFIG_DIR`; the caller (this crate's own
    /// `discovery::user_config_path`) is what resolves the real path, and
    /// no test here calls that function at all.
    fn no_real_home_touched_guard() {
        // Documented, not enforced by an assertion with nothing to assert
        // against -- see the isolation reasoning above and in this crate's
        // `tests/config_isolation_guard.rs`.
    }

    // ---- No existing file ----

    #[test]
    fn creates_a_fresh_file_with_parent_dirs_when_turning_a_plugin_on() {
        no_real_home_touched_guard();
        let dir = tempfile_dir();
        let path = dir.join("nested").join("settings.json");
        let wrote = set_plugin_installed(&path, "conway.memory", true).expect("write");
        assert!(wrote);
        let text = std::fs::read_to_string(&path).expect("read back");
        let value: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(
            value["plugins"]["install"],
            serde_json::json!(["conway.memory"])
        );
    }

    #[test]
    fn removing_from_a_nonexistent_file_is_a_no_op() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let wrote = set_plugin_installed(&path, "conway.memory", false).expect("write");
        assert!(!wrote);
        assert!(
            !path.exists(),
            "no file should have been created for a no-op removal"
        );
    }

    #[test]
    fn an_empty_existing_file_is_treated_like_a_missing_one() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, "").unwrap();
        let wrote = set_plugin_installed(&path, "conway.memory", true).expect("write");
        assert!(wrote);
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            value["plugins"]["install"],
            serde_json::json!(["conway.memory"])
        );
    }

    // ---- Invalid JSON: refuse, never corrupt ----

    #[test]
    fn refuses_to_touch_a_file_that_is_not_valid_json() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let original = "{ this is not json";
        std::fs::write(&path, original).unwrap();
        let err = set_plugin_installed(&path, "conway.memory", true).unwrap_err();
        assert!(err.to_string().contains("not valid JSON"), "got: {err}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "an invalid file must be left byte-for-byte untouched"
        );
    }

    // ---- The highest-risk case: a hand-edited file with comments (the
    // "//"-key convention), unusual key ordering, and unrelated keys ----

    fn hand_edited_fixture() -> &'static str {
        r#"{
  "//": "operator note: do not touch the backends section by hand",
  "zebra_first_key": "kept exactly as-is",
  "default_role": "coder",
  "backends": {
    "anthropic": { "kind": "anthropic", "api_key": "sk-unused" }
  },
  "plugins": {
    "_comment_plugins": "toggle plugins here",
    "install": [
      "conway.skills",
      "conway.stepguard"
    ]
  },
  "apple_last_key": 42
}
"#
    }

    #[test]
    fn adding_a_plugin_to_a_hand_edited_file_preserves_comments_ordering_and_unrelated_keys() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, hand_edited_fixture()).unwrap();

        let wrote = set_plugin_installed(&path, "conway.memory", true).expect("write");
        assert!(wrote);

        let new_text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&new_text).expect("still valid json");

        // The one thing that changed.
        assert_eq!(
            value["plugins"]["install"],
            serde_json::json!(["conway.skills", "conway.stepguard", "conway.memory"])
        );

        // Everything else: byte-for-byte substring survival, not just
        // semantic equality -- proves the "comment" keys, ordering, and
        // formatting were never touched, not merely that a reserialize
        // happened to produce equivalent JSON.
        assert!(new_text
            .contains("\"//\": \"operator note: do not touch the backends section by hand\""));
        assert!(new_text.contains("\"_comment_plugins\": \"toggle plugins here\""));
        assert!(new_text.contains("\"zebra_first_key\": \"kept exactly as-is\""));
        assert!(new_text.contains("\"apple_last_key\": 42"));
        assert!(new_text
            .contains("\"anthropic\": { \"kind\": \"anthropic\", \"api_key\": \"sk-unused\" }"));
        // Key order at the top level is unchanged -- "//" still precedes
        // "zebra_first_key", which still precedes "default_role", etc.
        let pos = |needle: &str| new_text.find(needle).expect(needle);
        assert!(pos("\"//\"") < pos("\"zebra_first_key\""));
        assert!(pos("\"zebra_first_key\"") < pos("\"default_role\""));
        assert!(pos("\"default_role\"") < pos("\"backends\""));
        assert!(pos("\"backends\"") < pos("\"plugins\""));
        assert!(pos("\"plugins\"") < pos("\"apple_last_key\""));

        // Existing array elements kept their own original formatting
        // (each on its own indented line) -- only the appended element is
        // new text.
        assert!(new_text.contains("      \"conway.skills\","));
        assert!(new_text.contains("      \"conway.stepguard\","));
    }

    #[test]
    fn removing_a_plugin_from_a_hand_edited_file_preserves_everything_else() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, hand_edited_fixture()).unwrap();

        let wrote = set_plugin_installed(&path, "conway.skills", false).expect("write");
        assert!(wrote);

        let new_text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&new_text).expect("still valid json");
        assert_eq!(
            value["plugins"]["install"],
            serde_json::json!(["conway.stepguard"])
        );
        assert!(new_text
            .contains("\"//\": \"operator note: do not touch the backends section by hand\""));
        assert!(new_text.contains("\"apple_last_key\": 42"));
        assert!(new_text
            .contains("\"anthropic\": { \"kind\": \"anthropic\", \"api_key\": \"sk-unused\" }"));
    }

    #[test]
    fn removing_the_only_element_collapses_the_array_to_empty() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"plugins": {"install": ["conway.skills"]}}"#).unwrap();
        let wrote = set_plugin_installed(&path, "conway.skills", false).expect("write");
        assert!(wrote);
        let new_text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&new_text).unwrap();
        assert_eq!(value["plugins"]["install"], serde_json::json!([]));
    }

    /// Toggling OFF must remove EVERY occurrence, not just the first.
    ///
    /// Regression for a review finding. `patch_install_array` locates one
    /// element per pass, so a single pass left a hand-written duplicate
    /// behind: the plugin stayed installed after restart while the toggle
    /// reported success and the UI showed it as off. A duplicated entry is
    /// an ordinary copy-paste slip in a hand-edited file, not a
    /// pathological input.
    #[test]
    fn removing_a_duplicated_id_removes_every_occurrence() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"plugins": {"install": ["conway.memory", "a", "conway.memory"]}}"#,
        )
        .unwrap();
        let wrote = set_plugin_installed(&path, "conway.memory", false).expect("write");
        assert!(wrote);
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            value["plugins"]["install"],
            serde_json::json!(["a"]),
            "every occurrence must go -- leaving one behind keeps the plugin \
             installed while the toggle claims it is off"
        );
    }

    /// JSON allows duplicate object keys and `serde_json` resolves them
    /// last-wins, so the LAST `plugins` block is the one that governs the
    /// loaded config. Editing the first would change the file and leave the
    /// effective configuration untouched -- a write that reports success
    /// and does nothing.
    #[test]
    fn a_duplicate_plugins_key_is_patched_where_serde_json_actually_reads_it() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"plugins": {"install": ["dead"]}, "plugins": {"install": ["live"]}}"#,
        )
        .unwrap();
        let wrote = set_plugin_installed(&path, "conway.memory", true).expect("write");
        assert!(wrote);
        let new_text = std::fs::read_to_string(&path).unwrap();
        // Parse exactly as the real loader does: last-wins.
        let value: serde_json::Value = serde_json::from_str(&new_text).unwrap();
        let install = &value["plugins"]["install"];
        assert!(
            install
                .as_array()
                .expect("install is an array")
                .iter()
                .any(|v| v == "conway.memory"),
            "the id must land in the block serde_json actually resolves to, \
             not the shadowed one: {new_text}"
        );
        assert!(
            install.as_array().unwrap().iter().any(|v| v == "live"),
            "the surviving block must be the live one: {new_text}"
        );
    }

    #[test]
    fn removing_the_first_of_several_elements_keeps_the_rest() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"plugins": {"install": ["a", "b", "c"]}}"#).unwrap();
        let wrote = set_plugin_installed(&path, "a", false).expect("write");
        assert!(wrote);
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["plugins"]["install"], serde_json::json!(["b", "c"]));
    }

    #[test]
    fn removing_a_middle_element_keeps_the_others_in_order() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"plugins": {"install": ["a", "b", "c"]}}"#).unwrap();
        let wrote = set_plugin_installed(&path, "b", false).expect("write");
        assert!(wrote);
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["plugins"]["install"], serde_json::json!(["a", "c"]));
    }

    // ---- Idempotence / no-op writes ----

    #[test]
    fn adding_an_already_present_plugin_is_a_no_op_and_leaves_the_file_untouched() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let original = r#"{"plugins": {"install": ["conway.memory"]}}"#;
        std::fs::write(&path, original).unwrap();
        let mtime_before = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));

        let wrote = set_plugin_installed(&path, "conway.memory", true).expect("write");
        assert!(!wrote);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let mtime_after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(
            mtime_before, mtime_after,
            "a no-op must never touch the file at all"
        );
    }

    #[test]
    fn removing_an_already_absent_plugin_is_a_no_op() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let original = r#"{"plugins": {"install": ["conway.memory"]}}"#;
        std::fs::write(&path, original).unwrap();
        let wrote = set_plugin_installed(&path, "conway.skills", false).expect("write");
        assert!(!wrote);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    // ---- Missing "plugins" / "install" keys, inserted fresh ----

    #[test]
    fn inserts_a_fresh_plugins_section_alongside_unrelated_top_level_keys() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            "{\n  \"default_role\": \"coder\",\n  \"limits\": { \"max_steps\": 40 }\n}\n",
        )
        .unwrap();
        let wrote = set_plugin_installed(&path, "conway.memory", true).expect("write");
        assert!(wrote);
        let new_text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&new_text).unwrap();
        assert_eq!(
            value["plugins"]["install"],
            serde_json::json!(["conway.memory"])
        );
        assert!(new_text.contains("\"default_role\": \"coder\""));
        assert!(new_text.contains("\"max_steps\": 40"));
    }

    #[test]
    fn inserts_a_fresh_install_array_when_plugins_exists_but_install_is_absent() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"plugins": {"mcp": []}, "default_role": "coder"}"#,
        )
        .unwrap();
        let wrote = set_plugin_installed(&path, "conway.memory", true).expect("write");
        assert!(wrote);
        let new_text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&new_text).unwrap();
        assert_eq!(
            value["plugins"]["install"],
            serde_json::json!(["conway.memory"])
        );
        // The pre-existing, unrelated "mcp" key survived.
        assert_eq!(value["plugins"]["mcp"], serde_json::json!([]));
    }

    // ---- Round trip: add then remove restores the original array ----

    #[test]
    fn add_then_remove_restores_the_original_array_contents() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, hand_edited_fixture()).unwrap();

        set_plugin_installed(&path, "conway.memory", true).expect("add");
        set_plugin_installed(&path, "conway.memory", false).expect("remove");

        let new_text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&new_text).unwrap();
        assert_eq!(
            value["plugins"]["install"],
            serde_json::json!(["conway.skills", "conway.stepguard"]),
            "round-tripping add-then-remove must land back on the original contents"
        );
    }

    // ---- Sequential toggles of DIFFERENT plugins accumulate correctly ----

    #[test]
    fn sequential_additions_of_different_plugins_all_persist() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        set_plugin_installed(&path, "conway.memory", true).expect("add 1");
        set_plugin_installed(&path, "conway.skills", true).expect("add 2");
        set_plugin_installed(&path, "conway.path", true).expect("add 3");

        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            value["plugins"]["install"],
            serde_json::json!(["conway.memory", "conway.skills", "conway.path"])
        );
    }

    // ---- The writer never reads the real environment on its own ----

    #[test]
    fn set_plugin_installed_takes_a_caller_supplied_path_and_never_resolves_one_itself() {
        // Structural: this function's own signature takes `path: &Path`
        // directly -- it never calls `discovery::user_config_path` (or
        // reads `HOME`/`CONWAY_CONFIG_DIR`) itself, so a caller (this
        // module's own tests included) fully controls where it writes.
        let _env: HashMap<String, String> = HashMap::new();
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        set_plugin_installed(&path, "conway.memory", true).expect("write");
        assert!(path.exists());
    }
}
