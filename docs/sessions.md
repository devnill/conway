# Sessions

A session is one agent's durable history: every turn, tool call, and result
it has ever produced, recorded as it happens. This page covers what's
persisted, the notes that can be written into a transcript without you or
the model asking, how resuming and forking-from-disk work, keep-alive
sessions, `/ask`'s ephemeral children, where the data lives, and the full
`conway sessions` command reference.

## The append-only log

Each session is a single JSONL file, one record per line, written in the
order things happened. A record is never edited or deleted once appended —
new information becomes a new record, not a rewrite of an old one. The
kinds you'll see:

| Kind | Carries |
| --- | --- |
| `header` | The session's metadata: id, owning agent, origin (if forked), agent def, role, cwd, labels. Always the first line. |
| `user_turn` | A prompt delivered to the agent — from you, or from a parent's steer/fork directive folded in as a turn. |
| `assistant` | One model turn: content blocks (text, thinking, tool calls the model requested), the model that served it, the routing reason, token usage, stop reason. |
| `tool_result` | A tool call's result. |
| `fork_directive` | The instruction a forking parent attached on top of the inherited prefix — a forked child's first record. |
| `parent_steer` | A steer message drained from the mailbox at a turn boundary. |
| `system_note` | A note written by the harness or by an installed plugin (e.g. a [result-contract](agents.md#result-contracts) violation, or a [repeated-step notice](#repeated-step-notices) if you installed `conway.stepguard`) — never something you or the model wrote. |
| `agent_result` | The agent's own terminal result: status, summary, facts, artifacts. |
| `child_result` | A CHILD's terminal result, recorded into the PARENT's own log at the next turn boundary after the child's `AgentMessage::Result` drains from the parent's mailbox — how a fan-out caller (`await: false`) learns a child finished without ever calling `conway_await` on it. See [`agents.md`](agents.md#a-model-tool-call). |
| `context_report` | What was actually sent to the model that turn: every segment, its provenance, its estimated token count, and any [tool calls dropped](#dropped-tool-calls) to make the request sendable. |
| `context_mask` | Marks an earlier record (by its seq) excluded from — or re-included in — a *future fork's inherited prefix*, without touching that record. It has no effect on the owning session's own later turns; nothing in conway today writes one. |

The one qualification to "never rewritten": a session's header line has
exactly one sanctioned later mutation, the one-way promotion of an
ephemeral `/ask` child into a permanent session (below) — an atomic
write-temp-then-rename, never an in-place edit. And on first access after a
crash, a session whose final line was left incomplete mid-write has that
one trailing line dropped (never a complete line touched) so the file reads
cleanly again; every record it recovers is exactly the bytes that were
durably written. Neither is a "the record can be revised" exception — the
log recovers to what was durably written and gains a single, explicitly
one-way lifecycle flip; it does not otherwise get edited.

## Repeated-step notices

**Nothing detects repeated tool calls unless you install something that
does.** conway itself holds no opinion about an agent that keeps making the
same call — deciding when a loop is a loop is a judgment about your workload,
and the harness would be guessing. Install the first-party `conway.stepguard`
plugin if you want the judgment conway's authors would make:

```json
{ "plugins": { "install": ["conway.stepguard"] } }
```

With it installed, it tracks per agent every tool call's name and arguments.
If the exact same call — same tool, same arguments — comes back a 3rd time, it
appends one `system_note` to the transcript and moves on; it does not refuse
or alter the call itself.

"Same call" means the tool name and the canonicalized JSON arguments hash
identically: object keys are sorted recursively before comparing, so
`{"a": 1, "b": 2}` and `{"b": 2, "a": 1}` count as the same call, but any
differing value (including a `null` versus an absent key) makes it a
different one.

The note fires once per repeated call, on its 3rd occurrence — not the
4th, 5th, or any later repeat of the same call, and not the 1st or 2nd. It
names the tool and the `seq` of that call's *first* result, so the model (or
you, reading the transcript) can go look at the existing answer instead of
running the call again. It also fires a `conway.stepguard.repeated_step`
event, which a [hook](plugins/hooks.md) can subscribe to.

This is **advisory only**: nothing about it blocks, retries, or rejects
the call — the 3rd (and every later) identical call still runs and
returns a result exactly as if the note weren't there. It applies to every
agent, including the interactive root agent you're talking to directly,
not just forked or spawned children. Sibling agents are tracked separately,
so a fan-out where ten children each make the same call once is not
repetition. The 3-call threshold and the window size (the most recent 64
distinct calls per agent, oldest evicted first) are the plugin's policy; fork
the crate if you want different ones.

If you see this note, something is stuck in a loop: point it at the cited
`seq` instead of repeating the call, or steer/cancel the agent if it
doesn't stop on its own.

Note that with the plugin installed this becomes the second mechanism
(besides [result contracts](agents.md#result-contracts)) that writes into
your transcript without the model or you having asked — which is exactly why
it is something you turn on rather than something you inherit.

## Where session data lives on disk

By default, sessions live under `.conway/sessions/` (`[session.root]` in
`settings.json`), one `<session-id>.jsonl` file per session plus an
`index.jsonl` conway maintains for fast listing.

**This path is resolved against your invocation directory, not against
wherever `settings.json` was discovered.** `settings.json` itself is found
by walking up from your current directory (`getting-started.md`'s
discovery rule), but `session.root`'s default, `.conway/sessions`, is a
*relative* path resolved against `--cwd` (or the process's own working
directory when `--cwd` is unset) every time conway starts — not against the
directory that held the config file that named it. Concretely: run conway
from a project's root, then again from a subdirectory of that same project
(no `.conway/` of its own), and both runs discover the identical
`settings.json` — but each writes its sessions into its own,
separately-created `<dir>/.conway/sessions/`, and `conway sessions list`
run from one location will not show sessions created from the other:

```console
$ cd my-project && conway -p "..." && conway sessions list
ID        CREATED               ROLE   ORIGIN
01KYWYAD  2026-07-31T20:37:14Z  coder

$ cd my-project/src && conway -p "..." && conway sessions list
ID        CREATED               ROLE   ORIGIN
01KYWYK9  2026-07-31T20:42:04Z  coder
```

Two different directories, two disjoint session stores, both reading the
same config. If you want one shared history regardless of where you invoke
conway from within a project, set `session.root` to an absolute path:

```json
// .conway/settings.json
{
  "session": {
    "root": "/Users/you/my-project/.conway/sessions"
  }
}
```

Your input history is separate from session data and always lives at
`~/.conway/history` (or `$XDG_CONFIG_HOME/conway/history`) — see
[`interactive.md`](interactive.md#composing-input).

## Resuming

`--resume <id>` (one-shot CLI), `/resume <id>` (TUI, see below), or
`Conway::resume(id)` (embedder) reattaches to a persisted session and
continues its transcript: the returned handle's next
`prompt` genuinely continues where the session left off, with the model
seeing its own full prior history. Verified end to end — asking a fresh
one-shot run to echo a string, then resuming that exact session id and
asking "what did I ask you to echo earlier?" gets the right answer back,
proving the persisted transcript round-trips through resume correctly, not
just that the flag is accepted.

`agent_def`/`role`/`cwd` are **not** overridable on resume — they come back
exactly as the persisted header recorded them; there's no flag or spec
field to change them on the way back in.

Three related but distinct ways to get a session handle -- **all one-shot
(`-p`) only.** The TUI refuses to start if you pass any of these three
flags (a usage error naming the alternative), rather than silently ignoring
them — see [`interactive.md`](interactive.md#starting-a-session).

| Flag / call | Effect |
| --- | --- |
| `--session <id>` | Use this exact id, creating it if it doesn't already exist. Colliding with an existing id is a usage error pointing you at `--resume` instead — never a silent overwrite. |
| `--resume <id>` | Reattach to a persisted session and continue it, as above. |
| `--fork-from <id>[@seq]` | Branch a **new** session from an existing one's log, at its current head or an explicit earlier point — no live parent agent involved, and the store copies zero parent records (a fork is always a reference, not a copy). Omit `--cwd` when using this flag: the child always inherits the parent's cwd, and there's no field to override it. |

The TUI's `/resume <session-id>` command does the same thing as `--resume`,
once the TUI is already running rather than at startup — see
[`interactive.md`](interactive.md). It has no equivalent for `--session` or
`--fork-from`.

## Keep-alive sessions

By default, a session's root agent task ends after its first completed
turn — a second `prompt` call on the same handle silently runs no turn at
all. `keep_alive: true` (`SessionSpec`/`ForkSpec`/`SpawnSpec`) opts out of
that: the agent idles after finishing a turn instead of terminating, ready
for your next `prompt`/`prompt_agent` call.

This is not a rare corner case — it's what makes an interactive session
possible at all. The TUI's own root session is created `keep_alive: true`
for exactly this reason (confirmed by reading `conway-cli`'s own session
setup: without it, your second chat message would run no turn, since the
first message's turn had already ended the agent's task). A bare `/fork` or
`/spawn` in the TUI — no explicit target, opening a fresh focused child —
is keep-alive for the same reason: you're about to have a conversation with
it, not fire one directive and walk away.

Use it whenever a caller — the TUI, or an embedder building a chat-style UI
over a forked or spawned child — needs to send more than one message to the
same agent over its lifetime. Leave it `false` (the default everywhere
else, including every model-invoked `conway_fork`/`conway_spawn`/`conway_ask` call,
which is always autonomous) for a child that does one job and reports back.

## `/ask` and ephemeral children

`/ask <text>` (TUI) and the `conway_ask` tool both fork the calling agent
at its current head, run one prompt against the child, and return the
answer — without touching the caller's own transcript. The child is created
with `SessionMeta.ephemeral: true`, which keeps it out of both
`conway sessions list`'s default output and `sessions tree` run against its
*parent's* id — verified directly: asking a live agent to run `conway_ask`
and then listing/tree-ing its parent shows neither the child nor any hint
one exists. The one CLI path that still reaches it is naming its id
directly — `sessions show <child-id>`/`sessions export <child-id>` read it
like any other session, since both resolve an explicitly-named id rather
than browsing a filtered catalog. `conway_ask`'s own tool output carries
that id as an `EphemeralSessionRef` artifact, so the calling agent (and
anything reading its persisted `tool_result`) always has it on hand. The
TUI's `/agents` panel is the exception in the other direction: it's driven
by the live runtime tree, not this filtered catalog, so an in-flight or
just-finished `/ask` child DOES show there, tagged `(ephemeral)` — see
[`interactive.md`](interactive.md). There's no CLI flag to list ephemeral
sessions in bulk today — `sessions list` only takes `--limit`, `--label`,
`--json`.

What happens to it next depends on the path:

| Origin | Fate |
| --- | --- |
| TUI modal `/ask` | You choose: **fork** it into a real, permanent session (one-way promote — it can never be discarded after), **pull in** its Q&A as a normal turn in your own transcript (merges the exchange into your log, then discards the child), or **discard** it outright. Quitting the TUI with the modal still open forces the discard fate. |
| `conway_ask` tool call | Never offered a fate at all, and never swept — the calling agent's own persisted tool output references the child's transcript by id (an `EphemeralSessionRef` artifact), so the child has to keep existing for that provenance link to resolve. It's tagged distinctly from a modal `/ask` internally for exactly this reason: a crash-recovery sweep that reaps abandoned modal-ask residue at TUI startup must never touch one of these. |

## `conway sessions` reference

`conway sessions <subcommand>` reads persisted sessions through the same
facade an embedder uses (`Conway::sessions`/`::resume`,
`SessionHandle::transcript`) — nothing here reads a session file off disk
directly. Every subcommand below was run against a real, freshly-created
session store to confirm its exact behavior, not just read from source.

| Subcommand | Effect |
| --- | --- |
| `sessions list [--limit N] [--label L] [--json]` | Lists sessions (id, created, role, origin), newest first. `--json` prints a JSON array instead of a table. Excludes ephemeral sessions; there's no flag to include them. |
| `sessions show <id> [--json]` | Prints that session's ancestry-resolved transcript — its own records plus, if it's a fork child, everything it inherited. Default output is one `--- <kind> seq=<n> ---` block per record in Rust debug form; `--json` prints one compact JSON object per line (JSONL), the same wire shape the log itself uses. |
| `sessions tree <id>` | Prints the session's fork/spawn tree as indented text: one line per node (role), starting from `<id>` itself and indenting each descendant under its parent. |
| `sessions export <id> [--out PATH]` | Writes the ancestry-resolved transcript as JSONL — to `PATH` if given, else stdout. Same content as `show --json`, without the interleaved per-line inspection framing. |

A few things worth knowing before you rely on the output:

- **`sessions <subcommand>` needs `permissions.mode` in your
  `settings.json` to be something other than `"prompt"`.** These
  subcommands are read-only and never invoke a tool, but conway still
  builds a full permission gate for every invocation, and `"prompt"` mode
  requires an interactive handler these subcommands don't supply. Set
  `permissions.mode` to `"deny"` or `"allowlist"` (with an empty allow
  list, which denies everything) if you're only ever going to run
  `sessions`/`routes` against that config — either is a no-op for a
  subcommand that never calls a tool.
- **An unknown session id is a usage error (exit 2), not a crash or an
  `AgentFailed` (exit 1)** — every one of the four subcommands maps "not
  found" and "malformed id" the same way.
- **The `ORIGIN` column reads `fork@<seq> <parent>` or `spawn@<seq>
  <parent>`**, matching the persisted `SessionMeta.origin.mode` — a forked
  child inherited its parent's entire context, a spawned one is clean-slate,
  and this column, not the `ROLE` column, is where that
  distinction shows up. `sessions list --json`'s `origin` object carries the
  same distinction as a `"mode": "fork"`/`"mode": "spawn"` field.
- Values passed to `--session`/`--resume`/`--fork-from` and
  `sessions show|tree|export <id>` must be full ULIDs — the CLI does not
  accept a shortened/prefix id anywhere, even though `list`/`tree`'s own
  table output truncates ids for display.

## Dropped tool calls

There is exactly one thing conway removes from a request without being asked,
and it exists because the alternative is a request no provider will accept.

A tool call must be accompanied by its result. If a transcript contains a call
with no answering result anywhere, every provider rejects the whole request
rather than tolerating it. Two ordinary situations produce one:

- **A fork taken mid-batch.** `conway_fork` runs as a tool call *inside* a
  batch, so the child's inherited prefix can end on calls whose results did not
  exist yet when the snapshot was taken.
- **A session that stopped mid-batch.** Killed between an assistant turn and
  its tool results, its own log ends the same way — which would otherwise make
  it unresumable.

conway drops the unanswered calls so the turn can proceed, and **records every
one it dropped** in that turn's `context_report`, under `dropped`. `/context`
in the TUI prints them; so does reading the record with `conway sessions show`.

The loss is real and is why it is recorded rather than merely accepted: the
model no longer sees that it made those calls and may re-issue them. A turn
that repeats work you thought was already done is explicable from the log
instead of mysterious. Synthesizing fake results was the alternative and was
rejected — it would put content in the transcript that no agent ever produced.

## conway does not compact context

Every turn, conway re-sends the model the full assembled transcript — it
does not summarize, truncate, or otherwise compact your session's history
on your behalf, ever, as a built-in behavior. This is deliberate: what's
safe to forget is a judgment call, and it's yours to make, not a policy the
harness applies silently on your session because the harness guessed the
window was getting full. See [`whitepaper.md`](whitepaper.md)
§3 and §4.5 for the reasoning.

The consequence is direct: a long-running session's context keeps growing,
turn over turn, and so does what every turn costs. conway doesn't apologize
for this or try to talk you out of noticing it — plan for it. Your actual
levers, all covered above: fork at a clean point instead of continuing to
pile onto one session indefinitely; spawn a child with no inherited history
for a task that doesn't need the accumulated context; and, if you're
embedding conway, the `ContextHook` extension point lets a host drop
individual segments before each request is sent, programmatically — but
that exclusion applies fresh to that one request, is not itself persisted
as a `context_mask` record, and is a Rust extension point for a host
application to program, not a slash command or CLI flag available in this
build today.
