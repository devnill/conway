# State of the Union: conway

**Reviewed 2026-08-14 against `main` @ `c760266`, version 0.9.0.**
**Revised three times the same day** as the operator answered the questions it
raised. Eight asked, eight answered; six of the answers changed the plan rather than
confirming it. See §5, §6, and §8.

> Written for the operator. It assumes you care about the shape of the system and
> not about the shape of any particular trait. Everything here was checked
> against the code in this run; where you might want to verify something
> yourself, the file and line are given.
>
> Snapshot document — replaced wholesale on the next run of
> [`REVIEW-PROMPT.md`](REVIEW-PROMPT.md).

---

## The headline

**The foundation is done and it is good.** The hard, structural, expensive-to-fix
things — the primitives, the log, the port boundaries, the honesty discipline —
are built, tested, and correct. Six months of decisions have gone the right way
almost uniformly.

**What is missing is the second storey.** conway is a very well-built harness that
is still mostly a *coding* harness, still mostly extensible *by Rust programmers*,
and still carrying its heaviest component (the terminal app) as a monolith rather
than as the composition it tells everyone else to use.

Against [`INTENT.md`](INTENT.md), the scorecard:

| What you asked for | Where it stands |
| --- | --- |
| Small, agnostic core | **Strong.** The core holds no policy. Every opinion is either configuration, a hook, or a plugin. |
| No compaction, no hidden context edits | **Complete.** Nothing in the tree rewrites, drops, or summarizes context on its own. |
| Fork and spawn as the two primitives | **Complete**, and better than specified — a finished or crashed session can be forked, which most harnesses cannot do. |
| Plugins all the way down | **Half.** The *seams* are real. The *ecosystem* is four plugins, all Rust, all compiled in. |
| Context as a navigable tree | **Foundations only.** The data is all there. Almost none of it is reachable by a human or a model. |
| A CLI made for humans | **Good and heavy.** The terminal app is the nicest thing in the tree and also the least conway-shaped. |
| A CLI good enough to dogfood daily | **Not yet.** Missing skills and memory, which are load-bearing for full-time use. This is the forcing function everything else is untested without. |
| One-shot as a general LLM tool | **Weak.** Five missing flags away; without them it is a coding agent you can script. |
| Embedded in other applications, for inference | **Weakest area.** A host must assemble ~14 config fields, open a session, and run an agent turn before it can ask a model anything. There is no "give me a completion" path and exactly one example in the workspace. |

---

## 1. The system in blocks

```
                        ┌───────────────────────────────────┐
   how you drive it     │  TUI          -p / --print        │   ← conway-cli.
                        │  (terminal)   (scripts, pipes)    │     ONE consumer,
                        │            conway-cli             │     two modes.
                        └─────────────────┬─────────────────┘
                                          │
     your application ───────────────────►│   ← a Rust host embedding the
     (you write it; not in this repo)     │     library. A peer of conway-cli,
                                          │     not of its two modes, and
                                          │     privileged in neither direction.
                        ┌─────────────────▼─────────────────┐
   the public API       │    conway — this IS the library   │   ← there is no other
                        │  build a harness, open a session, │     library and no
                        │  prompt it, watch events          │     libconway to build
                        └─────────────────┬─────────────────┘
                                          │
                        ┌─────────────────▼─────────────────┐
   the engine           │        conway-runtime             │
                        │  the turn loop · context assembly │
                        │  fork/spawn · permissions · hooks │
                        └────┬───────────────────────┬──────┘
                             │                       │
                ┌────────────▼──────┐     ┌──────────▼─────────┐
   storage      │ conway-session    │     │  conway-tools      │  capability
                │ the append-only   │     │  read write edit   │
                │ log — the single  │     │  glob grep cd bash │
                │ source of truth   │     │  fork spawn ask    │
                └────────────┬──────┘     └──────────┬─────────┘
                             │                       │
                        ┌────▼───────────────────────▼──────┐
   the contract         │           conway-core             │   ← ~8.8k lines
                        │  types + the 12 extension seams   │      what a plugin
                        │  no policy, no opinions           │      author depends on
                        └───────────────────────────────────┘

   ─────────────────────────────────────────────────────────────────────────
   optional, installed by choice          ┌──────────────────────────────┐
   (none of these are in the diagram      │ routing · backends · history │
    above; the core never depends on      │ stepguard · skeleton         │
    any of them, in either direction)     └──────────────────────────────┘
```

