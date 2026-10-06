# `conway.compaction`: an ephemeral fold of old tool results

The first-party plugin that packages `docs/plugins/cookbook.md` example 2's
`CompactOldToolResultsHook` (board item `01M1YVMTDJYEFC5PKSQHDRASJX`) as an
installable, off-by-default `ContextHook`, implemented by
`crates/conway-plugin-compaction`. Depends on [`concepts.md`](concepts.md)
for vocabulary (plugin, `ContextHook`, `[plugins].install`) and
[`cookbook.md`](cookbook.md) for the worked example this crate packages.

## Read the caveat first

This plugin is named `conway.compaction`, and that name is doing less work
than it looks like it does. Copied verbatim from `docs/vision/CATALOGUE.md`,
because it is load-bearing, not decoration:

> **The caveat, stated as plainly as the cookbook states it.** This is
> **explicitly not** what most people mean by "compaction": it recomputes
> the fold every turn (nothing persists it — `LogRecord::ContextMask` has no
> producer anywhere in the tree, and the item that would have built one was
> filed and then cancelled per `docs/plugins/hooks.md` point 9's citation),
> and `INTENT.md` §3 calls compaction "the enemy, not a feature" for good
> reason: it is a lossy, unauditable summary applied to material someone was
> reasoning over. Ship it anyway, off by default, labeled honestly as the
> weaker ephemeral form — because *some* users will want it regardless of
> the house opinion, and "you install it, you chose it" is the whole point
> of `PHILOSOPHY.md` §5's default-set test. Do not let this entry read as
> "conway now has compaction" on a feature-comparison chart; it has the
> weakest version of the feature everyone else means by that word.

## What it does

On every request, `before_request` scans the assembled context for
`ToolResult`-provenance segments. It keeps the `fold_after_turns` most
recent ones exactly as they are, and replaces every older one with a single
summary segment: a mechanically truncated (80 characters) excerpt of each
folded result, joined under a short header naming how many were folded. The
summary segment's own provenance is a `SystemNote` whose `reason` names
**`conway.compaction`** and the exact count folded — the transcript never
shows an anonymous note where this plugin acted, per `INTENT.md` §8.3's
"never silent" discipline, on two independent surfaces:

- **On demand:** `/context`'s rendering of provenance reads this `reason`
  text directly, so a folded segment reads `system note: conway.compaction:
  folded N earlier tool result(s)`.
- **Live, in the TUI transcript, while a fold is actually active:** the
  same text appears as its own transcript line the first time a request
  folds anything, and again only when the fold COUNT actually changes
  (the hook recomputes and re-appends its summary segment on every
  subsequent request once the threshold is crossed, every turn, forever —
  re-announcing an unchanged count every turn would be its own kind of
  noise). An operator who never opens `/context` still sees the fold
  happen, once, exactly when it starts and whenever it grows or shrinks.

**"Turns" is a label, not a literal count.** A `ContextHook` sees the
assembled `Vec<PromptSegment>` for one request, not the underlying
`LogRecord`s a curator like `conway.trim` walks — a segment carries no turn
number of its own. This hook orders by POSITION in the assembled payload
instead: the newest `ToolResult` segments are always last, because
`ContextBuilder` appends records in the order the log wrote them. In the
common case (at most one tool call/result pair per turn), that position IS
the turn boundary, so `fold_after_turns = N` reads as "keep the last `N`
turns' tool results verbatim" — a turn that issues several tool calls at
once makes this an approximation, since several results from ONE turn can
straddle the keep/fold boundary. Stated here rather than left for a reader
to discover the hard way.

## What it does NOT do

- **No persistence.** The fold is recomputed, identically, from the full
  unfolded segment list, on every single turn — nothing is written to the
  session log. `LogRecord::ContextMask` (the durable, reversible masking
  primitive) still has no producer anywhere in this tree; this plugin does
  not become one.
- **No summarizing of user/assistant text.** Only `ToolResult`-provenance
  segments are ever folded. A `UserPrompt`, `Skill`, `AgentDef`, or
  `Assistant` segment is never touched.
- **No model call.** The summary is a mechanical, truncated excerpt of each
  folded result's own text, computed locally — never an LLM-written digest.
- **No change to the admission gate.** This plugin's only seam is
  `ContextHook::before_request`. A request that still overflows the routed
  model's window after this hook runs is refused loudly by the runtime's own
  admission gate, exactly as it would be with no compaction hook installed
  at all — folding smaller is not the same contract as fitting.

## Install it

Off by default, and NOT a member of `conway-cli`'s opinionated first-run
set. An operator opts in explicitly:

```json
{ "plugins": { "install": ["conway.compaction"] } }
```

## Configuring the fold

`fold_after_turns` defaults to **1** (the cookbook's own `keep_last: 1`) if
you never set it. Set your own value under `[plugins.config.conway.compaction]`:

```json
{
  "plugins": {
    "install": ["conway.compaction"],
    "config": { "conway.compaction": { "fold_after_turns": 3 } }
  }
}
```

- `fold_after_turns` must be a JSON integer **`>= 0`**. Unlike
  `conway.trim`'s `keep_turns`, `0` is accepted here: "fold every tool
  result, keep none verbatim" is a coherent, if aggressive, policy for this
  plugin, not a value that silently means something else.
- Any OTHER key under `conway.compaction`'s own table — a typo, a renamed
  field, a key belonging to a different plugin — is refused **by name**,
  never silently ignored.

## Uninstalled, nothing changes — with one disclosed exception

With `"conway.compaction"` absent from `[plugins].install`, no hook runs and
no tool result is ever folded.

**Its `[plugins.config.conway.compaction]` table is still validated, even
then** — the same "validate every candidate this table names, whether or
not it ends up selected" posture `conway.trim`'s own page documents for
itself (see [`trim.md`](trim.md)'s "Uninstalled, nothing changes" section):
a malformed value fails the build even for an operator who configured this
plugin before installing it.

## This is not the only way to fold context

`PHILOSOPHY.md` §6's own "What to forget when context fills" section names
the structural answer first: control what enters context at all. This
plugin is the worked, installable example of the fallback — "when you do
want condensing, it is a context hook" — for an operator who has decided the
structural answer is not enough for one session and wants the weakest,
most honestly-labeled version of the alternative instead of writing their
own hook from `cookbook.md` example 2 by hand.
