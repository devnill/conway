# Scripts: the any-language hook convention

**This page documents a designed convention, mostly still not a built one --
`pre_tool_use` is the one exception.** Board item
`01KZDC0RDRMMMJHX7SAFMM2Q5A`'s own config-schema child,
`01KZRZW5CWMVQ0GPRT4GX4RV5G`, shipped the `[hooks]` block's typed, validated
shape (`conway::config::schema::HooksConfig`/`HookEntry`) — see the
correction note right after the JSON example below for exactly how it
differs from this page's original illustrative sketch. `01KZRZY1MNM872BZ6AKEBG3SKE`
(the script runner, `conway_core::ports::HookRunner`/
`conway_tools::hook_runner::ProcessHookRunner`) and `01KZS00JP5QNBJSSHNFP9C47GM`
(`pre_tool_use` enforcement) are BOTH now built: a `[hooks].rules[]` entry
with `event: "pre_tool_use"` and `enabled: true` really does spawn `command`
and can really deny a tool call, PROVIDED a binary or embedder also calls
`ConwayBuilder::with_hook_runner` (not automatic — see `hooks.md` point 13's
"Status" row for that exact precondition). Four more events now dispatch
too — `post_tool_use`, `session_starting`, `child_spawned` and
`prompt_submitted` — so this page no longer states a per-event boundary of
its own. `hooks.md` point 13 is the normative status row —
read it for the precise per-event boundary; this page is the how-it-works
tutorial built on top of that boundary, not a second source of truth for it.

Two worked examples below (one shell, one Python) **are real and were run**
— not against conway, which can't invoke them yet, but standalone, proving
the shape itself is sound and small enough to trust. See each example's own
note for exactly what was verified.

## The contract

**Declaration** — the shape board item `01KZDC0RDRMMMJHX7SAFMM2Q5A` itself
gave as the ORIGINAL worked example (an illustrative sketch, not the shipped
schema — see the correction immediately below), a JSON block naming an
event and a command:

```json
{ "hooks": {
    "pre_tool_use": [
      { "match": "bash", "run": "~/.conway/hooks/audit-command.sh" }
    ]
} }
```