**On "three ways to use conway" versus what this diagram shows.** Two different
axes were being conflated, and the first draft of this diagram conflated them
visually — it labelled the facade "three consumers" and drew two. Both statements
are true at different levels:

- **Three ways to use conway** is the *user's* choice: as a library, from a
  terminal, or one-shot from a script. This is the vocabulary `PHILOSOPHY.md` uses
  and it is correct.
- **One in-repo consumer of the facade** is the *dependency graph*. `conway-cli` is
  a single crate offering two modes to a human; a host application is a separate
  consumer that does not ship here. The TUI and one-shot mode are siblings inside
  one consumer — they share a process, a binary, and config discovery. An embedder
  is a peer of the binary, not of its modes.

The facade is **the** library. There is no `libconway` to build and no second
facade to write: `crates/conway/Cargo.toml` already carries zero CLI dependencies —
no `clap`, no `ratatui`, no `crossterm`, all of which live only in `conway-cli` —
so the isolation a split would buy is already enforced by the crate boundary. What
makes embedding feel second-class is ceremony, not packaging, and that is §3.3a.

**The one thing to take from this diagram:** the arrows only point downward, and
the bottom box has no opinions in it. That is the property that makes everything
above it replaceable, and it is genuinely true — machine-checked on every commit
by a CI test that fails if a forbidden dependency edge appears
(`crates/conway-cli/tests/cli_surface.rs`).

### Scale, honestly

| | Production Rust | Test Rust |
| --- | ---: | ---: |
| Everything | **~58,500** | ~102,000 |
| — the terminal app | 13,900 | 25,400 |
| — the engine | 11,300 | 21,200 |
| — the public API | 10,600 | 29,300 |
| — the contract others depend on | **8,800** | 7,500 |
| — everything else | 13,900 | 18,600 |

Two things worth noticing. The naïve line count of this repository is ~160,000,
and nearly two-thirds of that is tests — 117 separate test files. That ratio is
unusually good and is most of why the foundation can be called finished. And the
crate a third-party plugin author has to understand is 8,800 lines, which is the
number that actually matters for "is this thing lightweight."

---

## 2. What is genuinely good

**The log is the truth, and it is written before anything is acted on.** Every
turn, tool call, and result is appended to a plain JSONL file first. In-memory
state is a cache over that file. This is the decision that makes everything else
possible — forking a dead session, explaining why an agent did something
inexplicable, recovering from a crash mid-turn. It is not a common choice and it
is the right one.

**Fork is cheaper and stronger than it needs to be.** Forking writes one line and
copies zero records. Siblings forked at the same point share one allocation in
memory *and* one cache prefix at the provider — so ten children forked from one
point are largely paid for after the first. And because the thing being forked is
a record rather than a live process, you can fork a session that finished hours
ago, or crashed, at any point in its history. Most harnesses cannot do this at
all.

**Nothing touches your context behind your back.** No compaction anywhere. No
silent trimming. A request too large for its model is refused with an error that
names what did not fit, rather than being quietly moved to a bigger model. This is
§3 of `INTENT.md` and it is honoured without exception.

**The extension seams are real, and there is proof.** Four of them have been
tested the hard way — by taking something that used to be compiled into the core
and moving it out to a plugin that installs through the same surface a stranger
would use. Routing was a mandatory crate; it is now optional. Repeated-step
detection was in the agent loop; it is now `conway.stepguard`, and you decline it
by not installing it. Session rewind was going to be a TUI feature; it is
`conway.history`. This is the only convincing evidence that a plugin system works,
and this project has produced it four times.

