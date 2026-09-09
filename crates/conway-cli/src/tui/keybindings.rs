//! The rebindable key-dispatch table (board item `01M1YVJ4RA5V7FF95MFRQMTQW3`).
//!
//! **One table, not two.** [`Keymap::defaults`] is the built-in table;
//! [`Keymap::load`] merges an on-disk override file over it, action by
//! action. `crate::tui::input`'s dispatcher and `crate::tui::view::help`'s
//! `/help` overlay both resolve through the SAME [`Keymap`] value, carried
//! on `AppState::keybindings` -- built once per session (`App::new`, via
//! `app/startup.rs`, right after resolving [`keybindings_file_path`] and
//! calling [`Keymap::load`]) and read from there by every consumer, an
//! ordinary struct field rather than a thread-local or process-global (see
//! `AppState::keybindings`'s own doc for why: this crate's `#[tokio::main]`
//! runtime is multi-threaded, and a task can resume on a different OS
//! thread after any `.await`, which would silently strand a thread-local's
//! installed value on the thread that happened to install it). A binding
//! that only `/help` knows about, or only the dispatcher honors, is exactly
//! the failure mode this module exists to rule out.
//!
//! ## File shape
//!
//! `$CONWAY_CONFIG_DIR/keybindings.json` (or `~/.conway/keybindings.json`
//! when that env var is unset -- the same directory
//! [`conway::config::discovery::user_config_path`] resolves `settings.json`
//! into, mirroring [`crate::tui::history`]'s own `history` file placement
//! exactly). Its own file, never a `settings.json` key: `{ "context":
//! { "action": ["key", ...] } }`, e.g.
//! `{"transcript": {"toggle_tool_output": ["Ctrl-O"]}}`.
//!
//! An entry a file supplies REPLACES that action's default chords wholesale
//! -- it never merges with them. Rebinding `toggle_tool_output` off
//! `Ctrl-E` really turns `Ctrl-E` off; a rebind that only ADDED the new key
//! alongside the old one would not be a rebind at all. Binding an action to
//! `[]` disables it with no replacement.
//!
//! The same key bound in two DIFFERENT contexts is fine -- at most one
//! context is ever active at dispatch time, so there is nothing to
//! disambiguate. The same key bound to two DIFFERENT actions WITHIN one
//! context is a load error: see [`LoadError`].
//!
//! ## Validation
//!
//! An unknown context name, an unknown action name (for that context), a
//! key string that does not parse, or a same-context collision is a
//! [`LoadError`] naming the exact `context.action` entry at fault -- never
//! silently dropped, and never a fallback to "whatever half of the file
//! parsed." A missing or unreadable file is NOT an error: [`Keymap::load`]
//! degrades to plain defaults, the same untrusted-input posture
//! [`crate::tui::history::load`] already documents for its own file.
//!
//! ## Not (yet) rebindable
//!
//! Text-editing primitives (typing, `Backspace`, `Left`/`Right`/`Home`/`End`
//! cursor movement, `Enter`/`Alt-Enter`/`Shift-Enter`, the bare-arrow
//! transcript/multi-line-draft priority chain) and the two safety chords
//! `Ctrl-C`/`Ctrl-D` stay fixed, as do the ask-modal/intent-confirm/
//! trust-preview decision keys and the agent panel's own `Esc` -- none of
//! those live in the six contexts [`Context`] enumerates. No vim mode, no
//! chords/leader keys either -- a separate board item owns vim mode; noted
//! as a follow-up here, not attempted. See `docs/interactive.md`'s own
//! "Fixed, not remappable" section, which this module's own
//! `docs_interactive_md_documents_every_action` test keeps honest
//! against [`ACTIONS`] for the part that IS rebindable.
//!
//! ## An `AppState` field, not an ambient global
//!
//! [`Keymap`] carries no installation/lookup machinery of its own -- no
//! thread-local, no process-wide `OnceLock`-cached singleton. It is built
//! once (`Keymap::load`, called from `app/startup.rs`) and stored on
//! `AppState::keybindings`, an ordinary field read the same way every other
//! piece of session state is. This sidesteps two hazards a global would
//! invite: this crate's `#[tokio::main]` runtime is multi-threaded (a
//! thread-local could silently strand its installed value on whichever OS
//! thread happened to install it, once a later `.await` resumes the task
//! elsewhere), and this crate's own test suite loads many DIFFERENT
//! `CONWAY_CONFIG_DIR`s inside one test binary (a process-global
//! cache-once-forever singleton would leak one test's keymap into another's
//! `AppState` -- the identical ambient-global hazard
//! `crates/conway/tests/config_isolation_guard.rs` exists to catch for
//! `conway`'s own config loading). A test that never sets
//! `AppState::keybindings` explicitly always sees plain
//! [`Keymap::defaults()`] (`AppState::new`'s own default for the field),
//! regardless of what any other test did.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// The six existing TUI surfaces a binding can be scoped to. Exhaustive --
/// see this module's own "Not (yet) rebindable" doc for what deliberately
/// falls outside all six.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Context {
    /// The always-live input line: composing/editing/submitting, and
    /// recalling history -- never gated on any overlay being open.
    Prompt,
    /// The conversation view: scrolling, expanding tool output, cycling the
    /// permission mode.
    Transcript,
    /// The `y`/`a`/`p`/`n`/`Esc` permission-decision overlay
    /// (`Mode::AwaitingPermission`).
    Permission,
    /// The `/agents` panel, live only while `AppState::agent_view_open`.
    AgentsPanel,
    /// The `/` command palette's own arrow-key navigation.
    Palette,
    /// The `/settings` menu.
    Settings,
}

