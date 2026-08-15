# Agents: fork, spawn, and the tree

conway organizes work as a tree of agents. This page covers the two ways to
create a child, how you drive one from each of conway's three surfaces, what
an agent may act on, how to constrain what a child must hand back, and how
to inspect the tree once it exists.

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

**"The same point" means the same assistant turn.** N sibling forks
requested as N tool calls in ONE reply all read the parent's transcript
before any of them is dispatched, so they open with byte-identical bytes and
a caching backend serves N-1 of them from cache. Fork once, wait for that
child to finish or otherwise let the reply close, then fork again in a
LATER reply, and the second fork reads a transcript that has moved on in
the meantime — at minimum by the first fork's own tool result, often more —
so it no longer shares a cache-eligible prefix with the first. Same
children, same intent, a materially different bill, decided entirely by
whether the forks were batched into one reply. To get the fan-out
discount, request every sibling fork together, in one reply — see
`await`'s own description below for the tool-call shape this takes.

**There is no partial-inheritance knob, and there will not be one.** You
cannot ask a child to inherit "some" of the parent's context. If you want a
child that knows about part of what its parent knows, the answer is to
restructure the hierarchy — fork it, then narrow what you tell it to focus
on via the directive; or spawn it and hand it exactly what it needs in the
prompt — not to look for a third primitive that sits between fork and spawn.
This is a deliberate design position, not a gap: see
[`whitepaper.md`](whitepaper.md) §4.1 for the reasoning.

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
| Model tool call | `conway_fork` (or `conway_ask` for the fork-and-read-the-reply shorthand) | `conway_spawn` |

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

**`keep_alive` and `result_contract` cannot be combined**, and setting both is
a startup error naming both fields rather than a silent failure. They ask for
opposite things: a contract is checked when the child finishes and its
validated answer is handed back, and `keep_alive` is precisely the instruction
never to finish — so the answer would be validated and then have nowhere to go,
leaving your `await` hanging forever. Drop `keep_alive` to receive the
validated result, or drop `result_contract` to keep the child open. (The
model-invoked `conway_fork`/`conway_spawn` tools never set `keep_alive`, so
this only affects a library caller building a `ForkSpec`/`SpawnSpec`.)

### A model tool call

