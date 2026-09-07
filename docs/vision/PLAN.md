# Plan of attack

**Written 2026-09-04 from [`STATE-OF-THE-UNION.md`](STATE-OF-THE-UNION.md),
against the working tree at `6c85ace`.**

> For the agents doing the work. Snapshot document — replaced wholesale on the
> next run of [`REVIEW-PROMPT.md`](REVIEW-PROMPT.md), not merged into.
>
> **None of the items below exist on the board yet.** Unlike the last
> regeneration, this round's board was surveyed (`work_list`, paged to
> exhaustion) and found genuinely drained — three open items, two of them
> explicitly out of scope for anyone but the operator (§0). Every item below
> is new, drafted from this run's six-reviewer findings, and needs
> `work_create` before it can be claimed.

---

## 0. Board state

Surveyed live (MCP) 2026-09-04, paged to exhaustion: **3 open items
total.** `01M1FRVHDGCT7NTDW782WQRET9` (a Rust-vs-script context-hook
documentation item) is the operator's own live, currently-uncommitted edit
in a separate session — not a dispatch target. Its parent container, P5, is
correctly blocked on it. `01M0X1GRJ52SF38FV8E0V7V7B4` (CAPSTONE — the
virgin-install walk) states in its own spec that it is "not worker-
executable" and names the operator as its only valid executor. Nothing
below touches any of the three.

---

## 1. Domains and the collision table

Eleven findings from this cycle (`STATE-OF-THE-UNION.md` §3), grouped into
nine domains that can run in parallel without conflicting on a file. Two
domains (memory parity, embedder port paths) are blocked on an operator
ruling before they can be sized — see §2's DECISION items.

| Domain | Covers | Owns |
| --- | --- | --- |
| **D-ONRAMP** | F1 — silent hang on first one-shot use | `crates/conway-cli/src/oneshot.rs`, `crates/conway-cli/src/cli.rs`, `crates/conway/src/config/schema.rs` (the `deadline_secs` default only) |
| **D-LABELS** | F2 — session labels have no write path | `crates/conway-cli/src/commands/sessions.rs`, `docs/sessions.md`, `docs/plugins/discover.md` |
| **D-LIMITS** | F3 — two config limits parse but never wire | `crates/conway-runtime/src/subagent.rs`, `crates/conway-runtime/src/runtime/root.rs`, `docs/agents.md` |
| **D-PERM** | F4 — `PermissionBroker` triplication | `crates/conway-runtime/src/permission.rs` only |
| **D-TESTKIT** | F5 — no `FakePlugin` double | `crates/conway-testkit/src/lib.rs` (new file for the double), 8 files under `crates/conway-runtime/tests/` |
| **D-ATOMICWRITE** | F6 — atomic write reimplemented, fsync gaps | `crates/conway-tools/src/fs/` (new shared helper), `crates/conway-cli/src/session_names.rs`, `crates/conway-cli/src/tui/history.rs`, `crates/conway-plugin-names/src/lib.rs` |
| **D-PLUGINID** | F7 — plugin-id literals vs. `PLUGIN_ID` | `crates/conway-cli/src/tui/app/plugin_toggle.rs`, `crates/conway-cli/src/tui/view/plugins.rs`, `crates/conway-cli/src/plugin_rows.rs` — test code only, no production behavior change |
| **D-FIXTURE** | F8 — 21 test files still hand-roll `ConwayConfig` | The 21 files themselves, spread across `conway-cli`, `conway-plugin-backends`, `conway-plugin-claude`, `conway-plugin-skills`, and `conway`'s own `tests/` — re-derive the exact list with `grep -rl "ConwayConfig {" crates/*/tests` before claiming |
| **D-DOCFIX** | F9 — stale plugin count in README | `docs/plugins/README.md` only |

