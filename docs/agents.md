# Agents: fork, spawn, and the tree

conway organizes work as a tree of agents. This page covers the two ways to
create a child, how you drive one from each of conway's three surfaces, what
an agent may act on, and how to inspect the tree once it exists.

## Fork and spawn: the two primitives

There are exactly two ways to create a child agent, and they are kept
deliberately distinct rather than folded into one "subagent" call with a
mode flag:

| Primitive | The child gets | Typical use |
| --- | --- | --- |
| **Fork** | The forker's entire context, as an immutable prefix up to the fork point, plus a directive appended on top. | Branch off a running agent to try something without disturbing it — a second opinion, a parallel exploration, a narrower follow-up. |
| **Spawn** | A clean slate. Naming an agent definition gives it that definition's system prompt and tools; omitting one inherits the spawning session's role and model routing (not its transcript). | A focused, independent task that shouldn't carry the parent's accumulated history — a code reviewer, a test runner, a research subagent. |

Forking is cheap regardless of how much context it carries: nothing is
copied, the child holds the same underlying log by reference. Siblings
forked at the same point share an identical prefix, and because it's a
literal byte-prefix, a backend that does prompt-caching reuses it — the
economics get better with more forking, not worse.

**There is no partial-inheritance knob, and there will not be one.** You
cannot ask a child to inherit "some" of the parent's context. If you want a
child that knows about part of what its parent knows, the answer is to
restructure the hierarchy — fork it, then narrow what you tell it to focus
on via the directive; or spawn it and hand it exactly what it needs in the
prompt — not to look for a third primitive that sits between fork and spawn.
This is a deliberate design position, not a gap: see
[`.design/whitepaper.md`](../.design/whitepaper.md) §4.1 for the reasoning.

## Why the hierarchy exists

Context economy comes from structure, not from trimming. The top level
holds only the basics of its own task; detail is delegated downward. A
deeper agent ends up with a *more focused* context than its parent, not a
bigger one — narrower in scope even when it's carrying more raw tokens about
that scope. Over-inheritance (forking when a spawn
would do) is as much a design defect as under-inheritance (spawning a child
that then has to rediscover context its parent already had): both waste the
resource this whole hierarchy exists to spend deliberately.

**The cost consequence:** a fork is expensive in absolute terms — it carries
everything the forker knew — and cheap in relative terms on a provider that
caches the shared prefix, since every sibling forked at the same point pays
for that prefix once. Delegation *structure* is therefore a cost lever: a
flat pile of large forks off one root costs differently than a tree where
each layer narrows before delegating further. Decide fork vs. spawn, and
where in the tree to put each child, with that in mind.

## The three surfaces

Fork and spawn are reachable from all three ways of driving conway, and all
three ultimately go through the same runtime primitive
(`conway_core::ports::SubagentHost`) — there is exactly one fork mechanism
and one spawn mechanism underneath, not three.

| Surface | Fork | Spawn |
| --- | --- | --- |
| Interactive TUI | `/fork [<text>]` or `/fork @<agent> <directive>` | `/spawn [@<agent_def>] [<prompt>]` |
| Embeddable library | `SessionHandle::fork(from, ForkSpec)` | `SessionHandle::spawn(from, SpawnSpec)` |
| Model tool call | `conway_subagent` with `mode: "fork"` (or `conway_ask` for the fork-and-read-the-reply shorthand) | `conway_subagent` with `mode: "spawn"` |

### The TUI

`/fork @<agent> <directive>` and a plain `/spawn @<agent_def> <prompt>`
(with a target/def named explicitly) start an **autonomous** child: it runs
to completion in the background while you keep working with whatever you had
focused, and you see it appear (and finish) in the `/agents` panel.

