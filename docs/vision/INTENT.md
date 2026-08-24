# Intent: what conway is for

> **This is the operator's document.** It records what conway is *for* and what
> "good" means here, in the operator's own terms, so that a decision made six
> months from now can be checked against it. It is not a specification and
> nothing in it is directly implementable. [`PHILOSOPHY.md`](../../PHILOSOPHY.md)
> is the specification; [`ARCHITECTURE.md`](../../ARCHITECTURE.md) is the
> mechanism. This page is the thing both of those are *trying to be true to*.
>
> **It is written to be added to, and it is written in the present tense.** When
> a decision gets made that this page did not anticipate, the fix is to write
> down the sentiment that would have settled it — folded into the argument where
> it belongs, not appended as a dated amendment. This page is not a record of how
> the thinking changed. It is what the thinking currently is. Claims about what
> is *built* belong in
> [`STATE-OF-THE-UNION.md`](STATE-OF-THE-UNION.md), which is replaced each time
> the project is reviewed; this page outlives every one of those.

---

## 1. The one-sentence version

conway should be able to replace Claude Code for day-to-day coding without being
heavy, and should be equally usable as a general-purpose way to reach a language
model from a script, a pipeline, or another application.

Both halves matter. A harness that is only a coding agent is the thing being
replaced. A harness that is only a library is a dependency, not a tool anyone
actually uses.

---

## 2. The complaint that started it

Existing harnesses are opinionated, and they are getting heavier.

Claude Code in particular has a very large number of features. Many of them are
not used. Many of them had a moment when they were genuinely valuable and then
faded as something better came along — but they never left, because nothing ever
leaves. The weight is cumulative and the removal path does not exist.

The complaint is not that it has many features. It is genuinely powerful, and it
stays in daily use for good reasons — plenty of what is in there earns its keep.
The problem is that nothing has to earn its place and nothing can leave, so the
valuable and the faded accumulate identically until they are indistinguishable.
**conway is not trying to have fewer features. It is trying to make each one earn
its place, and to let it go when it stops.** Those are different goals, and only
the second survives contact with a tool people actually like.

Two separate problems live inside that complaint, and conway answers them
differently:

**Weight.** The fix is a small core. Not a small *product* — a small core, with
capability arriving from outside it. Features that fade should be uninstallable,
which means they have to have been installed in the first place.

**Opinion.** The fix is not "have better opinions." It is to make the opinion a
component. Compaction, memory, routing policy, what belongs in a context window —
every one of these is a judgment that depends on the work, the budget, and the
person. A harness that bakes one in is wrong for everyone whose answer differs,
and wrong *invisibly*, at the moment they are least equipped to notice.

---

## 3. Context is the whole problem

This is the belief the rest of the design hangs off.

To get good results, a harness has to build context the model was not trained
with: exploring the tree, reading files, running searches, calling tools. That
work is necessary. But the vast majority of what it produces is not relevant to
the task at hand — it is the *residue* of finding out what was relevant.

The claim: **with a high enough signal-to-noise ratio, a million tokens is more
than enough to do essentially anything, and you never need to compact.** The
problem was never window size. It was that harnesses fill the window with the
process of discovering the answer rather than the answer.

Which makes compaction the enemy, not a feature. A summary is never as good as
the full thing with attention properly spent on it. Every compaction event is a
lossy write you did not choose, applied to the material you were reasoning over,
at the moment the work got hard enough to fill the window. conway should not do
this on anyone's behalf, ever.

The first answer is structural: control what *enters* the context at all. That is
what fork and spawn are for, and it is why "do the work elsewhere, keep the
distillate" is the central idiom rather than a tip.

The second answer, for people who want it anyway, is a plugin they chose.

---

## 4. Unix is the reference, for a reason

The prior art that got closest to this is Unix — both the composability
philosophy and specifically how process creation works.

Sometimes you need a copy of an entire process exactly as it is: `fork`.
Sometimes you need to execute from square one with no inherited environment:
`exec` / `posix_spawn`. That distinction is not an implementation detail. It is
the *question* — does this work depend on what the caller already knows, or is it
self-contained? — and it turns out to be the same question you ask when deciding
what a sub-agent's context window should contain.

