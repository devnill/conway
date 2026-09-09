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
| `permission_decision` | One tool call's permission resolution: which tool, what was decided (allow, allow-always, a matching pattern, deny, deny-with-feedback, auto-allow, plan-mode's own refusal, a denying hook, or a matching `deny` rule), whether it came from you answering a prompt or from a rule/hook/mode resolving it without asking, how long you were shown a prompt for (only when you actually were), and the reason behind any denial. See [Permission decisions](permissions.md#permission-decisions) for the full field list and how to read one back. A SYSTEM record — never model text, never your own typed prose, and never part of what's sent to the model on a later turn. |

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

By default, sessions live centrally, under `~/.conway/sessions/` (or
`$CONWAY_CONFIG_DIR/sessions/` — the same directory `settings.json`/
`history` already live in), keyed by project — the way [Claude
Code](https://claude.com/claude-code) itself does it, rather than inside
the project's own working directory: a project checked out at
`/Users/you/my-project` gets `~/.conway/sessions/-Users-you-my-project/`,
one `<session-id>.jsonl` file per session plus an `index.jsonl` conway
maintains for fast listing. `.conway/sessions` is no longer created in
your project by a fresh, unconfigured run.

**The project key is the absolute path of the enclosing git repository's
root, with `/` (or `\` on Windows) replaced by `-`** — or, when your
invocation directory is not inside any git repository at all, your
invocation directory's own absolute path, encoded the same way. Deliberately
readable, not a hash — running `ls ~/.conway/sessions/` shows your own
project paths, not opaque digests. Two checkouts of the same repository at
different paths get two different keys (each is a genuinely separate
working tree, with its own history); a renamed or moved checkout gets a new
key of its own, and the old one is left where it was, still reachable by
name if you go looking for it. "Enclosing repository" is found by walking
up from your invocation directory looking for a `.git` entry (a directory
for an ordinary clone, or a file for a linked worktree or submodule) — the
same rule the `conway.idiom` plugin's own project-scope `instructions.md`
discovery uses (see [`plugins/idiom.md`](plugins/idiom.md)'s "An operator's
own standing instructions" section), so the two never disagree about where
a project starts. It does not walk past the filesystem root looking for
one, and it does not special-case a bare-repository checkout (where your
invocation directory sits directly inside `HEAD`/`objects`/`refs` rather
than under a nested `.git`) — that case falls back to your exact invocation
directory, same as being outside any repository at all.

**Run conway from a project's root, then again from a subdirectory of that
same project (no `.conway/settings.json` of its own), and both now resolve
to the SAME project key** — the fix for the exact problem this section used
to describe as the accepted default: a repository's root and a
subdirectory of it used to key to two disjoint stores, so `conway sessions
list` run from one location would not show sessions created from the other,
even though both located the identical `settings.json`. Now they share one
history:

```console
$ cd my-project && conway -p "..." && conway sessions list
ID        NAME  CREATED               ROLE   ORIGIN
01KYWYAD        2026-07-31T20:37:14Z  coder

$ cd my-project/src && conway -p "..." && conway sessions list
ID        NAME  CREATED               ROLE   ORIGIN
01KYWYAD        2026-07-31T20:37:14Z  coder
01KYWYK9        2026-07-31T20:42:04Z  coder
```

One shared session store (`~/.conway/sessions/-Users-you-my-project/`),
keyed by the repository root regardless of which of its subdirectories you
invoke conway from. Launching from somewhere with no enclosing git
repository at all still keys off the exact invocation directory, unchanged
from before. If you want a different sessions location entirely — shared
across repositories, or split further than the repository boundary — set
`session.root` explicitly, which opts back into the field's older, direct
meaning: `root` then names the sessions directory itself (resolved against
`--cwd` if you give a relative path), not a root further keyed by project:

```json
// .conway/settings.json
{
  "session": {
    "root": "/Users/you/my-project/.conway/sessions"
  }
}
```

`$CONWAY_CONFIG_DIR`, when set, redirects the central default the same way
it redirects `settings.json`/`history`: sessions land under
`$CONWAY_CONFIG_DIR/sessions/<project-key>/` instead of
`~/.conway/sessions/<project-key>/`.

### If you already have a project-local `.conway/sessions`

An operator upgrading from before this default changed may already have
sessions sitting in a project's own `.conway/sessions/`. conway does not
read, move, or delete that directory automatically — a migration mutates
data you might still need, and a half-completed one would be a worse
failure than doing nothing. It is not touched, and conway does not print a
warning about it either — an unconfigured `[session].root` simply resolves
to the new central default from here on, silently, the same as it does for
a project with no old directory at all. (An earlier version of conway did
print a repeating warning about this on every run; it was removed —
decision `01M0RW05G3Y81AZW96NVKTY1RV` — as ongoing ceremony for a one-time
fact, in a pre-1.0 tree where this paragraph is the correction's permanent
record.) If this is you, resolve it one of two ways:

- **Keep using the old location** — set `[session].root` to it explicitly
  (the example just above), which restores the exact pre-upgrade behavior.
- **Switch over** — move the old directory's contents into the new central
  location yourself; conway does not do this for you.

Either way, nothing is stranded silently: the old sessions are exactly
where they always were, findable at `<project>/.conway/sessions` whenever
you go looking, even though nothing in conway will mention it for you.

### If you already have subdirectory-keyed sessions

Before the project key started following the enclosing git repository
root, a session created from a subdirectory landed in its OWN, separately
keyed store (`~/.conway/sessions/-Users-you-my-project-src/` for the
`my-project/src` example above). Switching the key to the repository root
does not move, merge, or delete that old subdirectory-keyed store either —
the same no-silent-migration stance as the project-local-`.conway/sessions`
case just above, for the identical reason: a project's session history is
not something conway rewrites on your behalf as a side effect of a default
changing. Sessions already sitting under an old subdirectory key stay
exactly there, and stay reachable exactly as before — `conway sessions
list` run from that same subdirectory (or with `[session].root`/
`CONWAY_CONFIG_DIR` pointed at it directly) still finds them, and
`--resume`/`--fork-from`/`sessions show` still work on any of their ids
directly, from anywhere, once you know one. What changes is only where
NEW sessions land: from here on, launching from the repository root or
from any of its subdirectories converges on the one repository-root store,
so history stops splitting further. There is no flag that lists every
project key's sessions in one merged pass — today, as before, each project
key is its own independent catalog.

Your input history is separate from session data and always lives at
`~/.conway/history` (or `$CONWAY_CONFIG_DIR/history`) — see
[`interactive.md`](interactive.md#composing-input).

## Resuming

`--resume <id|name>` (one-shot CLI, or the TUI at startup), `/resume
<id|name>` (TUI, once already running -- see below), or `Conway::resume(id)`
(embedder) reattaches to a persisted session and
continues its transcript: the returned handle's next
`prompt` genuinely continues where the session left off, with the model
seeing its own full prior history. Verified end to end — asking a fresh
one-shot run to echo a string, then resuming that exact session id and
asking "what did I ask you to echo earlier?" gets the right answer back,
proving the persisted transcript round-trips through resume correctly, not
just that the flag is accepted.

**Where "that exact session id" comes from, in real one-shot output** — the
same captured run shown in [`scripting.md`](scripting.md#json)'s
`--output-format json` example:

```json
{
  "agent_id": "01KYWYFT24JSQPR1EYQXHMNQX2",
  "status": { "status": "completed" },
  "summary": "pong",
  "facts": [],
  "artifacts": [],
  "structured": null,
  "transcript_ref": "01KYWYFT24KYFWXSBS95GHCQMN",
  "usage": {
    "input_tokens": 15856,
    "output_tokens": 4454,
    "cache_read_tokens": 0,
    "cache_write_tokens": 0,
    "reasoning_tokens": 0
  },
  "steps_taken": 4
}
```

`--resume` takes `transcript_ref`, not `agent_id` — `conway --resume
<transcript_ref> …`, using the value shown above, reattaches to this
session. The same object's `agent_id` names the agent rather than the
session; passing it to `--resume` instead fails with
`session … not found`.

`agent_def`/`role`/`cwd` are **not** overridable on resume — they come back
exactly as the persisted header recorded them; there's no flag or spec
field to change them on the way back in.

Three related but distinct ways to get a session handle. `--session` stays
**one-shot (`-p`) only** — the TUI refuses to start if you pass it (a usage
error pointing at `--resume`/`--fork-from`/`--continue` instead of a silent
ignore). `--resume` and `--fork-from` are no longer one-shot-only: the TUI
now accepts both at startup too, opening straight onto the named session
with its full history drawn — see
[`interactive.md`](interactive.md#starting-a-session).

| Flag / call | Effect |
| --- | --- |
| `--session <id>` | Use this exact id, creating it if it doesn't already exist. Colliding with an existing id is a usage error pointing you at `--resume` instead — never a silent overwrite. One-shot only. |
| `--resume <id\|name>` | Reattach to a persisted session and continue it, as above. Accepts an operator-chosen name (`conway sessions name`) anywhere it accepts an id. One-shot **and** the TUI (opens the TUI on that session). |
| `--fork-from <id\|name>[@seq]` | Branch a **new** session from an existing one's log, at its current head or an explicit earlier point — no live parent agent involved, and the store copies zero parent records (a fork is always a reference, not a copy). Omit `--cwd` when using this flag: the child always inherits the parent's cwd, and there's no field to override it. One-shot **and** the TUI. |
| `--continue` / `-c` | **TUI only.** No id or name to type: opens the TUI on the most recently WRITTEN-TO session for the current project (ranked by each candidate session's own log file's last-write time, not by when it was first created — an older session you chatted in five minutes ago outranks a brand-new, still-empty one). A usage error naming `conway sessions list` if this project has no sessions at all yet. |

The TUI's `/resume <id|name>` command is genuinely `--resume`'s equivalent
now — same name-or-id argument, same effect (switches the running TUI onto
that session with its full history drawn), just reachable once the TUI is
already running rather than only at startup. An id or name that resolves to
nothing is a clear error naming `conway sessions list`, the same as an
unresolvable `--resume` argument is. There is no `/fork-from` or
`/continue` slash-command equivalent — those two stay startup-only flags.

**Bare `/resume` (no argument) opens a picker** over this project's own
sessions instead of erroring — each row shows the session's name (or, for
an unnamed session, its derived auto-title), its first prompt, when it was
last active, how many records its transcript holds, and any labels. `Up`/
`Down` move the highlighted row, `Enter` resumes it through the exact same
mechanism `/resume <id>` uses, and `Esc` cancels with nothing resumed. A
project with no sessions yet gets a clear notice instead of an empty
picker. The picker is scoped to this project's own session-root key only
— it does not (yet) reach across the "no flag that lists every project
key's sessions in one merged pass" boundary the paragraph above describes;
a session sitting under an old subdirectory key is still reachable, but
only by its id or name, typed directly (`/resume <id|name>`), the same as
before this picker existed.

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
directly. `list`/`show`/`tree`/`export` were each run against a real,
freshly-created session store to confirm their exact behavior, not just
read from source; `name`/`unname` (below) were not — see that row's own
note.

| Subcommand | Effect |
| --- | --- |
| `sessions list [--limit N] [--label L] [--json]` | Lists sessions (id, name, created, role, origin), newest first. `--json` prints a JSON array instead of a table. Excludes ephemeral sessions; there's no flag to include them. The `NAME` column (and `--json`'s `title` field) shows an unnamed session's auto-derived title instead of staying blank — see [Where a title comes from](#where-a-title-comes-from) below. |
| `sessions show <id-or-name> [--json]` | Prints that session's ancestry-resolved transcript — its own records plus, if it's a fork child, everything it inherited. Default output is one `--- <kind> seq=<n> ---` block per record in Rust debug form; `--json` prints one compact JSON object per line (JSONL), the same wire shape the log itself uses. |
| `sessions show <id-or-name> --diff` | Prints the cumulative diff of every path this session's own root agent edited or wrote, one `## <path>` section per path, instead of the ordinary record dump — the headless counterpart of the TUI's `/diff` command (`docs/interactive.md`'s "Diffs, not raw JSON"); both fold the same ordered sequence of successful `edit`/`write` calls through one shared reconstruction. See the note below the table for what "against the bytes it had when the session first touched it" actually means for a session inspected well after the fact, and the current single-agent (root only) scope. |
| `sessions tree <id-or-name>` | Prints the session's fork/spawn tree as indented text: one line per node (role), starting from `<id-or-name>` itself and indenting each descendant under its parent. |
| `sessions export <id-or-name> [--out PATH]` | Writes the ancestry-resolved transcript as JSONL — to `PATH` if given, else stdout. Same content as `show --json`, without the interleaved per-line inspection framing. |
| `sessions name <id-or-name> <name>` | Attaches `<name>` to a session, or — if `<id-or-name>` is itself an existing name — renames it. Refuses a `<name>` that parses as a valid ULID, and refuses one already bound to a *different* session, naming which session holds it — never a silent overwrite. A session carries at most one name; naming an already-named session moves its one name rather than adding a second. |
| `sessions unname <id-or-name>` | Removes whichever name is bound to the resolved session. The session and its transcript are entirely unaffected — see [Where a name lives](#where-a-name-lives) below. |
| `sessions label <id-or-name> <label>` | Attaches `<label>` to a session's own `SessionMeta.labels` — what `sessions list --label`/`conway.discover`'s `label` parameter match against. A session may carry any number of labels; attaching one it already has is a no-op success, not a refusal (unlike `name`'s ULID-shape/collision refusals — a label is not a bijective identifier). |
| `sessions unlabel <id-or-name> <label>` | Removes `<label>` from a session. Removing a label the session does not carry is a no-op success, not an error — see [Where a label lives](#where-a-label-lives) below. |

A few things worth knowing before you rely on the output:

- **Nothing to configure for permissions.** `sessions`/`routes` are
  read-only and never invoke a tool, so `conway` builds them a deny-all
  gate unconditionally, regardless of anything in `settings.json` — a
  no-op for a subcommand that never calls a tool, and one less thing to
  set up before either one works.
- **An unknown session id is a usage error (exit 2), not a crash or an
  `AgentFailed` (exit 1)** — `show`/`tree`/`export`/`name` map "not found"
  and "malformed id" the same way, and `label`/`unlabel` do too (via
  `Conway::add_label`/`remove_label`'s `StoreError::NotFound`).
- **The `ORIGIN` column reads `fork@<seq> <parent>` or `spawn@<seq>
  <parent>`**, matching the persisted `SessionMeta.origin.mode` — a forked
  child inherited its parent's entire context, a spawned one is clean-slate,
  and this column, not the `ROLE` column, is where that
  distinction shows up. `sessions list --json`'s `origin` object carries the
  same distinction as a `"mode": "fork"`/`"mode": "spawn"` field.
- **`--diff`'s "baseline" is reconstructed, not stored.** conway persists
  no full file snapshots — the log records each `edit`/`write` call's own
  arguments (the substring changed, or the new content), not the file's
  bytes before or after. The first time `--diff`'s walk touches a given
  path, it reads that path's CURRENT on-disk content and treats it as the
  baseline, then folds every recorded call for that path on top in memory.
  That is exactly right immediately after a session ends, against files
  nothing else has since touched; it is a best-effort answer, not a
  guarantee, for an old session whose files have diverged further or been
  reverted since. `--diff` only walks the session's own **root** agent
  today — a subagent's own `edit`/`write` calls are not yet included.
- Values passed to `--session`/`--resume`/`--fork-from` and
  `sessions show|tree|export|name|unname|label|unlabel <id-or-name>` accept
  either a full ULID or an operator-chosen name (below) — never a
  shortened/prefix id, even though `list`/`tree`'s own table output
  truncates ids for display.

### Where a name lives

`sessions name`/`unname`, and every flag above that resolves an
`<id-or-name>` value, are the only CLI surface with disk access outside the
`conway` facade: they read and write one small sidecar file,
`session-names.json`, kept beside the session store it names (the same
`root` directory `--session`/`--resume` create/reattach sessions in — see
[Where session data lives on disk](#where-session-data-lives-on-disk)
above), holding nothing but a flat `{name: session-id}` JSON object.

This is deliberate, and it is what makes naming and renaming safe: a
session's identity is its own append-only `<session-id>.jsonl` file, and
naming code never opens it. Attaching, moving, or removing a name is
entirely a read/mutate/write cycle over the separate sidecar — the
session's own persisted record is byte-for-byte unaffected either way, and
a lost or corrupted sidecar loses names, never sessions.

**Not live-verified against a real, freshly-created session store the way
`list`/`show`/`tree`/`export` above were** — `sessions name`/`unname` and
name resolution for `--session`/`--resume`/`--fork-from` are covered by an
integration suite run against the compiled binary
(`crates/conway-cli/tests/session_names.rs`) plus in-crate unit tests for
the sidecar itself (`crates/conway-cli/src/session_names.rs`), but neither
was exercised by hand against a live invocation the way this page's other
worked examples were.

### Where a title comes from

Unlike a name, a title is never stored anywhere — it is computed fresh
every time `sessions list` runs, and it is display-only. For a session
with an operator-chosen name (above), the title IS that name. For a
session with none, `sessions list` reads the session's own first
`user_turn` record (its own, or — for a forked child — the one it
inherited, the identical ancestry-resolved read `show`/`export` already
perform) and derives a title from it: the first LINE only (a multi-line
prompt's second and later lines are dropped, never folded in), trimmed,
and bounded to a fixed length measured in characters, never bytes (a byte-
index cut can land inside a multi-byte character and corrupt or crash on
it — this bound never does). A session with no user turn yet shows no
title, same as it always showed no name.

**Never written to `session-names.json`.** A derived title is a guess
about what a session is about, not an operator's deliberate choice — the
two must stay distinguishable, so `sessions list --json`'s `title` field
sits alongside `name` (which stays `null` for an unnamed session,
unchanged) rather than replacing it. Naming a session for real still goes
through `sessions name`, the only write path into that sidecar, exactly as
[Where a name lives](#where-a-name-lives) above describes. There is no LLM
call anywhere in this path — a title is deterministic and free to compute,
every time.

### Where a label lives

Unlike a name, a label lives INSIDE the session's own header — the same
`SessionMeta.labels` field `sessions list --label`/`conway.discover`'s
`label` parameter already read (`crates/conway-session/src/index.rs`'s
in-memory catalog, backed by line 0 of the session's own
`<session-id>.jsonl`). `sessions label`/`unlabel` therefore do NOT touch a
separate sidecar the way `name`/`unname` do: they go through the ordinary
`conway` facade (`Conway::add_label`/`remove_label`) straight to
`SessionStore::add_label`/`remove_label`, which durably rewrites that one
header line — the same crash-atomic tmp-file-plus-rename discipline the
store already uses for the `/ask` modal's ephemeral→persistent promote
(`SessionStore::set_ephemeral`), extended to a second, narrow kind of
in-place header mutation. Attaching or removing a label never appends,
rewrites, or otherwise touches any record line — only line 0.

Until board item `01M1WVKVSDXHB68J66VZ9HE8B3`, `SessionMeta.labels` could
only be set once, at session-creation time, and only by an embedder calling
`Conway::new_session` with `SessionSpec::labels` directly — no CLI flag
existed to set it, and no way existed to label a session after the fact
either. `sessions list --label`/`conway.discover`'s `label` parameter
existed and worked, but nothing shipped could ever produce a match.
`sessions label`/`unlabel` close that gap.

**Not live-verified by hand against a real invocation the way
`list`/`show`/`tree`/`export` above were** — `sessions label`/`unlabel` are
instead covered by an automated end-to-end integration suite run against
the compiled binary (`crates/conway-cli/tests/session_labels.rs`), which
drives the real subcommands and reads the result back through `sessions
list --label`, plus in-crate unit tests for `SessionStore::add_label`/
`remove_label` themselves (`crates/conway-session/tests/label_tests.rs`).

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

**Forking at a clean point is a lever on what gets SENT going forward, not a
discount on what a fork child itself pays.** A forked child's assembled
context is its parent's entire inherited prefix, unbounded, plus its own
new turns (`crates/conway-runtime/src/context/path.rs`'s fork-origin branch
walks the parent's ancestry with no truncation) — so "fork instead of
continuing to grow one session" trades one session's ever-larger resend for
a fresh session that itself resends that same accumulated prefix, in full,
on every one of its own turns. This is genuinely cheaper only when the
provider actually reuses the shared byte-prefix across requests — on a
provider that reports no cache accounting, or genuinely does none, a fork
child pays for its whole inherited history again on every turn it takes,
exactly like the un-forked session it was meant to relieve. See
[`agents.md`](agents.md#fork-and-spawn-the-two-primitives) for the fork-cost
model this applies to, and
[`providers.md`](providers.md#does-ollama-cloud-actually-cache-prefixes) for
which providers' caching is actually confirmed versus assumed — Ollama
Cloud, as of this writing, is the latter.
