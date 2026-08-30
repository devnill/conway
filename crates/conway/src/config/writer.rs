//! A config **writer** -- the missing half `crate::config::merge`'s own
//! module doc names ("a layered read with no writer outside test
//! fixtures"). Four public writers, all against a real `settings.json`,
//! resolved to decision `01M0K8BAXJ6THVJAPK0JZ17VV6`'s user layer
//! (`~/.conway/settings.json`, or `$CONWAY_CONFIG_DIR/settings.json` --
//! see [`super::discovery::user_config_path`], this module's own caller),
//! and each a materially different SHAPE of the one thing being edited:
//! [`set_plugin_installed`] edits `plugins.install`, an array of strings;
//! [`set_claude_compat_entry`] edits `plugins.claude_compat`, an array of
//! objects, matched by an `id` member; [`set_backend_provider`] (board item
//! `01M11XTB238YHXV01FWF8SFZH2`) edits `backends`, a **map** from provider
//! id to an open-ended entry -- a named table (`[backends.<id>]` in the
//! operator-facing notation `docs/providers.md` uses), not an array at
//! all; [`set_default_role`] (board item `01M18Q7P25DTSKQJDJJCC3E800`)
//! edits `default_role`, a bare top-level **scalar** -- the one shape none
//! of the other three cover, and the only writer here that refuses a
//! missing key rather than inventing one (see its own doc for why).
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
//! still-escaped source text. Both sides of every such comparison are
//! produced by `serde_json`, so they agree on one canonical spelling.
//!
//! WHAT THAT DOES AND DOES NOT GUARANTEE, restated when
//! [`set_backend_provider`] joined this module and widened the input
//! domain. The two plugin writers only ever toggle a bare
//! dotted-lowercase manifest id
//! (`conway_core::ports::plugin::PluginManifest::id`) with no JSON
//! metacharacter in it, so for them raw and decoded comparison cannot
//! disagree. A BACKEND id has no such guarantee: it reaches
//! [`set_backend_provider`] from an operator typing into a form, and may
//! legitimately carry a quote, a backslash, or a non-ASCII character.
//!
//! That is still safe for every file conway itself writes, because the id
//! is escaped through `json_string_literal` on the way in and compared in
//! that same escaped form on the way out -- one spelling on both sides.
//! The residual gap, named rather than left to be discovered: a
//! settings.json written by a DIFFERENT tool may spell the same key
//! another equally-valid way (Python's `json.dumps` escapes non-ASCII to
//! `\uXXXX` by default, so `café` where `serde_json` writes `café`).
//! Raw comparison then misses a member that is logically present, and an
//! add appends a second member with the same decoded key instead of being
//! the no-op it reports. No id conway ships or documents is affected --
//! every backend id in `docs/providers.md` is ASCII -- and closing it
//! means decoding both sides before comparing, which is a real parser and
//! is what this module deliberately is not.
//!
//! # Safety posture: refuse rather than guess
//!
//! A file that does not parse as strict JSON at all is never touched --
//! [`set_plugin_installed`] validates the WHOLE document with
//! `serde_json::from_str` before attempting any edit and returns a named
//! [`crate::error::FacadeError::Config`] instead, mirroring
//! `rewrite_permission_file_removing`'s own "not valid JSON, refusing to
//! rewrite it blindly" posture. A goal state already holding (the id is
//! already present when turning a plugin ON, or already absent when
//! turning one OFF) performs no write at all -- so a toggle that is a
//! no-op can never even flip the file's mtime, let alone risk corrupting
//! it.

use std::path::Path;

