# Plan of attack

**Written 2026-08-24 from [`STATE-OF-THE-UNION.md`](STATE-OF-THE-UNION.md),
against the working tree at `7654041`. Filed to the board 2026-08-24.**

> For the agents doing the work. Snapshot document — replaced wholesale on the
> next run of [`REVIEW-PROMPT.md`](REVIEW-PROMPT.md), not merged into.
>
> **The board is the authority; this page is the dispatch aid.** Every item
> below exists on the board with its full spec; the id is the thing to claim.
> What this page adds is what a board cannot express: which items collide, and
> the order shared files must be touched in.
>
> | Item | Board id | | Item | Board id |
> | --- | --- | --- | --- | --- |
> | REC-1 | `01M0TV3R2T4ZEDR1G7JG5QJ887` | | EMB-2 | `01M0TV643ZWRSR8Q79Z4Q1KVR5` |
> | REC-2 | `01M0TV447NAJ1R06S455DZPP54` | | CON-1 | `01M0TV6E2K6QF9VXP6C7TFH06X` |
> | REC-3 | `01M0TV7GFSNNRZV522XCRMTHVX` | | CON-2 | `01M0TV7ZDS8X4F4TEJPRZB9P6T` |
> | OP-1 | `01M0TV4J05PYE8PG6YTV0HX5HN` | | CON-3 | `01M0TV6WNCZ58N5BBGYWT6J06A` |
> | OP-2 | `01M0TV4Y1K9ESJQ4PDRCP7R3FA` | | CON-4 | `01M0TV8MSFRHHQ5BNZV3NHZCEW` |
> | OP-3 | `01M0TV5CYSP844XR8PJ59D8QM4` | | OP-4 | `01M0TNCAP1HH4YNC5K9753YG26` |
> | EMB-1 | `01M0TV5PN8RR9NN97AWP09E6K7` | | OP-5 | `01M0TNBACHQSAMMJ3TY14S47MX` |
>
> Fan-out is chosen at dispatch time. Honour the collision table and the
> serialisation notes rather than the domain count.

---

## 0. Board state

Surveyed live (MCP) during this review: **2 open items, 0 in progress, no
stale claims**. Both open items are from the 2026-08-24 cycle review, both are
claimable, and both fold into the operator domain below:

| Open on the board | Folded in as |
| --- | --- |
| `01M0TNCAP1HH4YNC5K9753YG26` — /tree prints a full agent id, /agents a short one, /tree's doc claims they match | OP-4 |
| `01M0TNBACHQSAMMJ3TY14S47MX` — /ask pull-in is not atomic; failure halfway leaves an orphan | OP-5 |

The six operator questions in `STATE-OF-THE-UNION.md` §7 were **answered
2026-08-24** — the accepted sentiments are folded into `INTENT.md` (§7, §7a,
§7b, §7c, §8.10) and `DESIGN-context-path.md` §10 is closed. Every item below
is claimable the moment it is filed.

---

## 1. Domains and the collision table

Four domains, workable in parallel. Each names the files it owns this round;
nobody else touches them.

| Domain | Owns this round |
| --- | --- |
| **D-REC** — the record | `ARCHITECTURE.md`, `PHILOSOPHY.md` §6, `docs/README.md`, `docs/plugins/README.md`, `docs/vision/DESIGN-context-path.md`, doc comments in `crates/conway-core/src/{path,log}.rs` and `crates/conway-runtime/src/runtime.rs`, the enum-guard test |
| **D-OP** — operator surface | `crates/conway-cli/**`, `docs/scripting.md`, `docs/sessions.md`, `docs/interactive.md` |
| **D-EMB** — embedding | `crates/conway/src/{builder,host_caps}.rs`, `crates/conway-core/src/ports/subagent.rs`, a new survey doc under `docs/vision/` |
| **D-CON** — consolidation | `crates/conway-plugin-mcp/**`, `crates/conway-plugin-subprocess/**`, `crates/conway-tools/src/process.rs`, `crates/conway/src/error.rs`, `crates/conway-testkit/**` |

Shared files, their single owner, and the serialisation order:

| Shared file | Owner | Order notes |
| --- | --- | --- |
| `PHILOSOPHY.md` | D-REC | only REC-3 touches it this round |
| `ARCHITECTURE.md` | D-REC | only REC-3 |
| `Cargo.toml` (workspace + crate manifests) | D-CON | REC-2 adds one conway-cli dependency **first** (small, mechanical); CON-2 lands after it if a shared crate is blessed |
| `crates/conway-core/src/ports/*` | D-EMB | nothing else touches ports this round; D-REC's core edits are doc comments in `path.rs`/`log.rs`, outside `ports/` |
| `crates/conway-runtime/src/agent_loop.rs` | — | **untouched this round**; if an item turns out to need it, stop and re-plan |
| `crates/conway-cli/src/tui/commands.rs` | D-OP | OP-4 before OP-2 — both edit the command surface, OP-4 is already specced on the board |
| `tests/` workspace-wide | D-CON | CON-4 **last of all items**, after every other domain's code has landed — it rewrites helpers other items' tests may add copies of |

---

## 2. D-REC — the record catches up (adherence · evidence)

**REC-1. Retire the three "no production caller" comments.** *(S, unblocked)*
Done: `crates/conway-core/src/path.rs:120-123`,
`crates/conway-core/src/log.rs:363-367`,
`crates/conway-runtime/src/runtime.rs:168-172` name
`conway.path`/`compose_context_path` as the producer (matching
`docs/plugins/hooks.md:778`); `Selector` added to the enum guard's
`WATCHED_ENUMS`. (`DESIGN-context-path.md` §10 was closed with the operator's
2026-08-24 ruling — already done, not part of this item.)