impl Context {
    /// The JSON object key AND `/help`/`docs/interactive.md` heading for
    /// this context -- the one spelling every direction (load, display,
    /// docs) agrees on.
    pub const fn key(self) -> &'static str {
        match self {
            Context::Prompt => "prompt",
            Context::Transcript => "transcript",
            Context::Permission => "permission_prompt",
            Context::AgentsPanel => "agents_panel",
            Context::Palette => "palette",
            Context::Settings => "settings",
        }
    }

    /// Every context, in the order [`ACTIONS`]/`/help`/the docs table list
    /// them.
    pub fn all() -> [Context; 6] {
        [
            Context::Prompt,
            Context::Transcript,
            Context::Permission,
            Context::AgentsPanel,
            Context::Palette,
            Context::Settings,
        ]
    }

    fn from_key(s: &str) -> Option<Context> {
        Context::all().into_iter().find(|c| c.key() == s)
    }
}

/// One entry in the rebindable action vocabulary: which [`Context`] it
/// lives in, its stable JSON name, the prose `/help`/the docs table render
/// next to it, and the default chord(s). [`ACTIONS`] is the ONE place this
/// vocabulary is spelled out -- [`Keymap::defaults`], `/help`'s overlay, and
/// `docs/interactive.md`'s own drift guard (this module's own test) all
/// read it, rather than each keeping an independent copy that could drift.
#[derive(Debug, Clone, Copy)]
pub struct ActionSpec {
    pub context: Context,
    pub name: &'static str,
    pub description: &'static str,
    pub defaults: &'static [&'static str],
}