use crate::error::{FacadeError, Result};

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
        Err(e) => return Err(FacadeError::Io(e)),
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
            return Err(FacadeError::Config {
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
                    return Err(FacadeError::Config {
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

    write_atomically(path, &new_text)?;
    Ok(true)
}

/// Create `path`'s parent directories if needed, then write `contents`
/// durably: to a sibling `path.json.tmp` first, then `rename` over `path`.
/// `rename` replaces the target as a single filesystem operation on every
/// platform this crate targets, so a reader (including this crate's own
/// five-source `load`) can never observe a partially-written file **across
/// a process crash** -- it either still sees the old bytes or already sees
/// the new ones in full, never a mix.
///
/// **Not a power-loss guarantee.** Neither the tmp file's contents nor the
/// directory entry `rename` updates are `fsync`ed before this function
/// returns, so a host that loses power (rather than just the writing
/// process crashing) can still lose the write, or -- on some filesystems --
/// observe the rename applied without the new content durably behind it on
/// disk. Closing that gap means an `fsync` of the tmp file before the
/// rename and of the containing directory after it, which is a genuine
/// durability upgrade this consolidation deliberately leaves for its own
/// decision rather than folding in unannounced -- see board item
/// `01M12ERK9WSJ10AT87WCJ9ZME9`'s report. This paragraph is the named limit
/// that future decision starts from, not a promise that no such gap exists.
///
/// **The single implementation of the tmp-then-rename step for every
/// operator-config file this crate writes, per P-14.** `pub(crate)` rather
/// than private to this module: besides this module's own three public
/// writers, `crate::permissions::rewrite_permission_file_removing`/
/// `crate::permissions::rewrite_permission_file_removing_structured` and
/// `crate::config::trust::TrustStore::trust` call this too, rather than
/// restating it -- six call sites in total, consolidated here across two
/// board items (`01M11XTB238YHXV01FWF8SFZH2` folded in this module's own
/// three; `01M12ERK9WSJ10AT87WCJ9ZME9` folded in the remaining three,
/// reaching into `permissions` and `trust`). Kept in `config::writer`
/// rather than moved to a new, more neutral home: every one of the six
/// current callers already writes a file under the operator's config tree
/// (`settings.json`, `permissions.json`, `trust.json`), so `config` is
/// where they all already are; the crate's structural guard on this
/// directory (`config::mod`'s `config_module_never_names_a_network_client_
/// identifier` test) only forbids naming an HTTP-client or socket type in
/// a production file under `config/` -- this function performs no network
/// I/O at all, so it does not implicate that guard in either direction.
/// The forbidden names are deliberately not spelled out here: that guard
/// matches on file CONTENT, so a doc comment listing the identifiers trips
/// it exactly as a real import would. This paragraph did, on its first
/// draft, and the guard was right to fail it. That rule exists because a restatement
/// drifts and the duplicate silently drops a guard -- a defect this tree
/// has already paid for more than once -- and because the next change here
/// (the `fsync` named above, a mode fix, a check for a `.json.tmp` left by
/// a crashed prior run) must land in one place rather than being applied
/// to some fraction of six copies.
///
/// `.json.tmp` is hardcoded, not derived from `path`'s own extension:
/// every current caller writes a `.json` file, so one literal extension is
/// a description of that fact, not an accidental restriction -- a future
/// caller writing a non-JSON file is the moment to reconsider this, not
/// something to silently paper over with a wrong extension today.
pub(crate) fn write_atomically(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
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

/// Adds (`present: true`) or removes (`present: false`) a
/// `{ "id": plugin_id, "dir": dir }` object element from the top-level
/// `plugins.claude_compat` array of the JSON document at `path` -- board
/// item `01M0VR96Y87FF2BVNTBSC6GEYR`'s config-writer half: a marketplace
/// install declares itself to conway as an ordinary
/// `conway::config::schema::ClaudeCompatPluginEntry` (spec update 1: a
/// fetched artifact needs nothing more than `{ id, dir }`), so this is the
/// writer that entry needs -- the array-of-OBJECTS sibling
/// [`set_plugin_installed`]'s own doc names as harder than its own
/// array-of-strings case ("Writing an object into an array has no writer
/// and is materially harder under [the operator's-formatting-must-survive]
/// constraint").
///
/// **Every safety property [`set_plugin_installed`] has, this has too, by
/// construction: it is built from the exact same primitives** (this
/// module's own hand-rolled scanner/splicer -- `scan_object_members`,
/// `scan_array_elements`, `insert_member`, `insert_array_element`,
/// `remove_array_element`), never a second, independent parser. A file that
/// does not parse as strict JSON is refused before anything is touched
/// (this module's own "Safety posture" doc); a byte outside the touched
/// span is never re-emitted from a parsed representation, only copied
/// verbatim; the write is tmp-then-rename; a goal state already holding
/// (the SAME `id` already present when installing, or already absent when
/// uninstalling) performs no write at all.
///
/// **Matching an existing element is by `id` ALONE, ignoring `dir`** --
/// mirrors `set_plugin_installed`'s own single-key match. Installing an id
/// that is already present is therefore a no-op even if `dir` differs (a
/// re-install pointing at a new store path does not retarget an existing
/// entry here; the caller is expected to uninstall-then-install when that
/// is genuinely wanted, keeping this writer's own contract as simple as its
/// sibling's).
///
/// Returns `Ok(true)` if a write happened, `Ok(false)` if the goal state
/// already held. A missing file is created (parent directories included)
/// when `present` is `true`; when `present` is `false` and the file does
/// not exist, there is nothing to remove, so this returns `Ok(false)` with
/// no filesystem write at all.
pub fn set_claude_compat_entry(
    path: &Path,
    plugin_id: &str,
    dir: &str,
    present: bool,
) -> Result<bool> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(FacadeError::Io(e)),
    };

    let new_text = if text.trim().is_empty() {
        // Same reasoning as `set_plugin_installed`'s identical branch: a
        // missing/whitespace-only file has no existing document to
        // preserve, so a no-op removal never creates one.
        if !present {
            return Ok(false);
        }
        fresh_claude_compat_document(plugin_id, dir)
    } else {
        if let Err(e) = serde_json::from_str::<serde_json::Value>(&text) {
            return Err(FacadeError::Config {
                path: Some(path.to_path_buf()),
                message: format!(
                    "{} is not valid JSON, refusing to rewrite it blindly: {e}",
                    path.display()
                ),
            });
        }
        match patch_claude_compat_array(&text, plugin_id, dir, present) {
            Ok(Some(patched)) => patched,
            Ok(None) => return Ok(false),
            Err(msg) => {
                return Err(FacadeError::Config {
                    path: Some(path.to_path_buf()),
                    message: format!("{}: {msg}", path.display()),
                })
            }
        }
    };

    write_atomically(path, &new_text)?;
    Ok(true)
}

/// The `plugins.claude_compat` sibling of [`fresh_document`].
fn fresh_claude_compat_document(plugin_id: &str, dir: &str) -> String {
    format!(
        "{{\n  \"plugins\": {{\n    \"claude_compat\": [\n      {}\n    ]\n  }}\n}}\n",
        claude_compat_object_literal(plugin_id, dir)
    )
}

/// The literal `{"id": ..., "dir": ...}` JSON object text spliced into (or
/// matched against) the `plugins.claude_compat` array. `timeout_ms` is
/// deliberately never written here -- `ClaudeCompatPluginEntry::timeout_ms`
/// already defaults via `#[serde(default = "default_hook_timeout_ms")]`
/// (`conway::config::schema`), so omitting it is not a loss, and writing it
/// unconditionally would make every installed entry's own JSON noisier for
/// no operator-visible benefit; an operator who wants a non-default timeout
/// edits the array by hand afterward, the same way they would edit any
/// other value this writer never re-touches.
fn claude_compat_object_literal(plugin_id: &str, dir: &str) -> String {
    format!(
        "{{\"id\": {}, \"dir\": {}}}",
        json_string_literal(plugin_id),
        json_string_literal(dir)
    )
}

/// The `id` member's raw (still-escaped) string value of array element
/// `elem`, if `elem` is a JSON object with a string-valued `"id"` member --
/// `None` for any other shape (a bare string element, a number, an object
/// with no `id`, or an object whose `id` is not a string), which this
/// module then simply never matches, exactly mirroring
/// [`patch_install_array`]'s own "an operator hand-added, non-string
/// element is left untouched, never a match candidate" posture applied to
/// object elements instead of string ones.
fn array_object_id<'a>(text: &'a str, elem: &Elem<'a>) -> Option<&'a str> {
    let bytes = text.as_bytes();
    if bytes.get(elem.start) != Some(&b'{') {
        return None;
    }
    let (members, _close) = scan_object_members(text, elem.start).ok()?;
    let id_member = members.iter().find(|m| m.key == "id")?;
    if bytes.get(id_member.value_start) != Some(&b'"') {
        return None;
    }
    let end = skip_string(bytes, id_member.value_start).ok()?;
    Some(&text[id_member.value_start + 1..end - 1])
}

/// The `plugins.claude_compat` sibling of [`patch_install_array`] -- same
/// locate-`plugins`/locate-array/insert-or-remove shape, over an
/// already-validated-as-JSON `text`, differing only in which array key it
/// targets (`"claude_compat"`, not `"install"`) and in matching an element
/// by its `id` MEMBER rather than by the element's own raw string value
/// (`plugins.claude_compat` holds objects, `plugins.install` holds bare
/// strings).
fn patch_claude_compat_array(
    text: &str,
    plugin_id: &str,
    dir: &str,
    present: bool,
) -> std::result::Result<Option<String>, String> {
    let bytes = text.as_bytes();
    let root_open = skip_ws(bytes, 0);
    if bytes.get(root_open) != Some(&b'{') {
        return Err("the top-level JSON value must be an object".to_string());
    }
    let (root_members, root_close) = scan_object_members(text, root_open)?;

    // LAST match, not first -- see `patch_install_array`'s own doc for why
    // (duplicate top-level keys resolve last-wins under `serde_json`, the
    // real loader).
    let Some(plugins_member) = root_members.iter().rev().find(|m| m.key == "plugins") else {
        if !present {
            return Ok(None);
        }
        let value = format!(
            "{{\"claude_compat\": [{}]}}",
            claude_compat_object_literal(plugin_id, dir)
        );
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

    let Some(cc_member) = plugins_members
        .iter()
        .rev()
        .find(|m| m.key == "claude_compat")
    else {
        if !present {
            return Ok(None);
        }
        let value = format!("[{}]", claude_compat_object_literal(plugin_id, dir));
        return Ok(Some(insert_member(
            text,
            plugins_member.value_start,
            &plugins_members,
            plugins_close,
            "claude_compat",
            &value,
        )));
    };

    if bytes.get(cc_member.value_start) != Some(&b'[') {
        return Err("\"plugins.claude_compat\" must be a JSON array".to_string());
    }
    let (elements, array_close) = scan_array_elements(text, cc_member.value_start)?;

    let found_index = elements
        .iter()
        .position(|e| array_object_id(text, e) == Some(plugin_id));

    match (present, found_index) {
        (true, Some(_)) => Ok(None),
        (false, None) => Ok(None),
        (true, None) => {
            let raw_value = claude_compat_object_literal(plugin_id, dir);
            Ok(Some(insert_array_element(
                text,
                cc_member.value_start,
                &elements,
                array_close,
                &raw_value,
            )))
        }
        (false, Some(idx)) => Ok(Some(remove_array_element(
            text,
            cc_member.value_start,
            array_close,
            &elements,
            idx,
        ))),
    }
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
/// why that is safe for every comparison this module performs). `value_end`
/// (the byte offset right after the member's own value, before any
/// subsequent whitespace or comma) exists solely for
/// [`remove_object_member`] -- every other user of this struct only ever
/// needed `value_start`.
struct Member<'a> {
    key: &'a str,
    key_start: usize,
    colon_pos: usize,
    value_start: usize,
    value_end: usize,
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
        let value_end = pos;
        members.push(Member {
            key,
            key_start,
            colon_pos,
            value_start,
            value_end,
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

/// Whether a scanned object `member`'s own RAW (still-escaped) key text is
/// the same JSON string as `key` -- compares the escaped forms
/// (`json_string_literal(key)`, quotes stripped), exactly the same
/// raw-vs-raw comparison [`patch_install_array`]'s own doc already argues is
/// safe for every id this module receives (no first-party id ever contains
/// a character that needs escaping, so raw and decoded comparison always
/// agree).
fn member_key_matches(member: &Member<'_>, key: &str) -> bool {
    let escaped = json_string_literal(key);
    // `escaped` is `"..."` (leading and trailing quote included); `key`'s
    // raw source form never contains those two bytes, so stripping exactly
    // one byte off each end recovers the same raw text `scan_object_members`
    // stored in `Member::key`.
    member.key == &escaped[1..escaped.len() - 1]
}

/// The object-member sibling of [`remove_array_element`]: removes
/// `members[idx]` from the object spanning `open_brace..=close_brace`,
/// along with exactly the one separating comma that becomes dangling -- the
/// comma AFTER it when it is the first of several members, the comma
/// BEFORE it otherwise. The sole remaining member case collapses the whole
/// object to a compact `{}`. Every OTHER member's own span -- including a
/// comment-shaped one (`"//": ...`, `"_comment_...": ...`) sitting
/// immediately before or after the removed one -- is untouched byte-for-
/// byte.
///
/// **The chosen rule for a comment sitting next to the removed section:
/// leave it, always.** `settings.json`'s comment convention (this module's
/// own top doc, "comments-as-keys") is an ORDINARY object member with no
/// structural link to what follows it -- JSON itself does not attach a
/// comment to a subsequent key the way a `//` line comment attaches to the
/// line below it. So there is no reliable way to tell, from the text alone,
/// whether a comment-shaped member sitting right above `backends.<id>` was
/// written ABOUT that one provider, about the whole `backends` map, or
/// about something else the operator happened to type nearby. Guessing --
/// and deleting a note that was not actually about the provider being
/// removed -- is a strictly worse outcome than leaving a now-stale comment
/// behind, so this function never inspects neighbouring members' content at
/// all: it deletes exactly the matched member's own key/value text and its
/// own dangling separating comma, nothing else. This is the identical
/// contract [`remove_array_element`] already carries for the array case,
/// applied to an object member instead of an array element.
fn remove_object_member(
    text: &str,
    open_brace: usize,
    close_brace: usize,
    members: &[Member<'_>],
    idx: usize,
) -> String {
    if members.len() == 1 {
        format!("{}{{}}{}", &text[..open_brace], &text[close_brace + 1..])
    } else if idx == 0 {
        format!(
            "{}{}",
            &text[..members[0].key_start],
            &text[members[1].key_start..]
        )
    } else {
        format!(
            "{}{}",
            &text[..members[idx - 1].value_end],
            &text[members[idx].value_end..]
        )
    }
}

/// Validates that `entry_json` (once trimmed of leading/trailing
/// whitespace) is a syntactically valid, standalone JSON **object**
/// literal -- refused otherwise, before anything is spliced into the real
/// document, so [`set_backend_provider`] can never write a malformed
/// `settings.json` just because its caller handed it malformed text (this
/// module's own "Safety posture: refuse rather than guess" doc, applied to
/// caller input rather than to the file on disk -- P-10's boundary check,
/// since a provider entry ultimately traces back to a human typing into a
/// form).
fn validate_backend_entry_json(entry_json: &str) -> std::result::Result<(), String> {
    match serde_json::from_str::<serde_json::Value>(entry_json.trim()) {
        Ok(serde_json::Value::Object(_)) => Ok(()),
        Ok(_) => Err("a backend provider entry must be a JSON object".to_string()),
        Err(e) => Err(format!("backend provider entry is not valid JSON: {e}")),
    }
}

/// Adds (`present: true`) or removes (`present: false`) the
/// `"<id>": entry_json` member of the top-level `backends` object of the
/// JSON document at `path` -- the third writer shape this module has, and
/// the harder one: [`set_plugin_installed`] edits an array of strings,
/// [`set_claude_compat_entry`] edits an array of objects, and `backends` is
/// neither an array nor a fixed-shape object -- it is a **map** from
/// provider id to an open-ended [`super::schema::BackendEntry`] (`kind` is
/// an open vocabulary; see that struct's own doc), so each provider is one
/// named member of that map, `[backends.<id>]` in the operator-facing
/// notation `docs/providers.md` already uses for it.
///
/// `entry_json` is the caller-supplied JSON object literal for the
/// provider's own value (e.g.
/// `r#"{"kind": "anthropic", "api_key_env": "ANTHROPIC_API_KEY"}"#`,
/// typically built by serialising a [`super::schema::BackendEntry`], or by
/// hand for a third-party `kind` carrying its own extra keys) -- this
/// module never enumerates a provider's own fields itself, matching
/// `BackendEntry`'s own doc's rejection of a "built-ins are first-class,
/// everyone else is a guest" shape: it is validated as a standalone JSON
/// object (`validate_backend_entry_json`, private to this module) and otherwise spliced in
/// verbatim, never decoded or re-rendered. Ignored (never even parsed) when
/// `present` is `false`, exactly as `set_claude_compat_entry`'s own `dir`
/// argument is ignored on removal.
///
/// **Every safety property [`set_plugin_installed`] and
/// [`set_claude_compat_entry`] have, this has too, by construction: it is
/// built from the exact same primitives** -- `scan_object_members`,
/// `insert_member` (reused verbatim for the insert side; a `backends`
/// member is an ordinary object member, exactly like `plugins.install`
/// itself), and the new `remove_object_member` (private to this module; its
/// own doc states the comment/whitespace rule this writer follows on
/// removal). A file that does not parse as strict JSON is refused before
/// anything is touched; a byte outside the touched span is never re-emitted
/// from a parsed representation, only copied verbatim; the write is
/// tmp-then-rename; a goal state already holding (the SAME `id` already
/// present when adding, or already absent when removing) performs no write
/// at all.
///
/// **Matching an existing provider is by `id` ALONE** -- the top-level
/// `backends` object's own member key, compared via `member_key_matches`
/// (private to this module). Adding an id that is already present is
/// therefore a no-op even if `entry_json` differs; a caller wanting to
/// change an existing provider's fields removes then re-adds, the same
/// two-step `set_claude_compat_entry`'s own doc already names for its
/// analogous case.
///
/// Returns `Ok(true)` if a write happened, `Ok(false)` if the goal state
/// already held. A missing file is created (parent directories included)
/// when `present` is `true`; when `present` is `false` and the file does
/// not exist, there is nothing to remove, so this returns `Ok(false)` with
/// no filesystem write at all -- matching `set_plugin_installed`'s own
/// no-op contract for a removal against an absent file.
///
/// **User scope only.** This function takes a caller-supplied `path`
/// exactly like its two siblings and never resolves one itself; the
/// project-scoped config file is deliberately never a valid target for a
/// provider credential (a project-scoped write invites committing a secret
/// into a repository) -- so every caller of this function is expected to
/// resolve `path` via the user-scope resolution
/// (`super::discovery::user_config_path`), never the project one.
pub fn set_backend_provider(
    path: &Path,
    id: &str,
    entry_json: &str,
    present: bool,
) -> Result<bool> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(FacadeError::Io(e)),
    };

    let new_text = if text.trim().is_empty() {
        // Same reasoning as `set_plugin_installed`'s identical branch: a
        // missing/whitespace-only file has no existing document to
        // preserve, so a no-op removal never creates one.
        if !present {
            return Ok(false);
        }
        if let Err(msg) = validate_backend_entry_json(entry_json) {
            return Err(FacadeError::Config {
                path: Some(path.to_path_buf()),
                message: format!("{}: {msg}", path.display()),
            });
        }
        fresh_backend_document(id, entry_json.trim())
    } else {
        if let Err(e) = serde_json::from_str::<serde_json::Value>(&text) {
            return Err(FacadeError::Config {
                path: Some(path.to_path_buf()),
                message: format!(
                    "{} is not valid JSON, refusing to rewrite it blindly: {e}",
                    path.display()
                ),
            });
        }
        if present {
            if let Err(msg) = validate_backend_entry_json(entry_json) {
                return Err(FacadeError::Config {
                    path: Some(path.to_path_buf()),
                    message: format!("{}: {msg}", path.display()),
                });
            }
        }
        match patch_backends_object(&text, id, entry_json.trim(), present) {
            Ok(Some(patched)) => patched,
            Ok(None) => return Ok(false),
            Err(msg) => {
                return Err(FacadeError::Config {
                    path: Some(path.to_path_buf()),
                    message: format!("{}: {msg}", path.display()),
                })
            }
        }
    };

    write_atomically(path, &new_text)?;
    Ok(true)
}

/// The `backends` sibling of [`fresh_document`]/[`fresh_claude_compat_document`].
fn fresh_backend_document(id: &str, entry_json: &str) -> String {
    format!(
        "{{\n  \"backends\": {{\n    {}: {}\n  }}\n}}\n",
        json_string_literal(id),
        entry_json
    )
}

/// The `backends`-object sibling of [`patch_install_array`]/
/// [`patch_claude_compat_array`] -- same locate-the-top-level-key,
/// insert-or-remove shape, over an already-validated-as-JSON `text` and an
/// already-validated-as-a-JSON-object `entry_json`, differing in the one
/// way `backends` itself differs: it is a **map**, so the id is a member
/// KEY at this level, never an array element or a nested `"id"` field.
fn patch_backends_object(
    text: &str,
    id: &str,
    entry_json: &str,
    present: bool,
) -> std::result::Result<Option<String>, String> {
    let bytes = text.as_bytes();
    let root_open = skip_ws(bytes, 0);
    if bytes.get(root_open) != Some(&b'{') {
        return Err("the top-level JSON value must be an object".to_string());
    }
    let (root_members, root_close) = scan_object_members(text, root_open)?;

    // LAST match, not first -- same last-wins reasoning as
    // `patch_install_array`'s own doc: a duplicate top-level `"backends"`
    // key resolves last-wins under `serde_json`, the real loader, so
    // editing an earlier one would change bytes the loader never reads.
    let Some(backends_member) = root_members.iter().rev().find(|m| m.key == "backends") else {
        if !present {
            return Ok(None);
        }
        let value = format!("{{{}: {}}}", json_string_literal(id), entry_json);
        return Ok(Some(insert_member(
            text,
            root_open,
            &root_members,
            root_close,
            "backends",
            &value,
        )));
    };

    if bytes.get(backends_member.value_start) != Some(&b'{') {
        return Err("\"backends\" must be a JSON object".to_string());
    }
    let (backend_members, backends_close) = scan_object_members(text, backends_member.value_start)?;

    // `rposition`, not `position`: the LAST member with this key is the one
    // `serde_json` actually resolves an operator-duplicated provider id to,
    // for the same last-wins reason as the top-level `"backends"` key
    // above.
    let found_index = backend_members
        .iter()
        .rposition(|m| member_key_matches(m, id));

    match (present, found_index) {
        (true, Some(_)) => Ok(None),
        (false, None) => Ok(None),
        (true, None) => Ok(Some(insert_member(
            text,
            backends_member.value_start,
            &backend_members,
            backends_close,
            id,
            entry_json,
        ))),
        (false, Some(idx)) => Ok(Some(remove_object_member(
            text,
            backends_member.value_start,
            backends_close,
            &backend_members,
            idx,
        ))),
    }
}

/// Sets the top-level `default_role` scalar to `role`, splicing only that
/// member's own value span -- see the module doc's "why a splice, not a
/// reserialize" for why this never touches anything else in the document.
/// Board item `01M18Q7P25DTSKQJDJJCC3E800`: `/settings`' "defaults"
/// section's own writer for the one persistent scalar that section can set
/// directly (`default_model` is a DERIVED read over `roles`, not a stored
/// value -- see `conway::config::schema::ConwayConfig::default_model`'s own
/// doc for that decision and its rejected alternative).
///
/// **Unlike the three siblings above, this never invents a missing key.**
/// `default_role` is REQUIRED wire schema -- `ConwayConfig::default_role`
/// carries no `#[serde(default)]` (see its own doc: "the binding config
/// always sets it explicitly") -- and every user `settings.json` this
/// writer's caller ever produces already has one (`first_run.rs`'s
/// onboarding flow always writes `"default_role": role`). Refusing a
/// missing key here, rather than picking one for the operator, matches
/// this crate's fail-loud design elsewhere in this module: a key nobody
/// asked this writer to create is exactly the kind of silent invention
/// [`set_plugin_installed`]'s own "goal state already holding" posture
/// exists to avoid in the other direction.
///
/// Returns `Ok(true)` if a write happened, `Ok(false)` if `role` already
/// matched the current value (no-op, matching the other writers' "a goal
/// state already holding never touches the file" posture).
///
/// See this module's own doc for the safety posture on a file that is not
/// valid JSON, and [`set_backend_provider`]'s own doc for why a missing
/// FILE (as opposed to a missing key inside an existing one) is still
/// refused rather than papered over: an operator's `settings.json` not
/// existing at all is a materially different problem than a key missing
/// from one that does, and this writer -- unlike the other three -- has no
/// "fresh document" shape to fall back to, since it never invents
/// `default_role`'s value.
pub fn set_default_role(path: &Path, role: &str) -> Result<bool> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(FacadeError::Config {
                path: Some(path.to_path_buf()),
                message: format!(
                    "{} does not exist; this writer never invents a `default_role` -- run \
                     first-run setup, or create the file with one set, first",
                    path.display()
                ),
            });
        }
        Err(e) => return Err(FacadeError::Io(e)),
    };
    if let Err(e) = serde_json::from_str::<serde_json::Value>(&text) {
        return Err(FacadeError::Config {
            path: Some(path.to_path_buf()),
            message: format!(
                "{} is not valid JSON, refusing to rewrite it blindly: {e}",
                path.display()
            ),
        });
    }
    match patch_default_role(&text, role) {
        Ok(Some(patched)) => {
            write_atomically(path, &patched)?;
            Ok(true)
        }
        Ok(None) => Ok(false),
        Err(msg) => Err(FacadeError::Config {
            path: Some(path.to_path_buf()),
            message: format!("{}: {msg}", path.display()),
        }),
    }
}

/// The scalar-member sibling of [`patch_backends_object`]/
/// [`patch_install_array`]: locates the top-level `"default_role"` member
/// and replaces its own value span with `role`'s JSON string literal.
/// Never inserts one -- see [`set_default_role`]'s own doc for why a
/// missing key is refused rather than invented.
fn patch_default_role(text: &str, role: &str) -> std::result::Result<Option<String>, String> {
    let bytes = text.as_bytes();
    let root_open = skip_ws(bytes, 0);
    if bytes.get(root_open) != Some(&b'{') {
        return Err("the top-level JSON value must be an object".to_string());
    }
    let (root_members, _root_close) = scan_object_members(text, root_open)?;

    // LAST match, not first -- same last-wins reasoning as
    // `patch_install_array`'s own doc: a duplicate top-level
    // `"default_role"` key resolves last-wins under `serde_json`, the real
    // loader, so editing an earlier one would change bytes the loader
    // never reads.
    let Some(member) = root_members.iter().rev().find(|m| m.key == "default_role") else {
        return Err(
            "\"default_role\" is missing from the document; this writer never invents one \
             -- see its own doc"
                .to_string(),
        );
    };

    let current_raw = &text[member.value_start..member.value_end];
    let new_raw = json_string_literal(role);
    if current_raw == new_raw {
        return Ok(None);
    }
    Ok(Some(format!(
        "{}{}{}",
        &text[..member.value_start],
        new_raw,
        &text[member.value_end..]
    )))
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

    // ---- `set_claude_compat_entry`: the array-of-OBJECTS writer (board
    // item 01M0VR96Y87FF2BVNTBSC6GEYR) -- the same test shapes as
    // `set_plugin_installed`'s own suite above, run again against the
    // object-array case.

    #[test]
    fn claude_compat_creates_a_fresh_file_with_parent_dirs_when_installing() {
        let dir = tempfile_dir();
        let path = dir.join("nested").join("settings.json");
        let wrote =
            set_claude_compat_entry(&path, "acme-tools", "/store/acme-tools", true).expect("write");
        assert!(wrote);
        let text = std::fs::read_to_string(&path).expect("read back");
        let value: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(
            value["plugins"]["claude_compat"],
            serde_json::json!([{"id": "acme-tools", "dir": "/store/acme-tools"}])
        );
    }

    #[test]
    fn claude_compat_removing_from_a_nonexistent_file_is_a_no_op() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let wrote = set_claude_compat_entry(&path, "acme-tools", "/store/acme-tools", false)
            .expect("write");
        assert!(!wrote);
        assert!(!path.exists());
    }

    #[test]
    fn claude_compat_refuses_to_touch_a_file_that_is_not_valid_json() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let original = "{ this is not json";
        std::fs::write(&path, original).unwrap();
        let err =
            set_claude_compat_entry(&path, "acme-tools", "/store/acme-tools", true).unwrap_err();
        assert!(err.to_string().contains("not valid JSON"), "got: {err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    /// Acceptance 4's own proof: a hand-edited file with the `"//"`-comment
    /// convention, unusual key ordering, and unrelated top-level sections
    /// survives an object-array install byte-for-byte outside the one
    /// array this touches.
    fn hand_edited_fixture_with_claude_compat() -> &'static str {
        r#"{
  "//": "operator note: do not touch the backends section by hand",
  "zebra_first_key": "kept exactly as-is",
  "default_role": "coder",
  "backends": {
    "anthropic": { "kind": "anthropic", "api_key": "sk-unused" }
  },
  "plugins": {
    "_comment_plugins": "toggle plugins here",
    "install": ["conway.skills"],
    "claude_compat": [
      { "id": "existing-plugin", "dir": "/home/op/plugins/existing" }
    ]
  },
  "apple_last_key": 42
}
"#
    }

    #[test]
    fn claude_compat_installing_into_a_hand_edited_file_preserves_comments_ordering_and_unrelated_keys(
    ) {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, hand_edited_fixture_with_claude_compat()).unwrap();

        let wrote =
            set_claude_compat_entry(&path, "acme-tools", "/store/acme-tools", true).expect("write");
        assert!(wrote);

        let new_text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&new_text).expect("still valid json");

        // The one thing that changed.
        assert_eq!(
            value["plugins"]["claude_compat"],
            serde_json::json!([
                {"id": "existing-plugin", "dir": "/home/op/plugins/existing"},
                {"id": "acme-tools", "dir": "/store/acme-tools"}
            ])
        );
        // The unrelated `install` array is untouched.
        assert_eq!(
            value["plugins"]["install"],
            serde_json::json!(["conway.skills"])
        );

        // Byte-for-byte substring survival of everything else -- the same
        // proof `adding_a_plugin_to_a_hand_edited_file_preserves_comments_
        // ordering_and_unrelated_keys` gives `set_plugin_installed`.
        assert!(new_text
            .contains("\"//\": \"operator note: do not touch the backends section by hand\""));
        assert!(new_text.contains("\"_comment_plugins\": \"toggle plugins here\""));
        assert!(new_text.contains("\"zebra_first_key\": \"kept exactly as-is\""));
        assert!(new_text.contains("\"apple_last_key\": 42"));
        assert!(new_text
            .contains("\"anthropic\": { \"kind\": \"anthropic\", \"api_key\": \"sk-unused\" }"));
        let pos = |needle: &str| new_text.find(needle).expect(needle);
        assert!(pos("\"//\"") < pos("\"zebra_first_key\""));
        assert!(pos("\"zebra_first_key\"") < pos("\"default_role\""));
        assert!(pos("\"default_role\"") < pos("\"backends\""));
        assert!(pos("\"backends\"") < pos("\"plugins\""));
        assert!(pos("\"plugins\"") < pos("\"apple_last_key\""));
    }

    #[test]
    fn claude_compat_uninstalling_from_a_hand_edited_file_preserves_everything_else() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, hand_edited_fixture_with_claude_compat()).unwrap();

        let wrote =
            set_claude_compat_entry(&path, "existing-plugin", "ignored", false).expect("write");
        assert!(wrote);

        let new_text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&new_text).expect("still valid json");
        assert_eq!(value["plugins"]["claude_compat"], serde_json::json!([]));
        assert!(new_text
            .contains("\"//\": \"operator note: do not touch the backends section by hand\""));
        assert!(new_text.contains("\"apple_last_key\": 42"));
    }

    #[test]
    fn claude_compat_matches_only_by_id_ignoring_dir() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        set_claude_compat_entry(&path, "acme-tools", "/store/v1", true).expect("install v1");

        // Installing the SAME id again with a DIFFERENT dir is a no-op --
        // matched by id alone (this function's own doc).
        let wrote = set_claude_compat_entry(&path, "acme-tools", "/store/v2", true).expect("write");
        assert!(!wrote);
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            value["plugins"]["claude_compat"],
            serde_json::json!([{"id": "acme-tools", "dir": "/store/v1"}]),
            "dir must stay whatever it was first installed with"
        );

        // Uninstalling names only the id too -- the dir argument is not
        // even matched against.
        let removed = set_claude_compat_entry(&path, "acme-tools", "/some/other/path", false)
            .expect("uninstall");
        assert!(removed);
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["plugins"]["claude_compat"], serde_json::json!([]));
    }

    #[test]
    fn claude_compat_adding_an_already_present_id_is_a_no_op_and_leaves_the_file_untouched() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let original =
            r#"{"plugins": {"claude_compat": [{"id": "acme-tools", "dir": "/store/acme-tools"}]}}"#;
        std::fs::write(&path, original).unwrap();
        let mtime_before = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));

        let wrote =
            set_claude_compat_entry(&path, "acme-tools", "/store/acme-tools", true).expect("write");
        assert!(!wrote);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let mtime_after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(
            mtime_before, mtime_after,
            "a no-op must never touch the file at all"
        );
    }

    #[test]
    fn claude_compat_removing_an_already_absent_id_is_a_no_op() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let original = r#"{"plugins": {"claude_compat": [{"id": "acme-tools", "dir": "/x"}]}}"#;
        std::fs::write(&path, original).unwrap();
        let wrote = set_claude_compat_entry(&path, "other-plugin", "/y", false).expect("write");
        assert!(!wrote);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn claude_compat_removing_the_only_element_collapses_the_array_to_empty() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"plugins": {"claude_compat": [{"id": "acme-tools", "dir": "/x"}]}}"#,
        )
        .unwrap();
        let wrote = set_claude_compat_entry(&path, "acme-tools", "/x", false).expect("write");
        assert!(wrote);
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["plugins"]["claude_compat"], serde_json::json!([]));
    }

    #[test]
    fn claude_compat_removing_the_first_of_several_keeps_the_rest_in_order() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"plugins": {"claude_compat": [
                {"id": "a", "dir": "/a"},
                {"id": "b", "dir": "/b"},
                {"id": "c", "dir": "/c"}
            ]}}"#,
        )
        .unwrap();
        let wrote = set_claude_compat_entry(&path, "a", "/a", false).expect("write");
        assert!(wrote);
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            value["plugins"]["claude_compat"],
            serde_json::json!([{"id": "b", "dir": "/b"}, {"id": "c", "dir": "/c"}])
        );
    }

    #[test]
    fn claude_compat_inserts_a_fresh_array_alongside_an_existing_install_array() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"plugins": {"install": ["conway.memory"]}, "default_role": "coder"}"#,
        )
        .unwrap();
        let wrote =
            set_claude_compat_entry(&path, "acme-tools", "/store/acme-tools", true).expect("write");
        assert!(wrote);
        let new_text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&new_text).unwrap();
        assert_eq!(
            value["plugins"]["claude_compat"],
            serde_json::json!([{"id": "acme-tools", "dir": "/store/acme-tools"}])
        );
        // The pre-existing, unrelated `install` array survived.
        assert_eq!(
            value["plugins"]["install"],
            serde_json::json!(["conway.memory"])
        );
        assert!(new_text.contains("\"default_role\": \"coder\""));
    }

    #[test]
    fn claude_compat_round_trip_add_then_remove_restores_the_original_array_contents() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, hand_edited_fixture_with_claude_compat()).unwrap();

        set_claude_compat_entry(&path, "acme-tools", "/store/acme-tools", true).expect("add");
        set_claude_compat_entry(&path, "acme-tools", "/store/acme-tools", false).expect("remove");

        let new_text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&new_text).unwrap();
        assert_eq!(
            value["plugins"]["claude_compat"],
            serde_json::json!([{"id": "existing-plugin", "dir": "/home/op/plugins/existing"}]),
            "round-tripping add-then-remove must land back on the original contents"
        );
    }

    #[test]
    fn claude_compat_a_non_object_element_is_never_a_match_candidate() {
        // An operator (or a future writer) could hand-add a non-object
        // element to this array; `array_object_id` must simply skip it,
        // never panic or misparse it as a match.
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"plugins": {"claude_compat": ["not-an-object", {"id": "acme-tools", "dir": "/x"}]}}"#,
        )
        .unwrap();
        let wrote = set_claude_compat_entry(&path, "acme-tools", "/x", false).expect("write");
        assert!(wrote);
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            value["plugins"]["claude_compat"],
            serde_json::json!(["not-an-object"])
        );
    }

    // ---- `set_backend_provider`: the MAP writer (board item
    // 01M11XTB238YHXV01FWF8SFZH2) -- `backends` is a table keyed by
    // provider id, the third shape this module writes, distinct from both
    // the array-of-strings and array-of-objects cases above.

    fn anthropic_entry_json() -> &'static str {
        r#"{"kind": "anthropic", "api_key_env": "ANTHROPIC_API_KEY", "base_url": "https://api.anthropic.com", "local": false}"#
    }

    fn openai_compat_entry_json() -> &'static str {
        r#"{"kind": "openai-compat", "base_url": "http://localhost:11434/v1", "local": true}"#
    }

    #[test]
    fn backend_provider_creates_a_fresh_file_with_parent_dirs_when_adding() {
        let dir = tempfile_dir();
        let path = dir.join("nested").join("settings.json");
        let wrote =
            set_backend_provider(&path, "anthropic", anthropic_entry_json(), true).expect("write");
        assert!(wrote);
        let text = std::fs::read_to_string(&path).expect("read back");
        let value: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(
            value["backends"]["anthropic"],
            serde_json::json!({
                "kind": "anthropic",
                "api_key_env": "ANTHROPIC_API_KEY",
                "base_url": "https://api.anthropic.com",
                "local": false
            })
        );
    }

    #[test]
    fn backend_provider_removing_from_a_nonexistent_file_is_a_no_op() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let wrote =
            set_backend_provider(&path, "anthropic", anthropic_entry_json(), false).expect("write");
        assert!(!wrote);
        assert!(!path.exists(), "a no-op removal must never create a file");
    }

    #[test]
    fn backend_provider_refuses_to_touch_a_file_that_is_not_valid_json() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let original = "{ this is not json";
        std::fs::write(&path, original).unwrap();
        let err =
            set_backend_provider(&path, "anthropic", anthropic_entry_json(), true).unwrap_err();
        assert!(err.to_string().contains("not valid JSON"), "got: {err}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "an invalid file must be left byte-for-byte untouched"
        );
    }

    /// P-10 boundary check: a caller-supplied `entry_json` that is not
    /// itself valid JSON must never be spliced into a real document --
    /// refused before anything is touched, exactly like an invalid whole
    /// document is.
    #[test]
    fn backend_provider_refuses_malformed_entry_json_and_leaves_the_file_untouched() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let original = r#"{"default_role": "coder"}"#;
        std::fs::write(&path, original).unwrap();
        let err = set_backend_provider(&path, "anthropic", "{ not json at all", true).unwrap_err();
        assert!(err.to_string().contains("not valid JSON"), "got: {err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    /// The same boundary check, but against the OTHER branch of
    /// `set_backend_provider` -- a missing file, which takes the
    /// `fresh_backend_document` path rather than `patch_backends_object`.
    /// Malformed `entry_json` must be refused there too, before any file is
    /// even created.
    #[test]
    fn backend_provider_refuses_malformed_entry_json_when_the_file_does_not_exist_yet() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let err = set_backend_provider(&path, "anthropic", "{ not json at all", true).unwrap_err();
        assert!(err.to_string().contains("not valid JSON"), "got: {err}");
        assert!(
            !path.exists(),
            "a refused write must never create the file at all"
        );
    }

    /// P-10 boundary check, second half: `entry_json` that parses as JSON
    /// but is not an OBJECT (a provider entry must be a table, never a bare
    /// array/string/number) is refused the same way.
    #[test]
    fn backend_provider_refuses_a_non_object_entry_json_and_leaves_the_file_untouched() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let original = r#"{"default_role": "coder"}"#;
        std::fs::write(&path, original).unwrap();
        let err = set_backend_provider(&path, "anthropic", r#"["not", "an", "object"]"#, true)
            .unwrap_err();
        assert!(
            err.to_string().contains("must be a JSON object"),
            "got: {err}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    /// Acceptance 1: adding a provider to a file with no `backends` section
    /// at all produces a valid `[backends.<id>]` table that conway then
    /// loads back -- proven by deserializing the WHOLE resulting document
    /// into the real `ConwayConfig` schema type (`super::schema::
    /// ConwayConfig`), the exact struct `config::load` deserializes every
    /// JSON layer against, not a bespoke ad hoc shape check.
    #[test]
    fn backend_provider_inserts_a_fresh_backends_section_that_conway_loads_back_as_conwayconfig() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            "{\n  \"default_role\": \"coder\",\n  \"limits\": { \"max_steps\": 40 }\n}\n",
        )
        .unwrap();
        let wrote =
            set_backend_provider(&path, "anthropic", anthropic_entry_json(), true).expect("write");
        assert!(wrote);
        let new_text = std::fs::read_to_string(&path).unwrap();

        let config: super::super::schema::ConwayConfig =
            serde_json::from_str(&new_text).expect("the real schema type must deserialize this");
        let entry = config.backends.get("anthropic").expect("provider present");
        assert_eq!(entry.kind, "anthropic");
        assert_eq!(entry.api_key_env, "ANTHROPIC_API_KEY");
        assert_eq!(entry.base_url, "https://api.anthropic.com");
        assert!(!entry.local);

        // The pre-existing, unrelated keys survived.
        assert!(new_text.contains("\"default_role\": \"coder\""));
        assert!(new_text.contains("\"max_steps\": 40"));
    }

    /// Acceptance 2: adding a SECOND provider leaves the first
    /// byte-identical -- not merely semantically equal after a re-parse.
    #[test]
    fn backend_provider_adding_a_second_provider_leaves_the_first_byte_identical() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"default_role": "coder"}"#).unwrap();

        set_backend_provider(&path, "mercury", anthropic_entry_json(), true).expect("add first");
        let after_first = std::fs::read_to_string(&path).unwrap();

        set_backend_provider(&path, "venus", openai_compat_entry_json(), true).expect("add second");
        let after_second = std::fs::read_to_string(&path).unwrap();

        // The exact byte span written for "mercury" the first time must
        // appear, unchanged, in the file after "venus" is added.
        let mercury_span = format!(
            "{}: {}",
            serde_json::to_string("mercury").unwrap(),
            anthropic_entry_json()
        );
        assert!(
            after_first.contains(&mercury_span),
            "sanity: the first write must itself contain this span"
        );
        assert!(
            after_second.contains(&mercury_span),
            "the first provider's own bytes must survive the second add unchanged:\n{after_second}"
        );

        let value: serde_json::Value = serde_json::from_str(&after_second).unwrap();
        assert_eq!(
            value["backends"]["mercury"],
            serde_json::from_str::<serde_json::Value>(anthropic_entry_json()).unwrap()
        );
        assert_eq!(
            value["backends"]["venus"],
            serde_json::from_str::<serde_json::Value>(openai_compat_entry_json()).unwrap()
        );
    }

    /// Acceptance 3, 6 and 7's shared fixture: operator comments (both the
    /// top-level `"//"` convention and a `backends`-local comment-shaped
    /// sibling key), an unrelated top-level section, non-alphabetical key
    /// order, and -- specifically for the `backends` object -- a comment
    /// key sitting immediately BEFORE the provider this suite adds/removes,
    /// so a removal that mishandled the "comment above a removed section"
    /// case would be caught here.
    ///
    /// Deliberately 2-space pretty-printed throughout (including inside
    /// `backends`) -- this is not incidental: [`insert_member`]'s own
    /// indentation choice (`outer_indent + "  "`) matches an operator who
    /// already writes 2-space-indented JSON, which is what makes the
    /// add-then-remove round trip in
    /// `backend_provider_round_trip_add_then_remove_restores_original_bytes`
    /// land back on IDENTICAL bytes rather than merely equivalent ones.
    fn hand_edited_backends_fixture() -> &'static str {
        r#"{
  "//": "operator note: do not touch the backends section by hand",
  "zebra_first_key": "kept exactly as-is",
  "default_role": "coder",
  "backends": {
    "_comment_backends": "operator note about backends specifically",
    "anthropic": { "kind": "anthropic", "api_key_env": "ANTHROPIC_API_KEY" },
    "ollama-local": { "kind": "openai-compat", "base_url": "http://localhost:11434/v1", "local": true }
  },
  "plugins": {
    "install": ["conway.skills"]
  },
  "apple_last_key": 42
}
"#
    }

    #[test]
    fn backend_provider_adding_to_a_hand_edited_file_preserves_comments_ordering_and_unrelated_keys(
    ) {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, hand_edited_backends_fixture()).unwrap();

        let wrote =
            set_backend_provider(&path, "mercury", anthropic_entry_json(), true).expect("write");
        assert!(wrote);

        let new_text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&new_text).expect("still valid json");

        // The one thing that changed.
        assert!(value["backends"]["mercury"].is_object());
        assert_eq!(
            value["backends"].as_object().unwrap().len(),
            4,
            "the comment key, the two original providers, and the new one"
        );

        // Byte-for-byte substring survival, not semantic equality.
        assert!(new_text
            .contains("\"//\": \"operator note: do not touch the backends section by hand\""));
        assert!(new_text.contains("\"zebra_first_key\": \"kept exactly as-is\""));
        assert!(new_text.contains("\"apple_last_key\": 42"));
        assert!(new_text
            .contains("\"_comment_backends\": \"operator note about backends specifically\""));
        assert!(new_text.contains(
            "\"anthropic\": { \"kind\": \"anthropic\", \"api_key_env\": \"ANTHROPIC_API_KEY\" }"
        ));
        assert!(new_text.contains(
            "\"ollama-local\": { \"kind\": \"openai-compat\", \"base_url\": \"http://localhost:11434/v1\", \"local\": true }"
        ));

        // Top-level key order is unchanged.
        let pos = |needle: &str| new_text.find(needle).expect(needle);
        assert!(pos("\"//\"") < pos("\"zebra_first_key\""));
        assert!(pos("\"zebra_first_key\"") < pos("\"default_role\""));
        assert!(pos("\"default_role\"") < pos("\"backends\""));
        assert!(pos("\"backends\"") < pos("\"plugins\""));
        assert!(pos("\"plugins\"") < pos("\"apple_last_key\""));

        // Inside `backends`: the new member lands FIRST (this module's own
        // documented insertion rule), and the two pre-existing members --
        // including the comment sibling -- keep their own relative order.
        // (Needles are the actual MEMBER keys, `"key": {`, not the bare
        // string -- `anthropic_entry_json()`'s own `"kind": "anthropic"`
        // value would otherwise make `"anthropic"` match too early, inside
        // the newly-inserted `mercury` entry itself.)
        assert!(pos("\"mercury\": {") < pos("\"_comment_backends\": \""));
        assert!(pos("\"_comment_backends\": \"") < pos("\"anthropic\": {"));
        assert!(pos("\"anthropic\": {") < pos("\"ollama-local\": {"));
    }

    /// Acceptance 3 and 6: removing a provider that sits BETWEEN a
    /// comment-shaped sibling and another provider must leave both
    /// neighbours -- and everything else in the file -- byte-for-byte
    /// intact. This is [`remove_object_member`]'s "comma before" branch
    /// (idx > 0), pinning the doc comment's own stated rule: a comment
    /// sitting next to the removed section is NEVER touched.
    #[test]
    fn backend_provider_removing_from_a_hand_edited_file_preserves_everything_else() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, hand_edited_backends_fixture()).unwrap();

        let wrote = set_backend_provider(&path, "anthropic", "ignored", false).expect("write");
        assert!(wrote);

        let new_text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&new_text).expect("still valid json");
        assert!(value["backends"].get("anthropic").is_none());
        assert_eq!(value["backends"].as_object().unwrap().len(), 2);

        assert!(new_text
            .contains("\"//\": \"operator note: do not touch the backends section by hand\""));
        assert!(new_text.contains("\"zebra_first_key\": \"kept exactly as-is\""));
        assert!(new_text.contains("\"apple_last_key\": 42"));
        // The comment sitting immediately above the removed provider
        // survives untouched -- the whole point of acceptance 6.
        assert!(new_text
            .contains("\"_comment_backends\": \"operator note about backends specifically\""));
        // The provider sitting immediately AFTER the removed one survives
        // untouched too.
        assert!(new_text.contains(
            "\"ollama-local\": { \"kind\": \"openai-compat\", \"base_url\": \"http://localhost:11434/v1\", \"local\": true }"
        ));
    }

    /// Acceptance 7: round-tripping add -> remove returns the file to its
    /// ORIGINAL BYTES, not merely to equivalent JSON.
    #[test]
    fn backend_provider_round_trip_add_then_remove_restores_original_bytes() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let original = hand_edited_backends_fixture();
        std::fs::write(&path, original).unwrap();

        set_backend_provider(&path, "mercury", anthropic_entry_json(), true).expect("add");
        set_backend_provider(&path, "mercury", "ignored", false).expect("remove");

        let new_text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            new_text, original,
            "round-tripping add-then-remove must land back on the ORIGINAL BYTES, \
             not just equivalent JSON"
        );
    }

    /// Acceptance 4: removing a provider that is not present is a reported
    /// no-op, never an error -- matching `set_plugin_installed`'s and
    /// `set_claude_compat_entry`'s own existing no-op contract.
    #[test]
    fn backend_provider_removing_an_already_absent_provider_is_a_no_op() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let original = r#"{"backends": {"anthropic": {"kind": "anthropic"}}}"#;
        std::fs::write(&path, original).unwrap();
        let wrote = set_backend_provider(&path, "does-not-exist", "ignored", false).expect("write");
        assert!(!wrote);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    /// Adding an already-present id is a no-op (matched by id alone), even
    /// when `entry_json` differs -- mirrors `set_claude_compat_entry`'s own
    /// "matches only by id" contract for its analogous case.
    #[test]
    fn backend_provider_adding_an_already_present_id_is_a_no_op_and_leaves_the_file_untouched() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let original = format!(
            r#"{{"backends": {{"anthropic": {}}}}}"#,
            anthropic_entry_json()
        );
        std::fs::write(&path, &original).unwrap();
        let mtime_before = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));

        let wrote = set_backend_provider(&path, "anthropic", openai_compat_entry_json(), true)
            .expect("write");
        assert!(!wrote);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let mtime_after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(
            mtime_before, mtime_after,
            "a no-op must never touch the file at all"
        );
    }

    /// Hostile fixture: a provider id that is a byte-for-byte PREFIX of
    /// another provider id. Matching must be exact-key, never
    /// prefix/substring, or removing "acme" would also (wrongly) match
    /// "acme-tools".
    #[test]
    fn backend_provider_id_that_is_a_prefix_of_another_id_is_never_confused_with_it() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        // Deliberately added in this order (the PREFIX id second, so it
        // scans as the RIGHTMOST/most-recent member): `rposition` searches
        // right-to-left, so a matcher that (wrongly) accepted a prefix
        // rather than requiring an exact key would hit "acme-tools" before
        // ever reaching "acme" and delete the wrong one.
        set_backend_provider(&path, "acme-tools", openai_compat_entry_json(), true)
            .expect("add acme-tools");
        set_backend_provider(&path, "acme", anthropic_entry_json(), true).expect("add acme");

        let removed = set_backend_provider(&path, "acme", "ignored", false).expect("remove acme");
        assert!(removed);

        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(value["backends"].get("acme").is_none(), "acme must be gone");
        assert!(
            value["backends"].get("acme-tools").is_some(),
            "acme-tools must survive removing acme -- a prefix match would have deleted it too"
        );
    }

    /// Hostile fixture: an unrelated key whose VALUE (not key) contains the
    /// literal text "backends" must never be mistaken for the `backends`
    /// object itself -- this module matches JSON KEYS structurally
    /// (`scan_object_members`), never raw substring search over the text.
    #[test]
    fn backend_provider_a_value_containing_the_literal_text_backends_is_not_confused_with_the_key()
    {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            r#"{"notes": "see backends for details", "default_role": "coder"}"#,
        )
        .unwrap();
        let wrote =
            set_backend_provider(&path, "anthropic", anthropic_entry_json(), true).expect("write");
        assert!(wrote);
        let new_text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&new_text).unwrap();
        assert_eq!(value["notes"], "see backends for details");
        assert!(value["backends"]["anthropic"].is_object());
    }

    /// Hostile fixture: an `entry_json` carrying arbitrarily nested objects
    /// (a third-party `kind`'s own `extra` catch-all, per `BackendEntry`'s
    /// own doc) must survive verbatim -- this module's scanner treats a
    /// nested `{...}`/`[...]` as an opaque, depth-counted span, never
    /// recursing into it.
    #[test]
    fn backend_provider_nested_objects_in_entry_json_survive_verbatim() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let nested_entry =
            r#"{"kind": "custom", "extra": {"nested": {"deep": [1, 2, {"three": 3}]}}}"#;
        let wrote = set_backend_provider(&path, "third-party", nested_entry, true).expect("write");
        assert!(wrote);
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            value["backends"]["third-party"]["extra"]["nested"]["deep"],
            serde_json::json!([1, 2, {"three": 3}])
        );
    }

    /// Hostile fixture: a provider id containing a quote, a backslash, and
    /// a newline -- exactly the characters a naive splicer would mishandle.
    /// `json_string_literal` (this module's existing, already-tested
    /// escaping helper) is reused for the key here exactly as it already is
    /// for a plugin id, so this is a regression guard, not new escaping
    /// logic.
    #[test]
    fn backend_provider_id_with_quote_backslash_and_newline_round_trips() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let hostile_id = "weird\"id\\with\nnewline";
        let wrote =
            set_backend_provider(&path, hostile_id, anthropic_entry_json(), true).expect("add");
        assert!(wrote);
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(value["backends"].get(hostile_id).is_some());

        let removed = set_backend_provider(&path, hostile_id, "ignored", false).expect("remove");
        assert!(removed);
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["backends"].as_object().unwrap().len(), 0);
    }

    /// Hostile fixture, documented as a KNOWN LIMITATION rather than
    /// silently assumed to work: a CRLF-terminated file. This module's
    /// insertion helpers always emit a bare `\n`, inherited unchanged from
    /// `insert_member`/`insert_array_element` (pre-existing behaviour, not
    /// introduced by this writer) -- so a CRLF file gains one mixed-
    /// line-ending line where a provider is inserted. What this test pins
    /// is the property that DOES hold despite that: the result is still
    /// valid, parseable JSON with the addition and removal both taking
    /// effect correctly, never a corrupted file.
    #[test]
    fn backend_provider_crlf_file_stays_valid_json_though_the_inserted_line_is_lf_only() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let crlf_fixture = "{\r\n  \"default_role\": \"coder\",\r\n  \"backends\": {\r\n    \"anthropic\": { \"kind\": \"anthropic\" }\r\n  }\r\n}\r\n";
        std::fs::write(&path, crlf_fixture).unwrap();

        let wrote =
            set_backend_provider(&path, "mercury", anthropic_entry_json(), true).expect("add");
        assert!(wrote);
        let new_text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&new_text).expect("still valid json despite CRLF");
        assert!(value["backends"]["mercury"].is_object());
        assert!(value["backends"]["anthropic"].is_object());

        let removed = set_backend_provider(&path, "mercury", "ignored", false).expect("remove");
        assert!(removed);
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(value["backends"].get("mercury").is_none());
        assert!(value["backends"]["anthropic"].is_object());
    }

    /// A trailing comma is not valid JSON at all -- the whole-document
    /// `serde_json::from_str` validation this function performs before
    /// touching anything already refuses it, the same as any other invalid
    /// document. Named here so "does this writer tolerate a trailing
    /// comma" has an explicit, checked answer: no, and it fails closed
    /// (refuses, leaves the file untouched) rather than guessing at a
    /// non-standard dialect.
    #[test]
    fn backend_provider_a_trailing_comma_is_refused_like_any_other_invalid_json() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let original = r#"{"backends": {"anthropic": {"kind": "anthropic",}}}"#;
        std::fs::write(&path, original).unwrap();
        let err =
            set_backend_provider(&path, "venus", openai_compat_entry_json(), true).unwrap_err();
        assert!(err.to_string().contains("not valid JSON"), "got: {err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    /// The writer never resolves its own path -- same structural contract
    /// as `set_plugin_installed`'s own identically-named test.
    #[test]
    fn set_backend_provider_takes_a_caller_supplied_path_and_never_resolves_one_itself() {
        let _env: HashMap<String, String> = HashMap::new();
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        set_backend_provider(&path, "anthropic", anthropic_entry_json(), true).expect("write");
        assert!(path.exists());
    }

    /// P-10: a backend id is operator-typed, so a multi-byte UTF-8 id must
    /// not panic. Every byte-offset slice in this module sits immediately
    /// beside an ASCII quote or brace, which is always a char boundary
    /// whatever sits between them -- but that was an argument from reading
    /// and nothing exercised it, so this pins it. Round-trips add then
    /// remove, which also exercises the sole-member collapse branch with a
    /// non-ASCII key.
    #[test]
    fn backend_provider_a_multibyte_utf8_id_round_trips_without_panicking() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let original = r#"{"backends": {}}"#;
        std::fs::write(&path, original).unwrap();

        let id = "café-日本語";
        assert!(set_backend_provider(&path, id, anthropic_entry_json(), true).expect("add"));
        let after_add = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&after_add).expect("valid JSON");
        assert!(
            parsed["backends"].get(id).is_some(),
            "the multi-byte id must be present under its decoded key: {after_add}"
        );

        assert!(set_backend_provider(&path, id, "", false).expect("remove"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "add then remove must restore the original bytes for a multi-byte id too"
        );
    }

    /// `Member::value_end` is computed by `skip_value`'s generic dispatch,
    /// but every other removal test here neighbours only string- or
    /// object-valued members. A number, a bool, a null and an array are
    /// the shapes with no coverage, and a wrong `value_end` on any of them
    /// would eat or strand a neighbour's bytes. Removes a provider sitting
    /// between them and asserts the whole file byte-for-byte.
    #[test]
    fn backend_provider_removal_preserves_scalar_and_array_valued_siblings_byte_for_byte() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        let original = concat!(
            "{\n",
            "  \"backends\": {\n",
            "    \"_retries\": 3,\n",
            "    \"_enabled\": true,\n",
            "    \"_fallback\": null,\n",
            "    \"doomed\": {\"kind\": \"anthropic\", \"api_key_env\": \"K\"},\n",
            "    \"_order\": [1, 2, {\"nested\": \"brace}\"}],\n",
            "    \"_note\": \"keep me\"\n",
            "  }\n",
            "}\n"
        );
        std::fs::write(&path, original).unwrap();

        assert!(set_backend_provider(&path, "doomed", "", false).expect("remove"));
        let after = std::fs::read_to_string(&path).unwrap();

        let expected = concat!(
            "{\n",
            "  \"backends\": {\n",
            "    \"_retries\": 3,\n",
            "    \"_enabled\": true,\n",
            "    \"_fallback\": null,\n",
            "    \"_order\": [1, 2, {\"nested\": \"brace}\"}],\n",
            "    \"_note\": \"keep me\"\n",
            "  }\n",
            "}\n"
        );
        assert_eq!(
            after, expected,
            "every scalar-, array- and string-valued sibling must survive byte-for-byte"
        );
    }

    // -----------------------------------------------------------------
    // `set_default_role` (board item `01M18Q7P25DTSKQJDJJCC3E800`).
    // -----------------------------------------------------------------

    /// ACCEPTANCE 3 ("setting either persists and is read back"): a write
    /// changes the on-disk value, and re-reading the file back shows it.
    /// The discriminating observable: `value["default_role"]` reads back
    /// the NEW role, not the one the fixture started with.
    #[test]
    fn set_default_role_writes_and_reads_back() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"default_role": "coder"}"#).unwrap();

        let wrote = set_default_role(&path, "reviewer").expect("write");
        assert!(wrote, "changing the value must report a write happened");

        let text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["default_role"], "reviewer", "{text}");
    }

    /// The "goal state already holding" posture every other writer in this
    /// module follows: setting the CURRENT value performs no write at all.
    #[test]
    fn set_default_role_is_a_no_op_when_the_value_already_matches() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"default_role": "coder"}"#).unwrap();
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();

        let wrote = set_default_role(&path, "coder").expect("no-op");
        assert!(!wrote, "an unchanged value must never touch the file");

        let after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(before, after);
    }

    /// Unlike the other three writers, a missing `default_role` key is
    /// refused, never invented -- see [`set_default_role`]'s own doc.
    #[test]
    fn set_default_role_refuses_a_document_with_no_default_role_key() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"backends": {}}"#).unwrap();

        let err = set_default_role(&path, "reviewer")
            .expect_err("a missing default_role key must be refused, not invented");
        assert!(
            err.to_string().contains("default_role"),
            "error must name the missing key: {err}"
        );
        // Refused, so nothing was written.
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, r#"{"backends": {}}"#);
    }

    /// A missing FILE is refused too -- this writer has no "fresh
    /// document" fallback, since it never invents the value it would need
    /// to seed one with.
    #[test]
    fn set_default_role_refuses_a_missing_file() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        assert!(!path.exists());

        let err = set_default_role(&path, "reviewer")
            .expect_err("a missing file must be refused, not silently created");
        assert!(err.to_string().contains("default_role"), "{err}");
        assert!(!path.exists(), "refusing must never create the file");
    }

    /// A document that is not valid JSON at all is never touched, matching
    /// every other writer's safety posture (this module's own doc,
    /// "Safety posture: refuse rather than guess").
    #[test]
    fn set_default_role_refuses_invalid_json() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, "{ not json").unwrap();

        let err = set_default_role(&path, "reviewer")
            .expect_err("invalid JSON must be refused, not blindly rewritten");
        assert!(err.to_string().contains("not valid JSON"), "{err}");
    }

    /// Acceptance 7's own "hand-edited files survive byte-for-byte" bar,
    /// applied to the fourth writer: an operator's own comment key, an
    /// unrelated top-level scalar, and a nested `backends`/`plugins`
    /// section all survive a `default_role` change untouched.
    #[test]
    fn set_default_role_preserves_comments_and_unrelated_sections_byte_for_byte() {
        let dir = tempfile_dir();
        let path = dir.join("settings.json");
        std::fs::write(&path, hand_edited_fixture()).unwrap();

        let wrote = set_default_role(&path, "reviewer").expect("write");
        assert!(wrote);

        let new_text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&new_text).expect("still valid json");
        assert_eq!(value["default_role"], "reviewer");

        assert!(new_text
            .contains("\"//\": \"operator note: do not touch the backends section by hand\""));
        assert!(new_text.contains("\"zebra_first_key\": \"kept exactly as-is\""));
        assert!(new_text.contains("\"apple_last_key\": 42"));
        assert!(new_text
            .contains("\"anthropic\": { \"kind\": \"anthropic\", \"api_key\": \"sk-unused\" }"));
        assert!(new_text.contains("\"_comment_plugins\": \"toggle plugins here\""));
        let pos = |needle: &str| new_text.find(needle).expect(needle);
        assert!(pos("\"//\"") < pos("\"zebra_first_key\""));
        assert!(pos("\"zebra_first_key\"") < pos("\"default_role\""));
        assert!(pos("\"default_role\"") < pos("\"backends\""));
    }
}
