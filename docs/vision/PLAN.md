# Plan of attack

**Drafted 2026-08-14 from [`STATE-OF-THE-UNION.md`](STATE-OF-THE-UNION.md).
Revised the same day** once the three open questions were answered — the answers
added a domain, reordered two, and changed what the compaction plugin is.

> Snapshot document — replaced wholesale on the next run of
> [`REVIEW-PROMPT.md`](REVIEW-PROMPT.md). For the reasoning behind any item, read
> the review; this page is the dispatch sheet.

---

## How to read this

Work is grouped into **eight domains**. A domain is a unit of *ownership*, not of
scheduling: it names a set of files that exactly one agent may write at a time.
Inside a domain, items are sequential. Across domains, everything is parallel
unless an explicit dependency says otherwise.

Fan-out is chosen at dispatch time, not baked in. Three agents is a legitimate run
of this plan. Twenty is also legitimate. The dependency edges are what make both
safe.

Sizes: **S** ≈ a session. **M** ≈ a few sessions. **L** ≈ a design pass followed
by a few sessions. **XL** ≈ a written design, reviewed, before any code.

### What changed in the revision

- **D1 is new and it is the keystone.** "Merging" turned out to mean a first-class
  *path* — an ordered selection of immutable records — and it lives in the core.
  Four other domains wait on it.
- **Skills and memory moved up.** The CLI has to become the daily driver, and those
  two are what stand between conway and being dogfooded. They are gating items, not
  shelf-stocking.
- **Compaction is a different plugin than it was.** Not a summarizer. A mechanical
  cherry-picker over immutable records, which is why it now depends on D1.
- **D5 is new** — the embedding surface, which the first draft scored through the
  wrong measurement.
- **The default-tier question dissolved.** The specification needs to distinguish
  the harness from the shipped application, not gain a middle tier of plugins.

### And in the second revision, same day

- **D1's shape is settled, and its hazards are named.** Nodes are referenced and
  never copied; a graph is owned by one session and freely rearranged; deriving a
  graph touches no node and no other graph. Five ways to get it wrong are enumerated
  in `INTENT.md` §5b and are now required content of D1-1's written design.
- **The direct-inference call is dead and something better replaced it.** No second
  API. Instead: prove conway can be *configured* down to a bare inference call, and
  treat whatever stops you as a defect in the composition surface.
- **A competitive survey lands early**, because it decides what the plugin shelf
  should hold, and it costs a fraction of building the wrong thing.

---

## Filed on the board, 2026-08-14

Charter: **`01M00QCEK68J1KF5YFCFXZKFYV`** — `[VISION] The context path, the three
surfaces, and the dogfooding bar`. Eight children, all claimable:

| Item | Covers | Board id |
| --- | --- | --- |
| Doctrine | D0-1…D0-7 | `01M00QD5SM21WAPJ0X29H6P5J8` |
| Design the context path | D1-1 | `01M00QDYK4T5MZNCTQ0ZXEBSZX` |
| The catalogue | R1 | `01M00QEQ0PVAM2S7Y9EQNZV32F` |
| PrefixKey ⟹ wire bytes | D1-2b | `01M00QF4WSAD3RYB8PZN7ZKPFB` |
| Non-Rust binding survey | D5-0 | `01M00QFMV84FTD3F6HVHCJRZN2` |
| The command line | D4 | `01M00QG7GHHVDKRC0J87NH0FNR` |
| Configure-down experiment | D5-2 | `01M00QGJEF40GGHP6SAD6Z8Z6H` |
| Embedding ergonomics | D5-1, D5-4, D5-5 | `01M00QGYR1M8F71HTAA1S3PEKS` |

**The rest of this plan was already tracked** and was referenced rather than
duplicated — the live board (MCP `work_list`, *not* `.ideate/work-items/*.yaml`,
which is a dead all-done export) held 32 open items when this was filed:

