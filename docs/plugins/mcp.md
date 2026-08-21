# The MCP client: bringing an existing MCP server's tools into conway

The MCP-over-stdio **client** plugin (board item `01M03GPNF0KN59FHAEEAEY2JD3`;
`PHILOSOPHY.md` §5: "An MCP server is a plugin that brings tools with it"),
shipped by `crates/conway-plugin-mcp`. Depends on
[`concepts.md`](concepts.md) for vocabulary and reads naturally alongside
[`subprocess-plugins.md`](subprocess-plugins.md) — the two are sibling
transports for reaching an external process, not a layering of one on the
other (see "How this differs from a subprocess plugin" below).

## This is a client, not a server — read this before anything else

**`conway-plugin-mcp` does NOT expose conway itself over MCP.** It does not
turn `conway` into an MCP server another tool can talk to — that is a
separate, lower-priority, unbuilt question. What this crate does is the
other direction: `conway` (the harness) is the MCP **client**; the command
you name in config is an existing **MCP server** someone else wrote, and
this plugin makes its tools appear as ordinary `conway::plugin::Tool`s. If
you came here wanting conway to *be* an MCP server for some other client to
call, there is nothing on this page for you.

## What this is, in one sentence

`conway` spawns an operator-named command as a persistent child process
**once**, completes the MCP `initialize`/`notifications/initialized`
handshake and a `tools/list` call, and exposes every tool the server
declared as an ordinary tool the model can call — each `tools/call`
answered over the same long-lived stdio connection.

## What installing it costs

```json
{
  "plugins": {
    "mcp": [
      { "id": "acme-mcp", "command": ["/path/to/mcp-server"], "timeout_ms": 5000, "env": [] }
    ]
  }
}
```

`command` is an argv vector (program, then its arguments) — never a single
shell string, the same shape and reasoning
[`subprocess-plugins.md`](subprocess-plugins.md)'s own `command` field uses.
`timeout_ms` (default 5000) is a **per-call** deadline on one framed
JSON-RPC round-trip (`initialize`, `tools/list`, or one `tools/call`) — not
a session-wide idle kill; a server that sits idle between calls is left
alone. `env` is explicit environment pairs the child inherits **in addition
to** the parent process's own env — for scoping credentials (an API key the
MCP server forwards to its own upstream provider) rather than relying on
implicit inheritance. Empty by default (`[plugins].mcp = []`): **no MCP
server is ever spawned unless named here.**

A discovery failure (spawn, timeout, a refused handshake, or a malformed
`tools/list` answer) fails the **whole build**, naming the offending entry's
own `id` — never silently skipped.

## How this differs from a subprocess plugin

`conway-plugin-subprocess` speaks conway's **own** wire protocol
(`tool.spec/1`, `tool/1`, ...); this crate speaks a **different** protocol —
JSON-RPC 2.0, the wire MCP itself defines (`initialize`,
`notifications/initialized`, `tools/list`, `tools/call`). This crate does
**not** depend on `conway-plugin-subprocess` and does not route through it —
the two are siblings that happen to share a shape (spawn once, keep the
child alive, frame requests/responses over stdio, kill the process group on
drop), not a layering of one atop the other. Resolved by its own,
separate choke point (`crates/conway-cli/src/mcp_plugins.rs`), distinct
from both `first_party_plugins::install` (a closed set of crates this
binary links) and `subprocess_plugins::install` (conway's own wire).

## What it deliberately does not do

- **No official MCP SDK dependency.** MCP's wire protocol *is* JSON-RPC
  2.0, hand-rolled here with `serde_json` (already in the workspace graph).
  The official `rmcp` SDK, or any MCP client library, is recommended against
  by design: it pulls in an async-runtime/HTTP stack disproportionate to a
  stdio JSON-RPC codec, and `cargo deny check` has previously caught an
  ungranted licence from exactly this kind of addition.
- **No HTTP+SSE transport.** Stdio only — HTTP+SSE MCP is a separate,
  unbuilt item, deliberately not folded into this crate.
- **No category/permission inference.** MCP's own `tools/list` answer
  carries no category or permission field, so an MCP tool is opaque to
  conway on that axis: every MCP tool is registered at the **most
  restrictive** pairing (`ToolCategory::Execute`, `PermissionClass::
  Dangerous`), mirroring how `conway-plugin-subprocess` degrades an unknown
  wire tag. There is no way to make an MCP-provided tool `Safe` from
  config — treat every one as requiring approval.
- **No automatic reconnect.** If the server's child process dies mid-call
  or closes its stdout, that session is marked dead: a typed error surfaces
  and every later call on it fails fast. You must re-`discover` (restart)
  to get a fresh child; nothing here silently respawns one for you.

## Its limits, stated plainly

- **The manifest id is derived, not chosen.** `PluginManifest::id` is
  `mcp.<serverInfo.name>` (falling back to `mcp.<config_id>` if the server's
  own name is empty) — you don't get to pick this plugin's id directly, only
  its config entry's `id` (used only in error messages).
- **A duplicate or invalid tool name/schema in the server's `tools/list`
  answer fails discovery entirely** — the whole MCP plugin, not just the
  offending tool, never registers.
- **Cancellation is a caller preference, not a session failure.** A
  `tools/call` cancelled mid-flight returns `ToolError::Cancelled` and the
  session stays alive for the next call — but the *write* half of a call is
  never cancellable (a cancel mid-write would corrupt the shared NDJSON
  framing for every tool on the session), so cancellation can only ever cut
  short the *read* half.

## Trust — read this before you name a server

**No new trust mechanism exists for this.** An MCP server's `command`
executes with your own privileges, unsandboxed — the identical footing
`[hooks].rules[].command` and `[plugins].subprocess[]` already have: no
sandboxing, no digest check, no allow/deny list. The operator's own review
of what they typed into `settings.json` is the only control point. Board
item `01KZHVFCN6ZEAXV7K5JHRQN1YB` (a digest-keyed `plugin` trust subject) is
under a **standing operator deferral** and is not built by this crate —
naming an MCP server here is exactly as trusted, and exactly as unaudited,
as naming a `[hooks].rules[].command` already is today. If you would not
paste an unfamiliar shell command into `[hooks].rules[]`, do not paste one
into `[plugins].mcp[]` either. See
[`trust-and-security.md`](trust-and-security.md) for the fuller argument
this crate's own module doc restates.
