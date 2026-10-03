//! `@`-mention path/agent completion, and plain-`Tab` path completion
//! (board item `01M1YVF4X864GKSGZM4PSCTMEH`).
//!
//! This module is deliberately a pure, disk-touching-only-when-asked set of
//! plain functions, with NO `AppState` dependency -- `crate::tui::state`'s
//! own `state/mentions.rs` is the thin `AppState`-side glue (caching the
//! walked candidate universe, wiring the cursor/anchor bookkeeping)
//! layered on top. Keeping the algorithm itself free of `AppState` is what
//! lets a fixture tree drive [`scan_paths`]/[`fuzzy_filter`]/[`tab_complete`]
//! directly in this module's own tests, with no terminal, no `AppState`,
//! and no live TUI session at all -- the identical split `view/palette.rs`'s
//! own `matches` already uses relative to `input.rs`'s `palette_navigate`.
//!
//! ## The bounded walk (C-04: no new dependency)
//!
//! [`scan_paths`] prefers `git ls-files --cached --others --exclude-standard`
//! (one bounded subprocess) whenever `base` is inside a git work tree --
//! that flag combination is exactly "every tracked file, plus every
//! untracked file `.gitignore` does not exclude," the same listing `git
//! status` itself is built on. This was chosen over pulling in the `ignore`
//! or `walkdir` crates: `ignore` (the `ripgrep` crate family) is the
//! heavier of the two, bundling glob/overlap logic this module does not
//! need; `walkdir` is lighter but still a new, permanent dependency for a
//! single call site, when the system's own `git` binary -- already a hard
//! requirement of every conway session that touches a repo -- does the
//! EXACT SAME `.gitignore`-aware walk, battle-tested, as a subprocess this
//! crate already knows how to spawn (`app/editor.rs`'s `$VISUAL`/`$EDITOR`
//! launch is the existing precedent for "shell out, do not vendor"). A
//! plain bounded walk with a fixed skip list (`SKIP_DIR_NAMES`) covers the
//! no-repo case, where there is no `.gitignore` to honor anyway.
//!
//! **Never unbounded.** `git ls-files` streams its output incrementally;
//! [`scan_paths`] stops reading (and kills the child) the moment it has
//! `cap + 1` entries, so the walk's cost is bounded by "how long it takes
//! to produce `cap` entries," not by the size of the tree -- a huge
//! monorepo cannot make this block past that point. The manual fallback
//! walk additionally bounds itself by wall-clock time
//! (`MANUAL_WALK_TIME_BUDGET`), since a directory with very few files but
//! very deep/wide fan-out could otherwise take a long time to enumerate
//! even under the count cap.

use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The default cap on how many candidates one bounded walk returns -- see
/// this module's own "The bounded walk" doc.
pub const DEFAULT_SCAN_CAP: usize = 2000;

/// The manual (non-git) walk's own wall-clock budget -- see this module's
/// own doc for why the count cap alone is not enough.
const MANUAL_WALK_TIME_BUDGET: Duration = Duration::from_millis(300);

/// Hard wall-clock bound on EACH `git` subprocess this module spawns
/// (the repo probe and the listing itself) -- review finding (CRITICAL,
/// round 1): a contended `.git/index.lock`, a stalled network mount, or
/// any other reason `git` itself never produces output/exits can otherwise
/// hang indefinitely. This is enforced two different ways because the two
/// calls block two different ways: [`is_inside_git_work_tree`] blocks on
/// the CHILD EXITING (no output to read at all), bounded by polling
/// `Child::try_wait` instead of a single blocking `.status()`; [`git_ls_files`]
/// blocks on `Read::read`, which -- if `git` writes nothing at all -- never
/// returns control to re-check a wall-clock guard inside the read loop on
/// its own, so a SEPARATE thread kills the child once the budget elapses,
/// which is what unblocks the read (the pipe closes, `read` returns).
/// Chosen generously enough that an ordinary repo never comes close (a
/// local `ls-files` typically completes in single-digit milliseconds), but
/// short enough that the caller -- which runs this off the render loop via
/// `tokio::task::spawn_blocking`, see `app/mention_scan.rs` -- gets SOME
/// answer back promptly even in the pathological case.
const GIT_SUBPROCESS_TIMEOUT: Duration = Duration::from_millis(500);

