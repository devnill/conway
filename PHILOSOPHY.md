# Philosophy: conway

> **This page is a specification, not a description.** It states the shape
> conway is meant to have at 1.0, and the tree is built to converge on it —
> when the two disagree, the defect is in the code. A handful of claims here
> are therefore ahead of what is built; each one is marked with a
> **Where the tree is today** note saying so, and
> [`CONTRIBUTING.md`](CONTRIBUTING.md) §2 is the rule that keeps those notes
> honest. Anything without such a note is expected to be true right now.

conway is a Rust agent harness for agentic coding. It runs LLM-driven agents
that call tools, create child agents, and route across model backends, behind an
explicit permission system and a durable session log.

There are three ways to use it, all written against the same API, and none of
them is more native than the others:

- **Embedded**, as a library inside a host application. A host reaches every
  primitive below — routing, permissions, the log, forking and spawning —
  through the same facade a terminal session uses, taking as much or as
  little of it as the host wants.
- **Driven interactively**, from a terminal. This is where the most design
  attention goes: a real tool meant to be pleasant to use, not a debug
  harness with a prompt attached.
- **One-shot**, from a shell or a pipeline: a single prompt in, a streamed
  answer out. Usable by someone who is not writing code, in a repository
  that may not exist.

Nothing conway can do is available in only one of them.

What it ships in all three is primitives rather than workflows. The pieces are
unopinionated and composable, which leaves no single right way to use them, and
leaves a pile of sharp primitives with no suggested arrangement. This page is
the suggested arrangement.

None of the three holds your hand. All three put the primitives in front of
you, and the distance between using conway adequately and using it well is
mostly knowledge of what those primitives do and why they are shaped the way
they are.

conway holds no opinions about how you should work. The guidance still has to
live somewhere, so it lives here, in prose you can disagree with and discard
while continuing to use the tool. Where this page describes what a primitive
does or what conway guarantees, that is the harness. Where it recommends a way
of working, nothing enforces it.

---

## 1. The primitives

conway is built around a small set of primitives, taken fairly directly from
Unix process semantics. They cover four jobs: creating a child agent, talking to
one that is already running, constraining what an agent can do, and recording
what happened.

Unix is the reference point because the questions are the same ones. How a child
process is created, what it starts with, how you send it a message, how you stop
it, and how you keep it away from things it should not touch. The analogy is
used below where it clarifies something, and flagged where it breaks.

### Creating a child: fork and spawn

**Fork** gives the child the parent's entire context at that moment, an
immutable ordered prefix, plus a directive telling it what to do with that
context. Like `fork(2)`, the child begins as a continuation of the parent's
state at a point in time, and both avoid paying for it up front, though by
different means: Unix copies page tables and defers the rest until someone
writes, while conway's prefix is immutable and shared by reference, so there is
nothing to copy later either. Siblings forked at the same point share one, and
because that prefix is literal bytes in order, backends that cache prompt
prefixes reuse it.

The child inherits once, at creation. The two sessions are independent
append-only logs from that point: prompting the parent does not reach the child,
and the child's work does not flow back into the parent's context. They can
still exchange messages through the channel described below, which is the part
of the Unix analogy that carries over. Context never arrives anywhere
implicitly.

**Spawn** gives the child nothing but what you hand it: an agent definition (its
system prompt and tool set) and a prompt. Unix reaches this by composition
rather than by a primitive. `fork` makes a copy and `exec` immediately discards
that copy's image in favour of a fresh program, with the arguments and
environment the parent chose; `posix_spawn` is a later convenience that packages
the pair. conway makes it a primitive, because what Unix discards there is a
memory image and what conway would discard is a context, which there is no
reason to assemble first. A spawned child never inherits.

