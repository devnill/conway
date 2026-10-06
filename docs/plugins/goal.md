# `conway.goal`: a standing reminder for long runs

The first-party plugin, implemented by `crates/conway-plugin-goal`, that
makes the BUILT-IN `/goal` TUI command (see
[`docs/interactive.md`](../interactive.md#goal-a-reminder-that-survives-twenty-turns))
actually store and surface something: a compact context segment near the
end of the request while a standing goal is set, and a `goal` status-line
entry. Depends on [`concepts.md`](concepts.md) for vocabulary (plugin,
`ContextHook`, `[plugins].install`).

## Why this exists

After enough turns of exploring, trying things, and backing out of dead
ends, neither the model nor the operator reading the transcript can see the
objective a long run started with. `/goal <text>` sets a single sentence
the model is reminded of near the end of its context on every subsequent
turn, so the run stays oriented without the operator having to restate it.

## `/goal` is a built-in, not a plugin command

Unlike `conway.todo`'s `/conway.todo.list` or `conway.checkpoint`'s
`/conway.checkpoint.rollback`, `/goal` is NOT namespaced to this plugin's
id. Setting a standing goal is ordinary TUI vocabulary an operator should be
able to type regardless of which plugins happen to be installed, the same
footing `/steer`/`/cancel`/`/context` already have. What this plugin owns
instead is the REMOVABLE half of the behavior: the context segment and the
status-line entry. The built-in command itself checks whether
`conway.goal` is installed before persisting anything — see "Uninstalling
it" below.

## In the default opinion set, deliberately

Unlike `conway.todo` (a MODEL-authored plan, which is a genuine opinion
about how an agent should work that not every task benefits from),
`conway.goal` is entirely operator-driven and inert until the operator
themselves types `/goal <text>`. A fresh install that never uses the
command pays nothing: no extra segment, no status-line entry, no tool, no
tool-schema cost. That is what makes it safe to install unprompted, the
same way `conway.checkpoint` is — see `docs/getting-started.md`'s
"Installing a first-party plugin" section for the exact set guided
first-run setup turns on.

## The context segment

A `ContextHook` renders the current goal as a single segment near the END
of the assembled request, reading `Standing goal: <text>`, only while a
goal is actually set — volatile, operator-changeable content belongs late,
not mixed into the stable, cacheable part of a request. This is a strict
APPEND: every other segment in the payload is returned byte-for-byte
unchanged, in the same order, so this segment never disturbs whatever a
backend might cache about the rest of the request.

**Cost stays flat, no matter how many times the goal changes.** Exactly one
`conway.goal` segment is ever in any one request, regardless of how many
`/goal` calls preceded it — see the next section for why.

## How the goal survives a resume, and how clearing actually clears it

The built-in `/goal` command persists its write directly: `/goal <text>`
appends a `LogRecord::SystemNote` onto the agent's own log (through the
new, narrowest-possible `SessionHandle::append_system_note` facade call —
there is no model turn involved, and no tool call either). `/goal clear`
appends ANOTHER one, under the identical reason, but with an EMPTY text —
a clear marker, not merely the absence of a note.

Context assembly already turns every past system-note record back into an
ordinary history segment on every subsequent request, including the first
one a freshly started process builds after `--resume` (a resumed session's
context is rebuilt from the full on-disk log, not from anything held in
memory by the process that wrote it). So when this plugin's own in-memory
cache has nothing for an agent — exactly the shape a resume leaves behind —
its context hook finds the MOST RECENT matching note in that replayed
history and decodes it: a non-empty text is the current goal, an empty one
means cleared. Resolving it this way — find the latest note first, then
interpret empty-vs-not — is what keeps a clear marker from being skipped
over in search of an earlier goal: once a clear marker is the most recent
note, it is the right answer for every later request, until a fresh `/goal
<text>` writes a new one.

**The persisted notes themselves never reach a request.** Left alone,
context assembly's own unconditional replay would mean a session with N
`/goal` changes carries N stale copies into every later request. Before
rendering its own segment (if any), the context hook removes every one of
its own plugin's persisted notes from the payload it was handed, on EVERY
request, including the very first one built right after a note was
written. One request therefore ever carries at most one `conway.goal`
segment, never more, regardless of session length.

This is session-log persistence only — a goal does not follow a model into
a different, unrelated session. A forked child inherits whatever goal was
set on its parent (it is already part of the log a fork inherits); a
spawned child starts with none, since a spawn inherits none of the
parent's log either.

## The status line

The status line's `goal` field shows `goal: <first words>` — truncated
sanely on a character boundary (never mid-codepoint), with a literal
newline/tab collapsed to a single space and every other control character
laundered before display — for the FOCUSED agent, always. This plugin's own
`Plugin::status_contributions` has no notion of "which agent is focused" at
all (the trait takes no argument), so on its own it can only answer for
whichever agent's context was most recently assembled — in a multi-agent
TUI session, a background subagent's own turn, not necessarily the one on
screen. The TUI itself closes that gap: it tracks the focused agent's own
goal directly (re-read from that agent's own transcript on every focus
switch, and updated immediately by `/goal` itself with no round trip) and
renders THAT, overriding whatever this plugin's own contribution says under
the `goal` key. Outside the TUI (a single agent, or any other host), this
plugin's own contribution is the only answer there is, and is correct by
construction — the "most recently active agent" ambiguity only exists once
more than one agent's context can be built in the same process.

A `/goal <text>` longer than 300 characters is refused outright, naming the
limit, rather than silently truncated — the cap exists so a long sentence
cannot grow into an ever-larger system-note record re-sent, unbounded, on
every later request. `truncate_for_display`'s own 60-character bound is a
SEPARATE, purely cosmetic limit for the status line's "first words" only —
the context segment itself (`Standing goal: <text>`) always carries the
goal in full, up to the 300-character persistence cap, never the
60-character display one.

## Uninstalling it

```json
{ "plugins": { "install": [] } }
```

With `conway.goal` uninstalled, the built-in `/goal` command still exists,
but says so instead of silently persisting a note nothing will ever read
back into context: no segment is ever added, and the status line never
shows a `goal` entry.

## What this plugin does NOT do

- No auto-continuation — nothing about a standing goal makes a session keep
  going on its own.
- No token budget tied to the goal — it is a reminder, not an enforcement
  mechanism.
- No cross-session persistence beyond the session's own log.
