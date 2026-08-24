# State of the Union: conway

**Reviewed 2026-08-18 against the working tree at `fa8b03b`, version 0.9.0.**

> Written for the operator. It assumes you care about the shape of the system
> and not about the shape of any particular trait. Everything in it was checked
> against the code in this run; where you might want to check something
> yourself, the file and line is given.
>
> Snapshot document — replaced wholesale on the next run of
> [`REVIEW-PROMPT.md`](REVIEW-PROMPT.md).
>
> **Note on this run.** It is the second review at this commit. The tree has not
> moved; what moved is what was verified. The previous run found the honesty
> gate red and said so. This run confirms it, finds the same defect in three
> more places the previous run did not name, finds a second gate red, and
> reaches a different conclusion about the largest piece of work in the tree.
> Where the two runs disagree, this one is the current reading.

---

## 0. The verdict, in three sentences

**conway works, and the extension story is now genuinely proven** — a plugin
written in any language, running as a separate process, can add tools to a
`conway` binary that was compiled before it existed. **Two of the project's own
CI gates are red on `main` right now**, and both are red for the same reason:
capabilities shipped and the documents describing them were not updated, so five
top-level pages tell a reader that things which work do not exist. **And the
single largest piece of engineering in the tree — the context-path and curation
machinery, about 5,500 lines — currently has no consumer at all**, after the one
feature built on it was moved off it.

The first is the best news the project has had. The second is a mechanical fix
with a deadline. The third is a decision only you can make, and it is the reason
to read this document.

---

## 1. What is built, as blocks

conway is one library consumed three ways. Everything below the facade line is
fixed; everything to the right of it is optional and swappable.

```
   terminal (TUI)  ─┐
   one-shot `-p`   ─┼──▶  ┌──────────┐
   your own app    ─┘     │  conway  │  the facade — one API, three consumers
                          └────┬─────┘
                               │
        ┌──────────────┬───────┴───────┬──────────────┐
        ▼              ▼               ▼              ▼
   agent loop &    tool registry   session log    domain types
   context build   & built-ins     (append-only)  & the 21 "ports"
   (conway-        (conway-tools)  (conway-       (conway-core)
    runtime)                        session)
        │              │               │              │
        └──────────────┴───────────────┴──────────────┘
                               │
                    ┌──────────┴───────────┐
                    │   THE PLUGIN TIER    │   nine crates, all optional
                    └──────────────────────┘

  in-process, compiled in:   routing · backends* · history · stepguard ·
                             skills · memory · skeleton
  OUT of process, no rebuild: subprocess-host · mcp-client
                                          (* the only one on by default)
```

A "port" is a socket in the core that something else plugs into: where a model
gets called, where permission is decided, where the log is written, where
context is edited. There are 21 of them
(`crates/conway-core/src/ports/`). **Every one has a working implementation
living outside the core**, which is the property that makes the architecture
real rather than decorative — with one exception, discussed in §3.

**Size.** About 36,000 lines of production code, 35,000 lines of comments in
that same code, and 131,000 lines of tests across 2,842 test functions. The
comment-to-code ratio is roughly 1:1 and the test-to-code ratio roughly 3.6:1.
That is unusual and, on the evidence of this review, load-bearing: nearly every
question I asked was answered by a comment that had already anticipated it.

**The bottom-right box is the news.** `conway-plugin-subprocess` (4,099 lines)
and `conway-plugin-mcp` (1,788 lines) let you name a command in your settings
file, and conway spawns it, asks it what tools it has, and offers them to the
model. No Rust, no recompile, no fork of the project. Between them they are the
largest capability in the tree and the strongest answer conway has to its own
central question — *can I extend this without forking it?*

---

## 2. Scored against [`INTENT.md`](INTENT.md)

### §2 Weight: a small core with capability from outside. **Strong.**

The core is 4,161 lines of code and holds no policy. Two mechanisms that a
harness normally owns outright have been physically moved out of it and now
install like anything else: model routing (`crates/conway-plugin-routing`) and
repeated-step detection (`crates/conway-plugin-stepguard`). The provider
adapters are also plugins; they are simply attached by default, because a
harness that cannot reach a model is inert rather than unopinionated.

The test you set — *can I swap this out without forking the project?* — now
passes at every level including the one that used to be theoretical.