Shared files and the serialisation they force: **none this round.** Every
domain above owns a disjoint file set — confirmed by cross-referencing the
"Owns" column pairwise. `PHILOSOPHY.md`, `Cargo.toml`, `ARCHITECTURE.md`,
`crates/conway-core/src/ports/*`, and `crates/conway-runtime/src/
agent_loop.rs` — the standing shared-file watchlist from prior rounds —
are untouched by every domain above.

**One real near-collision, named so nobody discovers it by breaking the
build:** D-ATOMICWRITE's new helper is the natural place a *future*
`SessionMeta.labels` sidecar-write (if D-LABELS chooses a new file rather
than reusing `settings.json`'s existing writer) would want to call. Not a
collision today — D-LABELS's own item should just check whether
D-ATOMICWRITE's helper already exists before adding a sixth hand-rolled
copy, if it lands second.

---

## 2. DECISION items — operator-only, before the two blocked findings can size

Two findings from §3 are real but their scope depends on an Intent question
`STATE-OF-THE-UNION.md` §7 raises and does not answer. Per this process's
own convention (`ideate/human-gate` items), these are not worker-executable
until you rule.

**DECISION-1** *(spec_format: `ideate/human-gate`)* — Does §7a's operator-
parity rule extend to `remember`/`forget`/`list_memories` (F10), or is it
scoped to agent-lifecycle tools only? Ties to `STATE-OF-THE-UNION.md` §7
Q1, which drafts the proposed `INTENT.md` addition either answer would fold
in. If yes: unblocks a small (S) `conway memory list/forget` item, filed as
**D-MEMORY** once ruled, owning a new `crates/conway-cli/src/commands/
memory.rs`. If no: no further action, but §7 Q1's proposed text should
still be folded into `INTENT.md` so the boundary is stated rather than
implicit.

**DECISION-2** *(spec_format: `ideate/human-gate`)* — For each of the three
ports with no embedder path (F11 — `SessionStore`, `ArtifactWriter`,
`HealthRegistry`): build a `with_*` injection point, or record a cited
rationale for declining one (the `SubagentHost` precedent)? Ties to
`STATE-OF-THE-UNION.md` §7 Q3 and Q5. Sizes the resulting item(s) — S–M
each if building, S (a doc comment) each if declining. Owns
`crates/conway/src/lib.rs`, `crates/conway/src/builder.rs` once ruled;
filed as **D-EMBEDPATH**.

---

## 3. The nine dispatchable domains

**D-ONRAMP** *(M)* — `output_format` defaults to `Text` with zero progress
signal, and `deadline_secs` defaults to `0` (unbounded); a fresh install's
first `conway -p` can sit silent for 90+ seconds against a slow local
backend. Give text mode a spinner/elapsed-time notice or first-token
stream before the full reply lands, and/or set a sane default deadline so
a stalled backend fails loud instead of hanging quiet. `STATE-OF-THE-
UNION.md` §3 F1 has the full reproduction. Highest-priority item this
round — it is the actual on-ramp experience §7a is scored against.

**D-LABELS** *(S)* — `SessionMeta.labels` has a complete read path
(`sessions list --label`, `conway.discover`'s `label` param) and no way to
ever be set outside a direct facade caller. Add a `sessions label`/
`unlabel` subcommand to `commands/sessions.rs`, mirroring the existing
`name`/`unname` pair exactly (same file, same pattern, confirmed at
`commands/sessions.rs:411`). If scope says "disclose, don't build" instead
(possible per DECISION-1's sibling question about facade-only paths), the
fallback is a one-line caveat on both docs pages naming the gap.

**D-LIMITS** *(S doc-only, or M if wired)* — `max_parallel_tools` and
`tool_timeout_secs` are already disclosed as unwired in `builder.rs`'s own
module doc; that disclosure hasn't reached `docs/agents.md`, which shows a
worked example using both with no caveat. Minimum bar: copy the existing
disclosure to the doc, next to the example. Full bar: wire both fields
through `RootSpec`/`AgentSpec` for real.

**D-PERM** *(M)* — `PermissionBroker`'s `remember_pattern_rule`/
`remember_deny_rule`/`remember_prompt_rule` (three write methods) and their
six read-side siblings all reimplement the identical fail-closed
canonicalization check. One prior shipped bug (`d508a5d`) came from exactly
this scatter. Factor one private write-side helper and one read-side
helper; the nine public methods become thin callers. No public API change;
existing tests in the same file pin behavior throughout.

**D-TESTKIT** *(S)* — Add `FakePlugin` to `conway-testkit`, matching the
shape of the existing `Fake*` doubles (configurable `id`/`tools`/manifest
fields). Switch the 8 files in `crates/conway-runtime/tests/` that
currently hand-roll their own copy (`report_only_agent.rs`,
`context_hook_scripts.rs`, `context_report_persistence.rs`, `steering.rs`,
`result_contract.rs`, `runtime_api.rs`, `agent_loop_e2e.rs`,
`tool_runner.rs`) to import it instead.

**D-ATOMICWRITE** *(S)* — Three of five independent "write-temp-then-
rename" implementations skip `fsync` before the rename
(`session_names.rs`, `tui/history.rs`, `conway-plugin-names/src/lib.rs`);
two get it right (`conway-tools/src/fs/write.rs`, `fs/beneath.rs`). Extract
one `atomic_write(path, bytes)` helper (fsync-before-rename) and have all
three under-implemented sites call it. Natural home: `conway-tools::fs` if
it can be exposed workspace-wide, else a small new shared location.

**D-PLUGINID** *(S, test-only, no behavior change)* — 129 plugin-id string
literals across 4 files restate `PLUGIN_ID` constants that production code
in the same files already imports correctly (115 of them in one file,
`tui/app/plugin_toggle.rs`). Swap the test-module literals for the
constant imports. Mechanical; verify `"conway.permissions"` is a genuine
test-only synthetic id with no real crate behind it before touching those
specific occurrences (the reviewer flagged this as the one exception).

**D-FIXTURE** *(S)* — Re-run the `ConwayConfig` shared-fixture sweep's own
`grep -rl "ConwayConfig {" crates/*/tests` against current `main`; it
returns 21 files today against 63 already on the shared fixture. Fold each
into `conway::test_support` or leave a one-line comment naming why not
(mirroring `conway-tools/src/testing.rs`'s existing practice of justifying
its own divergence in writing) — some of the 21 may legitimately need raw
construction (testing config parsing itself); this item's job is to check
each, not blanket-convert.

**D-DOCFIX** *(S, three-word fix)* — `docs/plugins/README.md:200,224,242`
each say "eleven shipped first-party plugins" quoting the section's own
pre-update title; the section header and bullet list already correctly say
twelve. Fix the three cross-references.

---

## 4. Dispatch

Nine dispatchable domains, none sharing a file, zero enforced ordering
between them — this round genuinely parallelizes to nine-wide if appetite
allows, or any subset. D-ONRAMP is the one item worth prioritizing above
the rest if fan-out is narrower than nine: it is the only SIGNIFICANT
finding with no Intent-gap dependency and the one most likely to be a real
operator's actual next bad experience.

Two DECISION items (§2) are quick, operator-only, and unblock D-MEMORY and
D-EMBEDPATH once ruled — worth resolving early in the same session even if
the resulting builds happen later, since they also settle the two
`INTENT.md` amendment questions those decisions are entangled with.

**Coverage debt for the next review**, carried from `STATE-OF-THE-UNION.md`
§6: three consecutive cycles with no pty to drive the TUI live — flagged
there as a process defect this round, not merely a caveat, and worth
addressing before the next full review rather than carrying a fourth time;
the root cause of D-ONRAMP's 90-second wait was observed, not isolated,
so confirm the fix actually addresses the wait once landed, not just the
symptom; and the two structural findings (§3 F13, F14 — non-Rust embedding
unbuilt, no port ever externally consumed) are deliberately NOT board items
this round — they are appetite-scale questions for you, not work items a
worker can size.
