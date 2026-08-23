# `conway.path`: the tool a model calls to compose a context path

The first-party plugin for `ContextPathHost` (board item
`01M0PEFMG96SVBBD5D2E06H34A`, decision `01M0K4QT6MBXPD6PXMBBBD2P7B`),
installed by `crates/conway-plugin-path`. Depends on
[`concepts.md`](concepts.md) for vocabulary and on
[`hooks.md`](hooks.md) point 18 for the `ToolCtx::context_path` contract
this plugin's tool calls through, and on
[`trust-and-security.md`](trust-and-security.md)'s "Composing a context
path" section for what it can and cannot reach.

**This tool takes resolved `(session, seq)` references; it does not find
them for you.** [`discover.md`](discover.md) — `conway.discover`'s
`search_sessions` — is the tool that finds a session or record the model
does not already hold a reference to (one it neither started this turn nor
was handed a `transcript_ref` for). Install the two together: search finds,
`compose_context_path` composes what search found.

**This tool composes MID-CHAIN, on an already-running session.** The
boundary-time counterpart — choosing a CHILD's starting context at
fork/spawn time, before it has a prefix to invalidate — is
`ForkSpec`/`SpawnSpec::context`, an embedder-facing surface (not a model
tool) documented in [`embedding.md`](../embedding.md#choosing-a-childs-starting-context-forkspecspawnspeccontext).
It shares this tool's own `derive_with`/`set_head` machinery rather than
reimplementing it, and composes at a point where there is no existing
prefix to pay the reorder/omission cost this tool's own "trap" section
below describes.

## What this is, in one sentence

A model-callable tool, `compose_context_path`, that changes what a session
actually sends as context on its NEXT turn — bring specific records in from
another session, leave specific records of this session's own history out,
or both — and reports what it did and what it cost.

## Why this exists

conway keeps an append-only log of every record a session ever produced.
Two mechanisms already existed to shape what a FUTURE turn reads from that
log — `conway_runtime::context::path::write_head` (freeze a selection as
the session's new default) and `ValidatedPath::derive_with` (compose a
selection, including records read from elsewhere) — but until this plugin
landed, nothing in a running `conway` build ever called either one.
`compose_context_path` is that caller: the operator states an intent in
ordinary language ("forget that dead end and use what we found in that
other session"), the MODEL resolves which records that means, and this
tool is what actually freezes the result.

**Not a slash command, and not a `Curator`.** An operator does not type
`/path` anything — conway ships no such verb, deliberately (decision
`01M0K4QT6MBXPD6PXMBBBD2P7B`). And this is not the mechanical, per-turn
`Curator` port either: a curator runs before routing, with no model to
consult, so it can only apply arithmetic rules (drop turns older than N).
Composing from a stated intent needs a model to have already interpreted
that intent, which is why this ships as an ordinary tool, called at a
point where inference is already in flight.

## What installing it costs

```json
{ "plugins": { "install": ["conway.path"] } }
```

Uninstalled, nothing changes: no `compose_context_path` tool is announced,
and `ToolCtx::context_path` sits unused on every other tool exactly as it
did before this plugin existed (the field costs nothing to a tool that
never reads it). Opt-in, exactly like every other member of the first-party
tier — see `PHILOSOPHY.md`'s "First-party plugins, and why they are not
defaults" for why nothing in this tier is on by default.

No durable store to worry about, unlike [`conway.memory`](memory.md): this
plugin needs no `[plugins].install`-adjacent resolution step of its own.
The context-path mechanism it calls into (`write_head`'s `ContextPathSet`
records, and the selection bodies `PathStore` holds) is already part of
every session's own log and the engine's existing `paths/` store — nothing
new to open, nothing new that can fail to open.

## What the model actually sends

Two arguments, both resolved record references — never a free-text
description for this tool to re-interpret, because by the time the MODEL
calls it, the interpreting has already happened:

- `include: [{ "session": "<id>", "seq": <n> }, ...]` — records to bring
  onto the path, from any session. The model's two ordinary sources for a
  foreign `session` id: the `transcript_ref` a completed `conway_fork`/
  `conway_spawn`/`conway_ask` call already returned (`seq: 0` is that
  child's own first turn), or a match [`discover.md`](discover.md)'s
  `search_sessions` just returned for a session the model neither started
  nor spawned.
- `exclude: [<seq>, ...]` — sequence numbers of THIS session's own records
  to leave off the path.
- `drop_own_tail: true` — durably drop this session's own history rather
  than naming individual sequence numbers. **Off by default**: composing a
  path never silently resets a session's own ongoing conversation as a side
  effect. See "The trap this tool exists to avoid," below.

## What you see afterwards

The tool's own reply states what changed, in structural terms — never a
token guess (that estimate belongs to the backend's own admission gate,
per the operator ruling `conway_core::path::CostEstimate`'s own doc cites):
how many records were genuinely brought in from elsewhere, how many records
are now on the path, the log position the new head was written at, and
whether the change falls inside the cached portion of context (so you know
in advance whether a later sibling agent's cache is about to miss).

If the composition would leave a tool call or its result stranded — a
recalled `ToolUse` whose answering `ToolResultBlock` never made the cut,
or the reverse — the call is REFUSED, not silently patched. The refusal
names the orphan and offers the two ways to resolve it (keep both halves,
or omit both); nothing is written to the session's log until a coherent
composition is given.

## The trap this tool exists to avoid

A head's `covers_upto` marks where a session's own "live" tail starts
reading from. Composing a selection that happens to carry NONE of a
session's own records resets that marker to the very beginning — so an
earlier, deliberate exclusion can resurface with no warning at all, purely
because a later, unrelated composition didn't happen to mention any of the
session's own content. `compose_context_path` always starts from the
session's CURRENT path (which already includes its own tail), so this
cannot happen by accident: only an explicit `drop_own_tail: true`, or an
`exclude` list covering the whole tail, reaches that state — and even then,
the tool keeps just enough of the session's own history to prevent that
same content from silently reappearing on a LATER turn once new content is
appended. See `crates/conway-plugin-path/src/lib.rs`'s own module doc for
the full mechanical argument, and its test suite for the behavior pinned
end to end.
