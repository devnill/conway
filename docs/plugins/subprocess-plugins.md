# Subprocess plugins: adding a tool to a binary you already built

The protocol reference for the out-of-process plugin host (board item
`01KZY8PATND84AKY0J376E3DWV`): the first slice of "plugins all the way
down" that reaches past the binary's own compile step. Depends on
[`concepts.md`](concepts.md) for vocabulary (plugin, tool, trust subject)
and on [`hooks.md`](hooks.md) points 1 and 2 for the in-process contract
this wire form projects.

**Read this before [`trust-and-security.md`](trust-and-security.md) if you
are about to name a command in `[plugins].subprocess`.** A subprocess
plugin is code your own machine executes with your own privileges. This
page states what conway checks before running it (a short list) right where
the mechanism is described, not only on that page.

## What this is, in one sentence

`conway` (a binary already built, with no knowledge this plugin would ever
exist) spawns a command named in your `settings.json`, asks it once what
tools it provides, and calls it again — a fresh process each time — every
time the model invokes one of those tools.

## What this is not

This is **not** the full remote-plugin design `concepts.md` and `hooks.md`
describe elsewhere in this set. Three things are deliberately narrower here,
each disclosed rather than silently assumed:

- **Transport.** The design's own eventual shape is a long-lived,
  persistent NDJSON JSON-RPC connection. Two transports now ship (board item
  `01M03VJHG1WFECFJB4ZH3CKWDX` added the persistent one alongside the
  original one-shot):
  - **One-shot exec (the default).** The SAME shape
    `conway_tools::hook_runner::ProcessHookRunner` already uses for
    `[hooks].rules[].command`: spawn fresh, write one JSON request to stdin,
    read one JSON response from stdout, tear the process down. No process
    outlives a single request. This is cheaper to build, cannot leak a
    wedged long-lived child, and costs an author nothing a one-shot script
    cannot already pay — see [`concepts.md`](concepts.md)'s "Language
    choice" section for the per-spawn cost this same trade-off already
    prices (10ms for a shell script, 200–400ms for Python).
  - **Persistent NDJSON JSON-RPC (opt-in).** Spawn the command ONCE, keep it
    alive across many `tool/1` calls, frame requests/responses as one JSON
    object per line (`\n`-delimited) over the child's stdin/stdout. Only
    `tool/1` rides this channel — `tool.spec/1` discovery stays one-shot
    under both transports (see "The persistent transport" below for the
    framing decision, the failure modes, and the trust statement).
- **Points.** Only `tool.spec/1` (declaration) and `tool/1` (execution) are
  wired. `permission.policy/1`, `context.hook/1`, `observe/1` are still exactly
  as design-only/unconsumed as [`hooks.md`](hooks.md)'s own tables state —
  nothing here widens any of them. `PluginManifest::required_host_caps` is the
  exception: the `tool.spec/1` wire answer now CARRIES it (board item
  `01M03VJXARFHSDAGHFXGCWKJTY` -- `WireManifest::required_host_caps`,
  `#[serde(default)]`; a MALFORMED cap tag fails closed at parse, but a
  WELL-FORMED, previously-unknown one now parses -- `HostCapability` is an
  open vocabulary, board item `01M0WWKA8K1E7JPK87J6RRQMZF` -- and is
  refused instead at the host-capability gate below), and the `conway`
  builder consults it at registration to refuse a plugin whose declared cap
  the host lacks. `PluginManifest::requires`/`optional` carry
  the same way (board item `01M0XCD3P8S3VR0T1H0KNG5TMD` --
  `WireManifest::requires`/`optional`, both `#[serde(default)]`, name-only
  plugin-id lists): a subprocess plugin can declare a dependency on another
  plugin exactly as an in-process one does, resolved and checked by the
  same `ConwayBuilder::build` code, over the resolved set — not a separate
  wire-only path. See [`hooks.md`](hooks.md) point 1 for the full
  consumed-status disclosure.
- **Trust.** No new trust mechanism was built. See "Trust" below.

## The wire protocol

Two request kinds, each its own process spawn. A well-behaved plugin reads
its entire stdin, writes exactly one JSON object to stdout, and exits 0.

### `tool.spec/1` — declare your tools

Sent once, when `conway` starts (or whenever a library embedder calls
`SubprocessPlugin::discover`), before any call reaches your plugin.

Request (stdin):

```json
{"op": "tool.spec/1"}
```

Response (stdout, exit 0):