/// Directory names the manual (non-git) walk never descends into --
/// `.git` (never meaningful to mention), `target`/`node_modules` (huge,
/// generated, never meaningful to mention either). A git-backed walk needs
/// no such list: `--exclude-standard` already keeps `.gitignore`'d
/// directories like these out, wherever a project's own `.gitignore`
/// already lists them.
const SKIP_DIR_NAMES: &[&str] = &[".git", "target", "node_modules"];

/// One bounded directory walk's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathScan {
    /// Every candidate path found, relative to the scanned base directory,
    /// forward-slash-separated regardless of platform.
    pub paths: Vec<String>,
    /// Whether the walk stopped early because it hit its cap (count or, for
    /// the manual fallback, wall-clock time) -- surfaced in the mention
    /// overlay's own title so a capped listing never silently looks
    /// complete.
    pub capped: bool,
}

/// The bounded, `.gitignore`-aware (when applicable) directory walk --
/// see this module's own doc for the exact strategy and why no new
/// dependency is pulled in for it.
pub fn scan_paths(base: &Path, cap: usize) -> PathScan {
    if is_inside_git_work_tree(base) {
        if let Some(scan) = git_ls_files(base, cap) {
            return scan;
        }
    }
    manual_walk(base, cap)
}

fn is_inside_git_work_tree(base: &Path) -> bool {
    let mut child = match Command::new("git")
        .arg("-C")
        .arg(base)
        .args(["rev-parse", "--is-inside-work-tree"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };
    match wait_bounded(&mut child, GIT_SUBPROCESS_TIMEOUT) {
        Some(status) => status.success(),
        None => {
            // Timed out: `git` itself is stuck (a contended index lock,
            // say). Kill it, and report "not a repo" -- `scan_paths` falls
            // back to the manual walk, which is a safe, if less complete,
            // answer. This kill only happens if this process is still
            // running: a forced exit (a second Ctrl-C, or a second
            // SIGTERM/SIGHUP, which call `std::process::exit`) inside the
            // timeout window leaves the `git` child running until it finishes
            // or next writes to its closed pipe. Accepted residual risk.
            let _ = child.kill();
            let _ = child.wait();
            false
        }
    }
}

/// Polls `child` for up to `budget`, sleeping briefly between checks,
/// rather than a single blocking `.wait()`/`.status()` call -- `std::
/// process::Child` has no native deadline/cancel primitive, so bounded
/// polling is the only portable way to cap how long this function can
/// block on a child that is merely slow to EXIT (as opposed to slow to
/// produce output, which [`git_ls_files`]'s own kill-switch thread handles
/// instead, since that case blocks on `Read::read`, not on exit). Returns
/// `None` on timeout, leaving `child` still running -- the caller kills it.
fn wait_bounded(child: &mut Child, budget: Duration) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        if start.elapsed() >= budget {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// `None` only when `git` itself could not be spawned at all (missing
/// binary) -- the caller falls back to [`manual_walk`] in that case. Once
/// spawned, this always returns `Some`, even for an empty listing (an
/// empty, git-tracked directory is a real, final answer, not a probe
/// failure) and even when [`GIT_SUBPROCESS_TIMEOUT`] is hit mid-read (a
/// partial, explicitly `capped` listing, never a hang).
fn git_ls_files(base: &Path, cap: usize) -> Option<PathScan> {
    let child = Command::new("git")
        .arg("-C")
        .arg(base)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let child = Arc::new(Mutex::new(child));

    // The kill-switch thread -- see [`GIT_SUBPROCESS_TIMEOUT`]'s own doc
    // for why a wall-clock check inside the read loop below is not enough
    // on its own: if `git` writes NOTHING (stuck behind a contended
    // `.git/index.lock`, say), the blocking `Read::read` call below never
    // returns control to re-check a clock. This thread is the only thing
    // that can unblock it in that case, by killing the child out from under
    // it -- the pipe then closes and `read` returns `Ok(0)`/an error
    // instead of hanging forever. Like every thread here, it dies with the
    // process: a forced exit inside the timeout window cannot kill the
    // child (see `is_inside_git_work_tree`'s note on that residual risk).
    let killer = Arc::clone(&child);
    let timer = std::thread::spawn(move || {
        std::thread::sleep(GIT_SUBPROCESS_TIMEOUT);
        if let Ok(mut child) = killer.lock() {
            let _ = child.kill();
        }
    });

    let stdout = child.lock().ok().and_then(|mut c| c.stdout.take());
    let Some(mut stdout) = stdout else {
        let _ = timer.join();
        return Some(PathScan {
            paths: Vec::new(),
            capped: false,
        });
    };

    let start = Instant::now();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut paths = Vec::new();
    'read: loop {
        match stdout.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                while let Some(pos) = buf.iter().position(|&b| b == 0) {
                    let entry: Vec<u8> = buf.drain(..=pos).collect();
                    let entry = &entry[..entry.len() - 1];
                    if let Ok(s) = std::str::from_utf8(entry) {
                        paths.push(s.to_string());
                    }
                    if paths.len() > cap {
                        break 'read;
                    }
                }
            }
            Err(_) => break,
        }
    }
    // Either the count cap tripped above, or the kill-switch thread fired
    // (the elapsed time will be at or past the budget) -- either is a
    // genuine "stopped early," surfaced identically to the caller.
    let timed_out = start.elapsed() >= GIT_SUBPROCESS_TIMEOUT;
    let capped = timed_out || paths.len() > cap;
    if paths.len() > cap {
        paths.truncate(cap);
    }
    // Drop the pipe and reap the child WITHOUT waiting for it to finish
    // producing the rest of a huge listing -- this is what bounds the
    // walk's cost to "enough bytes to find `cap` entries," not "the size of
    // the repo" (module doc).
    drop(stdout);
    if let Ok(mut c) = child.lock() {
        let _ = c.kill();
        let _ = c.wait();
    }
    // The timer thread either already fired (its `kill` was a harmless
    // no-op against an already-exited child) or is about to -- joined here
    // so this function never returns while another thread still holds a
    // reference into `child`.
    let _ = timer.join();
    paths.sort();
    Some(PathScan { paths, capped })
}