**The honesty discipline is the best thing here.** `PHILOSOPHY.md` is written in
the present tense, including for things not yet built — and every such claim
carries a visible "Where the tree is today" note. What makes that safe rather than
sloppy is `scripts/board-claims.md`: a ledger of falsifiable predicates that CI
evaluates on every change, in *both* directions. If a promised capability ships,
the check fails until the note is deleted. If a shipped capability regresses, the
check fails too. This came out of a real incident where two documents describing
the same work diverged and the un-enforced one went stale. It is a genuinely
unusual piece of engineering culture and it should be protected.

**The permission model says what it actually does.** The documentation states,
plainly rather than reassuringly, that a confinement root confines path arguments
and does **not** confine what a shell command does — so an agent holding `bash` is
not confined by a root alone. Being told the limit is worth more than being sold
the guarantee.

---

## 3. Where it falls short of the intent

Five areas, in the order I would fix them.

### 3.1 Extensibility is real but the exits are narrow

*Intent §6: "can I swap this out or extend it without forking the project?"*

There are twelve extension seams in the contract crate and every one of them
works. But there are only two ways to reach them:

- **Write Rust and compile it in.** This is the only way to add a tool, a backend,
  a router, or a context policy.
- **Write a hook** — name an event, name a command. No language requirement, no
  build step. Seven events are wired: before and after a tool call, session start,
  prompt submitted, request assembled, child spawned, child reported.

The hook path is the right idea and it is under-built relative to its promise. It
can *observe* nearly everything and *deny* two things. It cannot supply a tool, a
model, or a context policy. So the moment someone wants to add capability rather
than react to it, they are writing Rust — and `INTENT.md` says most of what people
want should never require that.

The missing rung is an **out-of-process plugin host**: a plugin that is a program
rather than a compiled crate. The groundwork is deliberately in place — the
tool-facing types are already serialization-ready, and comments across the tree
name a future out-of-process transport
(`crates/conway/src/lib.rs:208`, `crates/conway-core/src/ports/plugin.rs:389`) —
but none of it is built. This is the single highest-leverage missing piece in the
whole system, because it converts "plugins all the way down" from a property of
the architecture into a property anyone can use.

### 3.2 The context tree exists in the data and almost nowhere else

*Intent §5: context as a navigable tree, not a line.*

Everything needed is already recorded. Sessions have parents. Forks name a point.
Every fragment of context carries where it came from. There is even a persisted,
reversible mechanism for excluding a record from what a fork inherits.

What is missing is every way of *using* it:

- The exclusion mechanism has **no way to invoke it.** Nothing in the entire
  workspace creates one outside of tests (`crates/conway-runtime`, `crates/conway`
  — verified by search this run). A user who wants it must append the record to
  the log by hand.
- There is a `conway sessions tree` command and a TUI agent panel, but no view of
  the *context* tree — which branch carries what, what each cost, where two
  branches diverged.
- "Merge the distillate back" is a thing you do by hand: read the child's answer,
  decide, retype. The primitives support it; nothing names it.
- Branches have no names. You navigate by session id.

So the idiom you described — break off a side branch, find out if it was worth
anything, merge it or let it die — is *possible* today and *supported* by nothing.
Per your steer, the curation policy belongs in a plugin. But the plugin needs
something to hold onto, and today the handles are missing.

> **Superseded in part by §5.1.** This section framed the gap as naming and
> visualization. It is bigger than that: the missing handle is a first-class
> *path* — an ordered, nameable selection of immutable records that an agent's
> context is assembled from. Read §5.1 before acting on this section.

### 3.3 One-shot mode is a coding agent with the interactive parts removed

*Intent §7: flexible programmatic use, not just one-shot inference.*

This is the weakest area relative to the intent, and it is also the cheapest to
fix.

`conway -p "..."` gives you: streamed output, three output formats, strict
separation of model output from diagnostics, stable exit codes, and a gate that
fails closed. That is a solid scripting contract and it is well documented.

What it does not give you is any way to be something other than a coding agent:

