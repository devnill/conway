# Intent: what conway is for

> **This is the operator's document.** It records what conway is *for* and what
> "good" means here, in the operator's own terms, so that a decision made six
> months from now can be checked against it. It is not a specification and
> nothing in it is directly implementable. [`PHILOSOPHY.md`](../../PHILOSOPHY.md)
> is the specification; [`ARCHITECTURE.md`](../../ARCHITECTURE.md) is the
> mechanism. This page is the thing both of those are *trying to be true to*.
>
> It is written to be added to. When a decision gets made that this page did not
> anticipate, the fix is to write down the sentiment that would have settled it —
> not to leave it as tribal knowledge.

---

## 1. The one-sentence version

conway should be able to replace Claude Code for day-to-day coding without being
heavy, and should be equally usable as a general-purpose way to reach a language
model from a script, a pipeline, or another application.

Both halves matter. A harness that is only a coding agent is the thing being
replaced. A harness that is only a library is not a tool anyone uses on a Tuesday.

---

## 2. The complaint that started it

Existing harnesses are opinionated, and they are getting heavier.

Claude Code in particular has a very large number of features. Many of them are
not used. Many of them had a moment when they were genuinely valuable and then
faded as something better came along — but they never left, because nothing ever
leaves. The weight is cumulative and the removal path does not exist.

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

Every harness today models a conversation as a line: turn, turn, turn, sometimes
a reasoning turn, but always a single path of thought.

That is not how the work goes. Real work has decision points. You branch, you go
down a tangent, you work on a different project for an hour, you come back. Two
lines of thought that look unrelated trace back to a common root three decisions
ago.

Git already solved the visualization and the mental model for exactly this shape:
a tool that tracks change over time and, as a side effect, gives you a
hierarchical relationship between states you can navigate, compare, and merge.
The overlap with context management is large and mostly unexploited.

Applied to a codebase, the shape is natural:

- There is context every agent working in a repository needs.
- As you go deeper, you get more specific about less — a deep agent knows a great
  deal about one area and correspondingly little about everything else.
- That gives you a high-level view *and* real specificity, without compaction.
- It maps onto caching almost exactly: the shared upper reaches are stable and
  cache well; the specific lower reaches churn and do not need to.

And it gives the ephemeral-fork move a name. Break off a side branch, find out
whether what you are reasoning about is worth having, and either merge it back
into the trunk or let the branch die.

> **Status, decided 2026-08-14.** This is an **idiom, not a law.** The core stays
> agnostic: it owns fork, spawn, the log, and the ability to see the shape. How
> you *curate* the tree — what merges, what gets dropped, what a branch means — is
> itself an opinion, and opinions ship as plugins. Some of them may ship as
> plugins that are installed by default. None of them ship as core behavior.
>
> This distinction has a documentation consequence:
> [`PHILOSOPHY.md`](../../PHILOSOPHY.md) must not crosscut. Mechanism the harness
> guarantees and practice the operator recommends have to stay visibly separate
> on the page, or the idiom hardens into a law by accident.

### 5a. The turns and the path through them are separate things

*Added 2026-08-14, answering "does merging mean anything literal?" It does, and
this is the sentence the rest of the section was missing.*

Every turn and every tool call is an **immutable object**. Once written it is
never edited, never summarized in place, never replaced. An agent's *head* is a
pointer into that set of objects — immutability is what "head" means here. You can
put a new agent at any point in the tree and it picks up from there, because the
objects it needs have not moved and cannot.

What follows is the part that matters: **the objects and the path taken through
them are separate things.** A context window is not "the log." It is a *path* — an
ordered selection of immutable objects. Two agents can walk different paths across
the same objects. State curation operates on the path. It never operates on the
turns.

This is the same split git makes between objects and refs, and it is the reason
the git analogy is worth keeping rather than being a loose metaphor.

**The illustration is compaction, and it shows why the split is load-bearing.**