So conway has two primitives and no third:

- **Fork** — the prior context is required for the inference to be any good.
- **Spawn** — trained knowledge plus a written brief is sufficient.

And a third use that falls out of the first: fork *specifically in order to do
ephemeral work you do not want committed to the context tree*. Ask the question,
get the answer, decide whether the answer was worth keeping, throw the rest away.

Together these give fine-grained control over what any given context window
contains, while keeping a high degree of cacheability — because a fork's
inherited prefix is literal bytes in order, and siblings share it.

---

## 5. Context as a tree, not a line

Every harness today models a conversation as a line: turn, turn, turn, always a
single path of thought. That is not how the work goes. Real work branches — you go
down a tangent, work on something else for an hour, come back. Two lines of
thought that look unrelated trace back to a common root three decisions ago.

Git already solved the mental model for this shape, and the overlap with context
management is large and mostly unexploited. Applied to a codebase the fit is
natural: there is context every agent in a repository needs, and as you go deeper
you get more specific about less. That gives you a high-level view *and* real
specificity without compaction — and it maps onto caching almost exactly, since
the shared upper reaches are stable and the specific lower reaches churn.

The claim underneath all of it: **a context window is not a log. It is a path
through records that already exist**, so curation means choosing a different path
and never rewriting anything. What that buys is a cost you can predict and a
curator that does not have to be a model.

> **This is an idiom, not a law.** The core owns fork, spawn, the log, and the
> ability to see the shape. How you *curate* the tree — what merges, what gets
> dropped, what a branch means — is an opinion, and opinions ship as plugins. Some
> may ship as plugins installed by default. None ship as core behaviour.
>
> This has a consequence for how the specification is written:
> [`PHILOSOPHY.md`](../../PHILOSOPHY.md) must not crosscut. Mechanism the harness
> guarantees and practice the operator recommends have to stay visibly separate on
> the page, or the idiom hardens into a law by accident.

**The words, one idea each.** Where the code or another document uses one
differently, that is worth fixing there. And where plain language will do,
prefer it: a term earns its place by removing an ambiguity, not by existing.
Every word in this table is one more thing someone has to learn before a
sentence means anything to them, so the table stays short and the prose around
it keeps saying what things *are* rather than leaning on their names.

| Word | What it means |
| --- | --- |
| **record** | one thing that happened, written down once and never rewritten |
| **session** | one agent's log — the boundary on *whose turns these are*, not on who may read it or write to it |
| **agent** | the thing that takes turns, calls tools, and moves a head |
| **path** | the ordered records one agent is reasoning over, from as many sessions as it likes |
| **head** | where an agent is now: the pointer it moves as it works |
| **selection** | a path frozen, so it can be shared and referenced |
| **rendering** | a selection turned into the bytes one model sees |

A session is not a conversation, and reaching for it as one is the mistake this
table exists to prevent: an agent three forks deep is reading from three logs at
once. The conversation is the *path*. So "fork the session" is the wrong sentence
— forking puts a new agent at a point in an existing path and gives it a log of
its own to write to.

**What conway commits to.** Five promises about your context. The mechanism that
keeps them is [`PHILOSOPHY.md`](../../PHILOSOPHY.md) §1 and §4; the design that
works them out is [`DESIGN-context-path.md`](DESIGN-context-path.md).

- **5a — Nothing you wrote is destroyed or rewritten to make room for something
  else, and what belongs in a context is your judgment rather than conway's.**
  This is §3's objection to compaction one layer down: when a context has to get
  smaller you look at less of it, you never replace what happened with a summary
  of what happened. Curation acts on the path, never on the records. Which records
  belong is an opinion, and opinions are plugins — the core owns only the ability
  to assemble the context you asked for. Get that line wrong and the core acquires
  exactly the opinion this document exists to keep out of it. It also means the
  curator need not be a model: mechanical selection can be driven by record type,
  provenance, which file a turn touched, or what it costs — cheap, deterministic,
  testable, and incapable of hallucinating.