| Plan item | Existing board id |
| --- | --- |
| D2-1 confinement into `conway.fs` | `01KZDC30CBY9CPJ8YEM7HSRV0Y`, charter `01KZVYJ0MH5D4DKJBCY1XEXSJY` |
| D2-3 extract `conway-testkit` | `01KZVYWNA24EYMPVW3NPGBW51M` |
| D3a–D3e the plugin shelf | `01KZYM81YFE08ASM225A1R5H5X` |
| D3c out-of-process host | `01KZY8PATND84AKY0J376E3DWV` |
| D1-5 reachable `ContextMask` | `01KZY8QRAVVVKCRBZ6HAEGW3GG` |
| D6-1 / D6-2 split `state.rs` / `app.rs` | `01KZY8RARAGRJYJ202ARA4SYEM` / `01KZY8RV1H64T60WW9N3H4JCT1` |
| Dogfooding intake | `01KZY8V4MYNZJABZR0X0SJ2G5Y` |
| Cache degradation across turns | `01KZHDZKQXNYJME2CA3K52RNNY` |

Three of those need their scope amended rather than rewritten — the charter says
which and why: skills and memory become gating items, compaction becomes a
cherry-picker rather than a summarizer, and the subprocess host should not commit to
a protocol before the binding survey lands.

D1-2 through D1-5, D5-3, D6-3, D6-4 and all of D7 are deliberately unfiled: they
decompose from the path design, and filing them now would be guessing at a boundary
that has not been drawn yet.

## The ownership map

**An agent may write only the paths its domain owns.** Anything else is a request
to the owning domain, made in the completion report.

| Path | Owner | Notes |
| --- | --- | --- |
| `PHILOSOPHY.md` · `ARCHITECTURE.md` · `scripts/board-claims.md` · `docs/vision/*` | **D0 only** | See "The serialized files". |
| `docs/vision/CATALOGUE.md` (new) | **R1 only** | The one exception to the row above. R1 is read-only everywhere else, so it collides with nothing. |
| `crates/conway-core/src/path*.rs` (new) · `segment.rs` · `provenance.rs` | **D1 only** | |
| `crates/conway-session/src/**` | **D1 only** | |
| `crates/conway-runtime/src/context/**` | **D1 only** | |
| `crates/conway-core/src/containment.rs` · `ports/**` · `fakes.rs` | **D2 only** | File-level split inside `conway-core`. If D1 and D2 collide on a file, **D1 wins and D2 rebases** — the path is on more critical paths than the gaps are. |
| `crates/conway-tools/src/fs/**` | **D2 only** | |
| `crates/conway-plugin-skills/` · `-memory/` · `-compaction/` · `-mcp/` · `-host/` | D3a…D3e | One new crate each, no shared files. |
| `crates/conway-cli/src/first_party_plugins.rs` | **shared, append-only** | One `pub fn <name>_bundle()` per plugin. Never reorder or reformat existing entries. |
| `crates/conway-cli/src/cli.rs` · `oneshot.rs` · `commands/**` | **D4 only** | |
| `crates/conway/src/builder.rs` · `presets.rs` · `examples/**` · `docs/embedding.md` | **D5 only** | |
| `crates/conway-cli/src/tui/**` | **D6 only** | D7 waits on D6-1. |
| `crates/conway/src/**` (rest of facade) | **shared, by item** | Each item names the exact function it may add. Never restructure. |
| `Cargo.toml` (workspace) | **shared, append-only** | Members are a `crates/*` glob — a new crate needs no edit. Only `[workspace.dependencies]` additions, appended. |

### The serialized files

`PHILOSOPHY.md` and `scripts/board-claims.md` genuinely cannot be parallelized: CI
enforces that a "Where the tree is today" note disappears in the *same change* that
makes it false, and the claim ledger is checked on every commit.

Every such landing splits in two:

1. The capability item (parallel, owned by its domain) — builds the thing, touches
   neither file, and ends with the exact replacement prose in its report.
2. A **doctrine item** (serial, owned by D0) — a five-line edit that deletes the
   note and inverts the predicate.

D0 drains its doctrine queue one item at a time. No capability item ever blocks on
it, and the ledger is never in a broken intermediate state.

---

## D0 — Doctrine

