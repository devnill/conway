# `conway.toolindex`: deferred tool schemas

The first-party plugin for narrowing every non-built-in tool's announced
schema, installed by `crates/conway-plugin-toolindex`. The tool-shaped analog
of [`skills.md`](skills.md) — read that page first if you have not; this
plugin mirrors its "narrow in `ContextHook::before_request`, serve the full
thing through a companion tool" composition, applied to
[`ContextPayload::tools`](hooks.md) rather than a prompt segment. Depends on
[`concepts.md`](concepts.md) for vocabulary and [`hooks.md`](hooks.md) point
3 for the `ContextHook::before_request` contract this plugin's hook runs
through.

## What this is, in one sentence

A [`ContextHook`](hooks.md) that narrows every deferrable tool's announced
`description`/`schema` down to a one-line
`name: description (call describe_tool(name="...") for the full schema)`
index entry, paired with a `describe_tool` tool that returns the full
description and JSON schema on demand — and, once called for a given tool,
keeps that tool fully announced for the rest of the session.

## The problem, measured

A real session with one external MCP tool server installed showed the
`tool_registry` context segment at ~10,087 estimated tokens, versus ~4.4k
with only built-in tools — on a 32.7k context window that alone fires the
runway notice on turn one. `ContextBuilder`'s own `[2] ToolSchemas` step
independently measured a representative 14-tool built-in set at ~3.4k
tokens; an installed MCP server or subprocess plugin on top of that is what
pushes a small window over the edge. (Re-verify before quoting either
number — specs in this tree go stale fast.)

## What installing it costs

```json
{ "plugins": { "install": ["conway.toolindex"] } }
```

Uninstalled, `ContextPayload::tools` reaches the backend unedited — the
runtime's context hook is simply never set.

## Which tools are deferrable, and why this is mechanical

- **Always fully announced:** every tool from
  `conway::presets::builtin_plugins()` (`conway.fs`, `conway.shell`,
  `conway.subagent`, `conway.report`) — used on essentially every turn, so
  deferring them would trade a real extra round trip for no measured win —
  plus `describe_tool` itself, structurally: if it were deferred, no tool
  could ever be un-deferred.
- **Deferred by default:** everything else this plugin ever sees announced —
  MCP tools, subprocess-plugin tools, and (a disclosed simplification) any
  other installed plugin's own tool (`read_skill`, `remember`,
  `ask_question`, ...). A `ToolSpec` carries no plugin/source field, so
  "built-in vs. everything else" is the finest mechanical distinction this
  seam can draw without a `conway-core` change. This costs a small,
  already-cheap tool at most one extra `describe_tool` round trip the first
  time it is called; it never produces incorrect behavior (see "How the
  wire stays coherent" below).

Nothing here is a model-driven guess about which tools "matter" — membership
in the always-announced set is a fixed, computed-once name list, never
inferred from a tool's schema size, description text, or any runtime signal.

## How the wire stays coherent

Every deferrable tool stays **in** `ContextPayload::tools`, under its own
real name, on every turn. Narrowing never removes an entry from the
announced array — it replaces that entry's `description` with the one-line
index text and its `schema` with a trivial, always-valid `{}` placeholder.
Because the tool is always present under the same name, a call to it is —
by construction — always a call to an announced tool. There is no new "was
this announced this turn" check anywhere, and
`ensure_hook_payload_coherent` needs no change: that guard checks
`ToolUse`/`ToolResult` pairing within the assembled segments, and this
hook never touches segments at all, only `ContextPayload::tools`.

Two more facts make this genuinely safe:

1. **Real argument validation never reads the narrowed schema.** The
   runtime's tool registry compiles each tool's JSON-Schema validator once,
   at startup, from that tool's own real `Tool::spec()` — entirely
   independent of whatever a `ContextHook` later shows the model for a
   given turn. A model that calls a still-narrowed tool with a guessed
   argument set is validated against the REAL schema and refused with the
   existing, already-typed tool-call error on mismatch — no new refusal
   path, no silent fetch.
2. **`describe_tool` answers from data this plugin itself observed being
   announced, never a privileged registry read.** Every `before_request`
   call caches each currently-announced tool's real, pre-narrowing spec
   into a shared map before narrowing its own local copy — the tool
   universe is re-derived from the real registry every turn, so this cache
   is always accurate. `describe_tool` just looks a name up in it, the same
   "no privileged channel" shape `conway.skills`'s `read_skill` uses,
   adapted from a static, upfront-loaded map to a dynamic, turn-populated
   one (an MCP server's own tool list is only available after that
   server's handshake, unlike skills loaded from disk up front).

## Revealing a tool

`describe_tool`'s invoke records the requested name into a shared map keyed
by agent id — sibling agents in a fan-out never pool this state, mirroring
`conway.stepguard`'s own per-agent state keying. Once revealed, a tool stays
fully announced (real description, real schema) for the rest of that
agent's run; a sibling or later agent that never called `describe_tool`
still sees it narrowed. An unknown name is a model-visible `is_error: true`
result ("no such tool: ..."), never a hard error or crash.

## Default set or opt-in

**Opt-in, not part of the default first-run opinion set.** Unlike
`conway.skills` (whose narrowing is author-controlled and uniformly safe —
an operator's own skill files), this plugin changes model-facing
tool-calling behavior for every non-built-in tool: a deferred tool's first
call may need an extra `describe_tool` round trip before the model has the
real argument schema — a real, if usually small, reliability/latency
trade-off the default opinion set's own operator ruling never evaluated.
Turning it on unprompted for every fresh install would be this plugin
overriding that ruling rather than extending it — the same posture
`conway.web` takes, for a different (trust, not reliability) reason. An
operator who wants the token savings for a large MCP install opts in
explicitly, as shown above.

## Its limits, stated plainly

- **The always-announced/deferred split is per-tool-name, not
  per-plugin.** A first-party plugin's own small tool (e.g. `read_skill`)
  is deferred exactly like a large MCP tool, because a `ToolSpec` carries
  no source attribution this hook could read instead.
- **No cache-hint tuning** — narrowing happens after `ContextBuilder::build`,
  and this plugin's hook does not set `PromptSegment::cache_hint`.
- **State is per-process, not persisted.** A `describe_tool` reveal does
  not survive a process restart; a resumed session narrows every
  non-built-in tool again on its first turn back.

## Trust

No new trust mechanism — `describe_tool` is `PermissionClass::Safe` (a pure
read of an already-compiled tool spec), and the hook only rewrites the
already-assembled tool announcement, the same seam every other
`ContextHook` runs through. See
[`trust-and-security.md`](trust-and-security.md) for what a trusted plugin
can and cannot do more generally.