- **5b — You can rearrange your own context without disturbing anyone else's, see
  exactly what you did, and know the price before you pay it.** Curation nobody
  can inspect is worse than none, because it gets applied anyway. Non-interference
  and inspectability need the same thing: the arrangement is a separate artifact
  from the material it arranges, so the pointer that moves belongs to one agent
  while a frozen selection is shared freely. And because nothing is rewritten, the
  cost of a curation decision is knowable in advance — dropping your most recent
  records is nearly free, dropping the oldest spends the whole cached prefix, and
  reordering is strictly worse than either.
- **5c — Changing model mid-session is ordinary, and stays cheap.** It is one of
  the most consequential decisions available and people revise it constantly. A
  path therefore has two identities: a *selection* says which records in what
  order and depends on nothing else, while a *rendering* is the bytes on the wire
  and depends on the model, the system prompt and the tool set. A selection
  survives a model change; only the rendering — which is the cache, and is supposed
  to be invalidated — does not. An agent's head must reference the first, never the
  second. A design that makes model changes awkward has failed regardless of what
  else it gets right.
- **5d — conway constrains your context only where the wire does.** A tool result
  must follow its call because a request that violates it is rejected outright;
  that is the shape of the medium, not conway having a view, and it must be stated
  plainly rather than discovered. Every other limit — what belongs, how much is too
  much, when to summarize — is somebody's opinion about how you work, and belongs
  in a plugin you chose.
- **5e — You can pull from anywhere. The question is who is allowed to do the
  pulling.** A selection may name any record anywhere: a sibling's, another
  project's, an unrelated tree's. This costs no containment, because composition
  was never a confinement boundary — confinement governs what an agent can reach
  with a *tool call*, a different axis. The control belongs on the composer: an
  installed plugin through the curation seam, and a model only if a plugin hands
  it a tool that composes paths. The operator composes through the second of
  those — you say what you are working on and a model assembles the context for
  it — which is why conway has no curation verbs of its own and does not need
  any.

Two things follow that are easy to miss. **Whether a path fits is not a property
of the path**: fitting depends on the model, so it is asked later, by the thing
talking to the endpoint, and a selection that no longer fits produces a loud
refusal naming what did not fit rather than a silent re-curation. And **memory is
not curation**: a selection can only point at bytes that already exist, which
rules out deciding after the fact that a conversation mattered, un-remembering,
and writing down a distilled sentence nobody ever said. Memory is an annotation
*about* work rather than a selection *of* it, and it gets storage of its own.

**Five questions any curation mechanism must answer in writing before it is
built** — coherence (a context must never hold a tool call without its result, so
an invalid path is refused when built rather than repaired when sent), the cost
asymmetry between omitting and reordering, whether provenance survives a path
drawn from several sessions, what a path pins and for how long, and whether a
person can inspect the result. The answers are worked out in
[`DESIGN-context-path.md`](DESIGN-context-path.md) §4.

One obligation runs through all five, and the precedent is already in the tree:
when the harness intervenes to produce a sendable request, it puts the
intervention *in* the record rather than behind it, wherever the affected thing is
read from. An intervention recorded where nobody reads it is the same defect as
one not recorded at all.

---

## 6. Plugins all the way down

The point of "everything is a plugin" is not purity. It is that composability is
what lets one tool serve use cases its author did not think of, and it is the
only mechanism that lets a faded feature actually leave.

The test to apply at every level: **can I swap this out or extend it without
forking the project?**

- **Inference providers.** Ollama, llama.cpp, Kimi, any cloud API, anything that
  has not been invented yet. A provider conway has never heard of is something you
  install, not a patch you submit and wait on.
- **The CLI application itself.** Adding features to it and customizing it should
  be normal. The terminal app is not a privileged consumer of the library.
- **Context handling.** What happens when context gets too large. What gets
  committed to context and what does not. Which provider serves which agent.
- **Everything below those.** If there is a level where the answer is "you would
  have to fork conway," that level is a defect.

Extension comes at deliberately different costs, and the cheapest one should cover
the most ground:

- **An instruction.** Where conway has a view about how work should go — when to
  compose a fresh context, when to fork rather than continue, what a conway-shaped
  answer looks like — that view ships as text a model reads, in a file you can
  open, edit, or delete, arriving and leaving with the capability it describes. It
  is not compiled in. A plugin makes behaviour swappable; an instruction makes it
  *readable*, which is cheaper still. The cheapest way to change what conway does
  is to disagree with a paragraph.