/// The full rebindable action vocabulary -- see this module's own doc for
/// what deliberately is NOT here (text-editing primitives, the safety
/// chords, the modal-decision surfaces outside the six [`Context`]s).
pub const ACTIONS: &[ActionSpec] = &[
    ActionSpec {
        context: Context::Prompt,
        name: "open_editor",
        description:
            "open the current input in $VISUAL/$EDITOR (falling back to vi if neither is set)",
        defaults: &["Ctrl-G"],
    },
    ActionSpec {
        context: Context::Prompt,
        name: "delete_word_back",
        description: "delete the previous word",
        defaults: &["Ctrl-W"],
    },
    ActionSpec {
        context: Context::Prompt,
        name: "history_prev",
        description: "recall the previous input-history entry",
        defaults: &["Ctrl-P"],
    },
    ActionSpec {
        context: Context::Prompt,
        name: "history_next",
        description: "recall the next input-history entry",
        defaults: &["Ctrl-N"],
    },
    ActionSpec {
        context: Context::Transcript,
        name: "toggle_tool_output",
        description: "expand/collapse all tool output",
        defaults: &["Ctrl-E"],
    },
    ActionSpec {
        context: Context::Transcript,
        name: "scroll_page_up",
        description: "scroll the transcript up one page",
        defaults: &["PageUp"],
    },
    ActionSpec {
        context: Context::Transcript,
        name: "scroll_page_down",
        description: "scroll the transcript down one page",
        defaults: &["PageDown"],
    },
    ActionSpec {
        context: Context::Transcript,
        name: "cycle_permission_mode",
        description: "cycle the permission mode: prompt -> plan -> auto-allow",
        defaults: &["Shift-Tab"],
    },
    ActionSpec {
        context: Context::Palette,
        name: "navigate_up",
        description: "move the command-palette selection up",
        defaults: &["Up"],
    },
    ActionSpec {
        context: Context::Palette,
        name: "navigate_down",
        description: "move the command-palette selection down",
        defaults: &["Down"],
    },
    ActionSpec {
        context: Context::AgentsPanel,
        name: "scroll_up",
        description: "move the /agents panel selection up",
        defaults: &["Up"],
    },
    ActionSpec {
        context: Context::AgentsPanel,
        name: "scroll_down",
        description: "move the /agents panel selection down",
        defaults: &["Down"],
    },
    ActionSpec {
        context: Context::AgentsPanel,
        name: "cycle_visibility",
        description: "cycle the panel's visibility filter (active / all / finished)",
        defaults: &["v"],
    },
    ActionSpec {
        context: Context::Permission,
        name: "allow_once",
        description: "allow this call once",
        defaults: &["y"],
    },
    ActionSpec {
        context: Context::Permission,
        name: "allow_always",
        description: "allow always, at the current grant scope",
        defaults: &["a"],
    },
    ActionSpec {
        context: Context::Permission,
        name: "cycle_grant_scope",
        description: "cycle the remembered-grant scope: session -> agent -> subtree",
        defaults: &["s"],
    },
    ActionSpec {
        context: Context::Permission,
        name: "edit_pattern",
        description: "narrow the grant to specific argument fields before allowing",
        defaults: &["p"],
    },
    ActionSpec {
        context: Context::Permission,
        name: "deny",
        description: "deny this call",
        defaults: &["n"],
    },
    ActionSpec {
        context: Context::Permission,
        name: "deny_with_feedback",
        description: "deny this call, with a typed reason",
        defaults: &["Esc"],
    },
    ActionSpec {
        context: Context::Permission,
        name: "scroll_up",
        description: "scroll the shown command up",
        defaults: &["PageUp"],
    },
    ActionSpec {
        context: Context::Permission,
        name: "scroll_down",
        description: "scroll the shown command down",
        defaults: &["PageDown"],
    },
    ActionSpec {
        context: Context::Settings,
        name: "move_up",
        description: "move the selection up",
        defaults: &["Up"],
    },
    ActionSpec {
        context: Context::Settings,
        name: "move_down",
        description: "move the selection down",
        defaults: &["Down"],
    },
    ActionSpec {
        context: Context::Settings,
        name: "activate",
        description: "toggle a display setting, or expand/collapse a group",
        defaults: &["Enter"],
    },
    ActionSpec {
        context: Context::Settings,
        name: "step_left",
        description: "step the numeric setting down",
        defaults: &["Left"],
    },
    ActionSpec {
        context: Context::Settings,
        name: "step_right",
        description: "step the numeric setting up",
        defaults: &["Right"],
    },
    ActionSpec {
        context: Context::Settings,
        name: "close",
        description: "close the settings menu",
        defaults: &["Esc"],
    },
];

/// A parsed key chord -- modifiers plus a base key. `Char` keys match
/// case-insensitively and ignore the `SHIFT` bit (mirroring this crate's
/// own existing idiom of matching both `Char('y')`/`Char('Y')` regardless
/// of how a terminal reports the shift state for a letter); named keys
/// (`Enter`, `PageUp`, ...) require an EXACT match on `Ctrl`/`Alt`/`Shift`,
/// since those three chords (`Alt-Enter`/`Shift-Enter`/`Enter`) are
/// genuinely distinct bindings elsewhere in this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct KeyChord {
    ctrl: bool,
    alt: bool,
    shift: bool,
    code: ChordCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ChordCode {
    Char(char),
    Named(KeyCode),
    /// `Shift-Tab`/`BackTab` -- the same physical keypress, decoded two
    /// different ways depending on the terminal (mirrors `input.rs`'s own
    /// `KeyCode::BackTab`/`KeyCode::Tab`-with-`SHIFT` doc). Matches either
    /// encoding, regardless of which spelling the keymap file used.
    ShiftTab,
}