A **bare** `/fork` or `/spawn` — no explicit target, just optionally some
free text — instead opens a fresh **interactive, keep-alive** child and
switches your focus to it: free text is classified into a fork/spawn recipe
and confirmed before anything is created, so you can type a natural-language
request and conway decides which primitive it maps to. See
[`interactive.md`](interactive.md) for the full command syntax and the
confirmation-card flow. `/ask <text>` is a third, narrower TUI affordance —
a one-off fork-and-read-the-answer that never touches your live transcript;
see [`sessions.md`](sessions.md) for what happens to it afterward.

### The embeddable library

`SessionHandle::fork`/`::spawn` take a `ForkSpec`/`SpawnSpec` builder and
return the new child's `AgentId`:

```
handle.fork(from, ForkSpec::new("focus on the auth module").role(role)).await?;
handle.spawn(from, SpawnSpec::new("review this diff").agent_def("reviewer")).await?;
```

`ForkSpec` has no `cwd`/`root` field — a fork always inherits the forker's
working directory and confinement root, since overriding either would be
incoherent with the transcript the child inherits. `SpawnSpec` has both
(`SpawnSpec::cwd`/`::root`), for the embedder case of scoping a clean-slate
child to one directory of a larger checkout. Both specs take
`keep_alive(bool)` (default `false`): an opt-in for a child that idles for
your next `prompt_agent` call after each turn instead of finishing once its
directive/prompt is answered — see [`sessions.md`](sessions.md)'s keep-alive
section for when to set it and what changes.

### A model tool call

`conway_subagent` is the general primitive: `mode` (`"fork"` or `"spawn"`),
`prompt`, and optional `agent_def`/`role`/`budget`/`tools`/`result_contract`,
plus `await` (default `true`) to block for the child's result or return its
`agent_id` immediately for fan-out. `conway_ask` is a narrower, fork-only
shorthand — no `mode`, no `agent_def`/`role` argument — that returns the
child's full reply text rather than a structured `AgentResult`, meant for
drafting or curating context out-of-band without spending the caller's own
window on the reasoning. `conway_steer`/`conway_await`/`conway_cancel`
round out the control surface:

| Tool | Does | Key arguments | Permission class |
| --- | --- | --- | --- |
| `conway_subagent` | Fork or spawn a child. | `mode`, `prompt`, `agent_def?`, `role?`, `budget?`, `tools?`, `result_contract?`, `await` | Dangerous |
| `conway_ask` | Fork-only; returns the child's full reply text. | `prompt`, `budget?`, `tools?` | Dangerous |
| `conway_steer` | Send a message to a running child, landing at its next turn boundary. | `agent_id`, `text` | Requires approval |
| `conway_await` | Block for a child's terminal result. | `agent_id` | Safe |
| `conway_cancel` | Cancel a running child. | `agent_id`, `reason?` | Requires approval |

`conway_subagent`/`conway_ask` are `Dangerous`: starting a child hands it
the ability to make its own tool calls, transitively, one hop removed from
`bash`. `conway_await` alone needs no approval — reading a result back
carries no side effect. A model-invoked fork/spawn is always autonomous
(`keep_alive: false`); interactive keep-alive children exist only on the
TUI and embedder surfaces above.

## What an agent may act on

Every one of the five tools above — and their `SessionHandle` counterparts —
is scoped to the calling agent's own subtree: itself, or any descendant
reached by walking fork/spawn links downward. Steer, await, or cancel a
sibling or a stranger's agent and you get a typed refusal, not a forged
action landing somewhere else; start a child under a `parent` outside your
own subtree and the call is refused the same way. This applies uniformly —
a model-invoked tool call is checked against the *runtime-assigned* identity
of the agent that actually dispatched it, never an id the model supplied in
its arguments, so a tool call can't claim a wider reach than the agent
making it actually has. The session's root agent is the one exception that
isn't really an exception: its own subtree *is* the whole tree, so an
operator/embedder call through `SessionHandle` (which always acts as the
root) can reach anything in the session, the same guarantee every other
agent gets for its own, smaller subtree.