- **Hooks** — a named event and a command to run. No language requirement, no
  build step, no API to track. A shell script is a legitimate extension. Most of
  what people actually want is this, and it should stay that way.
- **A plugin in another language, running as its own program.** conway asks it
  what tools it has and calls it when the model asks for one. The right price for
  someone who wants to add a capability without learning Rust or rebuilding the
  binary.
- **A plugin compiled in.** Direct access to conway's own types, for a new tool, a
  new provider, or a new routing policy. The most capable and the most expensive.

The cheapest tier carries an obligation the others do not, because prose is not
compiled and a wrong instruction fails quietly: **an instruction may only name a
capability that is actually reachable.** Text telling a model to use something
that is not installed produces an agent that tries and fails forever — the same
defect as a configuration option that does nothing, and it needs the same kind of
gate.

---

## 7. Three surfaces, all first-class

Embedding is a use case in its own right, not something the other two happen to be
built on. Treating it as an afterthought is exactly how it stays one.

**One — the terminal application.** Built **for humans**. A real terminal tool
that is pleasant to use, not a debug harness with a prompt attached. See §7a: this
surface has a job beyond being a nice CLI.

**Two — one-shot, from a shell or a pipeline.** Claude Code's `-p` is used
constantly as shorthand for quick inference, and it shows its tracks: it was
clearly not made for anything except agentic coding. conway's equivalent should be
a general way to get an answer out of a model — usable by someone who is not
writing code, in a repository that may not exist.

**Three — embedded in another application.** A host application depends on conway
to reach models: routing, permissions, the log, the agent primitives if it wants
them, and none of them if it does not. It is a surface with its own users, its own
ergonomics, and its own definition of done.

The test for the third one is blunt: **how much ceremony stands between depending
on conway and getting a completion back?** If a host has to assemble the whole
world before it can ask a model a question, conway is not usable as an inference
layer no matter how good the layer underneath is.

None of the three should feel like it is borrowing a coding agent's plumbing.

> **Going straight to the model is a composition, not a feature.** There should be
> no second way in that shortcuts the harness — a parallel path is a thing every
> future feature has to support twice, and it would say that conway's own
> composition surface was not good enough to express the simplest possible case.
>
> The requirement is the other way round: the plugin and configuration
> architecture must be flexible enough that **routing straight to inference is
> something you configure**. No tools, no agent behaviour, one turn, out. If that
> cannot be assembled from what already exists, the finding is not "conway lacks
> an inference API" — it is *conway is too heavy and too opinionated to configure
> down*, which is a defect in the composition surface and should be fixed there.
>
> This makes a good falsifiable claim, and it belongs in the machine-checked
> ledger: **conway can be configured down to a bare inference call using only
> mechanisms a third party also has.**

### 7a. What the CLI is *for*

The shipped `conway` binary should be **fully functional and not heavy** — which
is not a contradiction, because the resolution is that every capability it has can
be turned on and off. It is an assembly of plugins, not a monolith with a plugin
socket.

This settles the question of what should be installed by default by moving it.
There are two different things and they are easily conflated:

- **The harness** answers *what must exist for conway to work at all?* That test
  stays sharp and stays narrow. Compaction, memory, and skills all fail it, and
  none of them may be harness defaults.
- **The shipped application** answers *what should a person get when they install
  the binary and run it?* A different question with a different answer: a
  fully-equipped coding agent, opinionated on purpose, with every opinion visible
  and removable.

An opinion in the binary is not an opinion in the core. That is the whole
distinction, and holding it is what lets conway be both agnostic and usable on the
first run.

Two reasons the binary has to be this, in increasing order of importance.

**It is the worked example.** The most persuasive demonstration that capability
composes granularly is a real application assembled that way, where you can see
each piece, switch it off, and watch what changes. Nobody is convinced by a
skeleton plugin. They are convinced by turning something off in a tool they use
and having it keep working.

**It has to become the daily driver.** conway's CLI must be good enough to replace
the harness currently being used, full time. This is the single strongest forcing
function available: a tool only improves through daily use by someone who notices
what is wrong with it, and every quality problem that matters will be found that
way and by no other means. Until conway is used that way, its priorities are
guesses.