**REC-2. `conway.trim` becomes reachable or declared unreachable.** *(S–M, owned by D-OP's crate but sequenced here, unblocked)*
Done: either conway-cli depends on `conway-plugin-trim` and `"conway.trim"`
resolves in `[plugins].install`, or every enumeration that should list it says
"built, embedder-only" explicitly. The decision drives REC-3's wording, so this
lands first. Touches `crates/conway-cli/Cargo.toml` +
`first_party_plugins.rs`.

**REC-3. The enumerations tell the whole plugin tier.** *(S, depends: REC-2)*
Done: `ARCHITECTURE.md` §2b lists all twelve plugin crates;
`docs/plugins/README.md` and `PHILOSOPHY.md` §6 account for `trim` per REC-2's
outcome; `docs/README.md` gains its `docs/dogfooding.md` row.

---

## 3. D-OP — the operator surface (operator lens + existing board items)

**OP-1. The resume handle is named where the JSON example lives.** *(S, unblocked)*
Done: `docs/scripting.md`'s JSON-output section states in prose that
`transcript_ref` — not `agent_id` — is what `--resume` accepts, and
`docs/sessions.md`'s resume walkthrough shows where the id comes from.
(Alias/rename of the field is a separate call; the doc sentence is the fix a
scripting user needs today.)

**OP-2. The operator can cancel one subagent.** *(M, unblocked; Q3 decides whether it becomes a standing rule)*
Done: a `/cancel <agent>` command (mirroring `/steer <agent>`) cancels a
specific non-focused subagent without ending the session, visible in the
palette, covered by a TUI-level test. `/await` parity rides along if cheap.
Serialise **after OP-4** (same file).

**OP-3. Sessions an operator returns to can be told apart.** *(S–M, unblocked — Q4 answered: names are in)*
Done: a session can carry an operator-chosen name (`--session` accepts it,
`sessions list` shows it); the id stays the identity, the name is furniture,
per INTENT §7b's ruling.

**OP-4. Board `01M0TNCAP1HH4YNC5K9753YG26`** — /tree vs /agents id mismatch.
*(claimable now; spec on the board)*

**OP-5. Board `01M0TNBACHQSAMMJ3TY14S47MX`** — /ask pull-in atomicity.
*(claimable now; spec on the board)*

---

## 4. D-EMB — the third surface (surfaces lens)

**EMB-1. The §7c binding survey.** *(S, unblocked; Q5 asks only whether to prioritise it)*
Done: a written comparison of Diplomat, UniFFI, and cbindgen against conway's
async, streaming public API — who drives the runtime, how an event stream
crosses, crash and memory ownership — ending in a recommendation and a rough
shape. No code. This is the deliverable INTENT §7c itself blesses as the
acceptable first step.

**EMB-2. The subagent host's non-swappability cites its ruling.** *(S — Q1 answered: core-owned)*
INTENT §7 now states fork/spawn are mechanism with exactly one implementation.
Done: `crates/conway/src/host_caps.rs:68-73` and
`crates/conway-core/src/ports/subagent.rs`'s header cite that decision instead
of describing the missing injection point as an unexplained absence.

---

## 5. D-CON — cost of change (sustainability ×2)

**CON-1. One authority for the subprocess timeout default.** *(S, unblocked)*
Done: a single `DEFAULT_TIMEOUT_MS` both plugin crates reference (the
`kill_group` facade-re-export precedent, `crates/conway-tools/src/process.rs`,
already shows an acceptable route); the "must match" comment in
`conway-plugin-mcp/src/lib.rs:72-77` deleted because nothing is left to match.

**CON-2. The subprocess twins share their lifecycle layer.** *(M, unblocked — Q6 answered)*
Done: process-lifecycle + fail-closed error taxonomy (spawn / timeout /
session-died / malformed-frame) defined once and consumed by both
`conway-plugin-mcp` and `conway-plugin-subprocess`; the wire protocols stay
separate on purpose. Shape per INTENT §8.10's ruling: facade re-export
preferred; a new shared crate only if the facade would have to learn something
it has no reason to know. Depends: CON-1 (it subsumes the constant's home).

**CON-3. One of the two `ConwayError`s is renamed.** *(S, unblocked)*
Done: `crates/conway/src/error.rs`'s enum takes a distinct name (the newer,
narrower of the pair), the `CoreConwayError` alias machinery in
`crates/conway/src/lib.rs:98-114` shrinks or disappears, no behaviour change.
Mechanical, wide, low-risk — run the full workspace suite after.

**CON-4. A facade-level test-support tier.** *(M, unblocked; lands LAST)*
Done: `build_conway`/`text_response` exist once in a test-support module only
workspace crates see (testkit itself stays core-only for third parties);
`fake_router` copies deleted in favour of testkit's existing
`FakeRouter::single`; the 46/52/36 hand-rolled copies gone. Touches test files
across every crate — serialise after all other items so it does not invalidate
in-flight work.

---

## 6. Dispatch

**All fourteen items are dispatchable** (Q1–Q6 answered 2026-08-24). Order:
OP-4, OP-5 (already on the board) and the small unblocked set — REC-1, REC-2 →
REC-3, OP-1, OP-2 (after OP-4), OP-3, EMB-1, EMB-2, CON-1, CON-3 — then CON-2,
with CON-4 last. Suggested fan-out 3: one worker each for D-REC, D-OP, D-CON;
EMB-1 and EMB-2 are small enough to ride with any worker or a researcher.

**Coverage debt for the next review** (from §6 of the state of the union): the
TUI has still never been driven under a real pty; the security-bearing pages
(`docs/permissions.md`, `docs/tools.md`) are unverified this round; the full
workspace suite was last run at `da9813c`. The next run should spend an
operator-lens budget on a real terminal, and an adherence budget on the
permissions pages.