The practical effect you'll encounter: fork or spawn a child three levels
deep, and neither that child nor its own descendants can steer, cancel, or
even discover an agent outside their own branch — `conway_subagent`'s
underlying tree listing is scoped the same way. Nothing here is
configurable per-agent; it follows structurally from where in the tree an
agent sits.

## Inspecting the tree

`/agents` (TUI) opens a panel listing every agent in the session's tree —
status, how it was created (`fork @seq N`, `@agent_def`, `(inherit)`,
`(ephemeral)`), and its place in the hierarchy. `/tree` is an undocumented,
hidden alias that prints that same panel's rows as plain transcript text
instead of opening the panel — typing it works even though it isn't in the
`/` palette.

`/context <agent>` shows one agent's assembled context as a list of
segments, each with an estimated token count and its **provenance** —
where that piece of context came from. Running it against a real session
shows entries like these (the same shape `conway sessions show`'s
`context_report` records expose, JSON-side):

```text
70N5H52ZCP1E... tool registry b685ff3e... 2504tok
4DC1P1FCQ7PT... user prompt 19tok
```

The provenance vocabulary you'll see: `user prompt`, `agent def` (a named
def's system prompt), `skill` (an injected prompt fragment), `tool
registry` (the schema set the model was told about, identified by hash),
`inherited` (a verbatim prefix carried over at fork time, naming the parent
session and range), `fork directive`, `parent steer`, `tool result`,
`system note` (runtime-authored, e.g. repeated-step detection), and `merged
/ask` (a pulled-in ephemeral question). See
[`interactive.md`](interactive.md) for the exact `/agents`/`/context` key
bindings and panel layout, and [`sessions.md`](sessions.md) for
`conway sessions tree`, the equivalent read over *persisted* sessions rather
than the live runtime tree — it works on a session from any past process,
not just the one you're currently in, which the live `/agents` panel can't
do: a resumed session's own past fork/spawn children don't come back as
live tree nodes (their history stays fully readable, just not through the
live tree).

## `--cwd` and `--root`

Both flags interact with fork/spawn: `--root` confines the root agent and,
by inheritance, every subagent it forks or spawns; a subagent can only
narrow it further, never widen it. See
[`getting-started.md`](getting-started.md)'s `--cwd`/`--root` section for
the full explanation and the danger of confusing the two. Permission
depth — pattern grants, project trust, how a `Dangerous`-class tool call
like `conway_subagent` gets approved — is covered in
[`permissions.md`](permissions.md).

## The `cd` tool: moving the working directory mid-session

Where `--cwd` sets the working directory once at launch, the built-in
`cd` tool lets the *agent* move it during a session. It takes one `path`
argument (absolute, or relative to the current working directory) and
every subsequent tool call that resolves a relative path — `read`,
`write`, `edit`, `glob`, `grep`, `bash` — starts from the new directory.

Three semantics worth knowing before you (or the model) reach for it:

- **A `cd` takes effect starting the *next* batch of tool calls, not the
  current one.** A `cd` issued alongside a `read` in the same batch does
  not affect that `read`. For a one-off move — run this single command
  somewhere else, then come back — the per-call `cwd` argument on
  `bash`/`glob`/`grep` applies immediately, like a `(cd X && cmd)`
  subshell, and leaves the session's working directory untouched. Use
  `cd` only for a persistent move.
- **`cd` never changes where the session started.** A resumed session
  returns to its original spawn directory; the move is live state, not
  part of the session's identity.
- **`cd` is confined by the agent's root.** Its `path` argument is a
  declared path, so the same confinement check that guards `read`/`write`
  applies: under `--root`, a `cd` to a directory outside the root is
  denied before the permission gate is consulted — an agent cannot move
  its working directory somewhere it is not allowed to work. It is in
  the `Move` category, which matters for [`permissions.md`](permissions.md)
  rules that select by category (and for plan mode, which does not permit
  `Move`).

