Standing instructions for an agent working in this repository.

- **Verify with tests, not description.** Run `cargo test -p <crate>` for
  every crate you touched (e.g. `cargo test -p conway-plugin-idiom`,
  `cargo test -p conway-cli`) before calling a change done.
- **Doc-comment changes need the doc gate, not just a build.**
  `scripts/check-fast-gates.sh` runs `RUSTDOCFLAGS="-D warnings" cargo doc
  --workspace --no-deps --all-features`, which catches a broken or
  private intra-doc link that `cargo build`/`cargo test` never touch.
- **If `bash` is not in this session's tool set, do not claim
  verification.** Report the change as unverified, listing every file you
  touched by path, instead of describing a command you could not run.
- **Commits carry no AI-attribution trailer.** No `Co-Authored-By: Claude
  ...`, no session/transcript link, no "Generated with" line — in a
  commit message, a PR body, or a `CHANGELOG.md` entry. See `CLAUDE.md`.
- **Declaration honesty.** Changing a mechanism means updating every doc
  comment, `docs/` page, and `CHANGELOG.md` entry that describes it, in
  the same change (`CONTRIBUTING.md` §2). A capability that ships ahead
  of its consumer is fine; label it a forward declaration explicitly, at
  every site that mentions it, rather than describing it as reached.
- **Config defaults live in two places — name both when you change one.**
  the `impl Default` blocks in `crates/conway/src/config/schema.rs`, and
  `default_document` in `crates/conway/src/config/merge.rs`.
- **The doc split.** `PHILOSOPHY.md` states the intended 1.0 shape
  (present tense, even for what is not yet built, checked by
  `scripts/check-design-claims.py`); `docs/` describes what conway does
  now; open gaps belong on the board, not silently in either doc.
- **Never edit a file outside this repository's working tree.** Not
  `~/.conway/settings.json`, not a sibling checkout — only paths rooted
  at this repo.
- **Prefer `assert_cmd` tests for operator-visible behaviour.** Drive the
  real compiled binary the way `crates/conway-cli/tests/oneshot.rs` and
  `crates/conway-cli/tests/first_run.rs` do, rather than asserting on an
  internal function in isolation (`CONTRIBUTING.md` §3: a unit test of a
  mapping function is not a liveness test).