So "is this needed to use conway as a full-time coding agent?" is a legitimate and
high-priority reason to build something, and it outranks architectural tidiness
when the two disagree.

Run it as a test and not only as a permission: **ask what the CLI using this looks
like, before deciding to build it.** If the answer is that the binary gets visibly
better — something you would switch on and keep on — that is the case for the
feature, and which surface carries it can be argued afterwards. If the CLI would
not use it, that is not a refusal; some capability genuinely belongs to a pipeline
or to an embedder. It is a demand for a different justification, which then has to
be given rather than assumed.

### 7b. The daily-driver bar is a ladder, not a switch

Nobody is switching away from an existing harness on a flag day, and pretending
otherwise produces a bar that is either never met or met dishonestly. Two rungs:

**Rung one — supplement.** conway is used alongside the current harness, for real
work, by choice, for some class of task. This is the rung that starts generating
honest signal, and it is much closer than it looks.

**Rung two — no longer needed.** Comfort that the other tool could be uninstalled.
This one has two conditions and both are required:

- **Coverage** of the features that actually matter day to day — which is a much
  shorter list than any harness's full feature set, and finding out which features
  those are is most of what rung one is for.
- **Output quality better than what the incumbent produces today.** Not
  comparable. Better. This is the condition that matters, because feature parity
  with worse results is not a reason for anyone to switch, including us.