fn manual_walk(base: &Path, cap: usize) -> PathScan {
    let start = Instant::now();
    let mut paths = Vec::new();
    let mut capped = false;
    let mut queue: VecDeque<PathBuf> = VecDeque::new();
    queue.push_back(PathBuf::new());
    'outer: while let Some(rel_dir) = queue.pop_front() {
        if start.elapsed() > MANUAL_WALK_TIME_BUDGET {
            capped = true;
            break;
        }
        let dir = base.join(&rel_dir);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if start.elapsed() > MANUAL_WALK_TIME_BUDGET {
                capped = true;
                break 'outer;
            }
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if SKIP_DIR_NAMES.contains(&name_str.as_ref()) {
                continue;
            }
            let rel = if rel_dir.as_os_str().is_empty() {
                PathBuf::from(&name)
            } else {
                rel_dir.join(&name)
            };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                queue.push_back(rel);
            } else {
                paths.push(rel.to_string_lossy().replace('\\', "/"));
                if paths.len() >= cap {
                    capped = true;
                    break 'outer;
                }
            }
        }
    }
    paths.sort();
    PathScan { paths, capped }
}

/// A `@`-mention currently being typed: `start` is the char index of the
/// `@` itself within the input, `fragment` is the (possibly empty) text
/// typed since then, up to the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionQuery {
    pub start: usize,
    pub fragment: String,
}

/// Finds the `@`-mention, if any, the cursor currently sits inside: scans
/// left from `cursor` over non-whitespace characters looking for an `@`
/// that itself sits at a word boundary (input start, or preceded by
/// whitespace) -- an email-shaped `foo@bar` typed elsewhere on the line
/// never qualifies, since the `@` there follows a non-whitespace character,
/// not whitespace/start-of-input. `input`/`cursor` are both char-indexed,
/// matching every other `AppState::input` consumer in this crate.
pub fn active_mention(input: &str, cursor: usize) -> Option<MentionQuery> {
    let chars: Vec<char> = input.chars().collect();
    let cursor = cursor.min(chars.len());
    let mut i = cursor;
    while i > 0 {
        let c = chars[i - 1];
        if c.is_whitespace() {
            return None;
        }
        if c == '@' {
            let boundary_ok = i == 1 || chars[i - 2].is_whitespace();
            if !boundary_ok {
                return None;
            }
            let fragment: String = chars[i..cursor].iter().collect();
            return Some(MentionQuery {
                start: i - 1,
                fragment,
            });
        }
        i -= 1;
    }
    None
}

