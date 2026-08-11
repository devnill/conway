# `ArtifactWriteHandle::noop`: compile evidence for board item 01KZJ5S3ZC8SPWTX94C4HTEC2R

Board item 01KZJ5S3ZC8SPWTX94C4HTEC2R: `docs/plugins/authoring.md`'s ten-minute
walkthrough, step 3, required a hand-rolled `ArtifactWriter` no-op double
before a hook author's first assertion — fifteen-odd lines with nothing to do
with the hook under test. This note is the compile evidence the item's own
acceptance criteria ask for ("re-executed verbatim"), kept in the same style
as `.design/backends-as-plugins-q1-compile-evidence.md` and `.design/
router-installation-q2-compile-evidence.md`.

## The method

Two scratch crates outside this workspace (built under this session's own
scratchpad directory, never added to `crates/`, deleted before this item
finished — `git status --porcelain crates/` carries nothing from either),
each with a `Cargo.toml` naming exactly `conway` (by path) and `async-trait`/
`tokio` (the same two dev-shape dependencies `docs/plugins/authoring.md`
itself tells a reader to add):

```toml
[dependencies]
conway = { path = "/Users/dan/code/conway/crates/conway" }
async-trait = "0.1"

[dev-dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

**`noop-writer-check-before/`** reproduces the walkthrough's pre-fix step 3
exactly (`git show 4a49d90:docs/plugins/authoring.md` — a hand-rolled
`NoopWriter` struct plus `#[async_trait] impl ArtifactWriter`, plus `Arc`/
`PathBuf` imports). It is not a claim that the old form is now broken —
`ArtifactWriteHandle::new` still takes any `Arc<dyn ArtifactWriter>` — only a
measurement of the boilerplate the fix removes.

**`noop-writer-check-after/`** is `docs/plugins/authoring.md`'s current step
2 and step 3, copied verbatim post-fix: `ArtifactWriteHandle::noop(agent_id)`
in place of the hand-rolled double.

Both were built with `cargo test` (no flags) from inside each crate's own
directory.

## `noop-writer-check-before/src/lib.rs` — `cargo test`

```text
   Compiling noop-writer-check-before v0.1.0 (.../noop-writer-check-before)
warning: struct `MyFirstHook` is never constructed
  --> src/lib.rs:13:8
   |
13 | struct MyFirstHook;
   |        ^^^^^^^^^^^
   |
   = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: `noop-writer-check-before` (lib) generated 1 warning
    Finished `test` profile [unoptimized + debuginfo] target(s) in 24.81s
     Running unittests src/lib.rs (target/debug/deps/noop_writer_check_before-a353e35126d77c94)

running 1 test
test tests::my_first_hook_appends_its_marker ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

(The one warning is the same "struct constructed only inside `#[cfg(test)]`"
shape `.design/router-installation-q2-compile-evidence.md`'s own claim 2
transcript carries — harmless, unrelated to this item.)

## `noop-writer-check-after/src/lib.rs` — `cargo test`

```text
   Compiling noop-writer-check-after v0.1.0 (.../noop-writer-check-after)
warning: struct `MyFirstHook` is never constructed
 --> src/lib.rs:9:8
  |
9 | struct MyFirstHook;
  |        ^^^^^^^^^^^
  |
  = note: `#[warn(dead_code)]` (part of `#[warn(unused)]`) on by default

warning: `noop-writer-check-after` (lib) generated 1 warning
    Finished `test` profile [unoptimized + debuginfo] target(s) in 25.23s
     Running unittests src/lib.rs (target/debug/deps/noop_writer_check_after-44d3bdd519b7086c)

running 1 test
test tests::my_first_hook_appends_its_marker ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

Both compile and both pass — the fix does not remove the old, hand-rolled
path (`ArtifactWriteHandle::new` still takes any `Arc<dyn ArtifactWriter>`,
unchanged signature); it adds a second, shorter path.

## Line count before the first assertion

Measured from `mod tests {` (the first line a reader adds beyond the hook
itself) through the first `assert_eq!` line, inclusive — the exact code a
reader has to write, not counting the hook definition both files share:

| | lines | 
|---|---|
| Before (hand-rolled `NoopWriter`) | 42 |
| After (`ArtifactWriteHandle::noop`) | 28 |

14 fewer lines, all of them boilerplate unrelated to the hook under test: no
`NoopWriter` struct, no `#[async_trait] impl ArtifactWriter`, no `Arc`/
`PathBuf` imports, no `ArtifactWriteError` import. This matches the item's own
estimate ("roughly fifteen extra lines") to within rounding.

## What this closes

`docs/plugins/authoring.md`'s own walkthrough (steps 2–3) was updated to the
`after` form and re-executed verbatim as this second scratch crate — the
page's own guarantee ("this page teaches ... real, F8 ... landed") now holds
for the ergonomics, not merely for reachability.