Owns the specification. One agent, start to finish. Two of its items unblock other
domains, so it starts first.

| # | Item | Size | Depends on |
| --- | --- | --- | --- |
| D0-1 | **Write the objects-and-path model into `PHILOSOPHY.md`.** Turns are immutable objects; an agent's head is a pointer; the path through them is a separate thing; curation acts on the path and never on the turns. State it as an idiom with a mechanism underneath it, and state explicitly that *which objects belong on a path* is policy the core does not hold. Source: `INTENT.md` §5a. | M | — |
| D0-2 | **Separate the harness from the shipped application.** §5's membership test currently answers "what must exist for conway to work?" and is being asked to answer "what should the binary ship with?" — two questions with different answers. Split them. The binary may be opinionated; the harness may not. Source: `INTENT.md` §7a. | M | — |
| D0-3 | **De-crosscut `PHILOSOPHY.md`.** Mechanism the harness guarantees and practice the operator recommends currently interleave. Separate them visibly so an idiom cannot harden into a law by accident. | M | D0-1, D0-2 |
| D0-4 | **Write the three surfaces as three.** The page names embedding as a consumption mode and then treats it as the substrate for the other two. Give it equal standing. Source: `INTENT.md` §7. | S | D0-3 |
| D0-5 | **Write the dogfooding ladder into `INTENT.md` as a checklist.** Two rungs — *supplement* and *no longer needed* — with the second gated on feature coverage **and** output quality better than the incumbent's, not comparable to it. Source: `INTENT.md` §7b. | S | R1 |
| D0-6 | **Add the composability claim to the ledger:** conway can be configured down to a bare inference call using only mechanisms a third party also has. Falsifiable, and it fails loudly if the harness gets heavier. | S | D5-2 |
| D0-7 | **Doctrine queue.** One small serial item per capability landing from D1–D7: delete the stale note, invert the claim predicate. | S each | each capability item |

### R1 — The catalogue *(research, runs alongside D0)*

Not a domain: one read-only research item that writes exactly one new file
(`docs/vision/CATALOGUE.md`) and therefore collides with nothing. Give it its own
agent and start it in Round 1.