| You cannot | Consequence |
| --- | --- |
| select an agent definition | Agent definitions exist and load from `.conway/agents/*.md`, but only a model or a role can be chosen from the command line. |
| set or extend the system prompt | Every one-shot run is the built-in coding agent. |
| ask for structured output | No schema flag. A pipeline wanting JSON parses prose. |
| bound the run | No turn, token, or time limit on the command line. |
| stream input | Prompt in, stream out — nothing resembling a conversation over a pipe. |

Add those five and conway becomes a general-purpose way to reach a model. Without
them it is a coding agent you can call from a script, which is the thing you said
you were tired of.

Second-order: a plugin can add a **slash command** to the terminal app but cannot
add a **subcommand** to the binary. So the CLI is extensible on the inside and
fixed on the outside.

### 3.3a The embedding surface has the most ceremony and the least attention

*Added on revision. The first draft of this review treated embedding as the thing
the other two surfaces are built on rather than as a use case with its own users.
That was the same mistake the tree makes.*

conway is documented as serving three consumption modes equally, and at the level
of API design that is true — the terminal app and one-shot mode really are written
against the same public facade, with no privileged path. What is not true is that
the three are equally *usable*.

To get an answer out of a model from a host application today, you construct a
config object with roughly fourteen fields, build a harness, open a session, and
run an agent turn. The one example in the entire workspace
(`crates/conway/examples/minimal_session.rs`) spends its first sixty lines on
config construction before anything happens, and it runs against fakes.

There is no path that means *"ask this model this question."* Everything goes
through the agent loop. A host that wants routing, the log, and permissions but not
an agent has to take the agent anyway.

Two things would change this and neither is architectural surgery: a builder that
discovers sensible defaults instead of demanding every field, and a direct
inference call that reuses the routing and logging machinery without starting an
agent. What is genuinely open is whether the second one should exist at all — see
§6.3.

### 3.4 The plugin shelf is nearly empty

`PHILOSOPHY.md` promises a tier of capabilities that ship in this repository and
install by choice: dynamic routing, **compaction, memory, skills, MCP**. Routing
exists. The other four do not — which the tree tracks honestly, with a CI check
that will fail the moment one of them lands and the note is not removed.

This matters more than "four features are missing," for two reasons. It is the
difference between conway being usable in place of Claude Code (skills and memory
are load-bearing for daily use) and being a well-built demonstration. And each one
is a *test of the extension surface* — if compaction cannot be written as a plugin
on the public API, the public API is unfinished, and that is worth discovering.

### 3.5 The terminal app is the heaviest, least conway-shaped thing in the tree

13,900 production lines, 11,500 of them the terminal UI, with single files of
6,200, 3,700, 3,500, and 3,200 lines. It is the nicest part of conway to use and
it is built as a monolith.

Three consequences worth caring about:

- It is the part of the tree that most needs to demonstrate the philosophy — it is
  the reference consumer of the public API — and it currently demonstrates the
  opposite.
- It is where feature accretion will happen, and it has no structure that resists
  it. This is the exact failure mode `INTENT.md` §2 is about.
- Extending it means editing a 6,200-line state file, which is the friction
  `INTENT.md` §6 says should not exist.

Note this is a *shape* problem, not a *quality* problem. The code is tested at
better than 2:1, and the redesign it went through was real work. It does not need
rewriting. It needs decomposing.

---

## 4. Three known gaps the tree already admits

These are declared in `PHILOSOPHY.md` with visible notes and pinned by CI, so they
are debt-with-a-receipt rather than surprises. Listing them so the picture is
complete:

1. **Confinement is enforced in the wrong place.** The spec says a filesystem
   root belongs to the filesystem plugin, so that the same component both checks
   the path and opens the file. Today the harness checks and the tool opens —
   which leaves a gap where a symlink created in between defeats the check. Same
   move also retires the one place the contract crate does file I/O.
2. **Context policy can watch routing but not steer it.** A context hook sees
   which model was chosen and cannot influence the choice. Content-aware model
   selection therefore has to happen above the harness, in the caller.