`conway_fork` and `conway_spawn` are two separate tools, not one call with a
mode argument — see [`PHILOSOPHY.md`](../PHILOSOPHY.md#choosing-between-them)
for why: a model reaches the fork/spawn choice by picking a tool name, which
settles it before any argument is filled in, and it keeps each tool's
`prompt` honest (a directive to a child that already has the context, for
`conway_fork`; a complete statement of the task to a child that has none,
for `conway_spawn`). Both take the same remaining arguments: `prompt`, and
optional `agent_def`/`role`/`budget`/`tools`/`result_contract`, plus `await`
(default `true`) to block for the child's result or return its `agent_id`
immediately for fan-out.

**For `conway_fork`, the fan-out discount above ("the same point means the
same assistant turn") only materializes if every sibling fork is issued as
its own tool call within ONE reply** — `await: false` on each is how a
model returns immediately instead of blocking on each child in turn, but
`await` alone does not batch anything: three `await: false` forks spread
across three separate replies still pay full price each, exactly like
three blocking ones would. `await` only controls whether THIS call blocks;
whether siblings share a cached prefix is controlled entirely by whether
they were requested together. `conway_spawn` has no equivalent caveat: a
spawned child inherits none of the caller's transcript by design, so its
request never carries anything for a turn boundary to affect either way.

`conway_ask` is a narrower, fork-only shorthand —
no `agent_def`/`role` argument — that returns the child's full reply text
rather than a structured `AgentResult`, meant for drafting or curating
context out-of-band without spending the caller's own window on the
reasoning. A `conway_ask` child inherits the caller's full context,
effective role, and the caller's own `agent_def` (system prompt, tools
selector, model pin) when the caller itself was spawned from one — exactly
like an ordinary `conway_fork`. The one exception is a def-declared
`result_contract`, which never reaches a `conway_ask` child: the reply is
always plain text (there is no structured field a contract could validate),
so applying one could only ever turn a good answer into a rejection.
`conway_steer`/`conway_await`/`conway_cancel` round out the control
surface:

| Tool | Does | Key arguments | Permission class |
| --- | --- | --- | --- |
| `conway_fork` | Fork this agent into a new child continuing its context. | `prompt`, `agent_def?`, `role?`, `budget?`, `tools?`, `result_contract?`, `await` | Dangerous |
| `conway_spawn` | Spawn a new, independent child with no inherited context. | `prompt`, `agent_def?`, `role?`, `budget?`, `tools?`, `result_contract?`, `await` | Dangerous |
| `conway_ask` | Fork-only; returns the child's full reply text. | `prompt`, `budget?`, `tools?` | Dangerous |
| `conway_steer` | Send a message to a running child, landing at its next turn boundary. | `agent_id`, `text` | Requires approval |
| `conway_await` | Block for a child's terminal result. | `agent_id` | Safe |
| `conway_cancel` | Cancel a running child. | `agent_id`, `reason?`, `mode?` | Requires approval |

`conway_fork`/`conway_spawn`/`conway_ask` are `Dangerous`: starting a child
hands it the ability to make its own tool calls, transitively, one hop
removed from `bash`. `conway_await` alone needs no approval — reading a
result back carries no side effect. A model-invoked fork/spawn is always
autonomous (`keep_alive: false`); interactive keep-alive children exist only
on the TUI and embedder surfaces above.

**A fan-out caller (`await: false`) is notified when a child finishes, even
without ever calling `conway_await` on it.** A child's `AgentLoop::finish`
always delivers its terminal result to its parent's mailbox; previously that
delivery was only ever consumed by a caller that had actually blocked on
that specific child's id via `conway_await`/`AgentTree::await_result` — a
caller that started several children and never awaited any one of them by
id had no way to learn any of them had finished. The parent's own very next
turn now carries that completion automatically: a `Role::System` segment
tagged `Provenance::ChildResult`, naming the child's `agent_id` and status
and summarizing its result — the exact same turn-boundary mechanism
`conway_steer` already uses to land a steer message, not a second, separate
notification channel. This is purely additive: `conway_await` still blocks
and resolves exactly as before, and the two paths never race — a child that
was awaited is resolved by `conway_await`'s own return value; the mailbox
notification is what covers every child that was not.

`conway_cancel`'s `mode` defaults to `immediate`: it stops the target right
away, without waiting for its current turn, and propagates to the whole
subtree — every descendant's own `CancellationToken` is derived from its
parent's, so a hard cancel collapses the branch structurally. `graceful`
instead lets the target finish its in-flight turn, then stops at the next
turn boundary — and stops only the named agent; it does not itself cancel
descendants. **A graceful cancel cannot reach an idle `keep_alive` agent
waiting between turns** (or a resumed root's very first iteration): that
wait is not a turn boundary a queued cancel drains at, so use `immediate`
for an agent you expect to be idle rather than mid-turn. `SessionHandle::
cancel_with` is the embedder-facing counterpart; `SessionHandle::cancel`
keeps calling it with `CancelMode::Immediate`, unchanged from before this
distinction existed.

## Budgets

Every agent runs under a `Budget` with four independent ceilings. The first one
to trip ends that agent with `ResultStatus::BudgetExceeded`, naming which
dimension it was — so an agent that set several can tell what stopped it. In
one-shot mode a root agent tripping any of them is [exit code
5](scripting.md#exit-codes).

| Dimension | Bounds | Scope |
| --- | --- | --- |
| `max_steps` | Turns taken. | Per user turn for a [keep-alive](sessions.md#keep-alive-sessions) session, otherwise the agent's whole life. |
| `max_tool_calls` | Tool calls dispatched. | Same. |
| `max_tokens` | Input + output tokens accrued. | The agent's whole life, always. |
| `deadline` | Wall clock. | The agent's whole life, always. |

The two turn-scoped dimensions are runaway-tool-loop guards, so they reset at
each user-turn boundary: a keep-alive session has to survive an unbounded number
of turns, each independently bounded, rather than having its whole lifetime
capped. The two lifetime-scoped ones are cost and time ceilings, where a total is
the thing you actually want to bound.

`max_tool_calls` counts calls **dispatched**, not results returned. A batch
cancelled part-way through still counts every call it started, because some of
them have already run their side effects.

Set the defaults for a session's own root agent in `settings.json`:

```json
{
  "limits": {
    "max_steps": 40,
    "max_tool_calls": 0,
    "max_tokens": 0,
    "deadline_secs": 0,
    "max_parallel_tools": 4
  }
}
```

`0` means no ceiling for `max_tool_calls`, `max_tokens`, and `deadline_secs`.
`max_parallel_tools` is not a budget — it caps how many calls in one batch run
concurrently, and never ends an agent.

A child overrides any of these per call: `budget` on `conway_fork`/
`conway_spawn`/`conway_ask` for a model, or `ForkSpec::budget`/
`SpawnSpec::budget` for an embedder. **A child never inherits its parent's
budget** — it gets its own defaults and you set the rest explicitly:

- **Model-invoked** (`conway_fork`/`conway_spawn`/`conway_ask`): 40 steps and a
  10-minute deadline, no token or tool-call ceiling. Override per call, or move
  the defaults with the `subagent.max_steps`, `subagent.deadline_secs`,
  `subagent.max_tokens`, and `subagent.max_tool_calls` plugin config keys.
- **Embedder-invoked** (`ForkSpec`/`SpawnSpec`): `Budget::default` — 40 steps
  and nothing else. There is no deadline here, because a host application that
  wants one knows its own workload better than a constant would.

A fan-out is where this matters: ten children with no explicit budget is ten
independent 40-step allowances, and nothing bounds the tree as a whole.

## Result contracts

A **result contract** is a JSON Schema a subagent's `structured` result
must satisfy before conway will let the child's run finish as `completed`.
A child sets `structured` by calling the `report` tool; if it never
reports one, `structured` is `null` and is validated as such. It's a
delegation-only mechanism, scoped to what a subagent hands back to whoever
started it:

| Declared by | How |
| --- | --- |
| The model, per call | `conway_fork`/`conway_spawn`'s `result_contract` argument |
| An embedder | `ForkSpec::result_contract`/`SpawnSpec::result_contract` on the facade builder |
| An agent definition | `result_contract` in a `.conway/agents/*.md` file's frontmatter |

A root agent never has a result contract — a root has no `SubagentSpec` to
source one from, on any surface.

A `.conway/agents/*.md` def's `result_contract` frontmatter key is applied
when a child spawns from that def, exactly like the def's role, system
prompt, tools, and model pin. It is the **default**, not an override: if the
call site that started the child (the model's `conway_fork`/`conway_spawn`
argument, or an embedder's `ForkSpec`/`SpawnSpec::result_contract`) also declares a
`result_contract`, the call site's contract wins and the def's is not
applied at all — only a spawn with no call-site contract of its own falls
back to the def's. This mirrors how the def's `tools` selector already
works: a call-site value shadows the def's, it does not merge with it.

**"Spawns from that def" means the call site *named* it** — a `conway_fork`/
`ForkSpec` that leaves `agent_def` unset and instead inherits the forker's
own def (see below) does NOT pick up that def's `result_contract`, even
though it picks up everything else the def carries (system prompt, tools,
model pin). A result contract is declared at a call site — the model's
argument, an embedder's builder field, or naming a def explicitly — and is
never inherited merely because the def that happens to define it was. This
is the concrete reason a bare `/fork` (which never sets `result_contract`
and inherits its def) does not require its interactive child to call
`report` while simultaneously denying it that tool.

**`conway_ask` takes no `agent_def` argument, but its child inherits the
caller's own def anyway** — the answer to what was, before, an open design
question: a forked child inherits its parent's `agent_def` (system prompt,
tools selector, model pin), the same as an ordinary `conway_fork`, enforced
at the `SubagentHost`/`Runtime::ask` trait boundary rather than at the
`conway_ask` tool callsite (`conway-tools` has no `SessionMeta` lookup
surface to do this itself). The def's `result_contract` is the one field
explicitly excluded from that inheritance: it can NEVER reach a
`conway_ask` child, regardless of what def the calling agent was itself
spawned from, because `conway_ask` always returns plain reply text — there
is no `structured` field on its result for a contract to validate, so
applying one could only ever turn a good answer into a rejection, never
satisfy anything the caller reads back. See
`crates/conway-tools/src/subagent/ask.rs`'s module doc for exactly where
this inheritance is filled in, and `conway_core::ports::subagent`'s
`SubagentHost::ask` doc for the enforcement itself.

Validation runs at the natural end of a turn — one with no tool calls in
it, not a check after every tool call:

| Outcome | What happens |
| --- | --- |
| `structured` satisfies the schema | The agent finishes normally, status `completed`. |
| First mismatch | conway appends a `system_note` to the transcript quoting the validation errors, and gives the agent one more turn to fix it. |
| Second consecutive mismatch | The run ends immediately, status `rejected` — no further retries. |

That corrective turn is exactly one, always — not configurable per
contract, per spec, or via `settings.json`; there is no retry-count knob.
The corrective turn is free to make tool calls of its own; validation only
re-runs on the next turn that itself ends with no tool calls, and if that
turn calls `report` again, the new call's `structured` value is what gets
checked (it replaces the earlier one). For a
[keep-alive](sessions.md#keep-alive-sessions) child, the one-retry budget
resets at the start of each new user turn, so a violation from an earlier
turn never counts against a later one.

The system note conway appends on the first mismatch reads, verbatim
(conway's source format string):

```
the structured result failed its result_contract: {}
```

— where `{}` is the schema validator's own messages, joined with `; `.

A `rejected` result is not a crash: the child ran to completion, its final
`structured` output just never satisfied the schema, even after the one
correction. The terminal `AgentResult` carries `status: "rejected"` and a
`missing` list — the validator's own failing-path messages (for example
`` `/summary: is a required property` ``), not necessarily literal field
names. How a rejection reaches the parent depends on which surface it
awaited from:

| Awaited via | What the parent gets |
| --- | --- |
| `conway_fork`/`conway_spawn` (with `await` at its default) or `conway_await` | The serialized `AgentResult` as the tool result, flagged `is_error: true` — anything other than `completed` counts as an error result |
| `SessionHandle::await_agent` (embedding) | The `AgentResult` itself; match on its `status` field — there is no `is_error` on the struct |

Read `missing` to tell whether the delegation itself went wrong or the
contract doesn't match what the agent can actually produce, then either
loosen the schema or clarify the prompt.

Result contracts are the one thing conway writes into a transcript without
you or the model asking. An installed plugin can write more — see
[`sessions.md`](sessions.md#repeated-step-notices) for the first-party one
that does.

## What an agent may act on

Every one of the six tools above — and their `SessionHandle` counterparts —
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
even discover an agent outside their own branch — `conway_fork`/
`conway_spawn`'s underlying tree listing is scoped the same way. Nothing here is
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
`system note` (written by the harness or an installed plugin, e.g. a
[result-contract](#result-contracts) violation or a
[repeated-step notice](sessions.md#repeated-step-notices)), `merged
/ask` (a pulled-in ephemeral question), and `child result` (a fan-out
child's terminal result, landed automatically on your next turn — see
[the model tool call section above](#a-model-tool-call)). See
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
like `conway_fork`/`conway_spawn` gets approved — is covered in
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