impl KeyChord {
    fn matches(&self, key: KeyEvent) -> bool {
        match self.code {
            ChordCode::ShiftTab => {
                key.code == KeyCode::BackTab
                    || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT))
            }
            ChordCode::Char(c) => match key.code {
                KeyCode::Char(pressed) => {
                    pressed.eq_ignore_ascii_case(&c)
                        && key.modifiers.contains(KeyModifiers::CONTROL) == self.ctrl
                        && key.modifiers.contains(KeyModifiers::ALT) == self.alt
                }
                _ => false,
            },
            ChordCode::Named(code) => {
                key.code == code
                    && key.modifiers.contains(KeyModifiers::CONTROL) == self.ctrl
                    && key.modifiers.contains(KeyModifiers::ALT) == self.alt
                    && key.modifiers.contains(KeyModifiers::SHIFT) == self.shift
            }
        }
    }
}

/// Parses one key string (`"Ctrl-G"`, `"PageUp"`, `"Shift-Tab"`, `"v"`,
/// ...) into a [`KeyChord`]. `Err` carries a human-readable reason, never a
/// panic -- the caller ([`Keymap::load`]) wraps it with the offending
/// `context.action` entry to build a [`LoadError`].
fn parse_chord(spec: &str) -> Result<KeyChord, String> {
    if spec.trim().is_empty() {
        return Err("empty key string".to_string());
    }
    let parts: Vec<&str> = spec.split('-').collect();
    let (mods, base) = parts.split_at(parts.len() - 1);
    let base = base[0];
    if base.is_empty() {
        return Err(format!("{spec:?}: missing key name after the last '-'"));
    }

    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    for m in mods {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" | "opt" | "option" => alt = true,
            "shift" => shift = true,
            other => return Err(format!("{spec:?}: unknown modifier {other:?}")),
        }
    }

    // Shift-Tab (either spelling: `Shift-Tab` or bare `BackTab`) -- see
    // `ChordCode::ShiftTab`'s own doc.
    if base.eq_ignore_ascii_case("tab") && shift && !ctrl && !alt {
        return Ok(KeyChord {
            ctrl: false,
            alt: false,
            shift: false,
            code: ChordCode::ShiftTab,
        });
    }
    if base.eq_ignore_ascii_case("backtab") && !ctrl && !alt && !shift {
        return Ok(KeyChord {
            ctrl: false,
            alt: false,
            shift: false,
            code: ChordCode::ShiftTab,
        });
    }

    let code = named_key(base)
        .or_else(|| {
            let mut chars = base.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                None
            } else {
                Some(ChordCode::Char(c.to_ascii_lowercase()))
            }
        })
        .ok_or_else(|| format!("{spec:?}: unrecognized key {base:?}"))?;

    Ok(KeyChord {
        ctrl,
        alt,
        shift,
        code,
    })
}

/// The named (non-single-char) keys this module recognizes -- everything
/// [`crate::tui::input`]'s own fixed `match key.code` arms already name,
/// case-insensitively, plus `F1`-`F12`.
fn named_key(base: &str) -> Option<ChordCode> {
    let lower = base.to_ascii_lowercase();
    let named = match lower.as_str() {
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "backspace" => KeyCode::Backspace,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "space" => return Some(ChordCode::Char(' ')),
        _ => {
            if let Some(rest) = lower.strip_prefix('f') {
                if let Ok(n) = rest.parse::<u8>() {
                    if (1..=12).contains(&n) {
                        return Some(ChordCode::Named(KeyCode::F(n)));
                    }
                }
            }
            return None;
        }
    };
    Some(ChordCode::Named(named))
}

/// A load-time error naming the exact offending `context.action` (or, for a
/// malformed top-level shape, the bare context name / file path) -- never a
/// silently-dropped bad entry. See this module's own "Validation" doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
    pub entry: String,
    pub reason: String,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "keybindings.json: entry {:?}: {}",
            self.entry, self.reason
        )
    }
}