3. **The test doubles are locked inside the contract crate.** Every seam has a
   fake, which is why this codebase is testable end to end with no network — but
   they are unreachable outside this workspace, so a third-party plugin author
   cannot use them.

---


## 5. The answers, and what they changed

The first draft of this review ended with three questions. All three have been
answered, and two of them changed the plan rather than confirming it. Recorded
here because a reader six months from now needs the resolution, not the question.

### 5.1 Merging means something, and it is bigger than naming

**The answer.** Every turn and tool call is an immutable object. An agent's head is
a pointer into that set. And — the load-bearing part — **the objects and the path
taken through them are separate things.** A context window is not the log; it is an
ordered *selection* from the log. Curation operates on the path and never on the
turns. `INTENT.md` §5a carries the full statement.

**Why it matters more than it sounds.** The worked example is compaction. The naive
form asks a model to summarize and puts prose where records were: lossy in a way
nobody can audit, and it destroys the cache because it rewrites the front of the
prefix. The form this model enables is **mechanical cherry-picking** — assemble a
new path from objects that already exist. Nothing is summarized, nothing is
rewritten, every record is byte-identical to one that was already there. Two things
fall out:

- **Cache cost becomes knowable before the decision is made.** Order preserved and
  bytes unchanged means a derived path shares a prefix with the original up to the
  first omission. Dropping from the tail is nearly free; dropping from the head
  spends everything. A policy can optimize against that; a summarizer cannot,
  because it changes all of it.
- **The curator does not have to be a model.** Structure — headings, record type,
  provenance, which file a turn touched, which tool ran, token cost — is all
  available without inference. Deterministic, testable, and incapable of
  hallucinating.

**What it changes in this review.** §3.2 said the context tree needed naming and
visualization. That was too small. What is actually missing is a **first-class
path**: a nameable, persistable, ordered selection of records that an agent's
context is assembled from, possibly spanning sessions. Today conway has exactly one
path per agent, computed by a fixed rule — a session's own records plus its
inherited prefix. Everything downstream (compaction, memory, the graph views, the
curation plugin) is waiting on that one piece.

This is now the largest single item in the plan and it sits in the core. The line
to hold: **the core owns the ability to express and assemble a path; which objects
belong on it is policy and lives in a plugin.** Getting that line wrong puts the
opinion back in the core.

### 5.2 The binary may be opinionated; the harness may not

**The answer.** The shipped `conway` binary should be fully functional and not
heavy, which is only a contradiction until you notice they are two different
questions:

- *What must exist for conway to work at all?* — the harness. The test stays sharp
  and narrow. Compaction, memory, and skills all fail it and none may be harness
  defaults.
- *What should someone get when they install the binary and run it?* — the
  application. A fully-equipped coding agent, opinionated on purpose, every opinion
  visible and removable.

An opinion in the binary is not an opinion in the core.

**What it changes.** The specification does not need a new middle tier of plugins;
it needs to distinguish the harness from the shipped application, which is a
smaller and cleaner edit. And it adds a priority that was not in the first draft:
**the CLI has to become the daily driver.** A tool only improves through daily use
by someone who notices what is wrong with it. Until conway is dogfooded full time,
its priorities are guesses — which makes `conway.skills` and `conway.memory`
gating items rather than shelf-stocking, and moves them up.

### 5.3 There are three surfaces, not two

**The answer.** Embedding conway inside another application to facilitate inference
is a use case in its own right, not the substrate the other two happen to sit on.

**What it changes.** A domain of its own in the plan, and the finding in §3.3a. The
first draft folded this into "general-purpose LLM access layer" and scored it
through the one-shot flags, which measured the wrong surface.

---


## 6. The second round of answers

The three questions §5 opened were answered the same day. None was a yes/no; all
three were "you are asking the wrong shape of question," and the corrections are
worth more than the answers.

### 6.1 Going straight to the model is a composition, not a feature

**The answer.** No second API that shortcuts conway. A parallel path through the
harness is something every future feature has to support twice, and building one
would be an admission that conway's own composition surface cannot express its
simplest case. What is required instead is that the plugin and configuration
architecture be flexible enough that **routing straight to inference is something
you configure** — no tools, no agent behaviour, one turn, out.

