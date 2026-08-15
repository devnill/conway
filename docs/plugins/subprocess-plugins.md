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
  persistent NDJSON JSON-RPC connection. This slice is one-shot exec instead
  — the SAME shape `conway_tools::hook_runner::ProcessHookRunner` already
  uses for `[hooks].rules[].command`: spawn fresh, write one JSON request to
  stdin, read one JSON response from stdout, tear the process down. No
  process outlives a single request. This is cheaper to build, cannot leak
  a wedged long-lived child, and costs an author nothing a one-shot script
  cannot already pay — see [`concepts.md`](concepts.md)'s "Language choice"
  section for the per-spawn cost this same trade-off already prices (10ms
  for a shell script, 200–400ms for Python).
- **Points.** Only `tool.spec/1` (declaration) and `tool/1` (execution) are
  wired. `permission.policy/1`, `context.hook/1`, `observe/1`, and
  `PluginManifest::required_host_caps` are still exactly as
  design-only/unconsumed as [`hooks.md`](hooks.md)'s own tables state —
  nothing here widens any of them.
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
section for why a digest-keyed `plugin` trust subject is a *decided but
deliberately deferred* extension (board item `01KZHVFCN6ZEAXV7K5JHRQN1YB`,
under a standing operator deferral) and not something this slice works
around by inventing a parallel mechanism.

If you would not paste an unfamiliar shell command into `[hooks].rules[]`,
do not paste one into `[plugins].subprocess[]` either.

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
`[hooks].rules[].timeout_ms` uses.

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
  `hooks.md`'s own tables for those points are unchanged by this page.
- **The persistent-connection transport.** This slice is one-shot exec,
  disclosed above. A future item may add the long-lived NDJSON JSON-RPC
  shape the original design sketched, if a stated need (e.g. a plugin with
  genuine per-call state, or per-spawn cost that matters at the scale it's
  called) shows the one-shot cost is wrong for it.
- **A `plugin` trust subject.** Deliberately not built here — see "Trust"
  above.
- **Backends, routers, capability negotiation.** None of the three is
  reachable through a subprocess yet — only `Tool`.