impl std::error::Error for LoadError {}

/// The merged, effective key-dispatch table -- see this module's own doc.
#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: HashMap<(Context, &'static str), Vec<(String, KeyChord)>>,
}

impl Keymap {
    /// The built-in table -- [`ACTIONS`]' own `defaults`, nothing else.
    pub fn defaults() -> Keymap {
        let mut bindings = HashMap::new();
        for spec in ACTIONS {
            let chords = spec
                .defaults
                .iter()
                .map(|s| {
                    let chord = parse_chord(s).unwrap_or_else(|e| {
                        panic!(
                            "built-in default {s:?} for {}.{} must parse: {e}",
                            spec.context.key(),
                            spec.name
                        )
                    });
                    (s.to_string(), chord)
                })
                .collect();
            bindings.insert((spec.context, spec.name), chords);
        }
        Keymap { bindings }
    }

    /// [`Self::defaults`], with `path` merged over it if it exists. A
    /// missing/unreadable file is NOT an error (plain defaults, mirroring
    /// [`crate::tui::history::load`]'s own untrusted-input posture). A file
    /// that exists but is malformed IS -- see this module's own
    /// "Validation" doc; the returned [`LoadError`] names the exact entry
    /// at fault.
    pub fn load(path: &Path) -> Result<Keymap, LoadError> {
        let mut keymap = Self::defaults();
        let contents = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Ok(keymap),
        };
        let raw: serde_json::Value = serde_json::from_str(&contents).map_err(|e| LoadError {
            entry: path.display().to_string(),
            reason: format!("not valid JSON: {e}"),
        })?;
        let raw_obj = raw.as_object().ok_or_else(|| LoadError {
            entry: path.display().to_string(),
            reason: "the top level must be a JSON object of context -> {action: [keys]}"
                .to_string(),
        })?;

        for (context_key, actions_val) in raw_obj {
            let context = Context::from_key(context_key).ok_or_else(|| LoadError {
                entry: context_key.clone(),
                reason: format!(
                    "unknown context {context_key:?} -- expected one of: {}",
                    Context::all()
                        .iter()
                        .map(|c| c.key())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            })?;
            let actions_obj = actions_val.as_object().ok_or_else(|| LoadError {
                entry: context_key.clone(),
                reason: "a context's value must be an object of action -> [keys]".to_string(),
            })?;

            for (action_name, keys_val) in actions_obj {
                let entry = format!("{context_key}.{action_name}");
                let spec = ACTIONS
                    .iter()
                    .find(|a| a.context == context && a.name == action_name)
                    .ok_or_else(|| LoadError {
                        entry: entry.clone(),
                        reason: "unknown action for this context".to_string(),
                    })?;
                let keys_arr = keys_val.as_array().ok_or_else(|| LoadError {
                    entry: entry.clone(),
                    reason: "an action's value must be an array of key strings".to_string(),
                })?;

                let mut chords = Vec::with_capacity(keys_arr.len());
                for key_val in keys_arr {
                    let key_str = key_val.as_str().ok_or_else(|| LoadError {
                        entry: entry.clone(),
                        reason: "every key must be a string".to_string(),
                    })?;
                    let chord = parse_chord(key_str).map_err(|reason| LoadError {
                        entry: entry.clone(),
                        reason: format!("key {key_str:?}: {reason}"),
                    })?;
                    chords.push((key_str.to_string(), chord));
                }
                keymap.bindings.insert((spec.context, spec.name), chords);
            }
        }

        keymap.check_no_intra_context_collisions()?;
        Ok(keymap)
    }