### §3/§5 Context as a tree; objects versus paths. **Built, unconsumed, and the decision point of this round.**

This is the belief the whole design hangs off, and it now has about 5,500 lines
of machinery behind it: a vocabulary for what a context path is, a validator
that refuses to build an incoherent one, a store that can persist and share a
selection, a `Curator` socket a plugin plugs into, and a stage in the agent loop
that calls it every turn. The code is good. I checked the refusal behaviour, the
cross-session reach, and the composition of several curators, and all of it does
what it says.

**Nothing uses any of it.** Specifically, and each of these was verified this
run:

- No curator exists anywhere in the project. The only thing implementing the
  socket is the wrapper that would combine several of them
  (`crates/conway/src/builder.rs:1798`), and unit-test doubles.
- The store that would persist a selection is never constructed
  (`FsPathStore`, `crates/conway-session/src/path_store.rs:482`).
- The operation that assembles a new path is never called outside its own tests
  (`derive_with`, `crates/conway-core/src/path.rs:798`).
- The resolver that would make paths the real way a turn is assembled is
  written, tested, and deliberately **not wired**; a translation bridge
  reproduces the old behaviour instead
  (`crates/conway-runtime/src/context/path.rs:489`).
- The label meaning "the operator chose this" is produced in one place: a unit
  test (`Selector::Operator`, `crates/conway-core/src/path.rs:121`). Its own
  documentation says it arrives "through `conway path` verbs". There is no
  `conway path` command.
- An embedder cannot supply a path store at all — the socket is not re-exported
  by the facade and there is no builder method for it.

Read one at a time, each of those is a reasonable staging decision with an open
board item behind it. **Read together, they are six accommodations around one
premise**, which is the pattern [`REVIEW-PROMPT.md`](REVIEW-PROMPT.md) §2 asks
this review to look for. The premise is that record-granular path curation is
the mechanism memory, compaction, and operator curation would all be built on.
Of those three, memory was built on it, failed, and was **moved off it**
(`crates/conway-plugin-memory/src/lib.rs:4-33` records why, honestly and in
detail). The other two are unwritten.

So the seam's remaining justification rests entirely on a compaction plugin
nobody has written and an operator command nobody has built. That may well be
right — it is a genuinely well-shaped seam and compaction is the obvious fit.
But it is a prediction, and the last prediction of this exact kind cost a
rebuild. **The question for you is not "should we finish wiring it" but "what
would prove this seam is the right one, and can that proof be produced cheaply
before more is built on it?"** §5 has a concrete proposal.

The parts of §5 that *are* live are good and worth saying: forking is O(1) and
shares a byte prefix, siblings forked at one point share a cache, a finished or
crashed session can still be forked from, and every context segment carries
where it came from.

### §6 Plugins all the way down. **Strong — and the ceiling moved this cycle.**

`PHILOSOPHY.md` §5 says the two extension costs are hooks (a shell script) and
plugins (Rust, compiled). That is no longer the whole picture, and the gap is in
conway's favour: an external program in any language can now register tools over
a wire protocol, and a standard MCP server can be pointed at directly. Eight
hook events fire from real code paths, and a plugin can declare its own events,
its own slash commands, its own permission rules, and its own status line
contributions.

The one thing you still cannot do without recompiling is add a *backend*
(a new inference provider) out of process.

### §7 Three surfaces. **Two strong, one thinner than it looks.**

**One-shot (`-p`) is excellent.** Twenty-one flags, three output shapes, strict
separation of model output from diagnostics, stable exit codes, fails closed on
permissions, and a JSON-schema result contract enforced identically for every
provider. It works outside a repository.

**Embedded is strong, and the falsifiable claim you asked for in §7 is proven in
runnable code**: `crates/conway/examples/bare_inference.rs` configures conway
down to one bare inference call — no tools, no agent behaviour — using only
mechanisms a third party also has. That claim was never added to the machine-
checked ledger the way §7 said it should be, which is a small miss.

**The terminal application is the thinnest of the three**, and that matters most
because §7a says this surface is the forcing function for everything else. It
has a live command palette, an agent-tree panel, permission prompts with a
grant-scope choice, an `/ask` side-question modal with three exits, a settings
screen that can revoke individual rules, and natural-language intent
confirmation. What it does not have is discussed next.