/// Which candidate universe a mention addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MentionMode {
    /// A file-path mention -- the common case.
    Path,
    /// `/fork`/`/spawn`'s own `@<agent>` addressing convention
    /// (`commands.rs`) -- a completely different vocabulary (live agent
    /// names/ids, not paths), reached only when the input's first word is
    /// one of those two commands.
    Agent,
}

/// Decides [`MentionMode`] from the input's first word -- see that enum's
/// own doc. Anything other than a leading `/fork`/`/spawn` is a path
/// mention, including every bare (non-slash-command) message.
pub fn mention_mode(input: &str) -> MentionMode {
    match input.split_whitespace().next().unwrap_or("") {
        "/fork" | "/spawn" => MentionMode::Agent,
        _ => MentionMode::Path,
    }
}

/// Scores `candidate` against `fragment` as a case-insensitive,
/// not-necessarily-contiguous subsequence match: `(start, gap, length)`,
/// ascending order is "best first" -- the earliest possible match start
/// (so a prefix match always outranks a scattered one), then the
/// tightest span (fewest extra characters interspersed among the
/// matched ones), then the shorter candidate, with the caller
/// tie-breaking on the candidate text itself for a fully deterministic
/// order. `None` when `fragment` is not a subsequence of `candidate` at
/// all. An empty `fragment` matches every candidate, at its minimal score.
///
/// `pub(crate)` (board item `01M1YVH1X49WYSQ9C2Z4D6B4XM`): `view::
/// palette::matches` reuses this SAME scorer for its own fuzzy/subsequence
/// ranking tier rather than hand-writing a second one -- the identical
/// "one algorithm, two call sites" shape this module's own doc already
/// describes for the `AppState` split.
pub(crate) fn score(candidate: &str, fragment: &str) -> Option<(usize, usize, usize)> {
    let cand: Vec<char> = candidate.chars().collect();
    if fragment.is_empty() {
        return Some((0, 0, cand.len()));
    }
    let frag: Vec<char> = fragment.chars().collect();
    let mut start = None;
    let mut end = 0usize;
    let mut fi = 0usize;
    for (ci, c) in cand.iter().enumerate() {
        if fi >= frag.len() {
            break;
        }
        if c.eq_ignore_ascii_case(&frag[fi]) {
            if start.is_none() {
                start = Some(ci);
            }
            end = ci;
            fi += 1;
        }
    }
    if fi < frag.len() {
        return None;
    }
    let start = start.unwrap_or(0);
    let gap = end - start + 1 - frag.len();
    Some((start, gap, cand.len()))
}

/// Every candidate in `candidates` that matches `fragment` as a
/// subsequence (`score`'s own rule, below), best match first, ties broken
/// lexicographically on the candidate text itself so the result is fully
/// deterministic.
pub fn fuzzy_filter<'a>(candidates: &'a [String], fragment: &str) -> Vec<&'a str> {
    let mut scored: Vec<((usize, usize, usize), &str)> = candidates
        .iter()
        .filter_map(|c| score(c, fragment).map(|s| (s, c.as_str())))
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    scored.into_iter().map(|(_, c)| c).collect()
}

/// The word ending at `cursor` -- the span of non-whitespace characters
/// immediately before it -- for plain `Tab` path completion (point 3: no
/// `@` involved). Returns `(start_char_idx, word)`; `None` when the cursor
/// sits at the start of the line, or right after whitespace (nothing to
/// complete).
pub fn word_before_cursor(input: &str, cursor: usize) -> Option<(usize, String)> {
    let chars: Vec<char> = input.chars().collect();
    let cursor = cursor.min(chars.len());
    let mut i = cursor;
    while i > 0 && !chars[i - 1].is_whitespace() {
        i -= 1;
    }
    if i == cursor {
        return None;
    }
    let word: String = chars[i..cursor].iter().collect();
    Some((i, word))
}