There is no third primitive and no partial-inheritance setting. Wanting part of
a context is common, and has an answer built from what already exists, described
in [Reshaping a tree](#reshaping-a-tree).

### Choosing between them

The question is what the work needs to know.

Fork when the work depends on what the caller already knows: the conversation so
far, the decisions made, the diff under discussion. The child needs no briefing
because it already has everything, and forking is cheap enough to do casually.

Spawn when the task is self-contained and can be stated in a prompt. Find every
callsite of this symbol. Run the suite and report what failed. Review this file
against these criteria. A clean slate is cheaper and less distractible, since
the caller's conversation would be material the child has to read past.

A model reaches them as two tools, `conway_fork` and `conway_spawn`, named after
the primitives rather than presented as one call with a mode argument. The
distinction is the same one this section is about, so it belongs in the tool
name, where it is settled before any argument is filled in. It also keeps each
schema honest: a prompt is a directive to a child that already has the context
or a complete statement of a task to someone who has none, and one field cannot
describe both without the description doing the work the tool name should.

### Forking a record, not a process

Fork and the session log compose, and the composition is worth understanding
because it removes a limit you might assume is there.

`fork(2)` acts on a running process. Once that process exits there is nothing
left to fork, because the thing being duplicated was memory belonging to a live
address space. conway's equivalent lives in the log instead. A fork names a
session and a point in it, the log holds every session and every point, and the
two facts together mean the agent's liveness has no bearing on the operation.

So a session that finished hours ago can be forked. So can one that crashed
mid-turn, or a child that reported and terminated, at any point in its history
rather than only its last turn. Work that went wrong three turns back is
recovered by forking from before it went wrong, instead of arguing with an agent
that has already convinced itself. A finished child whose understanding turned
out to be valuable can still be built on, without having planned for it. The
same starting state can be run twice with different prompts, which is what makes
an honest comparison between two approaches possible.

Ephemeral children are the exception. Discarding a `/ask` answer removes its
session, so there is nothing left to fork from.

### Talking to a running child: steer and report

Communication between agents is message passing rather than shared state. This
is the process model rather than the thread model, chosen for the usual reason:
concurrent access to mutable state is hard to reason about, and an agent's
context is unusually sensitive to corruption.

- **Steer** carries a message from parent to child. It is applied at a turn
  boundary, never mid-generation, which is the same discipline as delivering a
  signal at an instruction boundary rather than partway through one. A context
  injected into a half-finished turn is a corrupted context.
- **Report** carries progress and a terminal result back up. A parent awaiting a
  child cannot hang, because a terminal result is synthesized on panic, budget
  exhaustion, or cancellation. There is no equivalent of a zombie process or a
  `wait` that never returns.
- **Cancellation** stops a child. The `TERM`/`KILL` distinction is the model:
  the graceful form lets the child finish its current turn, and the immediate
  form does not. Only the immediate form propagates to the child's own
  descendants; the graceful form stops the named agent alone.

Messaging and context inheritance stay separate. What a child knows came from
its creation; everything since arrived through a channel you can point at.

### Constraining a child: its tool set

An agent can do what its tools let it do, and nothing else. A child spawned
without `bash` cannot run a shell command, whatever argument it constructs and
whatever it is told. Narrowing a child's announced tools requires no
interpretation of intent and offers nothing to circumvent, which is why it is
the mechanism the core provides.

conway does not inspect a call and judge what it would do. Judging a shell
command means predicting what a shell will make of a string, and a filter built
on pattern matching fails in both directions. Loose enough to permit ordinary
work, it misses the cases it was written for. Tight enough to catch those, it
rejects enough routine commands that the people relying on it turn it off.

Limits on reach belong to whichever plugin can enforce them, which in practice
means the plugin that performs the operation. `conway.fs` takes a root
confining every path it will read or write, and the reason to put it there
rather than above it is that one plugin doing both the checking and the opening
leaves no gap between the two. The distinction Unix draws holds inside it: the
working directory is where the agent is and changes freely, while the root is
what it can reach, set from outside and narrowing only.

> **Where the tree is today.** This closed. `conway.fs` enforces its own root
> now, open-relative: `read`, `write`, `edit`, and `cd` resolve every path
> through a `Dir` opened at the canonical root, so the containment check and
> the filesystem open are the same syscall sequence and a symlink created
> inside the root between them no longer defeats it. Two limits remain, and
> are real rather than closed by this: `glob` and `grep` close the window on
> their search *root* the same way but walk the tree by a separate mechanism
> that does not integrate with that check, a weaker guarantee than the other
> four; and `bash` is confined by no root at all — its `cwd` argument is
> checked, its command string runs verbatim and is declared unconfinable. See
> [`ARCHITECTURE.md`](ARCHITECTURE.md) §3.4 for the limits as they stand.

A root on `conway.fs` says nothing about what a shell command does, and does not
need to. `bash` belongs to a different plugin, and a boundary spanning plugins
would be advertising a promise no single one of them can keep. Confining an
agent's reach therefore means choosing its plugins first and configuring their
limits second, in that order, since a root over an agent holding a shell
constrains the tools that respect it and nothing else.

### The session log

Everything an agent does is appended to a log before it is acted on, one file
per session, and in-memory state is a cache over that file.

A session is JSONL, one record per line. `conway sessions list`, `show`, `tree`,
and `export` read it, and so will anything else you point at a line-delimited
JSON file.

### Objects, heads, and the path through them

Every turn and every tool call the log holds is an **immutable object**. Once
written it is never edited, summarized in place, or replaced — the append-only
discipline above already guarantees this. An agent's **head** is a pointer
into that set of objects, which is what makes forking cheap: putting a new
agent at any point in the tree costs nothing but a pointer, because the
objects it would need have not moved and cannot.

The objects and the path taken through them are two different things. A
context window is not "the log" — it is a **path**: an ordered selection of
immutable objects. Two agents can walk different paths across the same
objects, sharing whatever they select in common. Curation — merging a branch
back in, dropping one, condensing what a context holds — acts on the path. It
never acts on the objects. Nothing already written is deleted, rewritten, or
replaced by a curation decision; a curated context is a different selection
through material that still exists exactly as it was written.

This is the split git makes between objects and refs, carried one layer
further. A **record** is a blob: global, immutable, referenced rather than
copied. A **selection** — a path frozen and identified by what it selects,
not by who made it — is a commit. A **head**, the pointer an agent moves as it
works, is a branch ref. Ownership applies to the head: exactly one agent may
move it. A selection is shared freely: any agent may reference one, and
referencing costs nothing, because nothing is copied, only pointed at.

That sharing is what makes curation affordable rather than merely possible. Ten
children forked off one carefully curated path reference the same selection, so
they share its cached prefix by construction. If a path could reference only
individual records, each child would re-enumerate the same list, byte-identity
would become a coincidence somebody has to maintain, and when it drifted nothing
would fail — you would get a silent cache miss, which looks exactly like an
expensive workload rather than like a bug.

**Which records belong on a path is policy the core does not hold.** The core
owns the ability to express and assemble a path — to select, to order, to
render a selection to the bytes a model sees — never a default answer for
what a selection should contain. That line is what keeps a curation opinion
out of the core: the one selection rule the core ships is "everything, in
log order," which excludes nothing and is therefore not a judgment two
people could make differently. Past that point, what merges, what gets
dropped, and what a branch means are opinions, and
[opinions are plugins](#5-extending-conway).

> **Where the tree is today.** The path above is real but singular: conway
> computes exactly one per agent, by the fixed rule just described — a
> session's own records plus its inherited prefix, in that order, nothing
> omitted (see [`ARCHITECTURE.md`](ARCHITECTURE.md) §3.2 for that rule as
> it stands). Naming a path, persisting one apart from the session that
> produced it, or deriving a second by dropping or reordering records is
> designed and not yet built. When it lands, the fixed rule does not go
> away — it stays the one path a curator never has to build, because it
> already encodes no opinion.

---

## 2. Shaping a tree

A tree puts detail where it is used. The top level holds the shape of the
problem at low fidelity, meaning what is being done and roughly why. Each level
down narrows onto a specific area and sharpens. Deep agents often carry the
largest contexts in the tree, because they are the ones reading code, and they
are focused rather than small.

The hierarchy exists to put the right detail with the right agent. Making
children lightweight is not the goal, and treating it as one produces a tree
where every level knows a little about everything.

When placing information, the question is which level is the most specific one
that needs it. Put it there. Holding detail at a level that might need it later
costs quality rather than only money, because attention degrades well before the
nominal window limit, and a crowded context at the top of a tree affects every
decision made beneath it.

---

## 3. Do the work elsewhere, keep the distillate

This is the central idiom, and most other patterns vary it.

When an agent needs work done whose process it does not need to remember, it
creates a short-lived agent, lets that agent spend context freely, and keeps
only the distilled result:

```
create (fork or spawn)  ->  do the work  ->  distill  ->  report back  ->  discard
```

It is the shell pipeline instinct applied to agents: a child does one thing,
what comes back is its output, and its working memory is nobody else's problem.
What gets protected is the caller's context. A child that reads nine files, runs
the test suite twice, and follows two dead ends before answering has spent a
great deal of context, all of which disappears when the child does, and what
returns is a paragraph. Done inline, every tool result and every dead end lands
permanently in the context the caller has to keep reasoning in.

So: spend a child's context freely, and your own carefully.

### Reshaping a tree

Trees drift. An agent picks up a second job, a context that started tight fills
with material that turned out to be irrelevant, or work lands two levels below
where it belonged. Restructuring is the normal response, not a sign something
went wrong earlier.

The case that comes up most is wanting part of a context and not the rest, which
the primitives appear not to offer. They do, in combination:

```
fork  ->  distill the part that matters  ->  spawn a clean child with that briefing
```

That chain is partial inheritance, built instead of configured. The fork can see
everything, which puts it in a position to judge relevance; what it produces is
a briefing rather than a slice; and the spawned child starts clean with exactly
that. The reduction ends up as an artifact you can read, where a setting that
inherited "the last N records" would make the same cut invisibly, by a rule that
knows nothing about the work.

Three other moves are available:

**Fork from earlier.** Any point in any session can be forked from, including
sessions belonging to agents that have already finished. When a context went
wrong at a specific turn, go back to just before it and re-brief from there.

**Split a level.** When one agent carries two jobs, its context is the union of
two things that do not inform each other. Give each job a child that owns it and
let the parent keep the shape of the problem.

**Promote a side question, or fold it in.** An ephemeral child sometimes turns
out to be the real work. Folding it in means the child reports what it found and
terminates, so the answer joins the caller's context and the child's working
context is discarded. Promoting means the child becomes a session of its own and
the work continues where it already lives, leaving the caller's context
untouched.

Which one fits depends on which half is valuable. If it is the answer, fold it
in; a conclusion or a patch crosses the report channel cheaply, and that is the
distill pattern under another name. If it is the accumulated context, meaning
the files read and the understanding built on them, promote instead. A report is
a message, so a transcript pushed through it arrives as fresh content in the
caller's window, paying full input price for material the child already holds
and abandoning whatever cache prefix the child had built.

The three exits of `/ask` correspond to these choices: fork promotes, pull in
folds, discard does neither.

### `ask`, worked example of the composition

`ask` is not a primitive. It is fork, one turn, and a decision about the result,
and walking through it shows what building on the primitives looks like.

Forking at the current head produces a child that already knows everything the
caller does. Running exactly one turn on it produces an answer. The child is
marked ephemeral, so nothing about it is expected to persist. What remains is
the decision about whether that answer is worth keeping, and that decision is
why the pattern is useful at all.

Both surfaces offer the same composition, shaped for whoever is driving:

- `/ask` in the terminal runs it and then offers the choice explicitly: **fork**
  the child into a real session, **pull it in** so the question and answer join
  the transcript, or **discard** it. The command exists because the alternative
  is assembling a fork, a single turn, and a cleanup by hand every time you want
  to ask something in passing. The CLI takes that liberty for usability; the
  capability underneath is the same one the library exposes.
- `conway_ask` gives the model the same thing. It returns the child's full reply
  rather than a summary, so an orchestrator can draft the briefing for a
  `conway_spawn` without that drafting entering its own window. An
  optional `tools` argument narrows what the child may use, so a read-only
  inspection stays read-only.

Nothing in the harness knows what an "ask" is beyond that. It is two primitives
and a policy about the result, which describes most of what is useful here.

### Useful patterns

Recurring arrangements, none of them features, none of them known to the
harness.

**Distill.** One child, a job whose process you do not want, one result kept.
The others build on it.

**Map and gather.** Fork once so several children share a common inherited
prefix, then spawn N differently-prompted children beneath it and collect their
reports. The parent ends up holding N conclusions rather than N transcripts. It
suits problems that split cleanly into independent pieces: reviewing ten files,
checking one change against several criteria, searching a codebase in parallel.

**Panel.** The same arrangement with the children disagreeing on purpose. Give
each a different prompt over the same inherited context, and take either
independent attempts at one problem or one child arguing against another's
output. It suits questions where a single answer is likely to be confidently
wrong.

**Draft, then commit.** Use an ephemeral child to produce something expensive,
such as a plan or a briefing for another agent, and admit only the finished
artifact to the caller's context.

**Descend.** When a task needs real depth in one area, create a child that owns
that area and let it carry the large context, rather than widening the current
agent's.

---

## 4. Working with the cache

Prompt caching is most of what separates a tree of agents from an expensive way
to do one thing, and arrangements that cache well look almost identical on the
page to arrangements that do not.

**What caches is a prefix.** If the opening bytes of this request match the
opening bytes of an earlier one, that portion is cheap. Some backends match
implicitly; Anthropic wants explicit breakpoints, which conway places. The unit
is a prefix, in order, from the start, so what matters is how much two requests
share *before the first difference* rather than how much they share overall.

**Forking is cheap for this reason.** A fork's inherited context is a literal
immutable byte prefix frozen at the fork point, and siblings forked at the same
point open with the same bytes. Ten children forked from one point are largely
paid for after the first, which is the economic argument for map and gather. Ten
spawned children each carrying a long hand-written briefing share nothing at the
front, and every one is a fresh read.

**Churn at the front breaks it.** Anything that changes early bytes invalidates
everything after them. conway assembles context in a fixed order for this
reason: static content first, meaning system prompt and tool schemas, then the
inherited prefix, then the turn's own volatile records. Two consequences follow.
Put volatile things late, since a timestamp or a per-turn status line near the
top of a system prompt spends the entire cached prefix every turn for a few
tokens of content. And when a hook edits a request, where it edits matters more
than how much, because appending is nearly free while rewriting the opening
bytes is a full re-read.

**Curation has a price you can know in advance.** Because a derived path
selects from records that already exist and never rewrites them, it shares a byte
prefix with the path it came from up to the first thing it leaves out. So the two
curation moves cost very different amounts, and the difference is predictable
before either is made: dropping the most recent records is nearly free, dropping
the oldest spends the entire cached prefix, and reordering breaks at the first
moved element, which is strictly worse than either. A curation policy can optimise
against that. Summarizing cannot, because it changes everything after the point
it touches.

**It never changes results.** Cache hints change cost and never bytes. An
adapter has to produce byte-identical request content with all hints stripped,
and the adapter that emits hints has a test asserting that, so caching is safe
to ignore. You get the same answers and a larger bill.

**The numbers are recorded.** Every turn persists its usage: input tokens, cache
reads, cache writes. The terminal renders that as a per-turn percentage and a
script can read the same fields out of the session log, so this is a feedback
loop rather than a theory. Restructure, then look at the number. It is also the
only way to notice caching that has stopped working, which reads as a steady
zero and otherwise looks exactly like an expensive workload.

---

## 5. Extending conway

The core stays small, and capability arrives from outside it. Every opinion
baked into a core is a behavior an extension has to accommodate or work around,
and those accumulate until extending the harness costs more than using it.

The rungs below go from cheapest to most involved, and most needs are met well
before the top.

### Configuration

Backends and their credentials, which model a role names, permission rules,
budgets, and which plugins are installed are all declarative settings. Anything
a plugin adds is configured the same way, so the routing plugin's fallback
chains and breaker thresholds become settings once you have installed it. A
surprising amount of "conway does not do what I want" is a settings change.

### Hooks

Writing a plugin used to mean writing Rust, compiling it, and keeping up with
an API — still the only way to add a backend, or to reach conway's own types
in-process. For a new tool alone, that rung has widened rather than gained a
neighbor: an external program in any language can register one over a wire
protocol instead, without conway's source or a rebuild — conway's own
protocol ([`docs/plugins/subprocess-plugins.md`](docs/plugins/subprocess-plugins.md))
or MCP ([`docs/plugins/mcp.md`](docs/plugins/mcp.md)). Either shape is the
right cost for a new tool or a new backend, and far too high for what people
want most of the time: run the formatter after an edit, refuse tool calls that
touch a particular directory, log every command to a file, add a note to the
context when a session starts.

Hooks cover that. The shape will look familiar to anyone who has used Claude
Code's: you name an event in configuration and a command to run, the command
receives structured input on stdin, and it answers with its exit status and
whatever it writes back. No Rust, no build step, no API to track, and a shell
script is a legitimate extension.

The resemblance stops at the shape. The events have to name things conway
actually does, and conway's model is not that model, so the vocabulary is drawn
from the primitives rather than translated across.

```json
{ "hooks": { "rules": [
    { "id": "audit-shell", "event": "pre_tool_use", "match": "bash",
      "command": ["~/.conway/hooks/audit-command.sh"] },
    { "id": "fmt-after-write", "event": "post_tool_use", "match": "fs.write",
      "command": ["cargo", "fmt", "--"] }
] } }
```

`command` is an argument vector rather than one shell string, and that is the
same argument as the section above: a string only becomes a command once
something decides where the words break, and deciding that means predicting a
shell. The first element is the program and the rest are its arguments, settled
before anything runs. `id` is what makes a rule individually revocable, which a
deny-capable hook has to be.

What the core emits follows from [the primitives](#1-the-primitives): a tool
call about to run or just finished, a prompt submitted, a request assembled, a
child forked or spawned, a child reporting, a session starting. `pre_tool_use`
matters most, because the permission gate already sits there, and a hook
answering allow, deny, or deny with a reason the model reads is an
operator-authored permission policy with no Rust in it.

That list is open rather than fixed. A plugin declares the events it emits, so
installing one brings hook points along with whatever else it provides: a
routing plugin can offer a point before it commits to a candidate, a compaction
plugin one before it drops anything. Those events sit at the same level as the
ones conway emits, since a core that reserved the hookable moments for itself
would be privileging its own code in the one place this design says it should
not. An event a plugin declares and never fires is the same defect as a tool
that does nothing, and is treated as one.

Two consequences of the shape. A hook runs as a program with the operator's
privileges, so installing one is a trust decision of the same kind as installing
a plugin. And a hook that can deny a tool call is security-bearing: it fails
closed, it appears wherever other permission rules appear, and it is
individually revocable.

### Plugins

When a hook is not enough, meaning you need a new tool, a new backend, or
in-process access to conway's types, that is a plugin. The Rust plugin API is
the extension interface, and the built-in tools are written against it, so
there is nothing a built-in can do that yours cannot. Nothing in the tool
namespace is privileged, in the way that nothing in `/bin` is privileged over a
program you wrote yourself. An MCP server is a plugin that brings tools with it.

Backends are plugins too, which is easy to assume otherwise. There is no
privileged inference path in the core and no blessed list of providers, so:

- A provider conway has never heard of is a plugin you install, rather than a
  patch you submit and wait on.
- A local server and a hosted API are the same kind of thing here. Routing sees
  both through declared capabilities, so mixing them across roles in one session
  is ordinary.
- Wrapping a backend is as available as replacing one. A plugin that records
  every request for replay, injects a proxy, fakes a provider in tests, or fans
  one logical model across several endpoints is the same shape of thing as a
  backend.
- What a backend declares about itself is what the router reasons over: context
  window, tool-calling reliability, streaming behavior, caching mechanism.

### The default set

Shipping primitives rather than workflows still requires shipping something
that runs. This section answers one narrow question — **what must exist for
the harness itself to work at all?** The harness installs a small set of
plugins into itself because some questions have to have an answer before it
is a harness at all. That is not the same question as what the shipped
`conway` binary installs; see
[The binary is a different question](#the-binary-is-a-different-question)
below for that one.

Talking to an inference API is the clearest case. A harness that cannot reach a
model is inert rather than unopinionated. So the common dialects ship,
Anthropic-flavored and OpenAI-flavored endpoints, which between them cover most
of what anyone points a harness at, local servers included. Reading and writing
files is the same argument one step down. Inference is a core responsibility, so
something has to discharge it out of the box.

The test for membership is narrow: does conway still function with nothing at
all filling this role? If no, something must ship, and what ships is a default.
If yes, it belongs in the tier below and you install it deliberately.

That test also explains `bash`, which is built in and still not in the default
set. conway works with no shell tool, and the build without one is the safer of
the two, so it is opt-in.

A default is a default rather than a fixture. Swap it, wrap it, or remove it
through the same surface a third party uses, and instrument it without replacing
it, since a plugin that records requests or proxies them sits at the same level
as the thing it wraps. `coreutils` is the parallel: `ls` belongs to no kernel,
a system without it is unusable, and swapping the lot for busybox leaves a
working system. conway ships defaults because the questions cannot go
unanswered.

### The binary is a different question

The test above is deliberately narrow, and it has been asked to answer a
second question it was never built for: not "what must exist for the harness
to work at all," but "what should someone get when they install the `conway`
binary and run it?" Conflating the two either starves the binary — compaction,
memory, and skills all fail the harness's own test, so a binary held to that
test alone could never carry them — or widens the harness's own membership
test until it stops being narrow. Splitting the questions is the fix, and it
settles what the second answer looks like without a third tier of plugins to
hold it.

**The harness** stays exactly what "The default set" above describes: sharp,
narrow, the same for every embedder. Compaction, memory, skills, and MCP
support all fail its test, so none of them may become harness defaults on the
strength of being common or expected elsewhere.

**The shipped binary** is not the harness plus a little more of the same
thing — it is a fully-equipped coding agent, opinionated on purpose, built by
turning already-classified plugins on: some from the harness's own default
set above, some from the
[first-party tier](#first-party-plugins-and-why-they-are-not-defaults) below.
Every opinion it ships with is visible and removable through the same
surfaces a third party uses to extend conway at all — install it, and you can
also uninstall it. An opinion in the binary is not an opinion in the core,
and that is the whole distinction: it is what lets conway be agnostic in the
harness and usable on the first run of the binary, without either fact
costing the other.

> **Where the tree is today.** The binary does not yet turn on anything
> beyond the harness's own default set described above. `conway.routing`,
> `conway.stepguard`, `conway.skills`, `conway.memory`, `conway.path`, and
> MCP client support all exist and install, but ship off; compaction is the
> one first-party capability not written yet — see the note under
> [First-party plugins](#first-party-plugins-and-why-they-are-not-defaults)
> for exactly what exists. Becoming the opinionated, fully-equipped binary
> this section describes is tracked work, not a description of what
> installing conway gives you today.

**Why it has to become that**, in increasing order of importance. It is the
clearest demonstration that capability composes granularly at all — a real
application, assembled from pieces you can each see and switch off, is more
persuasive than a skeleton plugin arguing the same point in the abstract. And
it has to be good enough to replace whatever harness you are reading this on
now, full time, because a tool only improves through daily use by someone who
notices what is wrong with it — until conway is used that way, every priority
on this page is a guess.

That bar is a ladder, not a switch:

- [ ] **Supplement.** conway is used alongside your current harness, for real
      work, by choice, for some class of task.
- [ ] **No longer needed.** Comfort that the other harness could be
      uninstalled — which needs coverage of the features that actually
      matter day to day, *and* output quality better than the incumbent's,
      not merely comparable. Feature parity with worse results is not a
      reason to switch, including for conway's own authors.

### First-party plugins, and why they are not defaults

A capability being common does not make it neutral. Every serious harness has
compaction, memory, skills, MCP, and a policy for choosing which model gets a
turn. Each one makes a different choice about each,
and those choices are inseparable from how its authors work.

None of them passes the membership test above. conway runs without compaction,
without memory, and without MCP, with the core's own
one-line answer to which model gets a turn. They are wanted rather than
required.

So there is a tier between the default set and the wider ecosystem: plugins
written and maintained in this repository, shipped with it, and not installed by
default. Dynamic routing, context compaction, memory, skills, MCP support. You
get them by choosing them.

> **Where the tree is today.** Dynamic routing, memory (`conway.memory`), and
> skills (`conway.skills`) are all built and installable, as are five members
> this list does not name by capability — the provider adapters, a
> session-rewind command, repeated-step detection (`conway.stepguard`, which
> §6 below is about), the out-of-process subprocess plugin host, and
> context-path composition (`conway.path` — the tool a model calls to
> compose what a session sends as context on its next turn). MCP
> support is built too, but does not name a plugin id the way the others
> do: an operator lists an external MCP server under `[plugins].mcp` and its
> tools attach directly, the same shape as the subprocess host one line
> over. Compaction is the one member of this list not written yet.

This tier is permanent rather than a staging area for things on their way into
the core, for three reasons.

The opinion stays available without being imposed. Nobody rebuilds compaction
from nothing, and nobody inherits a compaction policy they did not ask for and
might not notice.

They demonstrate that the extension surface is real. Dynamic routing is the
clearest case, since model selection is the most core-looking thing a harness
does. If conway cannot write its own routing as a plugin on the surface a third
party uses, that surface is unfinished, and a first-party plugin needing a
private interface is a bug report against the plugin API.

They are worked examples with a maintainer. Reading one shows what a plugin of
that shape looks like, and forking one is a reasonable way to get the variant
you wanted.

Two of them are worth describing precisely, because they cover ground people
expect a core to own.

**Routing.** The core keeps the smallest thing that has to be true: a role names
a model, since a request has to go somewhere, plus the vocabulary for reporting
which model served a turn and why. The plugin owns ordered fallback chains,
filtering candidates on what they can do, and health tracking with circuit
breaking. Those are judgments about a particular deployment, and a laptop
talking to one hosted API wants different answers than a fleet failing over
between local servers.

**Admission.** Whether a request fits belongs with the backend, further down
than either. Only the thing talking to a given endpoint knows that model's real
window, how that provider counts tokens, whether a reasoning budget draws on the
same allowance, and what a refusal looks like when it arrives. A backend answers
admissible or not and says so with numbers; a router uses that answer to pass
over a candidate that cannot take the request. The core's own commitment is
narrower than either: when the answer is no, it surfaces the refusal instead of
trimming the request or moving it to a larger model.

### Supplying the policy yourself

An embedder has one more level available: the ports themselves. The permission
gate is supplied by the consumer, so the rule a tool call must satisfy is your
code, and a denial can carry a reason the model reads and adapts to. A
`ContextHook` sees every assembled request before it routes and may edit it,
drop parts of it, rewrite the system prompt, or narrow the tools the model is
told about. The router is a port on the same footing, so replacing conway's
model selection wholesale takes one builder call. Hooks at this level are async,
so a policy can call a model of its own to decide.

---

## 6. Decisions conway leaves to you

Each of the following is a judgment call that depends on your workload, your
budget, and your tolerance for surprise. An answer baked into the core would be
wrong for everyone whose answer differs, and wrong invisibly, at the moment they
are least able to notice.

Several already have a plausible answer written and shipped as a
[first-party plugin](#first-party-plugins-and-why-they-are-not-defaults), which
you install if you want it. What matters is that you took it, rather than
discovering later that it had been applied on your behalf.

**What to forget when context fills.** There is no automatic compaction. Nothing
is dropped, rewritten, or summarized behind your back, because a compactor
encodes a guess about what your work can afford to lose. The first answer is
structural: control what enters context at all, per
[Do the work elsewhere](#3-do-the-work-elsewhere-keep-the-distillate). When you
do want condensing, it is a context hook, and there is a first-party compaction
plugin to install or fork. Summarize the oldest N records, drop tool results
older than K turns, keep the diff and discard the exploration that produced it.
Which of those is right depends entirely on what you are doing.

> **Where the tree is today.** The `ContextHook` port this rests on is built
> and does everything described here. The first-party compaction plugin is
> not written, so today you write the hook yourself rather than forking one.

**Where a turn should go.** The core resolves a role to a model and stops. It
cannot see prompt text to do it, and it holds no view on what should happen when
that model is unavailable, overloaded, or too small. Ordered fallback, filtering
on capabilities, health tracking and breakers all belong to a routing plugin.
Choosing the role in the first place sits above both, in your calling code, an
agent definition, or a plugin that classifies and picks. Whether a summarization
turn belongs somewhere cheaper than a refactor is a budget question with no
general answer, and a harness that guesses is one you have to fight when it
guesses wrong.

**How much to isolate.** The core's boundary is the tool set, and each plugin
confines what it can: `conway.fs` takes a root, and a plugin that reaches
anything else is expected to do the same for its own operations. Stronger
isolation composes from outside, by running conway in a container or giving the
agent a tool that confines itself. A worktree per agent is the same kind of
answer, reached through a tool call rather than a harness feature. conway does
not sandbox, because doing that properly means constraining a process tree it
does not own across a platform matrix it would have to track, and implying
containment it cannot deliver is worse than saying plainly what it does.

**When to intervene in a loop.** Repeated-step detection, retry ceilings, and
circling-agent heuristics are not in the core. The seam exists — a
`ToolObserver` sees each finished call and returns what to record — so the
policy is yours to write, including writing none. `conway.stepguard` is the
first-party answer if you want one: it notices the third identical call and
says so, and you decline it by not installing it.

**What a failure should cost.** conway prefers a loud, predictable refusal to a
clever recovery. A request too large for the model it was headed to is rejected
with a typed error naming what did not fit, never silently trimmed or moved to a
larger model. A colliding session id is an error pointing at `--resume` rather
than an overwrite. If you would rather recover than fail, a hook gets a second
pass at an overflowing request before the error is raised.

---

## 7. Knowing what happened

conway does little on your behalf, which means that when something behaves
unexpectedly the explanation has to be recoverable rather than reconstructed.
Three properties of the record make it so, and they hold whatever you have
installed.

**Everything is written before it is acted on.** The log holds what was actually
sent rather than what was meant to be sent, and it survives a crash partway
through a turn.

**Every segment carries its origin.** For any agent, what its context contained
and where each part of it came from is available. This is the answer to why a
child did something inexplicable, which is usually that it was handed something
other than what you assumed.

**Every assistant record names the model and why it was chosen.** Which backend
served a given turn is a property of the record. That stays true when you
replace the router, because the vocabulary is core and any router has to return
a reason alongside a candidate.

How you read it is a separate matter, and depends on what you have installed.
The CLI lists, shows, and exports sessions. The terminal draws the agent tree
and can explain how a role would resolve right now, in as much detail as the
installed router can provide. The log itself is line-delimited JSON, so anything
you point at it will do. And any point in a session can be forked from, which
makes the record a debugging instrument rather than only an archive: return to
just before it went wrong and continue from there.

---

For how the pieces are built, meaning crates, ports, and the data flow of a
turn, see [`ARCHITECTURE.md`](ARCHITECTURE.md). For the discipline applied when
changing them, see [`CONTRIBUTING.md`](CONTRIBUTING.md).
