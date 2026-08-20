# Plan of attack

**Written 2026-08-19 from [`STATE-OF-THE-UNION.md`](STATE-OF-THE-UNION.md),
against the working tree at `fa8b03b`. Filed to the board 2026-08-20.**

> For the agents doing the work. Snapshot document — replaced wholesale on the
> next run of [`REVIEW-PROMPT.md`](REVIEW-PROMPT.md), not merged into.
>
> **The board is the authority; this page is the dispatch aid.** Every item below
> exists on the board with its full spec, and the id is the thing to claim. What
> this page adds is the one thing a board cannot express: which items would
> collide if run at the same time, and in what order the shared files have to be
> touched.
>
> Fan-out is chosen at dispatch time. Honour the collision table and the
> serialization notes rather than the domain count.

---

## 0. Board state

23 open items, 18 claimable. Five are blocked on purpose — see §7.

This round also closed three items that had drifted, and recovered two real
pieces of work from inside them:

| Closed | Why |
| --- | --- |
| `01M03GNPSPW37P6FQNZZWVMGB6` | The memory umbrella. Its acceptance was "stays open as the record that memory is wanted but not startable." Memory shipped; both named blockers are gone. |
| `01M03FPPPJP0P6K2H5ZE7SP85Y` | The subprocess holder. Its acceptance was "closes by being decomposed"; it was, and three of its four pieces shipped. |
| `01M03VKVR9Y0QF368938SA43SA` | Cancelled — a second marker waiting on the same operator ruling `01KZ844ZXZMVRWC7ZANT7PSM6X` already carries. |

Recovered from inside them and refiled: `01M0EKTVJE558SB4S6K3YYVXVZ`
(`permissions.md`) and `01M0EKVR1BEXXS75NV2JC4HZZ9` (`kill_group`).

**No umbrella parents were created for the new work.** Both closures were
roll-ups that became structurally unclaimable — the board refuses to complete a
parent with open children — and drifted for weeks while the work underneath them
shipped. Flat items with explicit `depends_on` express the same ordering without
that failure mode.

---

## 1. Collision table

The files two items would both want. Each has **exactly one owner at a time**.
Anyone needing a change in one waits for the stated point.

| File | Owner | Then |
| --- | --- | --- |
| `PHILOSOPHY.md`, `ARCHITECTURE.md` | `01M0EM83C5…` | `01M0EM8NK2…` — different sections, land together if one agent takes both |
| `README.md`, `GUIDE.md` | `01M0EM83C5…` | `01M0EMAWY0…` appends its links after |
| `scripts/board-claims.md` | `01M0EM97X1…` | `01M0EMEQJH…` strictly after — same file, and going first would resurrect the entry the other removes |
| `docs/permissions.md` | `01M0EKTVJE…` | — |
| `docs/plugins/README.md` | `01M0EMAWY0…` | — |
| `crates/conway-core/src/ports/**` | `01M0EMAC4C…` | `01M0EMCK55…` after |
| `crates/conway-core/src/path.rs` | `01M0EMAC4C…` | — |
| `crates/conway-runtime/src/agent_loop.rs` | `01M0EMAC4C…` | `01M08F5XYFZ…` after |
| `crates/conway-cli/src/tui/**` | `01M0EM9RW7…` | `01M0EMDVBJ…` after |
| `crates/conway-cli/src/cli.rs` | `01M0EM9RW7…` | `01M0EMC19F…` only if it builds `conway path` |
| `crates/conway-cli/src/first_party_plugins.rs` | `01M09V3S2A…` | — |
| `crates/conway-plugin-memory/**` | `01M09V3S2A…` | `01M0EMD54B…` after |
| `Cargo.lock`, `deny.toml` | `01M0ASG80Q…` | — |

**`agent_loop.rs` and `ports/` are the highest-traffic merge points in the
tree.** While `01M0EMAC4C…` is running, nothing else may touch either.

---

## 2. Truth in the documents — six items, no Rust

The tree is right and the specification is wrong, in the *understating*
direction, on two CI gates and six pages. Nothing here needs a decision, and the
only cost of delay is that every reader in between is misinformed about
capabilities that work.

| Id | What | Size |
| --- | --- | --- |
| `01M0EM97X118CZ43CGEPH2PB8F` | **CI is red.** The design-claims ledger asserts four first-party capabilities are unbuilt; three ship. | S |
| `01M0EM83C5ZZX75MSE0MTV7NZW` | Five top-level documents say memory/skills/MCP are unbuilt. Includes `GUIDE.md`'s "there is no runtime plugin host", the single worst line. | M |
| `01M0EM8NK2R36X4TW50D43S58H` | `PHILOSOPHY.md` and `ARCHITECTURE.md` still describe a symlink race `cap-std` closed. Security-bearing, understates a guarantee. | S |
| `01M0EKTVJE558SB4S6K3YYVXVZ` | `docs/permissions.md` says in-process is the only extension mechanism — on the security page. | S |
| `01M0EMAWY0B62RC966FQMQPGAC` | `conway.memory`, `conway.skills` and the MCP client ship with no findable page. | M |
| `01M0EMEQJHPR3XVNAN39YX7C38` | The bare-inference claim `INTENT.md` §7 nominated for the ledger was never added. | S |

**Land the ledger fix and the five-documents fix in one change.** The ledger is
what turned this from an unknown into a finding; splitting them recreates the
divergence it exists to prevent.

---

## 3. The curation premise — one item that gates three

