#!/usr/bin/env python3
"""Resolve every board-item id cited in the tree against the ideate stores.

WHY THIS EXISTS. The tree cites board items by ULID in ~980 places. A citation
that names a *closed* item in a pending-work sense is a dangling promise: a
reader who follows "tracked under 01K..." finds finished work rather than the
open question the sentence implied. A human reader found
that class by accident three times before this check existed.

WHY IT IS NOT A CI GATE, stated plainly because a check whose limits are
unstated is the defect one level up. Both stores it resolves against are
local-only and excluded by .gitignore:

  * the work board   -- .ideate-work/board.db  (sqlite, `items` table)
  * the record store -- .ideate/record/YYYY/MM/<ULID>.md  (decisions and more)

CI has neither, and committing board state was declined (the same operator
direction that declined vendoring the steering store). So this runs on a
maintainer checkout. When the stores are ABSENT it exits 2 and says SKIPPED --
it never exits 0 on a run that verified nothing, because "green because it
checked nothing" is exactly the failure this repository keeps finding.

WHAT IT FLAGS, and the distinction that keeps it usable. A citation naming a
`done` item is NOT automatically wrong: "implemented by 01K..." is legitimate
and useful provenance, and most of the ~980 are exactly that. The defect is a
citation that implies PENDING work -- "tracked under", "deferred to", "01K...
tracks that" -- while naming something `done` or `cancelled`. A checker that
flagged every done-item citation would report hundreds of false positives and
be switched off within a week, so the pending-sense cue must sit next to the
citation and govern it, not merely appear in the same paragraph.

A GAP-DISCLOSURE VARIANT of the same defect: prose that says a mechanism is
*unbuilt* rather than *scheduled* -- "not yet built", "not yet wired", "has no
consumer", "deliberately unbuilt", "remains a separate, larger piece" -- is the
same dangling-promise shape as "tracked under" once it names an id, and it is
the phrasing this tree's authors actually reach for (permission-modes.md and
statusline.md each disclosed an unbuilt mechanism this way with no id in
reach at all, and both were built the same review window under a
differently-named item -- nothing existed to notice the sentence go stale).
`PENDING_BEFORE`/`PENDING_AFTER` recognise this family too, bound exactly as
tightly as every other pending-sense phrase: the gap phrase and the id must
sit within the same short, punctuation-only span, not merely the same
sentence or paragraph. "Not yet built (01K...)" binds; "not yet built. See
the tracker for details, e.g. 01K..." does not, on purpose -- the loose form
is exactly the ordinary-prose shape ("not yet" with no citation in reach)
this check must not fire on.

TWO ID NAMESPACES. Work items and record entries (decisions among them) are
separate stores sharing one id shape. docs/plugins/hooks.md cites two record
entries as *decisions*; both resolve in the record store and return nothing
from the work board. A resolver
that checked only the board would call both dangling and be wrong.

A SECOND, UNRELATED CLASS THIS ALSO CHECKS: steering-shorthand leaking onto a
user-facing page. `GP-03`, `P-2`, `C-04` and similar are internal
governance ids from `.ideate/steering/`, a directory `.gitignore` excludes on
purpose (the same operator direction that declined vendoring the board).
`docs/plugins/authoring.md` is read by a third-party plugin author who will
never have that directory; a bare `GP-03` resolves for nobody but a
maintainer. This check runs unconditionally (no store needed, unlike the ULID
check above) over `docs/` plus the five root pages
measured (`README.md`, `ARCHITECTURE.md`, `PHILOSOPHY.md`, `CONTRIBUTING.md`,
`CHANGELOG.md`) and fails on any match. It does NOT cover the `T-`/`V-`/`F-`/
`R-` id families `docs/plugins/hooks.md` still quotes (e.g. `F12`, `T7`) --
those are historical labels from an item's own original spec text, always
paired inline with the real, resolvable board ULID that superseded them,
which is a different (legitimate) citation shape than a bare, untranslated
`GP-*`/`P-*`/`C-*` reference standing alone.

A THIRD CLASS, decided by board item `01M18Q8YWWQC6CNQSVCENGFC9B`: bare
steering shorthand in PUBLIC rustdoc. `crates/*/src/` used to be disclaimed
here entirely, on the reasoning that it had "its own regression guard for
invariant S0c, deliberately kept a separate item" -- no such guard or item
ever existed, and the reasoning did not survive being checked: a `///` doc
comment on a `pub` item is exactly as unresolvable to a third-party plugin
author with no `.ideate/steering/` as a bare id on a `.md` page is, and
`conway-plugin-ui/src/lib.rs` shipped exactly one. `scan_rustdoc_shorthand`
now checks every library crate's `src/` (every workspace member except
`conway-cli`, the one crate with no `[lib]` target for anyone to run `cargo
doc` against) for a `///`/`//!` comment that is actually part of that
crate's default, no-flags `cargo doc` output -- see that function's own doc
for exactly what counts and what does not (test code, `pub(crate)` items,
and orphaned private modules with no re-export are all excluded on purpose).

WHAT THIS DOES NOT CHECK, stated because a gate whose limits are unstated is
the defect one level up:

  * **Citation resolution (the ULID half) needs a maintainer checkout.** CI
    has no `.ideate-work/board.db`/`.ideate/record`, so it cannot notice a
    dangling `tracked under 01K...` citation -- only the steering-shorthand
    half runs there. Run on a checkout with both stores present before
    trusting a citation's pending/done status.
  * **The pending-sense phrase list (`PENDING_BEFORE`/`PENDING_AFTER`) is a
    fixed vocabulary, not language understanding.** It covers two phrase
    families -- "will happen" (tracked under, deferred to, blocked on, ...)
    and "hasn't happened" (not yet built, not yet wired, has no consumer,
    deliberately unbuilt, remains a separate larger piece) -- each bound
    tightly to a nearby id. A citation phrased a way neither regex
    anticipates is invisible to this check even naming a `done` item in a
    clearly pending sense, and a gap disclosure with no id within reach at
    all (the shape that motivated the second family) is invisible by
    construction: there is nothing to resolve.
  * **`crates/conway-cli/src/` is not scanned for rustdoc shorthand.** It is
    a bin-only crate (see "A THIRD CLASS" above) -- a bare id in one of its
    doc comments is still a defect, just not one this class of check covers;
    it would need the steering-shorthand-on-a-user-facing-page treatment
    instead, and `conway-cli` writes no public-facing `.md` of its own.
  * **The rustdoc-public-reachability model is a real model, not a general
    one.** `_module_is_public`/`_items_are_public` handle plain `pub mod`
    chains and `pub use` (named or `*`) re-exports of a private module's
    items -- every shape this tree's own crates use today -- but not a
    renamed re-export (`pub use foo::Bar as Baz;`) or `#[doc(hidden)]`/
    `#[doc(inline)]` attributes.
  * **`T-`/`V-`/`F-`/`R-` id families are not steering shorthand here.**
    `docs/plugins/hooks.md` still quotes some (`F12`, `T7`) as historical
    labels from an item's own original spec text, always paired inline with
    the real board ULID that superseded them -- a legitimate citation shape
    this check does not flag, distinct from a bare, untranslated `GP-*`/
    `P-*`/`C-*` reference standing alone.
  * **A false PRESENT-TENSE capability claim with no `GP-*`/`P-*`/`C-*` id at
    all is invisible to this script.** That class (a page asserting a
    default build does something only a plugin adds) is guarded instance by
    instance as a `scripts/board-claims.md` regression predicate, not
    mechanically detected here -- see `check-design-claims.py`'s own header.

Usage:  python3 scripts/check-board-citations.py [--verbose]
Exit:   0 clean | 1 violations found (either class)
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sqlite3
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
BOARD_DB = ROOT / ".ideate-work" / "board.db"
RECORD_DIR = ROOT / ".ideate" / "record"

SCAN_DIRS = ["crates", "docs"]
SCAN_SUFFIXES = {".rs", ".md", ".toml"}

ULID = re.compile(r"\b01[0-9A-HJKMNP-TV-Z]{24}\b")
QUOTED = re.compile(r'"[^"]*"')

# A citation carried in a structured field rather than prose, e.g.
# `board_item: "01K..."`. Scanned in addition to comment lines; the quoted-span
# mask below is skipped for these, since the id IS the quoted value here.
CITATION_FIELD = re.compile(
    r'\b(?:board_item|board|item|tracked_by|decision)\s*:\s*"(01[0-9A-HJKMNP-TV-Z]{24})"'
)

# A ULID citation immediately governed by a pending-work phrase. The phrase must
# sit within a short span of the id -- not merely somewhere in the paragraph.
_ID = r'(?:board\s+item\s+)?[`"(\[]*\b(01[0-9A-HJKMNP-TV-Z]{24})\b'
PENDING_BEFORE = re.compile(
    r"\b(?:tracked\s+(?:under|by|in)|deferred\s+(?:to|into)|blocked\s+on"
    r"|see\s+(?:the\s+)?open\s+item|pending\s+(?:in|under)|filed\s+as\s+the\s+open"
    r"|awaiting|owned\s+by\s+the\s+open"
    r"|will\s+be\s+(?:done|built|wired|closed|added)\s+(?:under|in|by)"
    # Gap-disclosure family: the mechanism is stated UNBUILT rather than
    # scheduled. Same tight \W{0,24} binding as every phrase above -- this
    # matches "not yet built (01K...)" and not "not yet built. Tracked
    # somewhere, e.g. 01K...", which is the ordinary-prose shape this must
    # tolerate. See CONTRIBUTING.md's "Citing a board item" section.
    r"|not\s+yet\s+built|not\s+yet\s+wired|has\s+no\s+consumer"
    r"|deliberately\s+unbuilt|remains\s+a\s+separate,?\s+larger\s+piece)"
    r"\W{0,24}" + _ID,
    re.IGNORECASE)
PENDING_AFTER = re.compile(
    _ID + r'[`")\]]*\s*(?:\([^)]{0,60}\)\s*)?'
    r"(?:tracks\b|is\s+tracking\b|remains?\s+open\b|is\s+still\s+open\b"
    r"|will\s+(?:wire|build|close|land|add|resolve|fix|track)\b"
    # Gap-disclosure family, id-first order: "01K... is not yet built".
    r"|is\s+not\s+yet\s+built\b|is\s+not\s+yet\s+wired\b|has\s+no\s+consumer\b"
    r"|is\s+deliberately\s+unbuilt\b|remains\s+a\s+separate,?\s+larger\s+piece\b)",
    re.IGNORECASE)

# ULIDs that appear in a comment or in prose but are NOT board citations. Each
# needs a reason; an unexplained entry here would hide a real dangling id.
ALLOWLIST = {
    "01ARZ3NDEKTSV4RRFFQ69G5FAV": "the ULID spec's own well-known example id, used as test data",
    "01KZWT0ET8E669YPNQWXB3GQZA": "a conway *session* id in a recorded walkthrough transcript, not a board item",
}

# Internal governance shorthand from `.ideate/steering/` -- unresolvable for
# anyone without that gitignored directory. See the module doc's "A SECOND,
# UNRELATED CLASS" section, and "A THIRD CLASS" below for the `crates/*/src/`
# half this used to disclaim and now checks.
STEERING_SHORTHAND = re.compile(r"\b(GP-[0-9]+|P-[0-9]+|C-[0-9]+)\b")

# --- A THIRD CLASS: bare steering shorthand in PUBLIC rustdoc ---------------
#
# Board item 01M18Q8YWWQC6CNQSVCENGFC9B decided this. `conway-plugin-ui/src/
# lib.rs` carried a bare `P-10:` citation on a `pub` struct field's own `///`
# doc comment -- read, via `cargo doc` or by opening the file, by exactly the
# third-party plugin author `docs/plugins/authoring.md` is written for, who
# has no more access to `.ideate/steering/` than a reader of a `.md` page
# does. The two are the same hazard; a bare id resolving to nothing for a
# reader with only the checkout does not stop being that hazard because the
# comment sits in a `.rs` file instead of a `.md` one.
#
# SCOPE, decided narrowly on purpose:
#   * Only library crates -- `crates/*/src/`, EXCLUDING `conway-cli`. It is
#     the one workspace member with no `[lib]` target (`[[bin]] name =
#     "conway"` only); nobody runs `cargo doc` against a bin-only crate the
#     way a dependent runs it against a library they depend on, and it ships
#     no extension-point trait a plugin author implements. `conway-core`
#     (the `Plugin` trait itself), `conway` (the embedder-facing facade),
#     `conway-runtime`, `conway-tools`, and every `conway-plugin-*` crate
#     (the worked reference implementations `docs/plugins/authoring.md`
#     itself points a plugin author at) are all in scope.
#   * Only a doc comment (`///`/`//!`) attached to something that is actually
#     part of that crate's default, no-flags `cargo doc` output -- an item
#     declared plain `pub` (never `pub(crate)`/`pub(super)`/`pub(in ...)`)
#     whose enclosing module chain is publicly reachable from the crate
#     root, OR whose module is private but its items are re-exported by a
#     `pub use` (named or `*`) an ancestor on a public path carries -- the
#     `mod path_store;` + `pub use path_store::*;` shape `conway-core::
#     ports` uses throughout is exactly this, and its own `//!` module doc
#     does NOT count (the module itself has no public path), while a `pub`
#     item's own `///` inside it does (the item does, via the re-export).
#     This is `module_is_public`/`items_are_public` below -- a real, if
#     partial, model of what `cargo doc` without `--document-private-items`
#     actually shows, not a proxy for "starts with the word pub".
#   * Test code is excluded (`#[cfg(test)]` blocks, tracked by brace depth)
#     -- a fixture asserting against a literal `"P-10"` string, or a comment
#     inside a `#[cfg(test)] mod tests`, is neither read as API documentation
#     nor rendered by a normal `cargo doc` run.
#   * A PLAIN `//`/`/* */` comment, public item or not, is never a rustdoc
#     comment at all and is never scanned -- only `///`/`//!` lines.
#
# KNOWN LIMITS, stated because an unstated one is the defect one level up:
# renamed re-exports (`pub use foo::Bar as Baz;`) and `#[doc(inline)]`/
# `#[doc(hidden)]` attributes are not modelled -- `items_are_public` treats
# any `pub use <name>::...` naming the right module as re-exporting it,
# which is right for every shape this tree actually uses (checked against
# all 44 real hits found the day this was written) but is not a general
# rustdoc-visibility engine.

_MOD_DECL = re.compile(r"^\s*(pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z0-9_]+)\s*;")
_PUB_USE = re.compile(r"^\s*pub\s+use\s+(?:crate::|self::)*([A-Za-z0-9_]+)::")
_BARE_PUB_ITEM = re.compile(r"^\s*pub(\s|\()")
_RUSTDOC_LINE = re.compile(r"^\s*(///|//!)")
_CFG_TEST = re.compile(r"^\s*#\[cfg\(.*test.*\)\]")

_module_pub_cache: dict[pathlib.Path, bool] = {}
_items_pub_cache: dict[pathlib.Path, bool] = {}


def _is_bare_pub(visibility: str | None) -> bool:
    """`True` for `pub`, `False` for `pub(crate)`/`pub(super)`/`pub(self)`/
    `pub(in ...)`/no visibility at all (private)."""
    if visibility is None:
        return False
    v = visibility.strip()
    return v == "pub"


def _find_declaration(rs_files: list[pathlib.Path], file: pathlib.Path, crate_src: pathlib.Path):
    """`"ROOT"` if `file` is the crate's `src/lib.rs`; else `(parent_file,
    declared_pub, modname)` for the `mod <modname>;` line that brings `file`
    into the module tree, or `None` if no declaration resolves to it (an
    unreachable/orphaned file, or a build-script-only module this scan
    cannot see)."""
    if file == crate_src / "lib.rs":
        return "ROOT"
    stem = file.parent.name if file.name == "mod.rs" else file.stem
    for cand_parent in rs_files:
        if cand_parent == file:
            continue
        for line in cand_parent.read_text(errors="ignore").split("\n"):
            m = _MOD_DECL.match(line)
            if not m or m.group(2) != stem:
                continue
            d = cand_parent.parent
            parent_stem = cand_parent.stem
            candidate_dir = d if parent_stem in ("lib", "mod") else d / parent_stem
            resolved = next(
                (
                    c
                    for c in (
                        d / f"{stem}.rs",
                        candidate_dir / f"{stem}.rs",
                        candidate_dir / stem / "mod.rs",
                        d / stem / "mod.rs",
                    )
                    if c.exists()
                ),
                None,
            )
            if resolved == file:
                return cand_parent, _is_bare_pub(m.group(1)), stem
    return None


def _module_is_public(rs_files: list[pathlib.Path], file: pathlib.Path, crate_src: pathlib.Path) -> bool:
    """`True` if `file`'s own `//!` module doc renders in a plain `cargo
    doc` run -- the module itself sits on an all-`pub mod` path from the
    crate root."""
    if file in _module_pub_cache:
        return _module_pub_cache[file]
    decl = _find_declaration(rs_files, file, crate_src)
    result = (
        True
        if decl == "ROOT"
        else False
        if decl is None
        else decl[1] and _module_is_public(rs_files, decl[0], crate_src)
    )
    _module_pub_cache[file] = result
    return result


def _items_are_public(rs_files: list[pathlib.Path], file: pathlib.Path, crate_src: pathlib.Path) -> bool:
    """`True` if a bare-`pub` item declared in `file` renders in a plain
    `cargo doc` run -- either the module itself is public, or a `pub use`
    on a publicly-reachable ancestor re-exports this module's items."""
    if file in _items_pub_cache:
        return _items_pub_cache[file]
    if _module_is_public(rs_files, file, crate_src):
        _items_pub_cache[file] = True
        return True
    decl = _find_declaration(rs_files, file, crate_src)
    if decl in (None, "ROOT"):
        result = decl == "ROOT"
    else:
        parent_file, _declared_pub, modname = decl
        reexported = any(
            m.group(1) == modname
            for line in parent_file.read_text(errors="ignore").split("\n")
            for m in [_PUB_USE.match(line)]
            if m
        )
        result = reexported and _items_are_public(rs_files, parent_file, crate_src)
    _items_pub_cache[file] = result
    return result


def scan_rustdoc_shorthand() -> list[tuple[str, int, str, str]]:
    """`(rel_path, lineno, id, line)` for every bare GP-*/P-*/C-* citation in
    a PUBLIC rustdoc comment -- see "A THIRD CLASS" above for exactly what
    counts. Needs neither store, like [`scan_steering_shorthand`]."""
    hits: list[tuple[str, int, str, str]] = []
    for crate_dir in sorted((ROOT / "crates").iterdir()):
        if not crate_dir.is_dir() or crate_dir.name == "conway-cli":
            continue
        crate_src = crate_dir / "src"
        if not crate_src.is_dir():
            continue
        rs_files = sorted(crate_src.rglob("*.rs"))
        for path in rs_files:
            lines = path.read_text(errors="ignore").split("\n")
            cfg_test_depth = None
            pending_cfg_test = False
            depth = 0
            for i, line in enumerate(lines):
                stripped = line.strip()
                if _CFG_TEST.match(stripped):
                    pending_cfg_test = True
                opens, closes = line.count("{"), line.count("}")
                in_test = cfg_test_depth is not None and depth >= cfg_test_depth
                if pending_cfg_test and opens > 0 and cfg_test_depth is None:
                    cfg_test_depth = depth + 1
                    pending_cfg_test = False
                depth += opens - closes
                if cfg_test_depth is not None and depth < cfg_test_depth:
                    cfg_test_depth = None
                if in_test or not _RUSTDOC_LINE.match(line):
                    continue
                masked = QUOTED.sub(lambda m: " " * len(m.group()), line)
                m = STEERING_SHORTHAND.search(masked)
                if not m:
                    continue
                if stripped.startswith("//!"):
                    is_public = _module_is_public(rs_files, path, crate_src)
                else:
                    j = i + 1
                    while j < len(lines) and (
                        lines[j].strip().startswith("///")
                        or lines[j].strip().startswith("#[")
                        or lines[j].strip() == ""
                    ):
                        j += 1
                    decl = lines[j] if j < len(lines) else ""
                    is_public = bool(_BARE_PUB_ITEM.match(decl)) and not re.match(
                        r"^\s*pub\((crate|super|self|in\s)", decl.strip()
                    ) and _items_are_public(rs_files, path, crate_src)
                if is_public:
                    rel = path.relative_to(ROOT).as_posix()
                    hits.append((rel, i + 1, m.group(1), stripped[:140]))
    return hits

# The exact surface measured: docs/ plus
# the five root pages a reader outside this repo actually lands on.
USER_FACING_ROOT_MD = ("README.md", "ARCHITECTURE.md", "PHILOSOPHY.md", "CONTRIBUTING.md", "CHANGELOG.md")


def load_stores() -> tuple[dict[str, str], set[str]] | None:
    if not BOARD_DB.exists() or not RECORD_DIR.is_dir():
        return None
    con = sqlite3.connect(f"file:{BOARD_DB}?mode=ro", uri=True)
    try:
        board = {row[0]: row[1] for row in con.execute("select id, status from items")}
    finally:
        con.close()
    record = {p.stem for p in RECORD_DIR.rglob("*.md")}
    return board, record


def citation_lines(path: pathlib.Path):
    """Yield (lineno, raw_line) for lines that can carry a citation.

    Rust: comment lines only. Markdown: prose outside fenced code blocks. This
    is what separates a citation from a ULID *literal* -- a test fixture id, a
    JSON example in docs, a session id in a log path. Quoted spans are masked
    on top of that, so an id inside a string on a comment line is skipped too.
    """
    fenced = False
    for lineno, line in enumerate(path.read_text(errors="ignore").split("\n"), 1):
        if path.suffix == ".md":
            if line.lstrip().startswith("```"):
                fenced = not fenced
                continue
            if fenced:
                continue
        else:
            # Comment lines, plus structured citation FIELDS. The second half
            # matters: `enum_variant_construction_guard.rs` carries its
            # citations in a `board_item: "01K..."` struct field, and a
            # comment-only scan misses them silently -- the exact "a regex
            # anchored to one context misses the others" hazard this check
            # was asked to avoid.
            if not (line.lstrip().startswith("//") or CITATION_FIELD.search(line)):
                continue
        yield lineno, line


