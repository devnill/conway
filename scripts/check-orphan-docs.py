#!/usr/bin/env python3
"""Every tracked `.md` file must be reachable from somewhere, or say why not.

WHY THIS EXISTS. Board item 01M0TV5PN8RR9NN97AWP09E6K7 (EMB-1) was filed
because a review reported "no mention of Diplomat, UniFFI, or cbindgen
anywhere in the tree, verified by search this run." False: `BINDINGS.md`
had held exactly that survey for ten days. The mechanism was not carelessness
-- every other mechanically-checkable claim in that review was exact -- it was
that `BINDINGS.md` was linked from nothing: no row in `docs/vision/README.md`,
no reference from any other page. A reviewer walking the documentation graph
cannot see a page nothing points at, no matter how carefully they look.

`CATALOGUE.md` was the same shape, unindexed and cited only from source, and
it carried a claim (the context-path layer being "not yet built") that a
sibling item retired everywhere it could reach except this page, because this
page was outside its ownership and outside anyone's index. An unindexed page
does not get swept, so its claims rot where the next review cannot find them.

WHAT THIS CHECKS, in two parts with different bars, because
`docs/vision/README.md` presents itself as an exhaustive map of its own
directory (a `Page | What it is | Lifetime` table) while nothing else in the
tree makes that promise:

  1. **`docs/vision/*.md` (not `review/`) must have a link row in
     `docs/vision/README.md`.** This is the literal shape of the defect above:
     a page in that directory with no entry in the page that claims to list
     them. `review/` is exempt as a directory -- it already has its own row
     ("Loaded by the reviewer that needs it, not pasted"), and enumerating six
     lens files individually there would just move the same staleness risk
     from "unindexed page" to "unindexed row."
  2. **Every other tracked `.md` file must be referenced from at least one
     OTHER tracked `.md` or source file** (its path or its bare filename
     appearing in another file's text), or be named in `ALLOWLIST` below with
     a stated reason. This is the general form of the same defect: a page
     nothing points at and nothing loads is invisible to a reader and to a
     future sweep alike.

WHAT COUNTS AS A REFERENCE, and why it is loose on purpose. This does not
parse markdown links or resolve relative paths -- it is a substring search for
the file's repo-relative path or its bare filename in every OTHER tracked
file's text. That is deliberately weaker than a real link-graph walk: this
codebase's own convention favours plain backtick code spans
(`` `CATALOGUE.md` ``) over `[text](path)` hyperlinks for exactly the fragile-
link reasons `cargo doc`'s intra-doc-link gate exists for source comments, so a
check that required a real markdown link would miss most of this tree's actual
cross-references. The substring test catches the one failure mode this item
was filed over -- a page mentioned NOWHERE, by ANYTHING -- and nothing finer.
A page linked once, sloppily, is not what broke; a page linked zero times is.

WHAT THIS DOES NOT CATCH, stated because a gate whose blind spots are unstated
is the same defect one level up:

  * **A reference that is itself stale or wrong.** This proves a string
    exists somewhere, not that the sentence around it is true, current, or
    even about the same file (a same-named file in a different directory
    satisfies the bare-filename search). `check-design-claims.py` and
    `check-board-citations.py` are the tools for claim freshness and citation
    resolution; this one is only for reachability.
  * **A page reachable only through a chain that itself starts nowhere.** If
    A references B and B references C, but nothing references A, this check
    passes B and C and fails only A -- it does not walk the graph transitively
    from a declared set of roots. `docs/vision/README.md`'s own table (part 1,
    above) is what keeps that directory's roots honest instead.
  * **Data files loaded by code, not read as prose**
    (`crates/*/tests/fixtures/**`, `crates/conway-plugin-*/fragments/*.md`)
    are correctly unlinked from any `.md` page; ALLOWLIST documents each
    directory rather than silently passing them by coincidence of being
    referenced from the `.rs` loader.

Usage:  python3 scripts/check-orphan-docs.py [--verbose]
Exit:   0 clean | 1 violations found
"""

from __future__ import annotations

import argparse
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent

# Source suffixes scanned, in addition to every tracked `.md`, when looking
# for a reference to a doc page. `.yml` covers `.github/workflows/ci.yml`
# referencing a script or doc by path.
SOURCE_SUFFIXES = (".rs", ".toml", ".py", ".sh", ".yml", ".yaml")

VISION_DIR = "docs/vision"
VISION_README = f"{VISION_DIR}/README.md"