**Correction: this is NOT the shape `01KZRZW5CWMVQ0GPRT4GX4RV5G` shipped.**
That item settled the config-parsing half of this page down to the field,
and it diverges from the sketch above in three ways, each decided for a
stated reason (see `conway::config::schema::HookEntry`'s own doc comment):
rules are a flat, top-level `rules` list rather than nested per event name
(`{"hooks":{"rules":[...]}}`, not `{"hooks":{"<event>":[...]}}`) — because
every rule now carries its own required, unique `id`, which an
operator-visibility item still to come needs to list and revoke rules
individually, and a rule's event lives on the rule itself (`event`) rather
than as its containing map key; and `run` (a single shell string) is
`command` (an argv vector: program, then arguments) — no shell string, so no
shell-quoting ambiguity exists in the config file itself. The worked shape
today is:

```json
{ "hooks": {
    "rules": [
      {
        "id": "audit-bash",
        "event": "pre_tool_use",
        "command": ["~/.conway/hooks/audit-command.sh"],
        "timeout_ms": 5000,
        "enabled": true
      }
    ]
} }
```

This parses, validates (deny-unknown-fields per rule, non-empty/unique
`id`), and round-trips today. **It still does nothing when loaded** — no
dispatcher reads `rules` yet; see the runner/enforcement board items named
above. The stdin/stdout contract described below (what the command receives
and how it answers) is still design only, for either shape.

**What's decided versus what's still open, stated plainly rather than
guessed at:**

- **Decided:** the command receives structured input on stdin and answers
  through its exit status plus what it writes back (`hooks.md` point 13's
  "Receives / May return" row). A hook that can deny a call is
  security-bearing: it **fails closed** on error,
  timeout, or an unreadable response — never treated as an allow — and every
  active rule is individually visible and revocable (the umbrella item's own
  "Security properties" section).
- **Not yet decided down to the field:** the exact per-event JSON schema on
  stdin. The umbrella item's own acceptance criteria list "each core event
  dispatches, with a documented stdin payload" as work still to do, and the
  modify-vs-observe question (may a hook edit a request, or only allow/deny/
  observe?) is explicitly an open question that item must answer and record
  *before* building. Don't trust a stdin/stdout schema more specific than
  what's written here from any source dated before that decision lands.

**Learn from the documented failure elsewhere, because it's the one thing
that must not slip through regardless of the exact schema.** Some hook
systems use an exit-code scheme where a *crashing* script (exit 1, the
conventional Unix failure) is indistinguishable from "no opinion" — the
script dies, and the system treats that identically to the script
deliberately abstaining. That is a landmine when the intent was to guard
something: a typo in your script becomes a silent bypass. **conway's
fail-closed contract is the opposite by construction**: a script that errors,
times out, or produces unreadable output is a denial, not an abstention — the
same standard conway's own one-shot exit-code contract was held to when it
was audited and repaired 2026-08-01 (unreachable codes were deleted or
wired; see `docs/scripting.md`'s "Exit codes" section for that contract, a
different one from this — conway's CLI exit codes, not a plugin hook's).

## Any language

Two small scripts implementing the same rule — deny a `bash` call whose
command contains an unbounded `rm -rf /`, allow everything else — reading a
JSON payload on stdin and writing a JSON verdict to stdout. Both were run
directly against sample payloads (not through conway, which has no
dispatcher yet) to confirm the shape works as described; see the note after
each.

### Shell

```bash
#!/usr/bin/env bash
# pre_tool_use hook: deny any bash call whose command contains "rm -rf /".
set -euo pipefail

payload="$(cat)"
command="$(printf '%s' "$payload" | grep -o '"command":"[^"]*"' | cut -d'"' -f4)"

if [[ "$command" == *"rm -rf /"* ]]; then
  printf '{"decision":"deny","reason":"refusing an unbounded rm -rf"}\n'
else
  printf '{"decision":"allow"}\n'
fi
```

Run directly against two sample payloads:

```console
$ echo '{"event":"pre_tool_use","tool":"bash","arguments":{"command":"rm -rf / --no-preserve-root"}}' | ./audit-command.sh
{"decision":"deny","reason":"refusing an unbounded rm -rf"}
$ echo '{"event":"pre_tool_use","tool":"bash","arguments":{"command":"ls -la"}}' | ./audit-command.sh
{"decision":"allow"}
```

Both lines above are pasted from an actual run of this exact script during
this item's own verification pass, not transcribed by hand.

### Python

```python
#!/usr/bin/env python3
"""pre_tool_use hook: deny any bash call whose command contains "rm -rf /".

A parse failure or an unhandled exception exits non-zero -- which the
fail-closed convention above reads as "no verdict produced", never as
consent -- rather than being caught and silently downgraded to an allow.
"""
import json
import sys

payload = json.load(sys.stdin)
command = payload.get("arguments", {}).get("command", "")

if "rm -rf /" in command:
    verdict = {"decision": "deny", "reason": "refusing an unbounded rm -rf"}
else:
    verdict = {"decision": "allow"}

json.dump(verdict, sys.stdout)
sys.stdout.write("\n")
```

Run directly, including the crash case the contract above depends on:

```console
$ echo '{"event":"pre_tool_use","tool":"bash","arguments":{"command":"rm -rf / --no-preserve-root"}}' | python3 audit_command.py
{"decision": "deny", "reason": "refusing an unbounded rm -rf"}
$ echo '{"event":"pre_tool_use","tool":"bash","arguments":{"command":"ls -la"}}' | python3 audit_command.py
{"decision": "allow"}
$ echo '{broken json' | python3 audit_command.py; echo "exit: $?"
Traceback (most recent call last):
  ...
json.decoder.JSONDecodeError: Invalid control character at: line 1 column 14 (char 13)
exit: 1
```

The third run is the point: malformed input **crashes the script** (exit 1)
rather than the script catching the error and answering `allow`. That crash
is exactly the signal the fail-closed contract above is built to treat as a
denial, not as consent — confirmed here by actually breaking the script and
watching it die loudly, the same break-the-guard discipline this project
holds security-bearing mechanisms to generally.

Any language that can read stdin, write stdout, and set a process exit code
qualifies — these two are chosen because they're the two most likely
languages an operator already has installed, not because the mechanism
favors either.

## The cost, stated honestly

"Any language" reads as free and isn't. Spawning a process per invocation
costs roughly **10–50 ms for a shell script** and **200–400 ms for a
Python one**, and that cost compounds across a batch of tool calls running
in parallel — a session issuing five concurrent tool calls with a
Python-backed `pre_tool_use` hook pays that cost five times over, on the hot
path of every one of them.

**The rule:** fine for a hook that fires occasionally — a formatter after an
edit, a notification on session start. Wrong for a hook wired to every tool
call in a busy session. For that hot-path case, write the hook in Rust
against `conway::plugin` instead (`authoring.md`) — the in-process path pays
no per-invocation process-spawn cost at all.

## Why this is still one mechanism

A script-backed hook is not a second extension API sitting beside the plugin
core — it's **an ordinary plugin whose own implementation happens to
dispatch to a configured script per event**. Lower-barrier extension
surfaces may be layered on top of the plugin core over time; they are
additions over the stable interface, never replacements for it. The script
runner registers against `tool/1`/`context.hook/1`/`permission.policy/1`
like any other out-of-process plugin would; what's unusual about it is only
that its own `invoke`/`before_request`/`check` implementation execs a
subprocess instead of running Rust directly. From the runtime's point of
view there is exactly one extension mechanism, and this is an instance of
it, not an exception to it. `concepts.md`'s "Language choice" section states
the same property in one paragraph if you want the shorter version.

## Where to go next

[`authoring.md`](authoring.md) — the Rust path this layers on top of, and
the one that's actually buildable today. [`inference-hooks.md`](
inference-hooks.md) — a hook whose decision is made by a model; a
script-backed hook can call out to one itself, but that's a choice inside
the script, not a distinct mechanism. [`hooks.md`](hooks.md) point 13 — the
normative status row this page is built on.