**What it changes.** The embedding domain loses "design a direct inference call"
and gains something better: *try to configure conway down to a bare inference call
using only mechanisms a third party also has, and report what stops you.* If
something does, the finding is not "conway lacks an inference API" — it is **conway
is too heavy and too opinionated to configure down**, which is a defect in the
composition surface and gets fixed there.

That is also a good falsifiable claim, and it should go in the ledger alongside the
others.

### 6.2 The graph is a separate artifact from the nodes

**The answer, and it settles the shape of D1.** Paths already span sessions — that
is what a fork's inherited prefix *is*, so this is a description of the present
rather than a proposal. What is new is that a path can be **rearranged**, and
rearranging must never affect any other session.

That looked like it broke the git analogy — if we select and reorder, we are
rebuilding a context by copying it, and copying is the thing conway does not do.
The analogy was just mapped one layer off. Records are git *blobs*: shared,
content-addressed, never rewritten. A path is a *commit*: cheap to rewrite, never
shared implicitly. `git rebase` rewrites history without copying a byte of file
content. Same move. So:

- **Nodes are referenced, never copied.** Cherry-picking a record does not
  duplicate it.
- **A graph is owned by exactly one session.** Local, cheap, freely rearranged.
- **Deriving a graph mutates no node and touches no other graph.**

Under those rules "assemble a new tree" is literally true and costs nothing but the
graph.

**The hazard I would flag hardest, because the tree already has the scar.** A
rendered context must never contain a tool call without its result — providers
reject the entire request rather than tolerating it. conway learned this the
expensive way: eight parallel forks once landed on a prefix cut mid-batch and all
eight died on their first request with zero steps taken
(`crates/conway-runtime/src/context/builder.rs:28`). The harness now drops
unanswered calls as a final assembly pass, and — this is the part worth copying —
**records every dropped call in the context report** rather than hiding the
intervention, so a turn where the model re-issues a call it appears never to have
made is explicable from the log instead of mysterious.

Arbitrary selection reintroduces that problem at will. So the design has to answer,
in writing: is an incoherent selection silently repaired, or refused? Silent repair
means quietly deleting part of a deliberate choice. `INTENT.md` §5b carries this
and four more hazards — rearranging costing strictly more than omitting, provenance
surviving a graph drawn from three sessions, a graph pinning the logs it
references, and whether a person can look at a rearranged context and tell what is
in it. All five belong in D1-1's written design and none should be discovered
during implementation.

### 6.3 The dogfooding bar is a ladder

**The answer.** Two rungs, not a flag day. **Rung one:** conway used alongside the
current harness, for real work, by choice, for some class of task — this is what
starts generating honest signal and it is closer than it looks. **Rung two:**
comfort that the other tool could be uninstalled, which needs coverage of the
features that actually matter *and* **output quality better than the incumbent
produces today**. Not comparable — better. Feature parity with worse results is not
a reason for anyone to switch, including us.