```json
{
  "id": "acme.greet",
  "version": "0.1.0",
  "tools": [
    {
      "name": "greet",
      "description": "Greets the caller by name.",
      "schema": {
        "type": "object",
        "properties": { "name": { "type": "string" } },
        "required": ["name"]
      },
      "category": "read",
      "permission": "safe"
    }
  ]
}
```

- `id`/`version` — your plugin's own identity. Not validated against
  whatever id you gave the entry in `settings.json`; the two are allowed to
  differ.
- `tools` — one or more. Zero is refused (a plugin declaring nothing is
  indistinguishable from a broken one). Every `name` must be non-empty and
  unique within this manifest.
- `schema` — an ordinary JSON Schema document (whatever shape you'd hand to
  any tool-calling model). Parsed and compiled on the host side; a schema
  that fails to compile fails discovery, the identical rule
  [`hooks.md`](hooks.md) point 1 states for an in-process `Plugin`.
- `category` — one of `read`, `edit`, `delete`, `move`, `search`, `execute`,
  `think`, `fetch`, `delegate` (`conway_core::content::ToolCategory`,
  aligned with ACP's own tool-call categories).
- `permission` — one of `safe`, `requires_approval`, `dangerous`
  (`conway_core::content::PermissionClass`). **Required, never defaulted**:
  an omitted field is a parse error, not a silent `safe`.

Two more fields sit alongside `id`/`version`/`tools`, both optional and both
absent from the example above on purpose — an older plugin that omits them
loads unchanged:

```json
{
  "id": "acme.greet",
  "version": "0.1.0",
  "tools": [ /* ... */ ],
  "requires": ["conway.ui"],
  "optional": ["conway.notifications"]
}
```

- `requires` — plugin ids this plugin's stated function cannot perform at
  all without. Default `[]`. Name-only, no version constraint. A `requires`
  id absent from the final installed plugin set is a hard build error naming
  both plugins (`ConwayBuilder::build`) — the same check an in-process
  `Plugin`'s `PluginManifest::requires` already goes through, not a
  subprocess-only variant of it.
- `optional` — plugin ids whose absence degrades only a presentation or
  convenience of this plugin. Default `[]`. A missing optional dependency
  never fails the build; it's loaded anyway with a `ConfigWarning` and a
  `tracing::warn!` naming both ids.

### `tool/1` — answer one call

Sent once per tool call, as a fresh process, whenever the model invokes one
of your declared tools.

Request (stdin):

```json
{"op": "tool/1", "tool": "greet", "call_id": "call-1", "arguments": {"name": "world"}}
```

Success response (stdout, exit 0):

```json
{"ok": true, "blocks": [{"type": "text", "text": "hello, world"}], "is_error": false}
```

`blocks` reuses `conway_core::content::ContentBlock`'s own wire shape
verbatim — the same tagged vocabulary a backend adapter already puts on the
wire for model output (`text`, `thinking`, `tool_use`, `tool_result_block`,
`image`) — rather than inventing a second content-block encoding for this
one transport. `artifacts` (default `[]`) is accepted alongside `blocks` for
non-text products (`conway_core::content::Artifact`'s own shape); omit it
if your tool produces none.

Declared-failure response:

```json
{"ok": false, "error": {"kind": "invalid_arguments", "detail": "name is required"}}
```

`kind` is one of `invalid_arguments`, `denied`, `cancelled`, `timeout`,
`io`, `internal` — the identical vocabulary `conway_core::error::ToolError`
already offers an in-process `Tool`, so a subprocess plugin can report
exactly the same failures a Rust one could, no narrower. `timeout` alone
also reads an `after_secs` field (default `0`).

## Every failure mode fails closed

| What happens | What conway does |
|---|---|
| The command can't be spawned (not found, not executable) | A typed error naming the entry's own `id`; discovery/the call never proceeds as if zero tools were declared |
| The process doesn't answer within `timeout_ms` | Killed (process-group SIGTERM, then SIGKILL after a grace period — the identical sequence `ProcessHookRunner` uses), a typed timeout error |
| The process exits nonzero | A typed error naming the exit code |
| stdout isn't valid JSON, or doesn't match the expected shape | A typed error — **never** read as an empty/default success |
| A call is already cancelled (`ToolCtx.cancel`) when it reaches the tool | The process is never spawned at all |

**A plugin that dies mid-call yields a typed error and the agent loop
continues** — the same guarantee `PHILOSOPHY.md` §1 states for a parent
awaiting a subagent ("a parent awaiting a child cannot hang"), applied here
to a subprocess instead of a subagent.

A discovery failure fails the **whole build**, naming the offending entry's
`id` — an operator who names a plugin in `settings.json` and gets nothing
for it, silently, is exactly the outcome this set's declaration rule exists
to prevent (see [`hooks.md`](hooks.md) point 1's identical "the whole
registry does not partially build" rule for an in-process `Plugin` with a
broken schema).

## Trust — read this before you configure one

**No new trust mechanism exists for this.** Naming a command in
`[plugins].subprocess` is on the exact same footing as naming one in
`[hooks].rules[].command` already is: no sandboxing, no digest check, no
allow/deny list. The operator's own review of what they typed into
`settings.json` is the only control point — see
[`trust-and-security.md`](trust-and-security.md)'s own "What trust is"
section for why a digest-keyed `plugin` trust subject was considered and
DECLINED (board item `01KZHVFCN6ZEAXV7K5JHRQN1YB`), not left open for lack
of a consumer: gating only the out-of-process transports while
`[hooks].rules[].command` stays permanently ungated would assert a
distinction the identical unsandboxed, full-privilege execution underneath
both does not support, and this slice does not work around the decision by
inventing a parallel mechanism of its own.

If you would not paste an unfamiliar shell command into `[hooks].rules[]`,
do not paste one into `[plugins].subprocess[]` either.

## The persistent transport

The persistent NDJSON transport (board item `01M03VJHG1WFECFJB4ZH3CKWDX`) is
an opt-in alternative to one-shot exec, for a plugin with genuine per-call
state or a per-spawn cost that matters at the scale it is called. It is
**off by default** — set `"transport": "persistent"` on the entry:

```json
{
  "plugins": {
    "subprocess": [
      { "id": "acme-mcp", "command": ["/path/to/mcp-bridge.py"], "timeout_ms": 5000, "transport": "persistent" }
    ]
  }
}
```

### Framing

NDJSON — one JSON-RPC object per line, `\n`-delimited, over the child's
stdin/stdout. The persistent channel carries ONLY `tool/1`: a request is
the one-shot `tool/1` body (`op`, `tool`, `call_id`, `arguments` — the SAME
field names as above) plus a JSON-RPC `id` this host assigns for
correlation, and the response is the one-shot `tool/1` answer (`ok`,
`blocks`, `is_error`, `artifacts`, `error`) plus the echoed `id`. Nothing
here invents a second content-block or error vocabulary. `tool.spec/1`
discovery stays one-shot under both transports — that sidesteps the one
real wire collision a persistent envelope would otherwise force (a
JSON-RPC correlation `id`, a number, against the manifest's own `id`, the
plugin's string identity).

### Failure modes — fail-closed, uniformly

| What happens | What conway does |
|---|---|
| The session's child exits (nonzero or otherwise) or closes stdout mid-call | A typed `SessionDied` error naming the plugin and the failure mode — never a hang. **No automatic reconnect:** a plugin that died has lost whatever session state it had; the death is surfaced and you must re-`discover` (restart) to spawn a fresh child. |
| A `tool/1` call does not answer within `timeout_ms` | The process group is killed (graceful SIGTERM-then-SIGKILL) and a typed `TimedOut` is reported. `timeout_ms` is a **per-call** deadline on the framed read, NOT a session-wide idle kill — a session that legitimately sits idle between calls is left alone. |
| An unterminated or malformed frame (no newline, invalid JSON, a partial line then EOF) | A typed `MalformedFrame` parse error, not a deadlock. The session is marked dead afterward — a plugin that garbles its framing cannot be trusted to recover. |
| A response's `id` does not match the outstanding request | A typed `SessionDied` (protocol error); the session is marked dead. |

stderr is drained concurrently for the session's lifetime (a plugin that
writes to stderr with nobody reading it cannot block) and **discarded** —
no log/event sink is wired, mirroring the one-shot path. When the session is
dropped, the process group is killed (best-effort SIGKILL on the group plus
`kill_on_drop` on the leader) so a long-lived child is never orphaned.

### Trust — read this before you set `"transport": "persistent"`

A persistent subprocess plugin holds a long-lived, unsandboxed process the
operator named in config — the same footing as a `[hooks].rules[].command`,
held for longer, with the larger exposure that implies. An operator who
would not paste an unknown command into a hook rule should not paste one
into a persistent subprocess plugin entry either.

This is a **larger exposure**, not a larger **capability grant**: the child
can do exactly what the one-shot child could do — it just does it for
longer, accumulates state across calls, and can fail in new ways (die
mid-session, write a partial frame, stall on a blocked pipe). None of those
are trust-mechanism gaps; they are the liveness/safety problems the failure
handling above solves. The declined digest-keyed `plugin` trust subject
(board item `01KZHVFCN6ZEAXV7K5JHRQN1YB`) would have addressed a DIFFERENT
threat — verifying the binary on disk is the one the operator reviewed —
that is identical for one-shot and persistent. Going persistent does not
change that calculus, so it is not an argument for revisiting the decline,
and this transport builds no parallel trust mechanism of its own.
See [`trust-and-security.md`](trust-and-security.md) for the persistent-
exposure entry in that page's inventory.

## Configuring one

```json
{
  "plugins": {
    "subprocess": [
      { "id": "acme-greet", "command": ["/path/to/greet.py"], "timeout_ms": 5000 }
    ]
  }
}
```

`command` is an argv vector (program, then its arguments) — never a single
shell string, matching `[hooks].rules[].command`'s own shape and reasoning
(no shell-quoting ambiguity between what you wrote and what actually gets
spawned). `timeout_ms` defaults to 5000, the same default and reasoning
`[hooks].rules[].timeout_ms` uses. `transport` defaults to `"one_shot"`
(the behavior above); set it to `"persistent"` for the long-lived NDJSON
JSON-RPC channel — see "The persistent transport" above for what that
changes and the trust statement that goes with it.

**Empty by default.** No subprocess plugin is ever spawned unless named
here — the same "nothing in this tier runs unasked" rule
`[plugins].install` states for the in-process first-party tier.

## Proving the mechanism, in order

This is the property the item's own acceptance criterion asks for, stated
as an executable sequence rather than a claim: build `conway` first, author
the plugin second, run third.

1. `cargo build -p conway-cli` (or use an already-shipped binary). The
   binary has never heard of `greet.py`.
2. Write `greet.py` — a plain Python 3 script that reads one JSON object
   from stdin and prints one back, per the protocol above. No Rust, no
   `cargo`, no dependency on this workspace at all.
3. Add the `[plugins].subprocess` entry above to `settings.json`, pointing
   at `greet.py`.
4. Run the SAME already-built binary: `conway -p "greet the world"
   --allowed-tools greet`.

The binary gained a tool. `crates/conway-cli/tests/subprocess_plugins.rs`
is this exact sequence, executed: the compiled binary
(`assert_cmd::cargo::cargo_bin("conway")`, already built by the time the
test process starts) is driven against a Python fixture the test writes at
its own runtime, and a companion test removes the `[plugins]` entry and
confirms the tool disappears again with no rebuild in between.
`crates/conway-plugin-subprocess/tests/end_to_end.rs` proves the identical
mechanism one layer down, through a library-embedder's own
`ConwayBuilder::with_plugin` call and a real agent turn (`ScriptedBackend`,
no network).

## A Python fixture, complete

```python
#!/usr/bin/env python3
import sys, json

req = json.loads(sys.stdin.read())
op = req.get("op")

if op == "tool.spec/1":
    print(json.dumps({
        "id": "acme.greet",
        "version": "0.1.0",
        "tools": [{
            "name": "greet",
            "description": "Greets the caller by name.",
            "schema": {
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"],
            },
            "category": "read",
            "permission": "safe",
        }],
    }))
elif op == "tool/1":
    args = req.get("arguments", {})
    name = args.get("name", "")
    print(json.dumps({
        "ok": True,
        "blocks": [{"type": "text", "text": f"hello, {name}"}],
        "is_error": False,
    }))
```

## What's left, named rather than lost

- **`permission.policy/1`, `context.hook/1`, `observe/1`.** Only
  `tool.spec/1`/`tool/1` are wired. A subprocess plugin cannot yet
  contribute a permission policy, edit context, or observe events —
  `hooks.md`'s own tables for those points are unchanged by this page. The
  persistent transport's JSON-RPC `id` correlation table is built so a
  later item can add `observe/1` notifications alongside requests without
  redesigning framing, but no notifications are wired yet.
- **A `plugin` trust subject.** Deliberately not built here — see "Trust"
  above.
- **Backends, routers, capability negotiation.** None of the three is
  reachable through a subprocess yet — only `Tool`.