### §5c Changing model mid-session. **Not possible. This is a real gap.**

[`INTENT.md`](INTENT.md) §5c is unusually emphatic: *"Changing model mid-session
is ordinary, not exceptional… A design that makes model changes awkward has
failed regardless of what else it gets right."*

There is no `/model` command, no role or model argument on `/fork` or `/spawn`
(`crates/conway-cli/src/tui/commands.rs`), and no facade method that changes a
session's model or role. The only levers are `--model` and `--role-override` at
process start. To use a cheaper model for a mechanical stretch of work you
restart conway.

The architecture is *ready* for this — §5c's own reasoning about selections
being model-free was written precisely so this would be cheap. Nothing consumes
that readiness.

### §7a/§7b The dogfooding ladder. **Still at rung zero.**

The intake machinery shipped: [`docs/dogfooding.md`](../dogfooding.md) and
`scripts/dogfood-note.sh`, which turns a moment of friction into a board item in
one command. The script defines its own falsifiable marker — every item it
creates is titled `[dogfooding] …`.

**Across all 358 board items, that marker appears zero times.** No session of
real work has been recorded through the mechanism built to record it.

There is one encouraging counter-signal: uncommitted work in the tree right now
adds an in-prompt permission-pattern editor to the TUI, which is exactly the
shape of change daily use produces. It has no board item.

§7a says *"is this needed to dogfood conway as a full-time coding agent?"*
outranks architectural tidiness when the two disagree. Since the last review the
work has been almost entirely architectural.

### §8.1/§8.3 and `CONTRIBUTING.md` §2: nothing may claim to be reached that isn't. **RED, in both directions, on two gates.**

This is the rule `CONTRIBUTING.md` calls the one the project has "broken most
often." Two CI gates are failing on the current tree:

**Gate one — the design-claims ledger.** `python3 scripts/check-design-claims.py`
reports 1 of 18 claims stale (`scripts/board-claims.md:100`). CI runs it
(`.github/workflows/ci.yml:92`). The stale claim asserts that memory, skills and
MCP are unbuilt. All three ship.

**Gate two — dependency advisories.** `cargo deny check` reports
`advisories FAILED`. Board item `01M0ASG80Q2ZWR8201F580M7RR` covers it.

The ledger failing is the mechanism working — it is doing exactly the job it was
built for. What it caught is that **five top-level documents currently understate
what conway can do**, which is the same defect as overstating and, per
`CONTRIBUTING.md` §2's own account, the kind that has bitten this repository
before:

| Document | Says | Actually |
| --- | --- | --- |
| `PHILOSOPHY.md:606`, `:651` | compaction, memory, skills, MCP "not written at all" | three of the four ship and install |
| `ARCHITECTURE.md:192` | "memory, skills, and MCP support remain separate, later work" | all three are crates in the tree |
| `README.md:166`, `:193` | "four members shipping today"; memory/skills/MCP "unbuilt" | nine plugin crates; three of them are these |
| `GUIDE.md:270`, `:273` | "no memory, skills, or MCP support"; **"there is no runtime plugin host — plugins are compiled in"** | all three exist; two runtime plugin hosts ship |
| `PHILOSOPHY.md:182`, `ARCHITECTURE.md:372` | the filesystem root has a symlink-swap race, and closing it "is out of scope here" | closed — `crates/conway-tools/src/fs/beneath.rs` does open-relative enforcement via `cap-std` |

The `GUIDE.md` line is the worst of these. It tells a reader that conway's
central promise — extend it without forking it — is unmet, at the exact moment
it became met.

The `PHILOSOPHY.md` §1 / `ARCHITECTURE.md` §3.4 row is the second worst, because
it is security-bearing and it understates a *guarantee*. The task-level docs
(`docs/tools.md:47`, `docs/plugins/trust-and-security.md:373`) already describe
the fix correctly; only the two top-level pages are stale.

None of this is on the board.

---

## 3. Findings

Ordered by what I would want decided first. All are now filed;
[`PLAN.md`](PLAN.md) groups them by which files they collide on.

