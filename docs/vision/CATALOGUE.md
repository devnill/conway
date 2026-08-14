# The catalogue: what a default install should have one toggle away

> Board item `01M00QEQ0PVAM2S7Y9EQNZV32F` (R1). Read-only research; this is
> the one file it writes. Source questions: `INTENT.md` §7a (the shipped
> binary may be opinionated; the harness underneath may not) and §7b (the
> dogfooding ladder — rung one is *supplement*, rung two is *coverage plus
> output quality better than the incumbent's, not comparable to it*).

## How to read this page

For every candidate: **what it is**, which of the [three surfaces](INTENT.md#7-three-surfaces-all-first-class)
it serves (terminal app / one-shot / embedded), whether it costs
**configuration**, a **hook**, or a **plugin** (`PHILOSOPHY.md` §5's three
rungs, cheapest first), roughly what it takes, and which rung of the
dogfooding ladder it belongs to (`INTENT.md` §7b).

**"Available" means installable and toggleable, not shipped active.** A
first-party plugin in this repository's tier — `PHILOSOPHY.md`'s "default
set" vs. "first-party, not default" split — is the shape everything below
should take. None of it belongs in the harness (the membership test stays
*does conway still function with nothing filling this role?* — see
`PHILOSOPHY.md` §5's "The default set").

Rung two needs **output quality better than the incumbent's, not
comparable** (`INTENT.md` §7b). Entries below marked **[quality]** are about
that axis, not feature coverage, and are the easiest thing on this page to
miss — a catalogue this size pulls attention toward the countable items.

This page is opinionated on purpose. It is not a transcription of every
feature four other harnesses ship; `INTENT.md` §2 names that as the failure
this whole project exists to avoid, and §9 already rules some of it out by
name.

---

## What conway already has, and where it already leads

Worth stating before the gaps, because two of the harnesses studied treat
these as unsolved and conway already has a stronger answer to both.

- **Tree-structured sessions with a real graph model**, not just navigable
  history. Fork and spawn already give conway what Pi's tree-structured
  sessions and the DeepSeek harness's forkable/replayable trajectory each
  reach for from a flatter starting point — an append-only log of immutable
  records, a persisted `rewind` command (`conway-plugin-history`,
  `/conway.history.rewind <seq>`), and a designed (not yet built) path
  layer, `INTENT.md` §5a/§5b, that turns "fork into a branch" into
  something with a literal, git-shaped meaning. **[quality]**
- **Typed refusal over silent recovery** as a load-bearing property, not a
  slogan: no truncation on overflow (`RoutingError::ContextTooLarge`), no
  permission-denied exit code that kills a run instead of feeding the model
  a result, no automatic compaction. Every plugin catalogued below inherits
  this posture or it is not a conway plugin in spirit, only in packaging.
  **[quality]**
- **Provenance on every context segment** — `user prompt`, `agent def`,
  `skill`, `tool registry`, `inherited`, `system note`, `child result`, and
  so on (`docs/agents.md`'s `/context` section) — which is the substrate
  memory and skills need to be inspectable rather than a black box, and
  which none of the three studied harnesses expose to this granularity.
  **[quality]**
- **`Provenance::Skill` and plan mode already exist as mechanisms.**
  `ContextBuilder` already injects one full-body `Skill` segment per
  configured skill (`crates/conway-runtime/src/context/builder.rs`), and
  `docs/permissions.md`'s `Plan` mode already allows non-mutating tool
  categories only. Both of the entries below that reuse these are cheaper
  than they look because of this.

---

## Tier 1 — rung one: ship first, cheap, load-bearing

Ordered by (value while using conway as a supplement) ÷ (cost). These are
what makes rung one "much closer than it looks" (`INTENT.md` §7b) rather
than a demo.

### 1. Project memory file (an `AGENTS.md`/`CLAUDE.md` equivalent)

**What it is.** Auto-discover a project-local instructions file (walking up
from cwd, same precedence rule `.conway/settings.json` already uses —
`docs/getting-started.md`) and inject it into every session's context as
its own provenance kind, not folded into `agent def`.

**Surface.** Terminal app and one-shot equally; embedded hosts opt in via
config, since not every embedding wants a filesystem convention imposed on
it.

**Cost.** Configuration, arguably a small hook (`session_starting` already
fires with `{agent_id, session, cwd}` — `docs/plugins/hooks.md` point 13 —
so a plugin can already read a file at that path and hand back a context
addition without any new core mechanism). No new port.

**Why it's near the top.** Claude Code's `CLAUDE.md` is the single most
used piece of standing context in daily coding work — described in the
survey material as the agent's "constitution" for a repo — and Hermes'
project-context file plays the identical role. This is the cheapest item
on the whole page relative to how much daily friction it removes, which is
exactly the "much closer than it looks" case §7b is describing.

**Rung.** One.

### 2. Progressive-disclosure skills as an installable first-party plugin

**What it is.** A packaged version of `docs/plugins/cookbook.md` example 4:
a `ContextHook` that narrows a `Provenance::Skill` segment to a one-line
`name: description (call read_skill(...))` index entry, plus a
`read_skill` tool that returns the full `SKILL.md` body on demand. Add a
directory-scanning layer (`.conway/skills/*/SKILL.md`, frontmatter
`name`/`description`) so skills are authored as files, not a Rust
constant, which is the one real gap between the cookbook toy and something
installable.

**Surface.** Terminal app (where a model decides mid-session it wants a
skill) and embedded (a host that wants the same index/fetch pattern for
its own tool set). One-shot benefits less, since a single prompt rarely
justifies the round-trip.

**Cost.** Plugin. Small — both halves are **already proven implementable
today** against the current architecture (`docs/plugins/cookbook.md`'s "The
two named acceptance verdicts": neither point needed anything new). The
work left is packaging (file format, directory scan, frontmatter parsing),
not architecture.

**Why it's near the top.** This is Pi's signature idea — "one line per
skill resident in context, full instructions loaded only on invocation" —
which the task brief explicitly asks to copy in spirit. It is also
Hermes's static half (the dynamic, self-authoring half is Tier 2, below).
It is a direct application of `INTENT.md` §3's signal-to-noise claim: the
model should not pay full attention-budget for a procedure it is not
currently using.

**Rung.** One (static skills, hand-authored). The self-improving half is
rung two — see below.

### 3. A todo/task-tracking tool

**What it is.** A small stateful tool (`todo_write`/`todo_read` or
equivalent) the model uses to externalize its own plan across a long turn
sequence, rendered back into context each turn as a compact segment.

**Surface.** Terminal app primarily; useful in one-shot for long
multi-step `-p` runs; embeddable as any other tool is.

**Cost.** Plugin — an ordinary `Tool` implementation plus a `ContextHook`
or `PromptSegment` contribution to keep the list visible. No new port.

**Why it matters.** In Claude Code this is one of the few "accretion or
load-bearing?" features that reads as load-bearing rather than a fad: it
measurably keeps a long agentic run on track and gives the operator a
glance-able plan without reading the transcript. It is small enough that
skipping it saves little, and multi-step work is exactly where conway is
being asked to prove itself.

**Rung.** One.

### 4. Background / async tool execution

**What it is.** A way for a long-running `bash` (or any tool) call to
return control to the agent loop immediately and be polled or awaited
later, rather than the turn blocking until the process exits.

**Surface.** Terminal app (a long build or test run shouldn't freeze the
session) and embedded (a host driving a long job wants the same).
One-shot mode is a poor fit by definition — it needs a nonblocking
consumer to poll, which a single `-p` invocation and exit is not.

**Cost.** Plugin extending `conway.shell`, plus (if the model itself is
meant to poll) a small tool pair (`bash_start`/`bash_poll`). Medium — this
touches the tool-execution seam directly rather than context assembly, so
it needs care around cancellation and the session log (a still-running
background job outliving its turn is new territory conway's log model
hasn't had to represent before).

**Why it's here and not lower.** Pi explicitly refuses to own this and
tells you to reach for tmux instead — a reasonable stance for a four-tool
harness, less reasonable for a tool being asked to replace a daily driver
where "wait for the test suite" is a constant. Medium priority rather than
top: composition (a tmux pane, `nohup` plus a status file) genuinely covers
this today, so it's a comfort feature, not a blocker.

**Rung.** One (comfort), leaning toward two (coverage) if dogfooding shows
it's used constantly.

### 5. Ship the ephemeral compaction hook as an installable plugin

**What it is.** Package `docs/plugins/cookbook.md` example 2's
`CompactOldToolResultsHook` — folds old `ToolResult` segments into a
labeled summary segment, every turn, ephemerally — as `conway.compaction`,
installable and off by default, exactly like `conway.stepguard` today.

**Surface.** All three, since it's a pure `ContextHook`.

**Cost.** Plugin. Small — the code already exists and runs
(`docs/plugins/cookbook.md`, twelve passing tests across the cookbook's
scratch crate). Packaging only.

**The caveat, stated as plainly as the cookbook states it.** This is
**explicitly not** what most people mean by "compaction": it recomputes
the fold every turn (nothing persists it — `LogRecord::ContextMask` has no
producer anywhere in the tree, and the item that would have built one was
filed and then cancelled per `docs/plugins/hooks.md` point 9's citation),
and `INTENT.md` §3 calls compaction "the enemy, not a feature" for good
reason: it is a lossy, unauditable summary applied to material someone was
reasoning over. Ship it anyway, off by default, labeled honestly as the
weaker ephemeral form — because *some* users will want it regardless of
the house opinion, and "you install it, you chose it" is the whole point
of `PHILOSOPHY.md` §5's default-set test. Do not let this entry read as
"conway now has compaction" on a feature-comparison chart; it has the
weakest version of the feature everyone else means by that word.

**Rung.** One, with the caveat above load-bearing enough to repeat in
release notes if this ships.

### 6. An MCP client

**What it is.** A plugin that speaks MCP as a client, turning an external
MCP server's tools into ordinary `conway::plugin::Tool` registrations. Not
an MCP *server* (conway exposing itself over MCP) — that's a different,
lower-priority question.

**Surface.** Terminal app and embedded; less relevant to one-shot, though
not excluded.

**Cost.** Plugin — larger than the entries above. `PHILOSOPHY.md` §5
already states the shape ("An MCP server is a plugin that brings tools
with it") but nothing implements the client protocol, connection
lifecycle, or credential handling for a remote server today. Large: this
is genuinely new ground, not a repackaging of something already proven.

**Why it's here despite the cost.** MCP is the one gap on this page that
blocks a whole category of real work outright rather than degrading it —
anyone with an existing MCP server (a database, a ticket tracker, a
design tool) has no path into conway today, full stop. It is explicitly
named as promised-and-missing in `PHILOSOPHY.md` §5's own "where the tree
is today" note and `docs/vision/STATE-OF-THE-UNION.md` §3.4.

**Rung.** One, because its absence is a hard wall for a specific and
common class of task, not a comfort gap.

---

## Tier 2 — rung two: coverage needed before the incumbent could go

These are bigger, and none of them is a blocker for using conway
*alongside* Claude Code — they're what's missing before Claude Code could
be *uninstalled*.

### 7. Self-improving skills (the learning loop half)

**What it is.** Hermes's actual differentiator, and the part of "skills"
that is genuinely novel rather than a repackaging of Pi's idea: an agent
that, after a complex task, writes or updates a `SKILL.md` of its own,
gated by an approval setting (`skills.write_approval`) before anything is
committed unattended.

**Surface.** Terminal app mainly (a supervised session is where a write
gets approved); one-shot and embedded can consume the resulting skill
library without participating in writing it.

**Cost.** Plugin, but a real new hook point is needed underneath it: a
"session finished, review it" moment nothing in `docs/plugins/hooks.md`'s
sixteen points currently offers (the closest, `child_reported`, fires per
child result crossing back to a parent, not per session end, and never
for a root — see `hooks.md` point 13). Large/XL.

**Why it's rung two, not one.** This is explicitly **[quality]**, not
coverage: a static skill library (Tier 1 #2) already gets most of the
day-to-day value; what this adds is skills that get *better* with use
instead of staying exactly as good as the day they were written. That is
squarely what §7b means by "output quality better than the incumbent's" —
Claude Code has no equivalent at all, so this is a case where conway could
beat the incumbent rather than merely match it. Do this only once Tier
1's static skills have proven the shape is used, or the learning loop has
nothing to learn from.

**A design note conway's own philosophy makes for free.** Whatever writes
these skills should prefer **mechanical, structural triggers** over an
LLM deciding "was that a complex task?" — `INTENT.md` §5a's argument for
non-LLM curation (cheap, deterministic, testable, incapable of
hallucinating) applies just as well to "should a skill get written" as it
does to context selection. Hermes's own trigger list (5+ tool calls, an
error resolved, a correction received) is already mostly structural in
this sense and is a reasonable starting point.

### 8. Persistent cross-session memory

**What it is.** Small durable facts (preferences, standing corrections,
project conventions) that should always be in context, distinct from
skills (procedures loaded on demand) — Hermes's own stated distinction,
and a sound one.

**Surface.** All three; a one-shot invocation benefits from memory just as
much as a terminal session does, arguably more, since it has no
transcript of its own to fall back on.

**Cost.** Plugin. Large — needs its own storage (SQLite is the obvious
choice given conway's existing session store), a write path (a tool the
model calls, plus an approval posture matching skills), a read path (a
`ContextHook` contribution at session start), and a retrieval policy for
what surfaces when.

**[quality] The retrieval policy is the part worth getting right, not
just building.** Hermes uses "FTS5 session search with LLM summarization
for cross-session recall." conway's own architecture argues against the
second half: `INTENT.md` §5a's case against LLM-driven curation (lossy,
unauditable, breaks the cache) applies to memory retrieval exactly as it
applies to context compaction — a model guessing which past fact is
relevant is the same shape of problem as a model guessing what to drop.
**Recommendation:** structural retrieval (FTS/BM25 over stored facts,
filtered by provenance, recency, and explicit tags) with no
summarization step, keeping every surfaced fact byte-identical to what was
written. This is where conway's own principles give it a real chance at
"better than the incumbent," not just "present like the incumbent" — worth
stating as the design constraint for whoever builds this rather than
leaving it to be rediscovered.

**Rung.** Two, and one of the two hardest items on this page.

### 9. The plugin-reachable status-line surface

**What it is.** `docs/plugins/hooks.md` point 12
(`status.declare/1`/`status/1`) — letting a plugin (not just an embedder)
contribute a field to the status line.

**Surface.** Terminal app only; this is purely a TUI concern.

**Cost.** Core-adjacent plugin-API work, not a plugin itself. Medium.
Currently **designed-not-built** and untracked by any other item
(`hooks.md` point 12's own status line).

**Why it's here at all.** It isn't user-visible value on its own — it's
infrastructure the memory and skill plugins above want (a small
"3 facts recalled" or "skill: git-commit active" status fragment is the
kind of ambient legibility that makes a quality-of-life feature feel
trustworthy rather than invisible, and `INTENT.md` §5b's fifth hazard —
"a person has to be able to see it" — is exactly this argument one layer
up from curation). Sequence it just ahead of memory/skills if those get
built, not after.

### 10. Out-of-process (non-Rust) plugin transport

**What it is.** The wire protocol for running a plugin as a separate
process, `docs/plugins/README.md`'s "Everything not in this set" item one:
"designed and never built."

**Surface.** All three, indirectly — this is what would let an MCP
ecosystem-style third-party plugin market exist without everyone writing
Rust.

**Cost.** Large/XL, core-adjacent.

**Why rung two, not one.** Nothing on Tier 1 needs this — every entry
there is in-process Rust. It matters for *coverage* in the sense that
Claude Code's plugin ecosystem is largely not-Rust, and a healthy
third-party shelf eventually wants the same. Not urgent; the in-process
plugin surface is nowhere near saturated yet (seven of `hooks.md`'s sixteen
points — 7-12 and 14 — are still designed-not-built even for in-process use).

---

## Explicitly not recommended

Naming these is the point of the exercise (`INTENT.md` §2's complaint is
exactly that nothing ever leaves) — a catalogue that only adds is the
failure mode the brief warns against.

- **Cron/scheduling as a harness feature.** Hermes ships this natively.
  conway's one-shot surface (`-p`) already exists specifically to be
  "usable by someone who is not writing code" (`INTENT.md` §7) — pointing
  system cron, launchd, or any external scheduler at `conway -p` is
  composition that already works, and building a scheduler *inside* the
  harness duplicates a solved problem the OS already owns. This is
  accretion in the exact shape `INTENT.md` §2 describes: it had a moment
  (Hermes needed it because its deployment model is a persistent bot), it
  is not conway's deployment model, and it should not become a feature
  conway now has to maintain and eventually can't remove.
- **A menagerie of terminal/sandbox backends** (Docker, SSH, Modal,
  Daytona, Vercel Sandbox — Hermes ships seven). `PHILOSOPHY.md` §6 already
  answers this: "stronger isolation composes from outside, by running
  conway in a container... a worktree per agent is the same kind of
  answer, reached through a tool call rather than a harness feature." Each
  backend Hermes ships is a maintenance surface and a plausible source of
  the exact feature-that-had-a-moment problem this project exists to
  avoid. If demand shows up for one specific backend, it's a plugin
  someone writes, not a harness commitment to seven.
- **Sandboxing.** `INTENT.md` §9 already rules this out by name: "conway
  cannot actually deliver [it]." Nothing above should imply otherwise.
- **A desktop application.** Also already an explicit non-goal
  (`INTENT.md` §9), and neither DeepSeek's harness nor this survey found a
  reason to revisit it — DeepSeek's own reference material is explicit
  that no desktop app is planned either.
- **Output styles / persona switches as a dedicated feature.** Claude
  Code's `/config` output-style picker is a solved problem one layer down:
  an agent def's system prompt already carries this, and conway's
  three-rung extension model (`PHILOSOPHY.md` §5) puts "change the system
  prompt" at the cheapest rung, configuration, already. A dedicated
  feature here would be an opinion (which styles ship, what they're
  called) about something that's already fully expressible without one —
  the textbook case of a feature that has "had its moment" the day it
  ships, since whoever wants a style writes an agent def.
- **Filesystem checkpointing/snapshotting as a harness feature.** conway
  already has the conversation-history half of this (rewind via
  `conway.history`). The filesystem half is exactly the case
  `PHILOSOPHY.md` §6 makes for `cd`/worktrees: "a worktree per agent...
  reached through a tool call rather than a harness feature." Git already
  solves this; conway should not grow a second, weaker version of git
  inside the harness.
- **A dedicated "thinking budget" dial as a harness concept.** This is
  provider-shaped, not harness-shaped — each backend already declares what
  it supports, and a role's routing config is where this belongs
  (`PHILOSOPHY.md` §5's "what a backend declares about itself is what the
  router reasons over"). Building a harness-level abstraction over it
  ahead of a second or third provider needing something different is
  guessing at a shape nobody has asked for yet.

---

## If only three things ship before the next dogfooding checkpoint

In order: **(1)** the project memory file — cheapest thing on this page
and the single most-used piece of standing context in the incumbent's
daily use; **(2)** progressive-disclosure skills, packaged from an
architecture that already proves it costs nothing new; **(3)** an MCP
client, because it is the one gap that blocks whole categories of work
rather than merely degrading them.

**The largest single lever for rung two is not on this list.** It's
`PLAN.md`'s D1 — the context path, mechanical cherry-picking over
summarization. Every memory and skill entry above inherits its output
quality from that keystone: a skill index that gets narrowed by structural
selection rather than a model's guess, a memory recall that shares a byte
prefix with its origin rather than re-reading the whole store, is what
"better than the incumbent, not comparable" actually cashes out to. This
catalogue answers *what* the shelf should hold; D1 is what makes anything
placed on it better than what Claude Code already has, rather than a
same-shaped feature with conway's name on it.