**The item.** Read what Claude Code, the [DeepSeek harness](https://deepseek.com/harness/en/),
and [Hermes Agent](https://github.com/NousResearch/hermes-agent) actually do and how
each approaches agentic coding. Hermes deserves the most attention: it ships skills,
persistent memory, and a learning loop as *harness* features rather than as things
each user hand-builds, which is the closest existing answer to the question conway's
plugin shelf is asking.

**The deliverable** is a catalogue of what should be **available in a default
installation** — available and one toggle away, not enabled. For each entry: what it
is, which of the three surfaces it serves, whether it is a plugin or a hook or
configuration, and what it would take. Size **M**.

**Why it is Round 1.** It decides what D3 builds. Building the shelf before knowing
what belongs on it is the expensive version of this work.

**Done looks like:** a reader can tell on every page whether they are reading a
guarantee or a suggestion; the path model is specified without the core acquiring a
curation opinion; and there is a written bar for "dogfooded."

---

## D1 — The context path *(keystone)*

The largest item on this page, the only one in the core that four other domains
wait on, and the one where a wrong boundary is expensive to undo. One agent.
**Design first, in writing, reviewed, before any code.**

| # | Item | Size | Depends on |
| --- | --- | --- | --- |
| D1-1 | **Design the path.** A nameable, persistable, ordered selection of immutable records that an agent's context is assembled from. Deliverable is a written design, reviewed, not code. See the settled inputs and the required answers below. | XL | D0-1 |
| D1-2 | **Make cache cost legible.** The economic argument for cherry-picking over summarizing is that a derived path shares a byte prefix with its origin up to the first omission — so the price of a curation decision is knowable *before* it is made. Something has to compute and expose that, or the argument stays theoretical. | M | D1-1 |
| D1-2b | **Pin the property the whole cache story rests on:** equal `PrefixKey` ⟹ byte-identical wire prefix. Nothing asserts this today. There is a golden test for context assembly and a byte-identity test for stripped cache hints, but no test tying the internal dedup key to what actually reaches a provider — so a wire-layer change could decouple them and the only symptom would be a bill. | S | — |
| D1-3 | **Implement path assembly.** Context building reads a path rather than applying a fixed rule. The existing behaviour becomes the default path, byte-for-byte identical — this must be provable, not asserted. | L | D1-1 |
| D1-4 | **Structural selection predicates.** The selectors a non-LLM curator needs: record type, provenance, tool name, touched path, token cost, heading structure. Mechanism only — no policy, no defaults, no built-in selection. | M | D1-3 |
| D1-5 | **Fold in `ContextMask`.** The existing per-record exclusion overlay is the seed of this mechanism and is reachable from nothing. Either it becomes the path's primitive or it retires; leaving both is the worst outcome. | M | D1-3 |

**Done looks like:** an agent's context is an explicit path; a plugin can derive a
new path from an existing one by mechanical selection; nothing in the core decides
what belongs on one; and the default path produces byte-identical requests to
today's.

**The line to hold.** The core owns *the ability to express and assemble a path*.
Which objects belong on it is policy and lives in a plugin. Every time this item
feels like it wants a default selection rule, that is the core acquiring the
opinion this whole project exists to keep out of it.

### D1-1's settled inputs — do not re-litigate these

Decided by the operator 2026-08-14, recorded in `INTENT.md` §5b. The design starts
from them rather than deriving them:

- **Paths span sessions.** They already do — a fork's inherited prefix is one. This
  is a description of the present, not a proposal.
- **Nodes are referenced, never copied.** Cherry-picking a record does not duplicate
  it. Records are git *blobs*; a path is a git *commit*. `git rebase` rewrites history
  without copying a byte of file content, and that is the move being made here.
- **A graph is owned by exactly one session**, and deriving one mutates no node and
  touches no other graph. This is the invariant to hold above all others.
- **Three layers, not two.** Record = blob (global, immutable). **Graph version** =
  commit (global, immutable, freely referenced by anything). **Graph head** = branch
  ref (owned by exactly one session, mutable). Ownership applies to the *head*; a
  *version* is shared freely. A graph must be able to reference another graph
  **version** as its prefix — reference a *head* and rearranging one session's path
  silently changes another's context.
- **Reuse `prefix_key`, do not invent a second identity.**
  `crates/conway-runtime/src/context/prefix.rs:20` already computes
  content-addressed, ownership-blind identity over exactly the shared portion of a
  context, and deliberately excludes per-agent segment ids so siblings hash equal.
  That is graph-version identity under another name.

### D1-1's required answers — all five, in writing, before code

From `INTENT.md` §5b. These are the ways this goes wrong, and every one is cheap on
paper and expensive in code.

1. **Coherence — answered, and it is now a settled input.** A rendered context must
   never carry a tool call without its result; providers reject the whole request.
   conway has the scar already — eight parallel forks landed on a prefix cut mid-batch
   and all eight died on their first request with zero steps
   (`crates/conway-runtime/src/context/builder.rs:28`).

   **Refuse, at derivation time.** An invalid path must never be created, so the
   *operation* that would produce one is what fails — loudly, with a typed error
   naming what it would have orphaned. No repair: there is no way to predict the
   correct fix, and choosing silently is guessing at intent. An invalid path is
   unrepresentable, not detected late.

   **Keep the existing render-time repair**, which covers a different case: a fork
   cut mid-batch is an accident nobody chose, and it keeps today's drop-and-record
   behaviour. Deliberate selection gets refused; harness-caused incoherence gets
   repaired. What remains for this item is the *implementation* question of where the
   validation boundary sits so the two cannot be confused.

   **And make the refusal usable** — name the valid neighbouring operation ("dropping
   record 7 orphans the call in record 6; drop both"). A refusal that only says
   "invalid" turns a safety property into an obstacle.
2. **Rearranging costs strictly more than omitting.** Omission preserves the byte
   prefix up to the first omission; reordering breaks at the first moved element.
   Both are legitimate, neither should feel like the other. Omission should be the
   cheap default; reordering a deliberate act with its price shown.
3. **Provenance must survive.** A graph drawn from three sessions either keeps
   origins legible or destroys the most valuable debugging property conway has.
4. **A graph pins the logs it references.** Ephemeral `/ask` children are discarded
   by design, so a dangling reference is reachable today with no new features.
   Retention needs a stated answer, not an emergent one.
5. **A person has to be able to see it.** As much a UX problem as a data-modeling
   one. Curation nobody can inspect is worse than none, because it is applied anyway.

**The precedent to follow** is already in the tree: when the harness must intervene
to produce a sendable request, the intervention goes *in* the record rather than
behind it. Everything built on paths inherits that obligation.

---

## D2 — Close the declared gaps

Owns the contract crate's existing modules and the filesystem tools. One agent,
sequential. Coordinates with D1 on `conway-core` per the ownership map.

| # | Item | Size | Depends on |
| --- | --- | --- | --- |
| D2-1 | **Move confinement into `conway.fs`.** The plugin that opens the file does its own checking, closing the window where a symlink created between check and open defeats the check. Retires the contract crate's one file-I/O exception and its CI guard. `crates/conway-core/src/containment.rs` records the four questions this has to answer first — answer them in writing before writing code. | L | — |
| D2-2 | **Let a context policy steer routing.** Today a hook sees which model was chosen and cannot influence it. Widen the payload so a hook may propose a role. Do not let it see request text if that breaks the router's content-blindness guarantee — that guarantee holds *by construction* today and is worth more than the feature. | L | D2-1 |
| D2-3 | **Extract `conway-testkit`.** Every seam has a fake and none are reachable outside this workspace. A third-party plugin author should be able to test against them. | M | — |

**Done looks like:** all three entries in `STATE-OF-THE-UNION.md` §4 are gone, and
the ledger entries pinning them are inverted or deleted.

---

## D3 — The plugin shelf

Five independent crates, five independent agents, no shared files except the
append-only registration file. Each is also a **test of the extension surface**: if
a capability cannot be written on the public API a stranger would use, that is a
finding against the API, and reporting it is worth more than working around it.

Ordered by what unblocks dogfooding, which is now the priority.

| # | Item | Size | Depends on |
| --- | --- | --- | --- |
| D3a | **`conway.skills`** — *gating for dogfooding.* Note the prior art: Pi keeps one line per skill resident and loads full instructions only on invocation, which is a direct application of the signal-to-noise argument. Copy the idea, not the implementation. | M | — |
| D3b | **`conway.memory`** — *gating for dogfooding.* Design question first: what is remembered, where it lives, and how it enters a window without violating "nothing arrives implicitly." Likely wants cross-session paths — see open question §7.2. | L | D1-1 |
| D3c | **The out-of-process plugin host** — a plugin that is a *program*, not a compiled crate. The tool-facing types are already serialization-ready and the tree names this transport as future work in several places. Independent of everything; start immediately. If it lands early, consider writing the remaining plugins against it — that would be the strongest possible proof the surface works. | L | — |
| D3d | **`conway.compaction`** — **rewritten by the revision.** Not a summarizer. A mechanical cherry-picker: derive a new path by selecting from immutable records, using structural predicates rather than inference. It must report what it dropped and what that cost in cache terms. An inference-driven variant may exist, but it is not the default and not the point. | M | D1-3, D1-4 |
| D3e | **`conway.mcp`** — a plugin that brings tools with it. Mostly a protocol adapter; the smallest design risk of the five. | M | — |

**Done looks like:** `plugins.install` accepts five names it does not accept today;
each has a doc page; each ships with the worked example that proves the surface it
used.

---

## D4 — The command line

Owns the command-line surface. One agent, or two if D4-6 is split out.

| # | Item | Size | Depends on |
| --- | --- | --- | --- |
| D4-1 | **`--agent`** — run one-shot mode as a named agent definition. They already load from `.conway/agents/*.md`; only a model or a role is selectable today. | S | — |
| D4-2 | **`--system-prompt` / `--append-system-prompt`** — stop every one-shot run from being the built-in coding agent. | S | — |
| D4-3 | **`--output-schema`** — structured output, so a pipeline stops parsing prose. | M | — |
| D4-4 | **Budget flags** — turn, token, and wall-clock ceilings. The runtime already enforces budgets; nothing exposes them to the command line. | S | — |
| D4-5 | **Streaming input** — a conversation over a pipe, not just prompt-in/stream-out. | M | D4-3 |
| D4-6 | **Plugin-contributed subcommands.** A plugin can add a slash command to the terminal app but not a subcommand to the binary, so the CLI is extensible inside and fixed outside. | M | — |
| D4-7 | **A non-coding quickstart** — reaching a model from a shell script with no repository, no coding agent, no tools. Proves the surface is real. | S | D4-1…D4-4 |

---

## D5 — The embedding surface *(new)*

Owns the builder, the presets, the examples, and the embedding documentation. The
surface with its own users that the first draft scored through the wrong
measurement.

| # | Item | Size | Depends on |
| --- | --- | --- | --- |
| D5-0 | **Survey how comparable Rust projects expose themselves to non-Rust hosts, then choose.** Answered yes (`INTENT.md` §7c), so this is design rather than a decision. Reach at least [Diplomat](https://github.com/rust-diplomat/diplomat) (proc-macro driven, no IDL, target list leads with C and C++, exists because ICU4X had this problem), [UniFFI](https://mozilla.github.io/uniffi-rs/) (IDL-driven, Kotlin/Swift/Python), and `cbindgen` as the floor. **Do not design a binding layer from scratch.** Deliverable: a written recommendation naming the tool and how it answers the three hard parts below. | M | — |
| D5-0b | **Build the binding layer** as its own crate depending on `conway`, never touching `conway-core` — the same shape as a first-party plugin. The core learns nothing about C. An adapter sitting further out is an acceptable outcome if direct bindings do not work. | L | D5-0 |
| D5-1 | **A builder that discovers instead of demanding.** Constructing a config today means supplying roughly fourteen fields before anything runs. Sensible discovery with per-field override. **No `libconway` and no second facade** — the isolation a split would buy is already enforced by the crate boundary (`crates/conway/Cargo.toml` carries no `clap`, `ratatui`, or `crossterm`). What makes embedding feel second-class is ceremony, not packaging. | M | — |
| D5-2 | **Configure conway down to a bare inference call** — no tools, no agent behaviour, one turn, out — using only mechanisms a third party also has. **This is an experiment with a report, not a feature.** There is to be no second API shortcutting the harness. Deliverable: the shortest working configuration, plus a list of everything that stood in the way. | M | — |
| D5-3 | **Fix what D5-2 found.** Every blocker is a defect in the composition surface, not a missing inference API — mandatory config fields, mandatory session ceremony, tools that register whether or not you want them. Ship the result as a preset, not as a new entry point. | M | D5-2 |
| D5-4 | **Examples that are not one.** There is exactly one example in the workspace and it runs against fakes. Add: inference against a real provider, an embedded agent with a custom gate, and a host consuming the event stream. | M | D5-1 |
| D5-5 | **Rewrite `docs/embedding.md` around the shortest path**, not around the full surface area. | S | D5-1, D5-4 |

**Done looks like:** a host application can go from `cargo add conway` to a model's
answer in a screenful of code, the documentation opens with that screenful, and a
C or C++ host can do the equivalent.

**The three hard parts D5-0 must answer**, all of which are constraints rather than
objections, and all of which the surveyed tools have already had to solve:

- **Async.** conway's facade is fully async and event-streamed. Who drives the
  runtime when the host owns the main loop, and how does a stream of events cross
  the boundary?
- **Panics.** Unwinding across an FFI boundary is undefined behaviour. Every entry
  point needs a catch, and the error taxonomy has to survive the crossing.
- **Ownership.** Who frees a returned string, and what happens when the host holds a
  handle past the harness's lifetime.

**No second facade.** A non-Rust host gets a projection of the same public API a
Rust host uses; anything it cannot reach is a gap in the projection rather than a
different product.

---

## D6 — Decompose the terminal app

Owns the terminal UI exclusively. One agent, sequential — refactoring does not
parallelize safely.

| # | Item | Size | Depends on |
| --- | --- | --- | --- |
| D6-1 | **Break up the state monolith.** 6,200 lines in one file is where feature accretion will happen and there is no structure resisting it. Split by concern, keep tests green, change no behavior. | L | — |
| D6-2 | **Same for the three other large files** (commands, app, input — 3,200–3,700 lines each). | M | D6-1 |
| D6-3 | **Make the terminal app a reference consumer.** Anything it reaches for that the public API does not offer is a finding against the API, not a reason to reach around it. Report those; do not fix them here. | M | D6-2 |
| D6-4 | **Make the shipped feature set toggleable.** Per D0-2, the binary is an assembly of plugins with every opinion visible and removable. Audit what the CLI does that is not switchable, and switch it. | M | D6-2, D0-2 |

**Done looks like:** no file over ~1,500 lines; behavior identical; a list of every
place the reference consumer had to reach past the public API; and every capability
in the shipped binary can be turned off.

**Constraint.** D6-1 through D6-3 are pure refactor. If they change user-visible
behavior, they have failed.

---

## D7 — Make the tree reachable

The idiom work, now sitting on top of a real mechanism rather than substituting for
one.

| # | Item | Size | Depends on |
| --- | --- | --- | --- |
| D7-1 | **Name paths and branches.** Navigating by session id is why the tree is invisible. | S | D1-1 |
| D7-2 | **`conway sessions graph`** — the branch structure, where two branches diverged, what each cost. The data is all recorded; nothing surfaces it. | M | D7-1 |
| D7-3 | **A context-tree view in the terminal app** — which path carries what, not just which agents exist. Show the cache consequence of a curation decision alongside it (D1-2). | M | D6-1, D7-2, D1-2 |
| D7-4 | **Tools for manipulating the path**, for a human and for a model. Deriving, cherry-picking, folding a child's distillate in. This is what "merge" means concretely. | M | D1-3, D1-4 |
| D7-5 | **A curation plugin** that uses all of it, proving the handles are sufficient. Per the operator's steer, the *policy* lives here and never in the core. | M | D7-4 |

---

## Suggested dispatch

**Round 1 — 7 agents.** D0-1/D0-2 (doctrine, with the operator), **R1 (the
catalogue — read-only research, writes one file, decides what D3 builds)**, D1-1
(the path design — written, reviewed, before any code), D3c (the out-of-process
host, independent of everything), **D5-0 (the binding survey — read-only, and its
recommendation should land before D3c commits to a protocol, since both are about
talking to non-Rust programs)**, D5-2 (the configure-down experiment — cheap, and
its report is a finding either way), D6-1 (terminal decomposition, independent and
slow).

Round 1 is deliberately weighted toward design, research, and independent work.
Four of the six produce documents or touch nothing anyone else owns. Fanning out
before D1-1 is reviewed means building on a boundary that may move, and building
D3 before R1 lands means building the wrong shelf.

**Round 2 — up to 14 agents.** D1-2 through D1-5; D2 as one sequential agent; D3a
and D3e (scoped by R1's catalogue), D3c continuing; D4-1 through D4-6; D5-1, D5-3;
D6-2; D7-1, D7-2; D0-5.

**Round 3 — whatever remains.** D3b, D3d (both waiting on the path), D5-4, D5-5,
D6-3, D6-4, D7-3 through D7-5, D4-7, and D0's doctrine queue draining alongside.

**The three rules that make any of this safe:**

1. Write only what your domain owns. Everything else is a request in your report.
2. Never touch `PHILOSOPHY.md` or `scripts/board-claims.md`. Hand D0 the prose.
3. If your work wants the core to decide *which records belong in a context*, stop
   and report it. That decision is the one thing that must not land in the core,
   and it will look reasonable every time it comes up.