def scan_steering_shorthand() -> list[tuple[str, int, str, str]]:
    """`(rel_path, lineno, id, line)` for every bare GP-*/P-*/C-* citation on a
    user-facing page. Needs neither store -- this is the half of the module
    that DOES gate CI, unlike the ULID-resolution half below.
    """
    files = sorted(
        [p for p in (ROOT / "docs").rglob("*.md") if p.is_file()]
        + [ROOT / f for f in USER_FACING_ROOT_MD if (ROOT / f).exists()]
    )
    hits: list[tuple[str, int, str, str]] = []
    for path in files:
        rel = path.relative_to(ROOT).as_posix()
        for lineno, line in citation_lines(path):
            masked = QUOTED.sub(lambda m: " " * len(m.group()), line)
            for m in STEERING_SHORTHAND.finditer(masked):
                hits.append((rel, lineno, m.group(1), line.strip()[:140]))
    return hits


def scan():
    shorthand = scan_steering_shorthand()
    for rel, lineno, ident, ctx in shorthand:
        print(f"{rel}:{lineno}: STEERING-SHORTHAND citation on a user-facing page: {ident}")
        print(f"    {ctx}")
    if shorthand:
        print(
            f"\n{len(shorthand)} steering-shorthand citation(s) found -- unresolvable "
            f"for a reader without .ideate/steering/. Reference the concept instead "
            f"of the bare steering id."
        )

    rustdoc_shorthand = scan_rustdoc_shorthand()
    for rel, lineno, ident, ctx in rustdoc_shorthand:
        print(f"{rel}:{lineno}: STEERING-SHORTHAND citation in PUBLIC rustdoc: {ident}")
        print(f"    {ctx}")
    if rustdoc_shorthand:
        print(
            f"\n{len(rustdoc_shorthand)} steering-shorthand citation(s) found in public "
            f"rustdoc -- unresolvable for a plugin author reading `cargo doc` or the "
            f"source with no .ideate/steering/. Reference the concept instead of the "
            f"bare steering id."
        )

    stores = load_stores()
    if stores is None:
        print(
            "\nCITATION-RESOLUTION HALF SKIPPED: the ideate stores are not present "
            "in this checkout."
        )
        print(f"  expected {BOARD_DB.relative_to(ROOT)} and {RECORD_DIR.relative_to(ROOT)}")
        print("  Both are .gitignore'd local tooling state, so CI and a plain clone")
        print("  cannot run it -- unlike the steering-shorthand check above, which just")
        print("  ran regardless. Run this on a maintainer checkout before trusting a")
        print("  board_item citation's pending/done status.")
        return 1 if (shorthand or rustdoc_shorthand) else 0
    board, record = stores

    unknown: list[tuple[str, int, str, str]] = []
    stale: list[tuple[str, int, str, str, str]] = []
    cited = 0

    def classify(ident: str) -> str | None:
        """`done`/`cancelled`/`open`/... for a board item, `record` for a
        record-store entry (a decision among others), None for neither.

        A pending-sense citation naming a `record` id is a defect in its own
        right: a decision is a settled ruling, so nothing can be "tracked
        under" one. That case is how `LogRecord::ContextMask`'s doc came to
        say "Tracked by board item <id>" while that id
        was a decision that had since been overtaken.
        """
        if ident in board:
            return board[ident]
        return "record" if ident in record else None

    files = [
        p
        for d in SCAN_DIRS
        for p in (ROOT / d).rglob("*")
        if p.is_file() and p.suffix in SCAN_SUFFIXES
    ] + [p for p in ROOT.glob("*.md")]

    for path in sorted(files):
        rel = path.relative_to(ROOT).as_posix()
        lines = list(citation_lines(path))
        for idx, (lineno, line) in enumerate(lines):
            masked = QUOTED.sub(lambda m: " " * len(m.group()), line)
            found = {m.group() for m in ULID.finditer(masked)}
            found |= {m.group(1) for m in CITATION_FIELD.finditer(line)}
            for ident in sorted(found):
                if ident in ALLOWLIST:
                    continue
                cited += 1
                if classify(ident) is None:
                    unknown.append((rel, lineno, ident, line.strip()[:120]))

            # Pending-sense detection runs over this line joined with the next,
            # so a citation wrapped across two comment lines is still seen.
            nxt = lines[idx + 1][1] if idx + 1 < len(lines) else ""
            window = " ".join(x.lstrip(" /!*#|->").strip() for x in (line, nxt))
            for rx in (PENDING_BEFORE, PENDING_AFTER):
                for hit in rx.finditer(window):
                    ident = hit.group(1)
                    if ident in ALLOWLIST:
                        continue
                    status = classify(ident)
                    if status in ("done", "cancelled", "record"):
                        stale.append((rel, lineno, ident, status, window.strip()[:140]))

    seen = set()
    stale = [s for s in stale if not (s[:3] in seen or seen.add(s[:3]))]

    for rel, lineno, ident, status, ctx in stale:
        what = (
            "a RECORD entry (a decision, not trackable work)"
            if status == "record"
            else f"a {status.upper()} item"
        )
        print(f"{rel}:{lineno}: PENDING-SENSE citation names {what}: {ident}")
        print(f"    {ctx}")
    for rel, lineno, ident, ctx in unknown:
        print(f"{rel}:{lineno}: UNKNOWN id {ident} resolves in neither store")
        print(f"    {ctx}")

    total = len(stale) + len(unknown) + len(shorthand) + len(rustdoc_shorthand)
    print(
        f"\nchecked {cited} citations across {len(files)} files "
        f"({len(board)} board items, {len(record)} record entries): "
        f"{len(stale)} stale, {len(unknown)} unknown, "
        f"{len(shorthand)} steering-shorthand, "
        f"{len(rustdoc_shorthand)} rustdoc-shorthand"
    )
    return 1 if total else 0


def main() -> int:
    argparse.ArgumentParser(description=__doc__).parse_args()
    return scan()


if __name__ == "__main__":
    sys.exit(main())