`01M0EMAC4CCDQ8QJYM21RXPKRY` — **prove the seam with the smallest real curator.**

About 5,500 lines of path and curation machinery exist and nothing uses any of
it. Six independent accommodations around one premise, and the premise's one
test — `conway.memory` — failed and moved off it. Do not start
`conway.compaction`. Do not build `conway path`. Write the cheapest honest
curator (*"drop tool results older than K turns"*, well under a hundred lines),
run it through the real seam on a real session, and report what the seam could
not do.

Blocked behind it, deliberately:

- `01M0EMC19FV3FJFJVR5697AV44` — settle `Selector::Operator`: build `conway path` or correct the tense.
- `01M0EMCK55628YJXGBQY8YGXHE` — `PathStore` is the one port an embedder cannot supply.
- `01M08F5XYFZ0JY42HW789AHX9J` — wire `resolve_default_path`. **This dependency was added this round**; the item was previously claimable. If the seam turns out not to carry a consumer, wiring the resolver is not automatically right.

Related and independent: `01M090HJEJBK24SX70Z9E25PZ4` (test
`compose_context_hooks`, the untested precedent `compose_curators` mirrors).

**Get C's answer to the operator before dispatching the three behind it.**

---

## 4. The operator surface

The domain [`INTENT.md`](INTENT.md) §7a says outranks architectural tidiness,
and the one that has had the least attention.

| Id | What | Size |
| --- | --- | --- |
| `01M0EM9RW7AYZAYXE5Z2XPNFND` | **`/model` and `/role`.** No way to change model mid-session, against §5c's "a design that makes model changes awkward has failed regardless of what else it gets right." Folds in the role/model argument on `/fork` and `/spawn`. | L |
| `01M0EMD54BWAVZGYWPXP4S5P1J` | **`/memory`.** Three tools a model can call, nothing an operator can type. Waits on the durable store. | M |
| `01M0EMDVBJVT510GBJHPWBZ3G6` | The in-flight `[p]` pattern editor: uncommitted, compiles, no item, no acceptance. Write its spec from the code, then finish or revert. | S |

---

## 5. Memory

`01M09V3S2AQYB2VK6MANFRH1JM` — wire the durable `FsMemoryStore`. Memory is real,
tested end to end, and forgets everything on restart because the CLI installs
`InMemoryMemoryStore`. The obstacle is real and stated in the code: `bundle()` is
sync, `FsMemoryStore::open` is async, and it has two callers that must not end up
with two unsynchronised stores over one directory.

**Say the limitation first if any of it survives.** This is the failure mode
[`REVIEW-PROMPT.md`](REVIEW-PROMPT.md) §2 lists fourth.

Unblocks `01M0EMD54B…` (`/memory`) and feeds `01M0EMAWY0…` (the memory page's
durability section).

---

## 6. Gates and hygiene

All independent, all claimable, all cheap relative to what they cost everyone
else.

| Id | What |
| --- | --- |
| `01M0ASG80Q2ZWR8201F580M7RR` | `cargo deny` red on RUSTSEC-2026-0258. Verified red this review. |
| `01M0APF2CFH3CCH9PJKX2HKTA5` | Long background workspace test runs get killed mid-flight. **Worth real diagnosis** — it taxes every other item's gate. |
| `01M09MPZ9C188AHNBKWEJ3CEQA` | Two timeout-budget tests fail only under full-suite parallelism. Do not widen a budget until it passes. |
| `01M0EKVR1BEXXS75NV2JC4HZZ9` | `kill_group` duplicated five times because `conway-tools::process` is private. A gap in the extension surface, not tidiness. |
| `01M0ASX466G3PW3SJJS3KGNS55` | Surface `Backend::token_fidelity` where an operator can see it. |

---

## 7. Waiting on you, not on work

- `01KZ844ZXZMVRWC7ZANT7PSM6X` — the `context.hook/1` REPLACE primitive, under a standing deferral recorded by id. Its verification anchor was repointed this round from the deleted `.design/extension-architecture.md` to `docs/plugins/hooks.md` §9; the deferral itself is untouched. **Do not claim it.**
- `01KZHVFCN6ZEAXV7K5JHRQN1YB` — nothing can trust a plugin. Also deferred, and **its priority went up while it sat**: filed when every plugin was compiled in, and today two crates spawn operator-named external programs with no digest trust. The highest-value deferred item on the board.
- Four open questions in [`INTENT.md`](INTENT.md) §11, none of them work: whether an operator gets a curation command, what proves an extension surface, whether a shipped capability owes a page, and whether §5f's vocabulary should be pushed into the other documents.

---

## 8. Dogfooding

`01M0EMBF2ZFJA5Z3NE21FYN8RF` — zero `[dogfooding]` items across 358. The intake
mechanism was built and has never fired.

**Run this concurrently with everything above, not after.** It is calendar time
rather than agent time, and it is the only item on this page that can produce a
finding no review would have.

---

## 9. Dispatch

**Minimum useful round:** the six document items in §2. Two red gates behind
them, no decisions required, no Rust.

**Three agents, nothing shared:** §2's ledger+documents pair, §6's gates, §8's
dogfooding.

**Wider:** add §4's `/model` and §5's memory wiring. Independent of §2 and of
each other, colliding only at the two points named in §1.

**§3 is serialized behind its own first item**, and its answer should reach the
operator before the three items behind it are dispatched. While it runs, nothing
else may touch `ports/` or `agent_loop.rs`.
