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

**The index entry itself used to not be one line, in the sense that
mattered.** DOGFOOD 3 (2026-09-30, a real session against a 32,768-token
window) found that deferral correctly dropped a tool's parameter schema
down to the trivial `{}` placeholder, but kept an MCP server's full,
multi-sentence `description` verbatim — one real example, `list_directory`,
ran to ~380 characters. The 14 deferred MCP tools in that session still
cost ~2,100 tokens on that basis alone: deferral was saving the schema, not
the description, which is most of what made "one-line" a promise this
plugin's doc made but its code did not keep.

Fixed: a deferred tool's description is now narrowed to its first sentence,
or (whichever is shorter) a 100-character bound cut at the last word
boundary — see [`src/lib.rs`'s own `truncate_description`](../../crates/conway-plugin-toolindex/src/lib.rs)
doc for the exact rule. The full, untruncated description is never lost; it
is exactly what `describe_tool` serves back. Measured before/after on this
crate's own `tests/toolindex_e2e.rs` fixture tool (`mcp_tool_3`, a single
169-character sentence — already fairly compact, since it predates this
fix): its index entry shrinks from 241 characters (full description) to 169
(truncated) — a modest ~30% cut on an already-short, single-sentence
description, and a far larger one on a real multi-sentence MCP description
like the `list_directory` case above (a ~380-character description, index
entry included, narrows to its first sentence alone, capped at 100
characters if that sentence itself runs long).

## The fixed per-request floor: conway's own subagent tools

DOGFOOD 3's other finding: conway's own `conway_fork`/`conway_ask`/
`conway_cancel`/`conway_spawn`/`conway_steer`/`conway_await` tools are
never deferred by this plugin (`always_announced_names`, above) and are
installed by default (`conway-tools`' `SubagentPlugin`), so their combined
wire cost is a floor every session pays on every request, with or without
`conway.toolindex` installed, and regardless of how many MCP tools an
operator adds on top. Measured (`crates/conway-tools/tests/
subagent_wire_cost.rs`, the exact `{"type":"function","function":
{"name":...,"description":...,"parameters":...}}` wire shape
`OpenAiCompatBackend` sends, bounded by budget rather than pinned to an
exact figure so a drift here is still a build failure, but shrinking
further is never one):

| Tool            | Before (chars) | After (chars) |
|-----------------|---------------:|--------------:|
| `conway_fork`   |          2,602 |          1,336 |
| `conway_ask`    |          2,544 |          1,053 |
| `conway_cancel` |          2,380 |            750 |
| `conway_spawn`  |          2,188 |          1,325 |
| `conway_steer`  |            840 |            443 |
| `conway_await`  |            825 |            457 |
| **Total**       |     **11,379** |      **5,364** |

Before: roughly 2,845 estimated tokens (`heuristic-chars4`) on every single
request, before any MCP/subprocess/first-party-plugin tool is counted at
all. **Board item `01M41BC4KJAE8J1X3GAA36ZW6Y` (operator ruling,
2026-10-03) resolved this in favor of shortening, not deferring**: these
six tools stay fully announced -- deferring them was never on the table,
since `always_announced_names`' own "mechanical, not a guess" rule above
keeps every built-in always announced, and deferring would also have
routed every subagent call through `describe_tool`'s first-use friction
(board item `01M3TEK20AERQNZRVY7G5F50VJ`) for tools used on essentially
every turn. Instead, each tool's description and JSON-schema field docs
were cut to the model-facing contract alone, moving rationale and
caching-mechanics prose that does not change how a tool is called into
`docs/agents.md`/`docs/tools.md` instead. After: roughly 1,341 estimated
tokens -- a drop of about 1,500 tokens (6,015 wire chars), clearing the
item's own >=1,500-token target. `always_announced_names`' own
"mechanical, not a guess" rule above exists precisely because membership in
that set is a fixed, disclosed list, not something this plugin should
start quietly trimming on its own
opinion of which built-in tools "matter less."

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