The naive form asks a model: *summarize what has been going on*, then puts the
prose where the records were. That is lossy in a way nobody can audit, it is
unverifiable, and it destroys the cache — it rewrites the front of the prefix, so
every byte after it is a fresh read.

The form this model makes possible is **mechanical cherry-picking**: assemble a new
tree by selecting from the objects that already exist. Nothing is summarized.
Nothing is rewritten. Every record in the resulting context is byte-identical to
one that was already there.

Two things fall out of that, and both are the point:

- **Cacheability becomes something you can reason about, in advance.** Because
  order is preserved and bytes are unchanged, a derived path shares a prefix with
  the original up to the first omission. So the price of a curation decision is
  knowable *before* it is made: dropping from the tail is nearly free, and dropping
  from the head spends the entire cached prefix. A curation policy can optimize
  against that. Naive summarization cannot, because it changes everything.
- **The curator does not have to be a model.** If selection is mechanical, it can
  be driven by structure — headings, record type, provenance, which file a turn
  touched, which tool ran, how many tokens it costs. All of that is available
  without inference. Cheap, deterministic, testable, and incapable of
  hallucinating. A non-LLM tool deciding whether a turn belongs in the window is a
  better default than a model guessing.

"Merge back into the trunk" therefore means something concrete: **derive a new path
that includes objects from a branch.** Not replay, not rewrite, not a diff applied
to a parent — a selection. And its opposite, dropping a branch, is just a path that
does not include it. Nothing is deleted either way, because nothing ever is.