/// Shell/readline-style `Tab` completion over `candidates`: a single
/// prefix match completes outright; several prefix matches complete only
/// as far as their shared prefix extends. `None` when nothing starts with
/// `word`, or the shared prefix does not extend `word` at all (nothing
/// left to fill in) -- the caller leaves the input untouched either way,
/// exactly as an unbound key would.
pub fn tab_complete(candidates: &[String], word: &str) -> Option<String> {
    if word.is_empty() {
        return None;
    }
    let matches: Vec<&str> = candidates
        .iter()
        .filter(|c| c.starts_with(word))
        .map(|s| s.as_str())
        .collect();
    match matches.as_slice() {
        [] => None,
        [one] => Some((*one).to_string()),
        many => {
            let mut prefix = many[0].to_string();
            for m in &many[1..] {
                while !prefix.is_empty() && !m.starts_with(&prefix) {
                    prefix.pop();
                }
            }
            if prefix.chars().count() > word.chars().count() {
                Some(prefix)
            } else {
                None
            }
        }
    }
}

/// The text [`accept_mention_text`] (the glue `input.rs::accept_mention`
/// calls to build what actually gets inserted) produces for `candidate`:
/// bare (`@docs/main.rs `) when it has no whitespace, quoted
/// (`@"docs/release notes.md" `) when it does -- review finding
/// (SIGNIFICANT, round 1): a bare space-containing path round-trips through
/// neither `str::split_whitespace` nor a plain re-typed word, so a
/// candidate with a space needs a representation [`extract_path_mentions`]
/// can parse back out WHOLE, not truncated at the first space. Quoting only
/// when needed keeps the common (no-space) case exactly as it read before
/// this fix.
pub fn accept_mention_text(candidate: &str) -> String {
    if candidate.chars().any(char::is_whitespace) {
        format!("@\"{candidate}\" ")
    } else {
        format!("@{candidate} ")
    }
}