**What it changes.** A survey item lands early: read what Claude Code, the DeepSeek
harness, and [Hermes Agent](https://github.com/NousResearch/hermes-agent) actually
do — Hermes especially, since it ships skills, persistent memory, and a learning
loop as harness features rather than as things each user hand-builds — and produce
the catalogue of what should be **available in a default installation**. Available,
not enabled. That catalogue is what turns "ship four plugins" into a decision about
which plugins, and it should exist before `conway.skills` starts.

---

## 7. What I would do, in one paragraph

Design the context path, because it is in the core, it blocks four other things,
and its five hazards are all the kind that are cheap to answer on paper and
expensive to discover in code. In parallel, run the competitive survey, because it
decides what the plugin shelf should hold and it costs a fraction of building the
wrong thing. In parallel, build the out-of-process plugin host, which is what turns
"plugins all the way down" from an architecture property into one anyone can use.
Then ship whatever the survey says gets conway to rung one of the dogfooding
ladder, which is almost certainly skills and memory. Give one-shot mode its five
flags, and try to configure conway down to a bare inference call — reporting
whatever stops you as a defect in the composition surface. Decompose the terminal
app. And close the three declared gaps, because a project whose credibility rests
on an honesty ledger should not carry entries longer than it must.

The plan that does all of that in parallel, with ownership boundaries, is in
[`PLAN.md`](PLAN.md).

---

## 8. The last two answers

Both of §7's open questions were answered the same day. Nothing on this page is
waiting on you now.

### 8.1 An invalid path is refused, not repaired — and refused at derivation

**The answer.** Fail fast and loudly. There is no way to predict the correct repair
— dropping the orphaned call and keeping the result are both plausible, and choosing
silently is guessing at intent. This is `PHILOSOPHY.md` §6's existing posture ("a
loud, predictable refusal to a clever recovery") applied one layer up, which is a
good sign the answer is the right one.

**The sharper half is *where*.** The operative sentence was "it shouldn't ever be
created in the first place," and that moves validation from render time to
**derivation** time: the operation that would produce an incoherent path is the
thing refused, with a typed error naming what it would have orphaned. An invalid
path becomes unrepresentable rather than detected late — which is strictly better,
because the error can name the operation that was wrong instead of describing a
request nobody assembled by hand.

**The existing repair stays, and the distinction is why.** Incoherence the *harness*
caused — a fork cut mid-batch, a session killed between an assistant append and its
tool results — is an accident nobody chose, and refusing it would punish someone for
something they did not do. That keeps today's behaviour: drop the unanswered calls,
record every drop. Incoherence a *deliberate selection* caused is an invalid change
being requested, and gets refused. Two situations, two answers, and the difference
is whether someone asked for it.

**One usability obligation comes with it.** A refusal has to name the valid
neighbouring operation — "dropping record 7 orphans the call in record 6; drop
both" is actionable where "invalid path" is not. Without that, a safety property
becomes an obstacle and people route around it.

### 8.2 Non-Rust hosts get to embed conway

**The answer.** Yes. Not everybody writes Rust, plenty of people embed compiled
code, and a harness reachable from one language is not an inference layer — it is a
Rust library that also runs in a terminal.

**I owe a correction here.** Last section I asserted the shape would be "a process
boundary with a serialized protocol, not a C ABI over the async facade." That was
too confident. The stated targets are C, C++, and compiled hosts generally, where a
subprocess is often exactly what you cannot have, so an in-process C ABI is
genuinely on the table. The reason behind my claim has not gone away — conway's
facade is fully async and event-streamed, and async across a C ABI is the hard part
— but that is now a **design constraint for the binding layer** rather than an
argument against it.

**Three constraints on how, in priority order.**

*It does not belong in the core or the engine.* The binding layer is another
consumer of the facade, the same shape as a first-party plugin: its own crate,
depending on `conway`, never touching `conway-core`. The core learns nothing about
C. If it turns out this has to sit further out as an adapter rather than being built
directly, that is an acceptable outcome and not a failure.

*Follow the prior art.* This is solved ground and the wheel is not worth rebuilding.
[Diplomat](https://github.com/rust-diplomat/diplomat) is the closest fit to the
stated targets — proc-macro driven, no external IDL file, and its language list
leads with C and C++; it exists because ICU4X had this exact problem.
[UniFFI](https://mozilla.github.io/uniffi-rs/) is the IDL-driven alternative, aimed
at Kotlin/Swift/Python. `cbindgen` is the low-level floor. Every one of them has
already had to answer the async, panic-safety, and memory-ownership questions above,
so read their answers before designing anything.

*No second facade.* A non-Rust host gets a projection of the same public API a Rust
host uses. Anything it cannot reach is a gap in the projection, not a different
product.

---

## 9. Nothing is open

Eight questions asked across three rounds; eight answered. Every one of the eight
turned out to be "you are asking the wrong shape of question," and in six cases the
correction changed the plan rather than confirming it — which is the argument for
running this review on a schedule rather than once.

What is left is work, and it is in [`PLAN.md`](PLAN.md).