**F1 — Two CI gates are red on `main`, and five documents understate the tree.**
The table in §2 is the whole finding. Mechanical to fix, and there is nothing to
decide; the only cost of delay is that every reader in between is misinformed
about a capability that works. *Filed 2026-08-20: `01M0EM97X118CZ43CGEPH2PB8F`
(the red ledger), `01M0EM83C5ZZX75MSE0MTV7NZW` (five documents),
`01M0EM8NK2R36X4TW50D43S58H` (the stale confinement notes),
`01M0EKTVJE558SB4S6K3YYVXVZ` (`permissions.md`, a sixth document found while
reconciling the board).*

**F2 — The curation seam has no consumer, and its one consumer was moved off it.**
About 5,500 lines, six independent accommodations, and a premise whose last
test failed. The right response is probably not "wire the rest"; it is to decide
what would prove the seam and produce that proof cheaply. *Now on the board: `01M0EMAC4CCDQ8QJYM21RXPKRY` asks the question, and
`01M08F5XYFZ0JY42HW789AHX9J` (wire the resolver) was made to depend on it — it
was claimable before 2026-08-20 and deliberately is not now.*

**F3 — Memory is real, tested end to end, and forgets everything on restart.**
The shipped binary installs the in-memory store while the durable one exists,
finished, one crate away (`crates/conway-cli/src/first_party_plugins.rs:174`;
`FsMemoryStore` at `crates/conway-session/src/memory_store.rs:163`). This is
stated plainly and at length in the code comment, which is the right conduct —
but it is stated nowhere a *user* looks, because `conway.memory` has no
user-facing documentation at all. *On the board:
`01M09V3S2AQYB2VK6MANFRH1JM`.*

**F4 — Three shipped plugins have no documentation.** `conway.memory` and
`conway.skills` appear in no page under `docs/`, in no README, and in neither
top-level specification. The out-of-process plugin host has one reference page
(`docs/plugins/subprocess-plugins.md`) and is absent from `README.md` and
`PHILOSOPHY.md` entirely. A capability nobody can find is not far from a
capability that does not exist. *Filed 2026-08-20:
`01M0EMAWY0B62RC966FQMQPGAC`.*

**F5 — You cannot change model without restarting.** Against an intent document
that calls this "one of the most consequential decisions available" and says
people "revise it constantly." *Filed 2026-08-20:
`01M0EM9RW7AYZAYXE5Z2XPNFND`.*

**F6 — There is no operator surface for anything built this cycle.** No
`conway path`, no `conway memory`, no `/memory`, no `/model`. The sockets exist;
only an embedder writing Rust can reach them. This is the question
[`INTENT.md`](INTENT.md) §11 records as open and unresolved. *Filed 2026-08-20:
`01M0EMD54BWAVZGYWPXP4S5P1J` (`/memory`) and `01M0EMC19FV3FJFJVR5697AV44`
(settle `conway path`, blocked behind the curator proof).*

**F7 — Dogfooding has produced nothing.** Zero `[dogfooding]` items in 358.
Rung one of the ladder — *used alongside your current harness, for real work, by
choice* — has not been reached. *The intake item is closed; the outcome is
untracked. Filed 2026-08-20: `01M0EMBF2ZFJA5Z3NE21FYN8RF`.*

