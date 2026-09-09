# `.changelog.d/` — one CHANGELOG entry per writer

A writer landing a user-facing change creates one file here, never edits
`CHANGELOG.md` directly. A build lane runs `scripts/collect-changelog.py` to
fold every pending fragment into `CHANGELOG.md`'s `## [Unreleased]` section at
a wave's gate, then deletes the fragments it consumed.

## Format

Name the file `<slug>.md`, `<slug>` derived from your work item's id or a
short kebab-case name unique to that item — this is what lets two concurrent
writers, each in their own git worktree, never touch the same lines. The
file's content is a `### <Subsection>` heading (`Added`, `Changed`, `Fixed`,
or `Removed` — no others) followed by one or more `- ` bullets, the same
bullets that would otherwise have gone at the top of that subsection. A
fragment may carry more than one heading if a single item both adds and fixes
something:

```markdown
### Added

- **A short bold title** — the same prose, board item id, and detail level
  `CHANGELOG.md`'s existing entries already use. One bullet per line; this
  mechanism does not support a bullet wrapping onto a continuation line.

### Fixed

- **Another entry**, if this same change also fixed something.
```

## Before you commit

Run `python3 scripts/check-changelog-fragments.py` (also wired into
`scripts/check-fast-gates.sh`) — it rejects a fragment naming an unknown
subsection, an empty fragment, one with no recognizable heading, or a
filename that collides with an entry already folded into `CHANGELOG.md`.

Never run `scripts/collect-changelog.py` yourself as part of landing your own
change — leave your fragment in place; the build lane collects the whole
batch at the wave's gate, not each writer individually.