# `(path, reason)`: a tracked `.md` file that is expected to have zero
# references from other prose, because it is data loaded by code rather than
# documentation read by a person or a reviewer. Each entry needs a reason;
# an unexplained one would hide a real orphan behind a rubber stamp.
ALLOWLIST: dict[str, str] = {
    "crates/conway-plugin-discover/fragments/when_to_search.md":
        "a plugin prompt fragment, loaded as data by conway-plugin-discover, "
        "not read as prose",
    "crates/conway-plugin-path/fragments/when_to_compose.md":
        "a plugin prompt fragment, loaded as data by conway-plugin-path, "
        "not read as prose",
}
_FIXTURE_PREFIX = "crates/conway/tests/fixtures/agents/"
ALLOWLIST_PREFIXES: dict[str, str] = {
    _FIXTURE_PREFIX:
        "agent-definition test fixtures, loaded by load_agent_defs in tests, "
        "not read as prose",
}


def tracked(patterns: list[str]) -> list[str]:
    out = subprocess.run(
        ["git", "ls-files", *patterns], cwd=ROOT, capture_output=True, text=True, check=True
    ).stdout
    return [line for line in out.split("\n") if line]


def is_allowlisted(rel: str) -> str | None:
    if rel in ALLOWLIST:
        return ALLOWLIST[rel]
    for prefix, reason in ALLOWLIST_PREFIXES.items():
        if rel.startswith(prefix):
            return reason
    return None


def markdown_link_targets(text: str) -> set[str]:
    """Every `(...)` target of a `[...](...)` markdown link in `text`."""
    return {m.group(1) for m in re.finditer(r"\]\(([^)\s]+)\)", text)}


def check_vision_index(md_files: list[str], contents: dict[str, str]) -> list[str]:
    """Part 1: every `docs/vision/*.md` (not `review/`) needs a row in
    `docs/vision/README.md` -- an actual link target, not just a mention."""
    readme_text = contents.get(VISION_README, "")
    if not readme_text:
        return [f"{VISION_README} is missing or empty -- cannot check the vision index"]
    link_targets = markdown_link_targets(readme_text)
    # A link target may be a bare filename (`CATALOGUE.md`) or carry the
    # directory (`docs/vision/CATALOGUE.md`) -- accept either.
    linked_names = {t.rsplit("/", 1)[-1] for t in link_targets}

    problems = []
    for rel in md_files:
        if not rel.startswith(VISION_DIR + "/"):
            continue
        remainder = rel[len(VISION_DIR) + 1:]
        if "/" in remainder:
            continue  # inside review/ (or any future subdirectory) -- exempt, see module doc
        if remainder == "README.md":
            continue  # the index does not need to index itself
        if remainder not in linked_names:
            problems.append(
                f"{rel}: no link row in {VISION_README} "
                f"(part 1 of this check -- that page presents itself as an "
                f"exhaustive index of its own directory)"
            )
    return problems


def check_general_reachability(md_files: list[str], contents: dict[str, str]) -> list[str]:
    """Part 2: every tracked `.md` outside `docs/vision/`'s top level, plus
    everything under `docs/vision/review/`, must be referenced by path or
    bare filename from some OTHER tracked `.md` or source file."""
    problems = []
    for rel in md_files:
        if rel.startswith(VISION_DIR + "/") and "/" not in rel[len(VISION_DIR) + 1:]:
            continue  # covered by part 1 instead
        reason = is_allowlisted(rel)
        if reason is not None:
            continue
        base = rel.rsplit("/", 1)[-1]
        found = any(
            other != rel and (rel in text or base in text)
            for other, text in contents.items()
        )
        if not found:
            problems.append(
                f"{rel}: referenced by nothing (part 2 of this check -- "
                f"neither its path nor its filename appears in any other "
                f"tracked .md or source file). Add a link, or add it to "
                f"ALLOWLIST in scripts/check-orphan-docs.py with a reason."
            )
    return problems


def scan(verbose: bool) -> int:
    md_files = tracked(["*.md"])
    src_files = tracked([f"*{suf}" for suf in SOURCE_SUFFIXES])

    contents: dict[str, str] = {}
    for rel in md_files + src_files:
        contents[rel] = (ROOT / rel).read_text(errors="ignore")

    problems = check_vision_index(md_files, contents) + check_general_reachability(
        md_files, contents
    )

    if verbose:
        print(f"scanned {len(md_files)} tracked .md files, {len(src_files)} source files")

    for p in sorted(problems):
        print(p)

    print(f"\n{len(problems)} orphaned/unindexed doc(s) found across {len(md_files)} tracked .md files")
    return 1 if problems else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args()
    return scan(args.verbose)


if __name__ == "__main__":
    sys.exit(main())