    /// The same key bound to two DIFFERENT actions within one [`Context`]
    /// is ambiguous -- refused here, naming whichever entry lost the race
    /// to be inserted first (deterministic: [`ACTIONS`]' own declared
    /// order). The same key across two DIFFERENT contexts is untouched by
    /// this check (a `HashMap` keyed separately per context can never
    /// collide across them).
    fn check_no_intra_context_collisions(&self) -> Result<(), LoadError> {
        for context in Context::all() {
            let mut seen: HashMap<KeyChord, &'static str> = HashMap::new();
            for spec in ACTIONS.iter().filter(|a| a.context == context) {
                let Some(chords) = self.bindings.get(&(context, spec.name)) else {
                    continue;
                };
                for (_, chord) in chords {
                    if let Some(other) = seen.insert(*chord, spec.name) {
                        return Err(LoadError {
                            entry: format!("{}.{}", context.key(), spec.name),
                            reason: format!(
                                "the same key is already bound to {}.{other} in this context -- \
                                 a key can be bound in two DIFFERENT contexts, but not twice \
                                 within one",
                                context.key()
                            ),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// Whether `key` fires `(context, action)` under this table. `action`
    /// names one of [`ACTIONS`]' own `name`s -- a typo here is a bug in the
    /// CALLER (this crate's own dispatcher/tests), not a [`LoadError`], so
    /// it degrades to "never matches" rather than panicking.
    pub fn matches(&self, context: Context, action: &str, key: KeyEvent) -> bool {
        self.bindings
            .get(&(context, action))
            .map(|chords| chords.iter().any(|(_, chord)| chord.matches(key)))
            .unwrap_or(false)
    }

    /// The CURRENT key strings bound to `(context, action)`, in file/
    /// default order -- what `/help` and `docs/interactive.md`'s own drift
    /// guard both display. Empty if the action was rebound to `[]`
    /// (deliberately disabled, not "missing").
    pub fn keys_for(&self, context: Context, action: &str) -> Vec<String> {
        self.bindings
            .get(&(context, action))
            .map(|chords| chords.iter().map(|(s, _)| s.clone()).collect())
            .unwrap_or_default()
    }
}

/// `$CONWAY_CONFIG_DIR/keybindings.json` (or `~/.conway/keybindings.json`)
/// -- alongside the resolved global `settings.json`, exactly mirroring
/// [`crate::tui::history::load`]'s own `history` file placement (see that
/// module's doc): [`conway::config::discovery::user_config_path`]'s
/// directory, different filename. `None` only when that function is (no
/// resolvable home directory and `CONWAY_CONFIG_DIR` unset) -- the session
/// still runs on plain built-in defaults in that case.
pub fn keybindings_file_path(env: &HashMap<String, String>) -> Option<PathBuf> {
    conway::config::discovery::user_config_path(env)
        .and_then(|settings| settings.parent().map(|dir| dir.join("keybindings.json")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    fn write_temp(name: &str, contents: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "conway-keybindings-test-{}-{}-{name}.json",
            std::process::id(),
            unique_suffix()
        ));
        std::fs::write(&path, contents).expect("write must succeed against a writable temp path");
        path
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn missing_file_yields_plain_defaults() {
        let path = std::env::temp_dir().join(format!(
            "conway-keybindings-test-does-not-exist-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        let _ = std::fs::remove_file(&path);

        let keymap = Keymap::load(&path).expect("a missing file must not be a load error");

        assert!(keymap.matches(Context::Transcript, "toggle_tool_output", ctrl('e')));
    }

    /// Acceptance check (a), first half: rebinding `toggle_tool_output` to
    /// `Ctrl-O` makes `Ctrl-O` match it. A wrong implementation that never
    /// reads the file at all (defaults only, ignoring `keybindings.json`
    /// entirely) fails THIS half.
    #[test]
    fn rebinding_toggle_tool_output_to_ctrl_o_makes_ctrl_o_match() {
        let path = write_temp(
            "rebind-ctrl-o-matches",
            r#"{"transcript": {"toggle_tool_output": ["Ctrl-O"]}}"#,
        );

        let keymap = Keymap::load(&path).expect("a valid rebind must load");

        assert!(keymap.matches(Context::Transcript, "toggle_tool_output", ctrl('o')));
        let _ = std::fs::remove_file(&path);
    }

    /// Acceptance check (a), second half: the SAME rebind makes `Ctrl-E`
    /// stop matching. A wrong implementation that only ADDS `Ctrl-O` as a
    /// second binding (rather than REPLACING the default) passes the first
    /// half above but fails THIS one -- the whole reason this is a
    /// two-halves check, not one.
    #[test]
    fn rebinding_toggle_tool_output_to_ctrl_o_makes_ctrl_e_stop_matching() {
        let path = write_temp(
            "rebind-ctrl-o-removes-ctrl-e",
            r#"{"transcript": {"toggle_tool_output": ["Ctrl-O"]}}"#,
        );

        let keymap = Keymap::load(&path).expect("a valid rebind must load");

        assert!(!keymap.matches(Context::Transcript, "toggle_tool_output", ctrl('e')));
        let _ = std::fs::remove_file(&path);
    }

    /// Acceptance check (b): an unknown action fails load, naming the
    /// entry. Catches an implementation that silently ignores an entry it
    /// does not recognize instead of refusing to load.
    #[test]
    fn unknown_action_fails_load_and_names_the_entry() {
        let path = write_temp(
            "unknown-action",
            r#"{"transcript": {"not_a_real_action": ["Ctrl-Z"]}}"#,
        );

        let err = Keymap::load(&path).expect_err("an unknown action must fail load");

        assert_eq!(err.entry, "transcript.not_a_real_action");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unknown_context_fails_load_and_names_the_entry() {
        let path = write_temp(
            "unknown-context",
            r#"{"not_a_real_context": {"whatever": ["a"]}}"#,
        );

        let err = Keymap::load(&path).expect_err("an unknown context must fail load");

        assert_eq!(err.entry, "not_a_real_context");
        let _ = std::fs::remove_file(&path);
    }

    /// Acceptance check (b), the "unparseable key" half: an entry whose
    /// key string does not parse fails load, naming the entry -- not the
    /// whole file, and not silently skipped.
    #[test]
    fn unparseable_key_fails_load_and_names_the_entry() {
        let path = write_temp(
            "unparseable-key",
            r#"{"prompt": {"open_editor": ["NotAKeyAtAll"]}}"#,
        );

        let err = Keymap::load(&path).expect_err("an unparseable key must fail load");

        assert_eq!(err.entry, "prompt.open_editor");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn same_key_twice_in_one_context_fails_load() {
        let path = write_temp(
            "collision",
            r#"{"permission_prompt": {"allow_once": ["x"], "deny": ["x"]}}"#,
        );

        let err =
            Keymap::load(&path).expect_err("the same key twice in one context must fail load");

        assert!(
            err.reason.contains("already bound"),
            "unexpected reason: {err:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_same_key_in_two_different_contexts_is_fine() {
        let path = write_temp(
            "cross-context-ok",
            r#"{"transcript": {"toggle_tool_output": ["v"]}, "agents_panel": {"cycle_visibility": ["v"]}}"#,
        );

        Keymap::load(&path).expect("the same key in two different contexts must be allowed");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn every_default_action_parses_without_panicking() {
        let keymap = Keymap::defaults();
        for spec in ACTIONS {
            let expected: Vec<String> = spec.defaults.iter().map(|s| s.to_string()).collect();
            assert_eq!(keymap.keys_for(spec.context, spec.name), expected);
        }
    }

    #[test]
    fn no_two_default_actions_collide_within_a_context() {
        // `Keymap::defaults()` must satisfy its own collision rule -- a
        // failure here means two `ACTIONS` entries in the SAME context
        // share a default key, a bug in this module's own table, not in
        // any keymap file.
        let keymap = Keymap::defaults();
        keymap
            .check_no_intra_context_collisions()
            .expect("the built-in defaults must not collide with themselves");
    }

    #[test]
    fn shift_tab_and_backtab_are_the_same_binding() {
        let via_shift_tab = parse_chord("Shift-Tab").expect("must parse");
        let via_backtab = parse_chord("BackTab").expect("must parse");
        assert_eq!(via_shift_tab, via_backtab);
    }

    /// The doc's own action-vocabulary table is generated from this SAME
    /// `ACTIONS` table, not a hand-maintained second list -- this test is
    /// the drift guard that keeps the two from disagreeing. A future
    /// action added to `ACTIONS` without a matching docs update fails
    /// here.
    #[test]
    fn docs_interactive_md_documents_every_action() {
        let doc = include_str!("../../../../docs/interactive.md");
        for spec in ACTIONS {
            let marker = format!("`{}.{}`", spec.context.key(), spec.name);
            assert!(
                doc.contains(&marker),
                "docs/interactive.md must document {marker}"
            );
            for key in spec.defaults.iter().copied() {
                assert!(
                    doc.contains(key),
                    "docs/interactive.md must mention default key {key:?} for {marker}"
                );
            }
        }
    }
}