**How to find the list.** Look at what Claude Code, the
[DeepSeek harness](https://deepseek.com/harness/en/), and
[Hermes Agent](https://github.com/NousResearch/hermes-agent) actually do and how
they approach agentic coding — Hermes is worth particular attention, since it
ships skills, persistent memory, and a learning loop as harness features rather
than as things each user hand-builds. Then decide **what should be available in a
default installation**: not necessarily enabled, but present and one toggle away,
so someone can assemble the quality-of-life experience that makes conway an
alternative rather than a downgrade.

"Available, not enabled" is the whole shape. It is §7a's distinction applied to
the catalogue: the binary ships opinions, and every one of them is visible and
removable.

**Familiarity is the on-ramp.** conway works differently underneath, and that is
the point — but someone arriving from another harness should not have to relearn
where things live in order to find that out. Where a convention costs nothing to
honour, honour it: configuration in a dot-directory in the home folder, not in a
location that assumes you already know a desktop standard. Novelty in the
internals is the product. Novelty in the furniture is a tax on the person deciding
whether to switch, charged before they have any reason to trust that the rest is
worth it.

**Grounding.** For interface and interaction design, the
[DeepSeek harness](https://deepseek.com/harness/en/) is the reference point —
plugin-mounted everything, an append-only trajectory you can inspect, fork, and
replay, and distinct runtime modes for distinct jobs. No desktop app is planned;
it is a grounding reference, not a target. For the shape of a lightweight core
with good extension surfaces, [Pi](https://pi.dev) is the reference: four tools, a
short system prompt, tree-structured sessions, and an explicit list of things it
refuses to own.

### 7c. Non-Rust hosts get to embed conway

Not everybody writes Rust, plenty of people embed compiled code, and a harness
reachable only from one language is not an inference layer — it is a Rust library
that also runs in a terminal.

Three constraints on how, in order of importance:

**It does not belong in the core or the engine.** The binding layer is another
consumer of the public API, the same shape as a first-party plugin: its own crate,
never reaching into the internals. The core learns nothing about C. If it turns
out this cannot be built directly and has to be an adapter sitting further out,
that is an acceptable outcome and not a failure.

**Follow the prior art; do not invent a binding layer.** This is solved ground
with mature tooling, and the wheel is not worth rebuilding. The survey should
reach at least [Diplomat](https://github.com/rust-diplomat/diplomat),
[UniFFI](https://mozilla.github.io/uniffi-rs/), and `cbindgen` at the lowest
level. Look at how comparable projects expose themselves before choosing.

**The hard part is async, and it is a design constraint rather than an
objection.** conway's public API is fully asynchronous and streams events. Who
drives the runtime, how a stream of events crosses the boundary, what happens to a
crash that would otherwise cross it, and who owns returned memory are all real
questions — and they are questions every one of the tools above has had to answer.
Read their answers first.

> **What this does not mean.** No second API, no divergence in capability. A
> non-Rust host gets a projection of the same public interface a Rust host uses,
> and anything it cannot reach is a gap in the projection rather than a different
> product.

---

## 8. What "good" means here

Ordered, most important first. **The numbered points below are cited elsewhere as
§8.1 through §8.9** — there is no sub-heading to jump to, so a citation like §8.3
means point 3 of this list.

1. **An open question is a failure of the spec, not a gap in the code.** If
   someone building on conway has to ask "should I do it this way or that way,"
   the guidance was insufficient. Fix the guidance. This is the single most
   important rule on this page, and the reason this page exists.
2. **The core is agnostic**, and here is how to tell whether something belongs in
   it. Every opinion in the core is a thing an extension has to accommodate or
   route around, forever — so the question comes up constantly and needs a test
   rather than an instinct.

   > **The test: does this encode a judgment that two reasonable people, doing the
   > same work, could answer differently?** If yes it is policy, and it belongs in
   > a plugin. If no it is mechanism, and the core may hold it.

   This is deliberately a different test from the one that decides what must
   *ship* (*does conway still function with nothing filling this role?*). That one
   decides the default set. This one decides what may live in the core at all, and
   the two are easily conflated.

   Worked examples, because the test is only useful if it discriminates. *The
   ability to bind a name to something* is mechanism — a pointer encodes no
   judgment, and two people would implement it identically. *Which names exist and
   what they mean* is policy. *Assembling a context from a stated selection* is
   mechanism. *Deciding what belongs in that selection* is policy, and it is the
   single most important instance of this whole rule.
3. **Nothing happens to your context that you did not ask for.** No silent
   compaction, no silent trimming, no silent model substitution. A loud, typed
   refusal beats a clever recovery.

   Stated as a rule rather than a list, so it settles cases nobody enumerated:
   **when conway cannot honour a request or a reference exactly, it refuses and
   names what changed.** The examples above are instances, not the boundary — the
   same rule covers a referenced configuration that has drifted, a selection that
   no longer fits after a model change, and whatever the next one turns out to be.

   This rule protects against **loss**, not against action. Silent compaction,
   silent trimming and silent substitution are intolerable because each destroys
   something you cannot get back — the silence hides the damage, but the damage
   is the offence. Composing a context takes nothing away: the records are still
   there, the old path still resolves, and a wrong choice costs a correction
   rather than a loss. So conway may work out what a stated task needs and act on
   it, provided it says what it did. Name the work and assembling the context to
   do it is part of doing it, not a separate decision to be asked about. What it
   may never do is choose an *outcome* — which model, what a refusal becomes,
   what "enough" means — because those are the judgments two reasonable people
   answer differently, which is §8.2's test doing its ordinary job.
4. **The idioms are few and clearly stated.** A small number of guiding idioms,
   applied consistently, beats a large number of features.
5. **Extension is low-friction at every level.** If the cheapest way to change
   behaviour is to fork the repo, that is a bug report against the extension
   surface.

   A surface is proven when **something that is not its author uses it to do a
   thing someone wanted** — not when it compiles, and not when its tests pass. A
   consumer written to exercise a seam demonstrates that the seam compiles, which
   was never in question, and it will happily certify a surface that cannot carry
   the case nobody had yet. So this is a sequencing rule: build a seam when there
   is a consumer for it, not in anticipation of one, and let the proof arrive
   before the seam accumulates dependents.

   The same rule stated from the other end, because it is the one that catches
   things earliest: **nothing is built on theory.** A feature lands with a
   well-defined use case someone can exercise on the day it ships — not a design
   that anticipates one, not a test that simulates one, but a real path through
   the shipped binary a person can walk. If there is nothing to exercise, the
   feature is not finished, whatever its tests say.

   The failure this prevents is worth naming, because it does not look like a
   mistake while you are making it: **a capability added silently, to be used
   later.** The machinery lands, nothing expresses it, and the intention to
   reach for it eventually lives in someone's head. What happens instead is that
   it accumulates weight, constrains later decisions that had no reason to
   accommodate it, and is found — much later — to have been the wrong shape for
   its first real caller. So a capability and the thing that expresses it ship
   together, in the same change. Memory arrives with a plugin that uses it, not
   as a store waiting for one.
6. **An invariant belongs to the seam, not to its call sites.** When something
   must be true across an extension point, enforce it at the point itself — a
   wrapper around it — rather than at each place that happens to use it.

   Two reasons, and the second is the one that matters. **Coverage:** checking at
   call sites means every new consumer has to remember, and a missed one fails
   silently. **Opinion:** many call-site checks are many independent judgments
   that can drift apart; one check at the seam is a single mechanical fact.
   Scattered enforcement accumulates opinion, and a wrapper reduces it.

   It is also what makes a surface safe to extend at all: a new consumer inherits
   the contract instead of re-deriving it, which is the difference between an
   extension point and a trap.
7. **It is genuinely usable for everyday work.** This is a tool for doing the
   work, not a demonstration of a philosophy. If the philosophy makes the tool
   unpleasant, the philosophy is wrong. The operational form of this rule is §7a:
   conway's own CLI must be good enough to replace the harness currently in daily
   use, and until it is, everything on this page is untested.
8. **A design document says what a feature will need. That is a prediction, not a
   requirement.** Building to it is right; treating it as a constraint the feature
   must satisfy is not. When holding on to a premise starts requiring a series of
   accommodations — each individually reasonable — the series is the signal, and
   the premise is what should be questioned.

   Watch especially for a symptom described as a virtue. A limit written down as
   "bounded by construction" is the shape this mistake takes: it sounds like a
   guarantee and it is a workaround for having chosen the wrong unit.
9. **Operating the harness safely is table stakes, and safety is a mechanism
   rather than an opinion.** A tool that runs commands on your machine has to be
   safe enough for real work before anything else about it matters. But two
   things are as bad as being unsafe: an approval process so verbose that people
   learn to click through it without reading, and a set of clever heuristics
   conway applies on your behalf about what looks dangerous. The first trains the
   user to defeat it. The second is an opinion in the core, which point 2 already
   rules out.

   So the split is the usual one. conway owns the **mechanism** — a decision
   point, a record of what was decided, and a way to answer. What counts as risky
   is a **judgment**, and judgments are plugins.

   **Where a convention already exists, borrow it.** Operating systems have spent
   decades on who may do what to which file, and people already know the answers
   they arrived at. conway's job is to meet those conventions rather than improve
   on them — the same argument §4 makes about Unix and §7b makes about where
   configuration lives.

   Ship the lightweight version first: enough to work daily, with sophistication
   arriving later as plugins that may bring inference and heuristics of their own.
   A mode that learns what you always approve is a plugin's job, not the core's.
   The thing that must not happen is a permissions model so elaborate that it
   cannot be replaced.

---

## 9. Explicit non-goals

- A desktop application.
- Being the harness with the most features.
- Guessing what you *want*. Where the outcome depends on the workload, conway asks
  or refuses; it does not pick. Working out what a task you named requires is not
  this — see §8.3.
- Sandboxing conway cannot actually deliver. Say what is confined and what is not,
  precisely, rather than implying containment that does not hold.

---

## 10. How to use this page

When a design decision comes up:

1. Check [`PHILOSOPHY.md`](../../PHILOSOPHY.md). If it settles the question, done.
2. If it does not, check this page for the sentiment that should settle it.
3. If *this* page does not settle it either, that is the failure described in
   §8.1. Write the missing sentiment here first, then make the decision, then push
   the consequence down into `PHILOSOPHY.md` as spec.

The order matters. Sentiment, then specification, then code.

When you add to this page, **fold the new sentiment into the argument it belongs
to** and leave the page reading as though it had always said that. Do not append a
dated note explaining what changed. If the reasoning that produced a decision is
worth keeping, it is worth keeping as reasoning, in the present tense, where
someone will actually read it.

---

## 11. What this page does not settle

Open questions, in the sense of §8.1: each one is a place the guidance is
currently insufficient, and each is waiting on a decision rather than on work.

There are none open right now. That is the intended state rather than a finished
one — §8.1 makes an open question a defect in this page, so an empty list means
the guidance has caught up with the decisions, not that no decision will ever
need making again.

