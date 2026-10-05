# `conway.todo`: a task list the model writes and ticks off

The first-party plugin, implemented by `crates/conway-plugin-todo`, that
gives a model two tools (`todo_write`, `todo_read`), a compact context
segment, a status-line entry, and an operator-facing slash command for
tracking a multi-step task as a short list of items with a status each.
Depends on [`concepts.md`](concepts.md) for vocabulary (plugin, `Tool`,
`ContextHook`, `[plugins].install`).

## Why this exists

On a long, multi-step run a model that writes its own plan down and ticks
items off as it finishes them tends to stay on that plan rather than
improvising one turn at a time, and an operator reading the transcript (or
just glancing at the status line) can see where the run actually is without
reading every tool call that came before. This plugin packages that as an
installable capability rather than something every agent definition has to
reinvent in its own system prompt.

## Opt-in, not a default

Installing `conway.todo` is **not** part of `conway-cli`'s own opinionated
first-run set. A model-authored plan is a genuine opinion about how an agent
should work: a short task gets no benefit from one, and an operator who
never asked for a visible task list should not have a new context segment
added to every request unasked. Opt in explicitly:

```json
{ "plugins": { "install": ["conway.todo"] } }
```

With it uninstalled, neither tool exists, no segment is ever added, and the
status line never shows a `todo` entry.

## The tools

- **`todo_write(items: [{id?, text, status}])`** replaces the WHOLE current
  list — not a patch against the previous call. `status` is one of
  `pending`, `in_progress`, or `done`; any other string fails the call with
  a plain argument error rather than being silently coerced into one of the
  three. Give an item an `id` to keep updating the SAME item across calls;
  omit it on a brand new item and a fresh one is minted and shown in the
  reply. Calling it with an empty `items` array clears the list.
- **`todo_read()`** reads the current list back, exactly as the most recent
  `todo_write` left it, with no side effect.

Both answer with the same compact text block: a `conway.todo (N/M done):`
header followed by one `- [<glyph>] <id>: <text>` line per item (`[ ]`
pending, `[~]` in progress, `[x]` done).

## The context segment

A `ContextHook` renders the current list as a single segment near the END
of the assembled request, only when the list is non-empty — volatile
content belongs late, not mixed into the stable, cacheable part of a
request. It is a strict APPEND: every other already-assembled segment is
returned byte-for-byte unchanged, in the same order, so this segment never
disturbs whatever a backend might cache about the rest of the request. Its
provenance is a system note naming the plugin and the current `done/total`
count, so `/context`'s rendering of that segment's provenance never reads
as an anonymous note.

## How the list survives a resume

A `Tool` has no privileged way to append an arbitrary record to a session's
own log. The channel this plugin uses is the same one
`conway-plugin-stepguard` already established for an unrelated reason: a
`ToolObserver` watches for this plugin's own successful `todo_write` calls
and answers with a note — the whole current list, JSON-encoded — which the
runtime turns into a real, persisted system-note log record. Context
assembly already turns every past system-note record back into an ordinary
history segment on every subsequent request, including the first one a
freshly started process builds after `--resume` (a resumed session's
context is rebuilt from the full on-disk log, not from anything held in
memory by the process that wrote it). So when this plugin's own in-memory
cache has nothing for an agent — exactly the shape a resume leaves behind —
its context hook finds the most recent matching note in that replayed
history, decodes the list from it, and uses that both to render the current
turn's segment and to refill the cache, so `todo_read` and the status line
see the same list immediately after, without waiting for another
`todo_write`.

This is session-log persistence only — a list does not follow a model into
a different, unrelated session.

## The status line and the operator command

- A `todo` status-line contribution shows `todo: N/M` for whichever agent
  most recently called `todo_write` — the most recent write wins, since the
  status line carries one value per key, not one per agent.
- `/conway.todo.list` prints the calling agent's current list as a plain
  text block, the same rendering the tools themselves reply with, for an
  operator who wants to see it without asking the model.

## What this plugin does NOT do

- No dependency graph between items — there is no notion of one item
  blocking another.
- No scheduling — nothing runs an item, reminds about one, or reorders the
  list on its own.
- No cross-session persistence beyond the session's own log, and no
  patch-style update: `todo_write` always replaces the whole list.
