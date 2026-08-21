# Using conway day to day

[`getting-started.md`](docs/getting-started.md) gets you installed and talking to a
model. This page is what comes after: the things you actually do in a working
session, the ones that are not discoverable from the screen, and the limits
worth knowing before you hit them.

It is a walkthrough, not a reference. Where a reference page already says a
thing precisely, this page links to it rather than restating it —
[`interactive.md`](docs/interactive.md) for the TUI surface,
[`agents.md`](docs/agents.md) for fork/spawn semantics,
[`plugins/authoring.md`](docs/plugins/authoring.md) for writing hooks and plugins,
[`scripting.md`](docs/scripting.md) for `-p`.

---

## The first thing that will confuse you

**`/help` does not list the slash commands.** It shows keybindings, and says so
in its own title. To see the commands, type `/` on an empty input line — the
palette that opens is the list, and it filters as you type.

That is worth knowing on minute one, because the obvious move fails.

---

## A session, start to finish

```console
conway                      # interactive
conway --root ~/code/thing  # ...confined to one directory
```

`--root` is the security boundary; `--cwd` only sets where relative paths
start. A root confines every path argument of every path-taking tool, for the
root agent and everything it forks or spawns. It does **not** confine what a
shell command does — `bash`'s command string runs verbatim. A genuinely
confined agent is one spawned *without* `bash`.

Type a message and press Enter. `Alt-Enter` or `Shift-Enter` inserts a newline
instead (both are bound, because some terminals send a plain Enter for
Shift-Enter).

### When the model asks to use a tool

You get a prompt with four answers, and the fourth is the interesting one:

| Key | Meaning |
|---|---|
| `y` | allow once |
| `a` | allow always — remembered, and revocable later in `/settings` |
| `n` | deny |
| `Esc` | **deny with feedback** |

`Esc` denies *and hands the model a reason it can read and adapt to*. That is
usually what you want when the call is wrong rather than forbidden — "not that
file, use the one in `src/`" gets you a corrected second attempt instead of a
dead end.

`PageUp`/`PageDown` scroll the command while the prompt is up, which matters
when a `bash` invocation is longer than the box.

### Watching it work

`Ctrl-E` expands or collapses **all** tool output at once. Long `grep` results
and file reads collapse to a preview line by default; one key gets you the
whole thing and back.

