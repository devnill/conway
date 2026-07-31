# Sessions

A session is one agent's durable history: every turn, tool call, and result
it has ever produced, recorded as it happens. This page covers what's
persisted, how resuming and forking-from-disk work, keep-alive sessions,
`/ask`'s ephemeral children, where the data lives, and the full
`conway sessions` command reference.

## The append-only log

Each session is a single JSONL file, one record per line, written in the
order things happened. A record is never edited or deleted once appended —
new information becomes a new record, not a rewrite of an old one. The
kinds you'll see:

| Kind | Carries |
| --- | --- |
| `header` | The session's metadata: id, owning agent, origin (if forked), agent def, role, cwd, status, labels. Always the first line. |
| `user_turn` | A prompt delivered to the agent — from you, or from a parent's steer/fork directive folded in as a turn. |
| `assistant` | One model turn: content blocks (text, thinking, tool calls), the model that served it, the routing reason, token usage, stop reason. |
| `tool_call` | One tool invocation the model requested. |
| `tool_result` | That tool call's result. |
| `fork_directive` | The instruction a forking parent attached on top of the inherited prefix — a forked child's first record. |
| `parent_steer` | A steer message drained from the mailbox at a turn boundary. |
| `system_note` | A runtime-authored note (e.g. repeated-step detection) — never something you or the model wrote. |
| `agent_result` | The agent's terminal result: status, summary, facts, artifacts. |
| `context_report` | What was actually sent to the model that turn: every segment, its provenance, and its estimated token count. |
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
ID        CREATED               ROLE   STATUS  ORIGIN
01KYWYAD  2026-07-31T20:37:14Z  coder  active

$ cd my-project/src && conway -p "..." && conway sessions list
ID        CREATED               ROLE   STATUS  ORIGIN
01KYWYK9  2026-07-31T20:42:04Z  coder  active
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

`--resume <id>` (CLI/TUI) or `Conway::resume(id)` (embedder) reattaches to a
persisted session and continues its transcript: the returned handle's next
`prompt` genuinely continues where the session left off, with the model
seeing its own full prior history. Verified end to end — asking a fresh
one-shot run to echo a string, then resuming that exact session id and
asking "what did I ask you to echo earlier?" gets the right answer back,
proving the persisted transcript round-trips through resume correctly, not
just that the flag is accepted.

`agent_def`/`role`/`cwd` are **not** overridable on resume — they come back
exactly as the persisted header recorded them; there's no flag or spec
field to change them on the way back in.

Three related but distinct ways to get a session handle:

| Flag / call | Effect |
| --- | --- |
| `--session <id>` | Use this exact id, creating it if it doesn't already exist. Colliding with an existing id is a usage error pointing you at `--resume` instead — never a silent overwrite. |
| `--resume <id>` | Reattach to a persisted session and continue it, as above. |
| `--fork-from <id>[@seq]` | Branch a **new** session from an existing one's log, at its current head or an explicit earlier point — no live parent agent involved, and the store copies zero parent records (a fork is always a reference, not a copy). Omit `--cwd` when using this flag: the child always inherits the parent's cwd, and there's no field to override it. |

The TUI's `/resume <session-id>` command does the same thing as `--resume`,
without restarting the process — see [`interactive.md`](interactive.md).

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
else, including every model-invoked `conway_subagent`/`conway_ask` call,
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
| `sessions list [--limit N] [--label L] [--json]` | Lists sessions (id, created, role, status, origin), newest first. `--json` prints a JSON array instead of a table. Excludes ephemeral sessions; there's no flag to include them. |
| `sessions show <id> [--json]` | Prints that session's ancestry-resolved transcript — its own records plus, if it's a fork child, everything it inherited. Default output is one `--- <kind> seq=<n> ---` block per record in Rust debug form; `--json` prints one compact JSON object per line (JSONL), the same wire shape the log itself uses. |
| `sessions tree <id>` | Prints the session's fork/spawn tree as indented text: one line per node (role, status), starting from `<id>` itself and indenting each descendant under its parent. |
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
- **The `ORIGIN` column always reads `fork@<seq> <parent>` for any child
  with a parent, including one created by `spawn`.** The persisted header
  actually does distinguish fork from spawn (`SessionMeta.origin.mode`),
  but `sessions list`/`sessions tree`'s own formatting doesn't consult it —
  confirmed by spawning a child via `conway_subagent` and observing its
  `ORIGIN` cell print `fork@N`, indistinguishable from a real fork. Don't
  read that column as proof a child was forked rather than spawned.
- **`status` reads `active` for essentially every session you'll ever
  list**, including ones whose agent has long since finished. Every header
  is written with `status: Active` at creation, and nothing in this build
  transitions it to `Completed`/`Failed`/`Cancelled` afterward — the field
  exists and round-trips, but nothing currently sets it past its initial
  value. Use the session's own `agent_result` record (via `sessions show`)
  to find out how a specific run actually ended.
- Values passed to `--session`/`--resume`/`--fork-from` and
  `sessions show|tree|export <id>` must be full ULIDs — the CLI does not
  accept a shortened/prefix id anywhere, even though `list`/`tree`'s own
  table output truncates ids for display.

## conway does not compact context

Every turn, conway re-sends the model the full assembled transcript — it
does not summarize, truncate, or otherwise compact your session's history
on your behalf, ever, as a built-in behavior. This is deliberate: what's
safe to forget is a judgment call, and it's yours to make, not a policy the
harness applies silently on your session because the harness guessed the
window was getting full. See [`.design/whitepaper.md`](../.design/whitepaper.md)
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