**F8 — Small things, verified.** All three are filed:
`01M0EMEQJHPR3XVNAN39YX7C38` (the ledger claim §7 nominated) and
`01M0EMDVBJVT510GBJHPWBZ3G6` (the pattern editor's missing spec); the memory
coverage gap is folded into `01M0EMAWY0B62RC966FQMQPGAC`'s page.
 The bare-inference claim `INTENT.md` §7 says
"belongs in the ledger" was never added to `scripts/board-claims.md`. The memory
plugin's write path (the `remember` tool) is unit-tested and its read path is
integration-tested, but no single test drives tool → store → context in one run;
the two halves share a real store, so this is a coverage gap rather than the
one-directional-verification defect, but it is the shape to watch. The
in-flight permission-pattern editor has no board item.

---

## 4. What is good, said plainly

A review that only lists problems is not a state of the union, and this tree has
had an unusually strong cycle.

**The out-of-process plugin host is the real thing.** Two wire transports, a
version handshake, per-call deadlines, graceful degradation on unknown message
tags, a plugin that declares the host capabilities it needs and is refused if
they are absent, and a permission policy a remote plugin can declare that can
narrow but never widen. This is the capability that turns "everything is a
plugin" from a claim into a property.

**The filesystem confinement fix is excellent work.** `conway.fs` now enforces
its own root through open-relative syscalls, using `cap-std` rather than a
hand-rolled loop, with the reasoning for that choice written down and the
remaining uncovered case (`glob`/`grep` tree walks) disclosed rather than
glossed. A check-then-open race in a security boundary was closed properly.

**The memory rework is the best conduct in the repository.** A design document
predicted memory would need no storage of its own. It was built that way, it
failed on five specific counts, and the plugin was rebuilt on a store of its
own. The module doc names all five failures, names the cap that had been written
up as "bounded by construction" as the tell, and says the seam it abandoned is
still the right seam for something else. Then the lesson was pushed up into
[`INTENT.md`](INTENT.md) §5e as a general rule about design documents being
hypotheses. That is the loop this project says it wants, executed.

**The honesty machinery works.** The ledger caught the stale claim without
anyone looking for it. The citation checker verified 401 citations across 452
files with zero stale. The architecture invariants are machine-checked, down to
"`conway-core` may do I/O in exactly one file."

**The comments.** Roughly one line of prose per line of code, and it is the good
kind — why a choice was made, what was rejected, what a mechanism deliberately
does not cover. Several findings in this review came from a comment that had
already anticipated the question and answered it against its own interest.

---

## 5. What I would do next

Four moves, in this order. Every one is a claimable board item;
[`PLAN.md`](PLAN.md) says which of them collide on shared files.

1. **Green the gates and fix the six documents this week.** It is mechanical, it
   needs no decision, and it is the rule you have said matters most. Fixing
   `GUIDE.md:273` alone changes what a new reader believes conway is.
   (`01M0EM97X118CZ43CGEPH2PB8F` and `01M0EM83C5ZZX75MSE0MTV7NZW` land together;
   then `01M0EM8NK2R36X4TW50D43S58H`, `01M0EKTVJE558SB4S6K3YYVXVZ`,
   `01M0EMAWY0B62RC966FQMQPGAC`, `01M0EMEQJHPR3XVNAN39YX7C38`.)

2. **Answer the curation question before writing more curation code.** My
   suggestion: do not start `conway.compaction` and do not build `conway path`
   yet. Instead spend one small item writing the *cheapest possible* curator —
   "drop tool results older than K turns" is about fifty lines — and run it
   through the real seam on a real session. If it works, the seam is proven and
   everything else is wiring. If it does not, you have found that out for fifty
   lines rather than five hundred. Either answer is worth more than another
   wiring item. (`01M0EMAC4CCDQ8QJYM21RXPKRY`, which now gates three items
   behind it.)

3. **Give the operator something to type.** `/model` is the highest-value single
   command in the tree, because `INTENT.md` §5c says so and because it is the
   thing you will want on your first real session. `/memory` is second.
   (`01M0EM9RW7AYZAYXE5Z2XPNFND`, then `01M0EMD54BWAVZGYWPXP4S5P1J`.)

4. **Then use it.** Rung one is a session of real work, not a feature. Every
   review since the beginning has audited prose against prose; the one input that
   would change what this document says next time is friction from an actual
   afternoon of use. (`01M0EMBF2ZFJA5Z3NE21FYN8RF` — run it alongside the rest,
   not after.)

---

## 6. Questions for you

These are findings about [`INTENT.md`](INTENT.md), not about the code. All three
are written up as open questions in that page's §11, in its own terms — I have
not decided any of them.

1. **Does an operator get a curation command?** Still open from the last run
   ([`INTENT.md`](INTENT.md) §11). Nothing about the tree has changed: the
   machinery is there and there is nothing to type.

2. **What proves an extension surface?** `INTENT.md` §8.5 says extension must be
   low-friction, and `PHILOSOPHY.md` §5 says a first-party plugin needing a
   private interface is a bug report against the plugin API. Neither says what
   makes a surface *proven*. The `Curator` port is the case that needs the
   answer.

3. **Does a shipped capability owe a page?** Three plugins ship undocumented.
   `CONTRIBUTING.md` §1 has a docs gate for changes; there is no rule that a
   plugin crate implies a user-facing page, and the absence is why F4 happened
   quietly.