The status line carries the numbers that matter: which model served the turn,
context used, tokens, and the session id with its current sequence —
`session 01ABC…@42`. Hold on to that `@42`; [rewinding](#when-a-turn-goes-wrong)
needs it.

---

## Hooks: making conway match your habits

This is the cheapest way to make conway behave the way you work, and it needs
no Rust. A hook is a command conway runs at a named moment.

Put this in `~/.conway/settings.json`:

```json
{
  "hooks": {
    "rules": [
      {
        "id": "fmt-after-edit",
        "event": "post_tool_use",
        "match": "edit",
        "command": ["cargo", "fmt", "--"],
        "timeout_ms": 5000
      }
    ]
  }
}
```

**`match` is the field that makes hooks usable.** Without it a rule fires on
every tool call — every read, every glob — and your script has to work out for
itself whether it should do anything. `match` takes an exact tool name or a
`*`-glob (`"fs.*"`). It applies to `pre_tool_use` and `post_tool_use`, the two
events that carry a tool name.

Three behaviours worth knowing:

- **Omitting `match` fires for everything.** That is loud, and therefore
  self-correcting.
- **A `match` on an event with no tool name** — `session_starting`,
  `child_spawned`, `request_assembled`, `child_reported`, `prompt_submitted` —
  is a load-time config error naming your rule's `id`. It will not silently do
  nothing.
- **A misspelled tool name is the one case with no safety net.** It matches
  nothing, quietly, forever. Check the tool's registered name rather than the
  name you remember.

### What a hook can and cannot do

A hook can **observe**, and on `pre_tool_use` and `prompt_submitted` it can
**deny** — with a reason, exactly like the `Esc` answer above. It **cannot edit**
what the model sees. There is no shape in the answer type that carries
replacement text; rewriting context is an in-process `ContextHook`, which means
Rust. That is deliberate, not an oversight.

Hooks fail **closed** where they can deny (a missing script, a timeout, garbage
on stdout all mean "deny") and **open** where they only observe (a broken
logging script will not break a working tool call).

### Seeing and turning off what you installed

`/settings` lists every deny-capable hook rule as a fourth revocable group
beside your allow, deny and prompt rules — id, event, and its `match`. A hook
whose script is broken still appears, which is precisely when you most need to
find it. Revoking is session-scoped, like every other `/settings` toggle.

A hook runs with **your** privileges. There is no sandbox between a hook command
and everything you can touch. See
[`plugins/trust-and-security.md`](docs/plugins/trust-and-security.md).

---

## Working the agent tree

The whole idiom is: **spend a child's context freely, and your own carefully.**
A child reads nine files, runs the suite twice, follows two dead ends — and what
comes back to you is a paragraph. Done inline, all of that would live
permanently in the context you have to keep reasoning in.

Two primitives, and the choice is about what the work needs to know:

- **`/fork`** — the child inherits your entire context at this moment. Use it
  when the work depends on the conversation so far.
- **`/spawn`** — the child gets nothing but the prompt and, optionally, an
  agent definition. Use it when the task is self-contained. A clean slate is
  cheaper and less distractible.

Free text after either gets classified by a cheap model into a confirmation
card first: `Enter` to accept, `e` to edit the classified prompt, `Esc` to fall
back to your raw text. Inference never silently chooses for you.

### `/ask` — the one you will use most

`/ask <question>` forks an ephemeral child, runs exactly one turn, and shows you
the answer over a modal. Closing it forces one of three choices:

| Key | What happens |
|---|---|
| `f` | **fork** — promote the child to a real session |
| `p` | **pull in** — merge question and answer into this transcript |
| `Esc` | **discard** — it never happened |

That third exit is the point. Most side questions are not worth keeping, and
`/ask` is how you ask one without polluting the conversation you are trying to
keep clean.

### Seeing the tree

`/agents` opens the panel. Every row shows *how* the agent was made —
`fork @seq N`, `@<agent_def>`, `(inherit)`, `(ephemeral)`. `v` cycles a
draw-time visibility filter (active / all / finished) that never mutates
anything. `Esc` closes the panel keeping the focused agent; press again to
return to the root conversation.

`/steer <agent> <message>` sends a message to a running child. It is applied at
a turn boundary, never mid-generation.

`/context <agent>` shows what an agent's context actually contains. When a child
does something inexplicable, the answer is usually that it was handed something
other than what you assumed — and this is where you find that out.

`/why` shows the last routing decision: which model served the turn and why.

---

## When a turn goes wrong

Install the history plugin, in `~/.conway/settings.json`:

```json
{ "plugins": { "install": ["conway.history"] } }
```

Then `/conway.history.rewind 42` forks the session at sequence 42 and points
the TUI at the child. The `42` is the number in the status line.

The parent is untouched — a rewind creates a child, it never truncates. Your
original session is still there, still listed by `conway sessions list`, still
forkable. Nothing is lost by rewinding; you are branching, not deleting.

Plugin commands are namespaced by their plugin's id, which is why it is
`/conway.history.rewind` rather than `/rewind`. That is what makes it impossible
for a plugin to shadow a built-in command.

---

## Tips that are not on the screen

**Fork siblings in one turn.** A fork's inherited context is a literal byte
prefix, and siblings forked at the same point share it — so ten children forked
together are largely paid for after the first. That is the economic argument for
fanning out. Spreading the same ten forks across ten turns does not share
nearly as well.

**Put volatile things late.** Context assembles in a fixed order — static
content, then the inherited prefix, then the turn's own records. Anything that
changes early in that order invalidates everything after it. A timestamp near
the top of a system prompt costs you the whole cached prefix every turn.

**Watch the cache percentage.** It is in the status line, and it is a feedback
loop rather than a theory. Restructure, then look at the number. It is also the
only way to notice caching that has silently stopped working, which reads as a
steady zero and otherwise looks exactly like an expensive workload.

**`Ctrl-P` / `Ctrl-N`** recall previous and next input history — faster than
retyping a long prompt you want to vary.

**`Ctrl-D` quits, but only when the input line is empty.** Handy, and it will
not fire mid-draft.

**`Home` / `End`** jump the transcript to top and tail when the input is empty;
with text present they move the cursor instead. The keys are shared and go to
whichever thing currently owns them.

**Turn `bash` on deliberately.** It ships off for the TUI. Add it to
`tools.builtin_plugins` in your settings when you want it. In one-shot mode
it is always registered and `--allowed-tools` is what actually gates it.

**Use `-p` for anything scriptable.** Stable exit codes, `--output-format
json|jsonl`, model output on stdout and diagnostics on stderr. It fails closed:
an empty allow-list denies every tool. See [`scripting.md`](docs/scripting.md).

**Any session can be forked at any point, including finished ones.** The log is
the truth and it outlives the process, so `--fork-from <sid>@<seq>` works on a
session that ended hours ago or crashed mid-turn.

---

## What conway deliberately will not do

Knowing these up front saves you looking for them.

**There is no compaction.** Nothing is dropped, summarized, or rewritten behind
your back. A session that outgrows the model's window gets a loud, typed error
naming what did not fit — it is never silently trimmed or moved to a bigger
model. The intended answer is structural: push work into children and keep the
distillate. A first-party compaction plugin is named in
[`PHILOSOPHY.md`](PHILOSOPHY.md) as a thing you would install, and **does not
exist yet**.

**Installing a new backend still means building a binary.** A backend needs
full in-process access to `conway-core`'s types, so that rung stays Rust,
compiled in, with `plugins.default_backends` selecting among the ones your
binary links. A new *tool* is no longer the same story: an external program
in any language can register one over a wire protocol, without conway's
source or a rebuild — see [`docs/plugins/subprocess-plugins.md`](docs/plugins/subprocess-plugins.md)
(conway's own protocol) and [`docs/plugins/mcp.md`](docs/plugins/mcp.md) (MCP).

**conway does not sandbox.** The harness's responsibility ends at the permission
gate. Stronger isolation composes from outside — a container, a worktree per
agent, a tool that confines itself.

None of these are oversights. Each is a judgment call conway leaves to you
rather than guessing at, and [`PHILOSOPHY.md`](PHILOSOPHY.md) §6 argues each
one. The point of knowing them here is that you will not waste an afternoon
looking for a setting that was never going to exist.

---

## Recording friction while you work

If you're using conway on conway's own tree, something in this page will
turn out to be awkward in a way it doesn't describe. The moment that
happens, record it — reconstructing it later from memory is the same
failure mode as prose checked against prose:

```console
scripts/dogfood-note.sh friction --title "<short title>" --body "<what happened>"
```

That's the whole ceremony: one command, filed as a board item, no board
tooling to learn. See [`docs/dogfooding.md`](docs/dogfooding.md) for the
full loop — attaching friction to an existing item instead of filing a new
one, the end-of-session note, and how to check whether the path is actually
being used.

## Where to go next

- [`interactive.md`](docs/interactive.md) — the full TUI reference: every command,
  the settings menu, the status line field by field
- [`agents.md`](docs/agents.md) — fork and spawn in depth, result contracts, what an
  agent may act on
- [`plugins/authoring.md`](docs/plugins/authoring.md) — writing your first hook, then
  your first plugin
- [`plugins/skills.md`](docs/plugins/skills.md), [`plugins/memory.md`](docs/plugins/memory.md) —
  the two first-party context plugins, off by default, on when you install them
- [`permissions.md`](docs/permissions.md) — the full permission model and its stated
  limits
- [`PHILOSOPHY.md`](PHILOSOPHY.md) — how these primitives are meant to be
  composed, and why they are shaped this way