/// Every `@<token>` mention in `text` at a word boundary (start-of-text or
/// preceded by whitespace): a quoted `@"...with spaces..."` form (produced
/// by [`accept_mention_text`] for a space-containing candidate) is
/// parsed back out WHOLE, including its internal spaces; every other `@`
/// token is the plain bareword up to the next whitespace (`str::
/// split_whitespace`'s own tokens are, by construction, each preceded by
/// whitespace or the start of the string, so a bareword mention needs no
/// separate boundary check the way [`active_mention`] does for a live,
/// in-progress cursor position). An UNTERMINATED quote (`@"no closing
/// quote`, a hand-typed malformed mention) is treated as a mention running
/// to the end of the string, rather than silently dropped or mis-split --
/// the honest reading of "whatever came after the `@"` is what the
/// operator meant to reference." Order of first appearance; duplicates
/// kept (mirrors how many times the operator actually referenced each
/// path).
pub fn extract_path_mentions(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut mentions = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let at_boundary = i == 0 || chars[i - 1].is_whitespace();
        if chars[i] == '@' && at_boundary {
            let mut j = i + 1;
            if j < chars.len() && chars[j] == '"' {
                j += 1;
                let start = j;
                while j < chars.len() && chars[j] != '"' {
                    j += 1;
                }
                let end = j; // exclusive; `j == chars.len()` means unterminated
                let token: String = chars[start..end].iter().collect();
                if !token.is_empty() {
                    mentions.push(token);
                }
                i = if j < chars.len() { j + 1 } else { j };
                continue;
            } else {
                let start = j;
                while j < chars.len() && !chars[j].is_whitespace() {
                    j += 1;
                }
                let token: String = chars[start..j].iter().collect();
                if !token.is_empty() {
                    mentions.push(token);
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    mentions
}

/// Appends a clearly-delimited provenance hint listing every `@`-mentioned
/// path in `text`, so the model reads an `@mention` as a reference to a
/// real path rather than ordinary prose -- see this module's own doc for
/// why a text suffix, not a `system_note`.
///
/// **Why a text suffix, not a `system_note`.** `SessionHandle::
/// prompt_agent` takes a single `impl Into<String>` for the whole turn --
/// `conway`'s public facade has no seam today for attaching separate
/// per-turn metadata to a user turn. `LogRecord::SystemNote` exists, but
/// every call site that appends one lives inside `conway-runtime` itself
/// (budget warnings, pull-in truncation, ...); nothing on `conway`'s own
/// public surface lets a CALLER attach one to a turn it is about to send,
/// and adding that primitive is a `conway-core`/`conway-runtime` change,
/// out of this module's own `conway-cli`-only scope. A clearly-delimited
/// suffix is the honest, scoped alternative: the model reads it as part of the same turn
/// (which it is), never as something the operator themselves typed.
///
/// A no-op for `/fork`/`/spawn`'s own `@<agent>` addressing
/// ([`mention_mode`] returns [`MentionMode::Agent`]) -- those `@`-tokens
/// name a live agent, not a file, and relabeling them as a "path reference"
/// would be actively wrong.
pub fn append_mention_hint(text: String) -> String {
    if mention_mode(&text) != MentionMode::Path {
        return text;
    }
    let mentions = extract_path_mentions(&text);
    if mentions.is_empty() {
        return text;
    }
    let list = mentions.join(", ");
    format!(
        "{text}\n\n[conway: the @-mentions above reference these paths -- read them with your \
         own tools rather than treating this line as part of the operator's own words: {list}]"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_fixture_tree(root: &Path) {
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("src/markdown.rs"), "// md").unwrap();
        std::fs::write(root.join("src/mod.rs"), "// mod").unwrap();
        std::fs::write(root.join("lib/main.rs"), "// lib").unwrap();
        std::fs::write(root.join("README.md"), "# readme").unwrap();
    }

    fn tempdir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "conway-mentions-test-{}-{}-{name}",
            std::process::id(),
            {
                use std::sync::atomic::{AtomicU64, Ordering};
                static COUNTER: AtomicU64 = AtomicU64::new(0);
                COUNTER.fetch_add(1, Ordering::Relaxed)
            }
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    // ---- wait_bounded: the generic bounded-wait primitive both git calls
    // below are built on (review finding CRITICAL, round 1: a contended
    // `.git/index.lock` or any other reason `git` never exits/writes must
    // not hang this module indefinitely) ----

    #[test]
    fn wait_bounded_times_out_on_a_genuinely_slow_child_without_overrunning_its_budget() {
        let mut child = Command::new("sleep")
            .arg("5")
            .spawn()
            .expect("the system `sleep` binary must be spawnable in CI/dev");
        let start = Instant::now();

        let result = wait_bounded(&mut child, Duration::from_millis(100));

        assert!(
            result.is_none(),
            "a 5s sleep must not report an exit status within a 100ms budget"
        );
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "wait_bounded must return close to its own budget, not block past it: {:?}",
            start.elapsed()
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn wait_bounded_returns_promptly_once_the_child_actually_exits() {
        let mut child = Command::new("true")
            .spawn()
            .expect("the system `true` binary must be spawnable in CI/dev");

        // `true` exits almost immediately -- a generous budget here proves
        // this does not ALWAYS wait out the full budget, only when the
        // child is genuinely still running.
        let result = wait_bounded(&mut child, Duration::from_secs(5));

        assert!(result.is_some_and(|status| status.success()));
    }

    // ---- active_mention ----

    #[test]
    fn at_sign_at_start_opens_a_mention() {
        let q = active_mention("@src/ma", 7).expect("must find a mention");
        assert_eq!(q.start, 0);
        assert_eq!(q.fragment, "src/ma");
    }

    #[test]
    fn at_sign_after_whitespace_opens_a_mention() {
        let input = "look at @src/ma";
        let q = active_mention(input, input.chars().count()).expect("must find a mention");
        assert_eq!(q.start, 8);
        assert_eq!(q.fragment, "src/ma");
    }

    #[test]
    fn at_sign_mid_word_is_not_a_mention() {
        // "foo@bar" -- the '@' is not preceded by whitespace/start.
        assert_eq!(active_mention("foo@bar", 7), None);
    }

    #[test]
    fn whitespace_before_cursor_closes_the_mention() {
        // Cursor is right after a space, following a finished mention.
        assert_eq!(active_mention("@src/main.rs ", 13), None);
    }

    #[test]
    fn bare_at_sign_with_empty_fragment_still_opens() {
        let q = active_mention("@", 1).expect("must find a mention");
        assert_eq!(q.start, 0);
        assert_eq!(q.fragment, "");
    }

    // ---- mention_mode ----

    #[test]
    fn fork_and_spawn_address_agents() {
        assert_eq!(mention_mode("/fork @foo"), MentionMode::Agent);
        assert_eq!(mention_mode("/spawn @foo"), MentionMode::Agent);
    }

    #[test]
    fn an_ordinary_message_addresses_paths() {
        assert_eq!(
            mention_mode("please look at @src/main.rs"),
            MentionMode::Path
        );
        assert_eq!(mention_mode("/steer @foo"), MentionMode::Path);
    }

    // ---- fuzzy_filter: acceptance anchor, `@src/ma` against a fixture tree ----

    #[test]
    fn src_ma_matches_main_then_markdown_not_mod_or_lib_main() {
        let candidates: Vec<String> = [
            "src/main.rs",
            "src/markdown.rs",
            "src/mod.rs",
            "lib/main.rs",
            "README.md",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let found = fuzzy_filter(&candidates, "src/ma");
        assert_eq!(
            found,
            vec!["src/main.rs", "src/markdown.rs"],
            "src/mod.rs lacks an 'a' at all, and lib/main.rs lacks 'src/' -- neither can match \
             'src/ma' as a subsequence; of the two that DO match, main.rs ranks first for being \
             shorter"
        );
    }

    #[test]
    fn empty_fragment_keeps_every_candidate_shortest_first() {
        let candidates: Vec<String> = ["bb.rs", "a.rs", "ccc.rs"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            fuzzy_filter(&candidates, ""),
            vec!["a.rs", "bb.rs", "ccc.rs"]
        );
    }

    #[test]
    fn a_fragment_that_matches_nothing_yields_no_candidates() {
        let candidates: Vec<String> = vec!["src/main.rs".to_string()];
        assert!(fuzzy_filter(&candidates, "zzz").is_empty());
    }

    // ---- scan_paths: respects .gitignore, and the cap ----

    #[test]
    fn scan_paths_in_a_git_repo_honors_gitignore() {
        let root = tempdir("gitignore");
        write_fixture_tree(&root);
        std::fs::write(root.join("ignored.log"), "noise").unwrap();
        std::fs::write(root.join(".gitignore"), "ignored.log\n").unwrap();

        let init = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["init", "-q"])
            .status()
            .expect("git init must run");
        assert!(init.success());
        // `ls-files --others` only lists untracked files; nothing need be
        // committed for this test -- an untracked, non-ignored file must
        // still show up, and the ignored one must not.

        let scan = scan_paths(&root, DEFAULT_SCAN_CAP);
        assert!(
            scan.paths.contains(&"src/main.rs".to_string()),
            "an ordinary untracked file must be listed: {:?}",
            scan.paths
        );
        assert!(
            !scan.paths.iter().any(|p| p == "ignored.log"),
            ".gitignore'd files must never be listed: {:?}",
            scan.paths
        );
        assert!(!scan.capped);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_paths_outside_a_repo_skips_the_fixed_skip_list() {
        let root = tempdir("no-git-skip-list");
        write_fixture_tree(&root);
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::write(root.join("target/debug/binary"), "noise").unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::write(root.join("node_modules/pkg/index.js"), "noise").unwrap();

        let scan = scan_paths(&root, DEFAULT_SCAN_CAP);
        assert!(scan.paths.contains(&"src/main.rs".to_string()));
        assert!(!scan.paths.iter().any(|p| p.starts_with("target/")));
        assert!(!scan.paths.iter().any(|p| p.starts_with("node_modules/")));
        assert!(!scan.capped);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_paths_caps_the_candidate_count_and_says_so() {
        let root = tempdir("cap");
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..20 {
            std::fs::write(root.join(format!("file-{i:03}.txt")), "x").unwrap();
        }

        let scan = scan_paths(&root, 5);
        assert_eq!(scan.paths.len(), 5);
        assert!(
            scan.capped,
            "a walk that found more than the cap must say so"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    // ---- tab_complete ----

    #[test]
    fn a_single_prefix_match_completes_outright() {
        let candidates = vec!["src/main.rs".to_string(), "README.md".to_string()];
        assert_eq!(
            tab_complete(&candidates, "src/ma"),
            Some("src/main.rs".to_string())
        );
    }

    #[test]
    fn several_prefix_matches_complete_only_to_their_shared_prefix() {
        let candidates = vec!["src/main.rs".to_string(), "src/markdown.rs".to_string()];
        assert_eq!(
            tab_complete(&candidates, "src/m"),
            Some("src/ma".to_string())
        );
    }

    #[test]
    fn no_match_at_all_yields_none() {
        let candidates = vec!["src/main.rs".to_string()];
        assert_eq!(tab_complete(&candidates, "zzz"), None);
    }

    #[test]
    fn an_empty_word_never_completes() {
        let candidates = vec!["src/main.rs".to_string()];
        assert_eq!(tab_complete(&candidates, ""), None);
    }

    // ---- word_before_cursor ----

    #[test]
    fn word_before_cursor_takes_the_contiguous_non_whitespace_span() {
        let input = "read src/ma";
        assert_eq!(
            word_before_cursor(input, input.chars().count()),
            Some((5, "src/ma".to_string()))
        );
    }

    #[test]
    fn word_before_cursor_at_a_space_is_none() {
        assert_eq!(word_before_cursor("read ", 5), None);
    }

    // ---- extract_path_mentions / append_mention_hint ----

    #[test]
    fn extract_path_mentions_finds_every_at_token() {
        assert_eq!(
            extract_path_mentions("look at @src/main.rs and @README.md please"),
            vec!["src/main.rs".to_string(), "README.md".to_string()]
        );
    }

    #[test]
    fn extract_path_mentions_ignores_a_mid_word_at_sign() {
        assert_eq!(
            extract_path_mentions("my email is foo@bar.com"),
            Vec::<String>::new()
        );
    }

    // ---- space-in-path round trip (review finding SIGNIFICANT, round 1)
    // ----

    #[test]
    fn accept_mention_text_quotes_only_when_the_candidate_has_a_space() {
        assert_eq!(accept_mention_text("src/main.rs"), "@src/main.rs ");
        assert_eq!(
            accept_mention_text("docs/release notes.md"),
            "@\"docs/release notes.md\" "
        );
    }

    #[test]
    fn extract_path_mentions_parses_a_quoted_space_containing_mention_whole() {
        assert_eq!(
            extract_path_mentions("please check @\"docs/release notes.md\" for typos"),
            vec!["docs/release notes.md".to_string()]
        );
    }

    #[test]
    fn extract_path_mentions_round_trips_through_accept_mention_text() {
        let candidate = "docs/release notes.md";
        let inserted = accept_mention_text(candidate);
        let text = format!("please check {inserted}for typos");

        assert_eq!(extract_path_mentions(&text), vec![candidate.to_string()]);
    }

    #[test]
    fn extract_path_mentions_handles_a_mix_of_quoted_and_bare_mentions() {
        assert_eq!(
            extract_path_mentions("see @\"my notes.md\" and @src/main.rs too"),
            vec!["my notes.md".to_string(), "src/main.rs".to_string()]
        );
    }

    #[test]
    fn extract_path_mentions_treats_an_unterminated_quote_as_running_to_the_end() {
        assert_eq!(
            extract_path_mentions("look at @\"docs/release notes.md"),
            vec!["docs/release notes.md".to_string()]
        );
    }

    #[test]
    fn append_mention_hint_names_a_space_containing_path_in_full() {
        let inserted = accept_mention_text("docs/release notes.md");
        let text = append_mention_hint(format!("please check {inserted}for typos"));
        assert!(
            text.contains("docs/release notes.md"),
            "the hint must name the WHOLE path, not truncate at the first space: {text}"
        );
    }

    #[test]
    fn append_mention_hint_lists_every_mentioned_path() {
        let text = append_mention_hint("please check @src/main.rs".to_string());
        assert!(text.starts_with("please check @src/main.rs"));
        assert!(text.contains("src/main.rs"));
        assert_ne!(text, "please check @src/main.rs");
    }

    #[test]
    fn append_mention_hint_is_a_no_op_with_no_mentions() {
        assert_eq!(
            append_mention_hint("no mentions here".to_string()),
            "no mentions here"
        );
    }

    #[test]
    fn append_mention_hint_never_fires_for_fork_agent_addressing() {
        let text = append_mention_hint("/fork @some-agent do the thing".to_string());
        assert_eq!(text, "/fork @some-agent do the thing");
    }
}
