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
      {
        "id": "acme-mcp",
        "command": ["/path/to/mcp-server"],
        "timeout_ms": 5000,
        "first_call_timeout_ms": 20000,
        "env": []
      }
    ]
  }
}
```

`command` is an argv vector (program, then its arguments) — never a single
shell string, the same shape and reasoning
[`subprocess-plugins.md`](subprocess-plugins.md)'s own `command` field uses.
`env` is explicit environment pairs the child inherits **in addition to**
the parent process's own env — for scoping credentials (an API key the MCP
server forwards to its own upstream provider) rather than relying on
implicit inheritance. Empty by default (`[plugins].mcp = []`): **no MCP
server is ever spawned unless named here.**

A discovery failure (spawn, timeout, a refused handshake, or a malformed
`tools/list` answer) fails the **whole build**, naming the offending entry's
own `id` — never silently skipped.

### Three timeout tiers, plus a bounded grace before any kill

A single flat per-call deadline has no room for a server that answers a
*little* late for an ordinary reason — a `cargo build` (or anything else)
briefly starving the process of CPU is not the same failure as a server
that is genuinely wedged or malicious, and treating them identically cost a
real dogfooding session its plugin mid-task (board item
`01M1YQ3MJQSCQTMVAZ3GCSTB8P`). So there are three deadlines, each a
different answer to "how long may this legitimately take," plus one grace
rule that applies uniformly to all of them:

1. **The opening handshake** (`initialize`/`notifications/initialized`/
   `tools/list`) gets a fixed, generous, **not operator-configurable** 120s
   budget — long enough that a Claude Code plugin which builds itself on
   first launch (`npm install && npm run build`, no bundled runtime) still
   opens a session. `[plugins].claude_compat[]` always uses this budget for
   its translated servers too.
2. **The first ordinary round trip after the handshake** — a session's
   first real `tools/call` — gets `first_call_timeout_ms` (default 20000).
   A freshly-spawned server's first real request commonly pays a one-time
   warm-up cost (opening a database connection, priming a cache) that an
   already-warm call never pays again, so it gets a larger, operator-tunable
   budget of its own.
3. **Every ordinary round trip after that** uses `timeout_ms` (default
   5000) exactly as before — an operator who tuned `timeout_ms` alone sees
   no change in what it governs.

**On any of these three deadlines elapsing, this host does not kill
immediately.** It emits a `tracing::warn!` naming the plugin and the
overrun, then waits again — for the SAME pending response, never a
resend — up to a small, fixed, documented multiple of that deadline (3×,
today) before giving up. If the answer lands inside that extension, the
call succeeds, late, with only the warning to show for it. **Never a
resend, ever**: a duplicate `tools/call` could double a side-effecting
operation, so this is strictly "wait longer for the same answer," never a
retry. If the full ceiling still elapses, the outcome is exactly what it
always was — the process group is killed and the call fails closed with
`McpPluginError::TimedOut` — so a genuinely hung or malicious server is
caught exactly as before; only a server merely running a little late gets
patience it did not have before. **This "never a resend" rule holds on the
auto-respawn path too** — see the bullet below: a killed process's own
`tools/call` is never re-sent to the fresh child that replaces it, for the
identical reason.

This machinery lives once, in the process-lifecycle layer both
`conway-plugin-mcp` and `conway-plugin-subprocess` share
(`conway_tools::process::child_session::ChildSession`), so a subprocess
plugin's own `tool/1` calls inherit the SAME bounded grace uniformly (that
crate has no `first_call_timeout_ms` config knob of its own, since it has
no equivalent "warm-up" concept — see
[`subprocess-plugins.md`](subprocess-plugins.md)).

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
- **The session never reconnects; the plugin auto-respawns, bounded — and
  the call that hit the death is resent *only* when its tool declared
  itself safe to retry.** Three different claims. If the server's child
  process dies mid-call or closes its stdout, that *session* is marked dead
  permanently — it never comes back, because whatever conversational state
  that child held is genuinely gone. The *plugin* does not stay stuck on
  it: the call that found the session dead (or whose own round trip just
  died) transparently spawns a fresh child from the same command/env/
  timeouts, re-runs the handshake, and checks the fresh server's
  `tools/list` against the tool set this plugin registered at `discover`
  time (a mismatch fails closed with a typed error naming the difference,
  rather than silently adopting a different tool set) — so the *next* tool
  call already finds a fresh session waiting, no operator action, no conway
  restart.
  Whether the call that discovered the death is itself retried against
  that fresh child depends on one thing: did the specific tool it called
  declare `annotations.idempotentHint: true` in the `tools/list` answer
  this plugin was originally discovered with? Over stdio, a closed pipe
  carries no information about whether the killed process had already
  completed the work it was asked to do, so this host cannot tell a
  never-sent request apart from one that ran to completion right before the
  process died — normally the ONLY safe answer is to fail that one call
  closed and let the caller decide whether to ask again. (This mirrors
  gRPC's own transparent-retry rule, which permits a retry only when the
  client can prove the RPC was never sent — a proof this host can never
  produce over a closed stdio pipe.) MCP itself carries the opt-in that
  situation needs: `ToolAnnotations.idempotentHint`, which *defaults to
  `false`* — the protocol's own posture is that an unannotated tool is
  unsafe to retry, and this client follows that default exactly (absent,
  `null`, or an explicit `false` all fail closed, never retried, exactly as
  every tool behaved before this exception existed). Only a tool that
  declared `idempotentHint: true` gets the one retry, resent through the
  same channel as the original request, once, against the fresh session
  the respawn just put in place.
  **This is a trust exception, not a verified one.** `idempotentHint` is
  the *server's own, self-declared, advisory* claim — MCP's own authors
  describe `ToolAnnotations` as a risk vocabulary a server volunteers, not
  a property a client can check. conway has no way to confirm that calling
  a given tool twice is actually safe; it trusts whatever the server said
  in `tools/list`. A server that declares `idempotentHint: true` for a
  tool that is *not* actually safe to call twice can cause conway to
  double-execute that tool's side effect — that is the declaring server's
  error, not a bug in this client, but you should know conway is relying on
  the server telling the truth here, not verifying it independently.
  The respawn itself is bounded (a small, fixed number per plugin, for its
  whole lifetime — see `conway_plugin_mcp::MAX_AUTO_RESPAWNS`'s own doc for
  the argued number, which applies identically whether or not a retry was
  attempted): once that bound is spent, a typed `SessionDied` surfaces on
  every later call, permanently, and you do need to restart to get a fresh
  child. A crash-looping server still fails closed; a server that stumbled
  once is no longer a whole-session outage, and no call is ever silently
  doubled to buy that recovery unless its own tool asked for exactly that.
- **No MCP prompts or resources.** MCP defines three server-offered
  primitives — tools, prompts, and resources — and this crate speaks only
  the first: `prompts/list`, `prompts/get`, `resources/list`,
  `resources/read` are never called (`grep -rn 'prompts/\|resources/'
  crates/conway-plugin-mcp/src` matches nothing). A consumer would look
  different for each: an MCP prompt is a server-authored, parameterized
  message template, which maps onto conway's own `Plugin::commands()` as a
  namespaced slash command; an MCP resource is server-exposed read-only
  content, addressable by URI, which maps onto either a read-only `Tool` or
  a `Plugin::instructions()` fragment depending on whether the model should
  pull it on demand or always see it. Neither is built or scheduled — not
  tracked, since no item exists to build one; forward-declared here so an
  author who wants to publish a prompt or a resource from an MCP server
  finds out here, not by watching `tools/list` silently ignore both.

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
item `01KZHVFCN6ZEAXV7K5JHRQN1YB` (a digest-keyed `plugin` trust subject)
was reopened once both out-of-process transports shipped and worked to a
conclusion: **considered and DECLINED**, not deferred — gating only the
out-of-process transports with a digest check, while
`[hooks].rules[].command` stays permanently ungated, would assert a
distinction (plugins reviewed, hooks not) that the identical unsandboxed,
full-privilege execution underneath both does not support. Naming an MCP
server here is exactly as trusted, and exactly as unaudited, as naming a
`[hooks].rules[].command` already is today. If you would not paste an
unfamiliar shell command into `[hooks].rules[]`, do not paste one into
`[plugins].mcp[]` either. See
[`trust-and-security.md`](trust-and-security.md) for the fuller argument
this crate's own module doc restates.