> **What this does not settle, and must not be decided by whoever implements it
> first.** conway today has one path per agent, computed by a fixed rule (a
> session's own records, plus its inherited prefix). Making the path a
> first-class, nameable, persisted thing is a real design change to the core, and
> it is the enabling work for every curation plugin. The core owns *the ability to
> express and assemble a path*. Which objects belong on it is a policy and belongs
> in a plugin. Get that line wrong and the core acquires the opinion this whole
> page exists to keep out of it.

### 5b. The graph is a separate artifact from the nodes

*Added 2026-08-14, answering "does a path span sessions?" It does, and working out
why is what settles the shape of the whole mechanism.*

Start from what fork already is. Forking creates a second agent to work on the
**existing session tree** — it does not copy anything, it references a prefix. So
paths already span sessions today; that is not a new capability being proposed, it
is an accurate description of the one path conway has.

What is new is that a path can be **rearranged**. And rearranging must never affect
any other session.

That combination looks, at first, like it breaks the git analogy: if we are
selecting and reordering pieces, we are rebuilding a context by copying it, and
copying is the thing conway does not do. But the analogy does not break — it was
just being mapped one layer off. In git, file contents are blobs: shared freely,
content-addressed, never rewritten. History is commits and trees: cheap to rewrite,
never shared implicitly. `git rebase` rewrites history without copying a single byte
of file content. That is exactly the move here.

So the rule, in the operator's own words: **the graph is a separate artifact from
the individual nodes.**

- **Nodes are referenced, never copied.** Global, immutable, shared by anything
  that wants them. Cherry-picking a record does not duplicate it.
- **A graph is owned by exactly one session.** Local, cheap, freely rearranged.
- **Deriving a graph mutates no node and touches no other graph.** That is what
  makes rearrangement safe, and it is the invariant to hold above all others here.

Under those rules, "assemble a new tree" is literally true and costs nothing but
the graph.

#### "Graph" is doing two jobs, and separating them is what protects caching

*Refined 2026-08-14, after asking whether single ownership costs expressiveness
around caching. It does not — but only once this distinction is explicit, and the
version that leaves it implicit does cost something real.*

Git needs three layers here, not two, and so does this:

| | git | conway | Owned by |
| --- | --- | --- | --- |
| content | blob | **record** | nobody — global, immutable |
| structure, frozen | commit / tree | **graph version** | nobody — global, immutable, freely referenced |
| structure, moving | branch ref | **graph head** | **exactly one session** |

**Ownership applies to the head. A version is shared freely.** Read that way, the
rule above is exactly right and costs nothing.

Two reasons it has to be this way.

**Expressiveness.** Fork ten children off one carefully curated path and they should
share that path's cache. If a graph may reference *another graph version* as its
prefix, that sharing is structural — one named version, ten heads pointing at it,
byte-identity guaranteed by construction. If a graph may reference only *records*,
each child re-enumerates the same selection, byte-identity becomes a coincidence
somebody has to preserve, and when it drifts nothing fails: you get a silent cache
miss, which looks exactly like an expensive workload rather than like a bug.

**Correctness.** If a session's graph could reference another session's *head*, then
that session rearranging its own path would change this one's context underneath it
— violating "deriving a graph touches no other graph" from the other direction. A
reference must name something frozen. This is the discipline fork already uses: an
inherited prefix is frozen at a sequence number and immutable by construction.

> **The mechanism for this already exists and was built for another reason.**
> `conway-runtime`'s `prefix_key` is `blake3(model ‖ canonical bytes of every
> segment up to the static/inherited boundary)`, and it *deliberately excludes each
> segment's per-agent id* so that siblings forked at the same point produce the same
> key from byte-identical content
> (`crates/conway-runtime/src/context/prefix.rs:20`). That is already
> content-addressed, ownership-blind identity for exactly the shared portion of a
> context — which is to say conway already computes graph-version identity and calls
> it something else. The path design should build on it rather than invent a second
> one.

#### The five ways to get this wrong

This is the part that needs care. Each of these is a real hazard, not a
hypothetical, and the design has to answer all five in writing before any code.

1. **Coherence — settled 2026-08-14: refuse, and refuse early.** A rendered context
   must never contain a tool call without its result; providers reject the whole
   request rather than tolerating it. conway already knows this the hard way: eight
   parallel forks once landed on a prefix cut mid-batch and all eight died on their
   first request with zero steps taken.

   **An invalid path must never be created in the first place.** Validation belongs
   at *derivation* time, not at render time: the operation that would produce an
   incoherent path is refused, loudly, with a typed error naming what it would have
   orphaned. An invalid path is therefore unrepresentable rather than detected late.
   There is no repair, because there is no way to predict the correct fix — dropping
   the orphaned call and keeping the result are both plausible, and choosing silently
   is guessing at intent. This is `PHILOSOPHY.md` §6's existing posture ("a loud,
   predictable refusal to a clever recovery") applied one layer up.

   **The one existing repair stays, and the distinction is the reason.** Incoherence
   the *harness* caused — a fork cut mid-batch, a session killed between an assistant
   append and its tool results — is an accident nobody chose, and refusing it would
   punish someone for something they did not do. That path keeps today's behaviour:
   drop the unanswered calls and **record every drop in the context report**.
   Incoherence a *deliberate selection* caused is an invalid change being requested,
   and gets refused. Two different situations, two different answers, and the
   difference is whether a human or a plugin asked for it.

   > **A refusal has to be usable, not just correct.** When an operation is refused,
   > say what the valid neighbouring operation is — "dropping record 7 orphans the
   > call in record 6; drop both" is actionable where "invalid path" is not. Refusing
   > without that turns a safety property into an obstacle.
2. **Rearranging costs more than omitting.** A path that only *drops* records shares
   a byte prefix with its origin up to the first omission. A path that *reorders*
   them breaks at the first moved element, which is strictly worse and often total.
   Both are legitimate, but they are not the same operation and should not feel like
   it. Omission should be the cheap, obvious default; reordering should be a
   deliberate act with its price shown.
3. **Provenance has to survive.** Every segment carrying where it came from is what
   makes an inexplicable agent explicable. A graph assembled from three sessions
   either keeps that legible or destroys the single most valuable debugging property
   conway has.
4. **A graph pins the logs it references.** If a session's graph references another
   session's nodes, that other session cannot be discarded. Ephemeral `/ask` children
   are discarded by design, so a dangling reference is reachable today with no new
   features. Retention needs a stated answer, not an emergent one.
5. **A person has to be able to see it.** This is as much a user-experience problem
   as a data-modeling one. If someone cannot look at a rearranged context and tell
   what is in it and where each piece came from, they will not trust it — and
   curation nobody trusts is worse than no curation, because it is applied anyway.

> **The precedent to follow is already in the tree.** When the harness has to
> intervene to produce a sendable request at all, it puts the intervention *in* the
> record rather than behind it, so a strange turn is explicable from the log instead
> of mysterious. Every curation mechanism built on paths inherits that obligation.
>
> **Where an intervention has to be recorded, precisely:** *wherever the thing it
> affected is read from.* "In the record" is otherwise ambiguous between the session
> log, the per-turn context report, and any derived artifact — and an intervention
> recorded somewhere nobody reads is the same defect as one not recorded at all.

### 5c. A path is identified twice, and conflating the two costs you model changes

*Added 2026-08-14. This section exists because five open questions came out of the
first path design, and three of them dissolve once this distinction is written down.*

Changing model mid-session is **ordinary**, not exceptional. It is one of the most
consequential decisions available and people revise it constantly — cheaper model for
a mechanical stretch, larger window when the work gets big, a different provider when
one is degraded. A design that makes model changes awkward has failed regardless of
what else it gets right.

That requirement forces a distinction §5b did not draw. A path has **two identities,
and they answer different questions**:

| | **Selection** | **Rendering** |
| --- | --- | --- |
| Answers | *which records, in what order* | *what bytes go on the wire* |
| Depends on | nothing but the nodes | model, system prompt, tool set |
| Kind of fact | curatorial — a judgment someone made | mechanical — derived, recomputable |
| Lifetime | durable; the thing worth naming and reusing | disposable; a cache key |
| Who authors it | a person or a curation plugin | the harness |

**`prefix_key` is the second one.** It folds in the `ModelId` and the whole static
prefix, because it exists to identify a *cacheable wire prefix* — which is exactly
right for its job and exactly wrong as the identity of a selection.

Three consequences, and the third is the one that matters:

- **Ten siblings share one selection and get N renderings**, one per model they route
  to. §5b's "one named version, ten heads" is about the selection. Nothing
  contradicts anything once the layers are named.
- **A selection is model-free, so it survives a model change.** Switching models
  invalidates the rendering — which is just the cache, and is *supposed* to be
  invalidated — and leaves the curation untouched. That is the behaviour anyone would
  expect, and it is only expressible if the two are separate.
- **Therefore a session's head must reference a selection, not a rendering.** A head
  keyed on a model-dependent identity could not survive a model change without being
  rewritten, and two agents with identical curation but different models would appear
  to have different paths. That is backwards.

#### Fitting is not a property of a selection

The related trap: a curation decision is usually made *for* something — "drop these
turns so it fits." Fitting depends on the model, so it is tempting to bake a target
into the selection.

Do not. **A selection says what belongs in context. Whether it fits is a separate
question, asked later, and it already has an owner:** admission belongs to the
backend, because only the thing talking to an endpoint knows that model's real
window and how that provider counts (`PHILOSOPHY.md` §5).

So the layering is: *selection* (curatorial, durable) → *rendering* (mechanical,
per-model) → *admission* (backend, per-model). A selection that no longer fits after
a model change produces a loud refusal naming what did not fit — the behaviour
`PHILOSOPHY.md` §6 already specifies — and the operator or a plugin curates again.
Never a silent re-curation, and never a selection that quietly meant something
different under a different model.

### 5d. What conway constrains, and why — two different things

*Added 2026-08-14.*

§5b says a path can be "freely rearranged." That word is too strong, and the
overclaim produced a question it should have prevented.

Two kinds of constraint exist and they are not comparable:

- **Constraints a provider requires.** A tool result must follow its call, because a
  request that violates it is rejected outright. This is not conway having a view; it
  is the shape of the medium. Such constraints are legitimate, unavoidable, and must
  be **stated plainly** rather than discovered.
- **Constraints conway would impose because it has an opinion.** *These are the ones
  conway refuses to have.* A rule about what belongs in a context, how much is too
  much, when to summarize — every one of those is a judgment that differs between
  users, and it belongs in a plugin.

So the accurate claim is: **a path may be rearranged freely, subject only to what the
wire permits.** Any constraint that cannot be traced to a provider requirement is a
defect, and any constraint that can be must be documented where a curator will hit it.

---

## 6. Plugins all the way down

The point of "everything is a plugin" is not purity. It is that composability is
what lets one tool serve use cases its author did not think of, and it is the
only mechanism that lets a faded feature actually leave.

The test to apply at every level: **can I swap this out or extend it without
forking the project?**

- **Inference providers.** Ollama, llama.cpp, Kimi, any cloud API, anything that
  has not been invented yet. Reached through an interface that can be extended
  and can integrate with services regardless of how they handle inference. A
  provider conway has never heard of is something you install, not a patch you
  submit and wait on.
- **The CLI application itself.** Adding features to it and customizing it should
  be normal. The terminal app is not a privileged consumer of the library.
- **Context handling.** What happens when context gets too large. What gets
  committed to context and what does not. Which provider serves which agent.
- **Everything below those.** If there is a level where the answer is "you would
  have to fork conway," that level is a defect.

Two mechanisms, deliberately at different costs:

- **Plugins** — Rust, compiled, direct access to conway's types. The right price
  for a new tool, a new backend, a new router.
- **Hooks** — a named event and a command to run. No language requirement, no
  build step, no API to track. A shell script is a legitimate extension.

Most of what people actually want is the second one, and it should stay that way.

---

## 7. Three surfaces, all first-class

*Revised 2026-08-14. This section used to name two surfaces and fold embedding
into a bullet under the second. That was wrong: embedding is a use case in its own
right, and treating it as an afterthought is exactly how it stays one.*

**One — the terminal application.** Built **for humans**. A real terminal tool that
is pleasant to use, not a debug harness with a prompt attached. See §7a: this
surface has a job beyond being a nice CLI.

**Two — one-shot, from a shell or a pipeline.** `-p` in Claude Code is used
constantly as shorthand for quick inference, and it shows its tracks: it was
clearly not made for anything except agentic coding. conway's equivalent should be
a general way to get an answer out of a model — usable by someone who is not
writing code, in a repository that may not exist.

**Three — embedded in another application, to facilitate inference.** A host
application depends on conway to reach models: routing, permissions, the log, the
agent primitives if it wants them, and none of them if it does not. This is not
"the library that the other two happen to be built on." It is a surface with its
own users, its own ergonomics, and its own definition of done.

The test for the third one is blunt: **how much ceremony stands between depending
on conway and getting a completion back?** If a host has to assemble the whole
world before it can ask a model a question, conway is not usable as an inference
layer no matter how good the layer underneath is.

None of the three should feel like it is borrowing a coding agent's plumbing.

> **Going straight to the model is a composition, not a feature.**
> *Added 2026-08-14.* There should be no second API that shortcuts conway — a
> parallel path through the harness is a thing every future feature has to support
> twice, and it would say that conway's own composition surface was not good enough
> to express the simplest possible case.
>
> The requirement is the other way round: the plugin and configuration architecture
> must be flexible enough that **routing straight to inference is something you
> configure**. No tools, no agent behaviour, one turn, out. If that cannot be
> assembled from what already exists, the finding is not "conway lacks an inference
> API" — it is *conway is too heavy and too opinionated to configure down*, which is
> a defect in the composition surface and should be fixed there.
>
> This makes a good falsifiable claim, and it belongs in the ledger: **conway can be
> configured down to a bare inference call using only mechanisms a third party
> also has.**

### 7c. Non-Rust hosts get to embed conway

*Added 2026-08-14. The third surface has a complete answer for Rust hosts and had
none at all for anyone else, which is most people.*

**Yes.** Not everybody writes Rust, plenty of people embed compiled code, and a
harness reachable only from one language is not an inference layer — it is a Rust
library that also runs in a terminal.

Three constraints on how, in order of importance:

**It does not belong in the core or the engine.** The binding layer is another
consumer of the facade, the same shape as a first-party plugin: its own crate,
depending on `conway`, never touching `conway-core`. The core learns nothing about
C. If it turns out this cannot be built directly and has to be an adapter sitting
further out, that is an acceptable outcome and not a failure.

**Follow the prior art; do not invent a binding layer.** This is solved ground with
mature tooling, and the wheel is not worth rebuilding. The survey should reach at
least [Diplomat](https://github.com/rust-diplomat/diplomat) (proc-macro driven, no
external IDL, and its target list leads with C and C++ — it exists because ICU4X
had this exact problem), [UniFFI](https://mozilla.github.io/uniffi-rs/) (IDL-driven,
aimed at Kotlin/Swift/Python), and `cbindgen` at the lowest level. Look at how
comparable projects expose themselves before choosing.

**The hard part is async, and it is a design constraint rather than an objection.**
conway's facade is fully async and event-streamed. Who drives the runtime, how a
stream of events crosses the boundary, what happens to a panic that would otherwise
unwind across it, and who owns returned memory are all real questions — and they
are questions every one of the tools above has had to answer. Read their answers
first.

> **What this does not mean.** No second facade, no `libconway`, and no divergence
> in capability. A non-Rust host gets a projection of the same public API a Rust
> host uses, and anything it cannot reach is a gap in the projection rather than a
> different product.

## 7a. What the CLI is *for*

*Added 2026-08-14, answering "which curation plugins install by default?" The
answer turned out to be about the binary rather than about the plugins.*

The shipped `conway` binary should be **fully functional and not heavy** — which is
not a contradiction, because the resolution is that every capability it has can be
turned on and off. It is an assembly of plugins, not a monolith with a plugin
socket.

This settles the default-install question by moving it. There are two different
things and they were being conflated:

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
skeleton plugin. They are convinced by turning something off in a tool they use and
having it keep working.

**It has to become the daily driver.** conway's CLI must be good enough to replace
the harness currently being used, full time. This is the single strongest forcing
function available: a tool only improves through daily use by someone who notices
what is wrong with it, and every quality problem that matters will be found that
way and by no other means. Until conway is dogfooded, its priorities are guesses.

So "is this needed to dogfood conway as a full-time coding agent?" is a legitimate
and high-priority reason to build something, and it outranks architectural
tidiness when the two disagree.

### 7b. The dogfooding bar is a ladder, not a switch

*Added 2026-08-14. "Replaces the current harness" was going to be declared met and
then quietly not be, so here is what it actually means.*

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
- **Output quality better than what the incumbent produces today.** Not comparable.
  Better. This is the condition that matters, because feature parity with worse
  results is not a reason for anyone to switch, including us.

**How to find the list.** Look at what Claude Code, the
[DeepSeek harness](https://deepseek.com/harness/en/), and
[Hermes Agent](https://github.com/NousResearch/hermes-agent) actually do and how
they approach agentic coding — Hermes is worth particular attention, since it ships
skills, persistent memory, and a learning loop as harness features rather than as
things each user hand-builds. Then decide **what should be available in a default
installation**: not necessarily enabled, but present and one toggle away, so
someone can assemble the quality-of-life experience that makes conway an
alternative rather than a downgrade.

"Available, not enabled" is the whole shape. It is §7a's distinction applied to the
catalogue: the binary ships opinions, and every one of them is visible and
removable.

**Grounding.** For interface and interaction design, the
[DeepSeek harness](https://deepseek.com/harness/en/) is the reference point —
plugin-mounted everything, an append-only trajectory you can inspect, fork, and
replay, and distinct runtime modes for distinct jobs. No desktop app is planned;
it is a grounding reference, not a target. For the shape of a lightweight core
with good extension surfaces, [Pi](https://pi.dev) is the reference: four tools, a
short system prompt, tree-structured sessions, and an explicit list of things it
refuses to own.

---

## 8. What "good" means here

Ordered, most important first.

1. **An open question is a failure of the spec, not a gap in the code.** If
   someone building on conway has to ask "should I do it this way or that way,"
   the guidance was insufficient. Fix the guidance. This is the single most
   important rule on this page, and the reason this page exists.
2. **The core is agnostic**, and here is how to tell whether something belongs in
   it. Every opinion in the core is a thing an extension has to accommodate or
   route around, forever — so the question comes up constantly and needs a test
   rather than a instinct.

   > **The test: does this encode a judgment that two reasonable people, doing the
   > same work, could answer differently?** If yes it is policy, and it belongs in
   > a plugin. If no it is mechanism, and the core may hold it.

   This is deliberately a different test from the one `PHILOSOPHY.md` §5 applies to
   the *default plugin set* (*does conway still function with nothing filling this
   role?*). That one decides what must **ship**. This one decides what may live in
   the **core surface**, and the two were being conflated.

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
4. **The idioms are few and clearly stated.** A small number of guiding idioms,
   applied consistently, beats a large number of features.
5. **Extension is low-friction at every level.** If the cheapest way to change
   behavior is to fork the repo, that is a bug report against the extension
   surface.
6. **An invariant belongs to the seam, not to its call sites.** *Added
   2026-08-14.* When something must be true across an extension point, enforce it
   at the point itself — a wrapper around the seam — rather than at each place that
   happens to use it.

   Two reasons, and the second is the one that matters. **Coverage:** checking at
   call sites means every new consumer has to remember, and a missed one fails
   silently. That is not hypothetical — it is exactly how `ContextHook::on_overflow`
   ended up unguarded while `before_request` was the one being discussed.
   **Opinion:** N call-site checks are N independent judgments that can drift apart;
   one seam-level check is a single mechanical fact. Scattered enforcement
   accumulates opinion, and a wrapper reduces it.

   It is also what makes a surface safe to extend at all: a new consumer of the seam
   inherits the contract instead of re-deriving it, which is the difference between
   an extension point and a trap.
7. **It is genuinely usable on a Tuesday.** This is a tool for doing the work, not
   a demonstration of a philosophy. If the philosophy makes the tool unpleasant,
   the philosophy is wrong. The operational form of this rule is §7a: conway's own
   CLI must be good enough to replace the harness currently in daily use, and
   until it is, everything on this page is untested.

---

## 9. Explicit non-goals

- A desktop application.
- Being the harness with the most features.
- Guessing what the user meant. Where the answer depends on the workload, conway
  asks or refuses; it does not pick.
- Sandboxing conway cannot actually deliver. Say what is confined and what is not,
  precisely, rather than implying containment that does not hold.

---

## 10. How to use this page

When a design decision comes up:

1. Check [`PHILOSOPHY.md`](../../PHILOSOPHY.md). If it settles the question, done.
2. If it does not, check this page for the sentiment that should settle it.
3. If *this* page does not settle it either, that is the failure described in §8.1.
   Write the missing sentiment here first, then make the decision, then push the
   consequence down into `PHILOSOPHY.md` as spec.

The order matters. Sentiment, then specification, then code.
