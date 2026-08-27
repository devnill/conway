# Inference-evaluated hooks: judging with a model instead of code (ABANDONED)

**STATUS: ABANDONED, 2026-08-27. This page is a retired design record, not
a plan** — nothing in the tree is being built against it, and nothing is
scheduled to be. It is kept, rather than deleted, because a materially
different follow-up may one day want the fork/spawn, cost, and recursion
reasoning below and should read it before re-deriving it from nothing. Do
not read anything past this notice as forthcoming work.

**Why.** The one concrete use this page was written for —
`docs/vision/DESIGN-permission-modes.md`'s `conway.permissions`, a
`pre_tool_use` hook that judges a tool call by calling a local model — was
tested against a 48-case corpus and failed: false-allow rates of 27.3%
(`gemma4:e4b`, 8.0B) and 12.1% (`qwen2.5:14b`, 14.8B), including the exact
case the whole design existed to catch (telling a scratch `git reset
--hard` from a real one via `cwd`), missed 100% of the time at **both**
sizes. Operator ruling, decision record `01M128AP39WXE01BBZV4RENC4M`:
*"The inference-gated permission guard is abandoned: a local model judging
arbitrary tool calls is not good enough to be an AUTO-ALLOW deny-gate, so
conway.permissions is cancelled and Plugin::hooks() is cancelled with it
for want of a consumer."* **This is not a model-size finding.** The
decision record is explicit: *"the finding is not 'the model was too
small'. Scaling does not fix this, and a future reader should not re-open
this on the theory that a bigger model would."* Do not read anything below
as awaiting a bigger model, more parameters, or a better prompt — the
failure mode this page's whole premise rests on was shown to persist
across the one variable the experiment controlled for.

**What this does, and does not, say about `Plugin::hooks()`.** The
cancellation above named `Plugin::hooks()` too, "for want of a consumer" —
and that half of the ruling was itself corrected the same day (process
finding `01M129RRA6394T6JP2WQ30A9R3`): the hook *registration* method has a
real, already-shipped consumer independent of this page — the claude-compat
translation (`crates/conway-cli/src/claude_compat_plugins.rs`), which used to
be served by `ConwayBuilder::config_mut`, a whole-config escape hatch, for
want of a narrower seam. Board item `01M129QW0GV90QTQS6B3BY3DAR` built
`Plugin::hooks()` as that narrower registration surface, moved the
claude-compat translation onto it, and removed `config_mut` entirely — that
consumer is entirely unrelated to anything on this page. **`Plugin::hooks()`
is not dead — it exists and is wired.** What is dead, for want of any
consumer at all, is a hook that reaches conway's own inference to produce
its verdict —
`run_ephemeral_turn`, a `subagent_mode` declaration, and a `hook.fork`
capability, which is everything this page describes below. See
`hooks.md` point 13 and point 14 for the normative status of each half.

**What remains genuinely open, and is not this page's business to close.**
The evidence explicitly leaves room for a *differently-scoped* proposal:
pattern rules as the actual permission gate, with a model consulted only
as an **additional** narrowing check for the residual cases pattern rules
cannot express. In the decision record's own words, that is "a materially
different proposal … not a tuning knob on the one tested here," recorded
as a follow-up and **not** a recommendation — this decision neither
authorises nor refuses it. Two spec questions also remain unanswered:
what conway's fail-closed posture actually feels like from the operator's
seat with the model server stopped mid-session, and whether the emphatic
`AUTO-ALLOW` status label misleads while a guard is silently running or has
silently died. Both need a human at a live TUI, not a corpus replay, and
the evidence says explicitly they must not be marked resolved on its
basis. They may be moot for the cancelled design; they are not answered.

---

## What this page recorded, kept for history

Everything below is the design as it stood before abandonment, unedited
except for this notice. It described a **first-class supported shape**
(per an earlier decision), not a workaround, and settled four open
questions the shape had left (how fork-vs-spawn is declared, cost and
attribution, determinism, mask production). None of it is sourced from
this abandonment — it predates the corpus test entirely.

## What they are

A hook is allowed to be evaluated **by inference** rather than by code — it
issues its own LLM call to decide, running as a subagent instead of a
function. `concepts.md`'s "Fork vs spawn, for inference-evaluated hooks"
section owns the base vocabulary; this page is the deeper how-to on top of
it. The clearest example the design corpus works through is a permission
policy that classifies a tool call from its own inference call rather than a
fixed pattern list — but the shape generalizes to any hook point that reads
text and returns a decision: a compaction hook judging what to summarize, a
context-mask hook deciding what's safe to durably exclude.

## Choosing fork or spawn

This is the author's choice to make, and it's sharper than it looks:

- **Fork** — the hook's subagent inherits the calling agent's entire
  ancestry context as an immutable prefix. Informed: the hook sees
  everything the parent saw. Expensive, in both token cost and exposure.
- **Spawn** — a clean slate. The hook's subagent gets none of the parent's
  transcript. Cheap, and *structurally* cannot leak it — there's nothing to
  leak.

Frame it as "judge with full context" versus "judge in isolation." A
permission classifier deciding whether one specific tool call should
proceed almost never needs the whole conversation; a compaction-decision
hook plausibly does, because "what's safe to summarize" is a question about
the conversation as a whole.

**State the security asymmetry plainly, because it's the reason the default
below is what it is.** An inference-evaluated hook is already reading
attacker-reachable text — the tool call, the surrounding segments, all of it
can carry indirect injection riding on content a hostile source produced.
Forking multiplies that exposure twice over: the hook's own classifier now
sees strictly more attacker-reachable text than a spawned equivalent would,
*and* the hook's own output (a deny reason, a mask) is itself a channel back
into the model's context — so a wider input widens what an attacker's
injected content can try to launder back out through the hook's own
verdict.

**The declaration surface, default, and override rule — taken from, not invented here:**

- A hook declares `subagent_mode: Fork | Spawn` **per registration**, not
  per plugin. One plugin may register a classifier needing no ancestry
  alongside a compaction hook wanting the whole conversation; a single
  manifest-wide flag would force one of them to the wrong default.
- **`Spawn` is the default.**
- `Fork` requires a separately granted **`hook.fork`** capability, following
  `subagent.spawn`'s exact shape: default off, never implied by trust,
  requested (`required_host_caps`/`optional_host_caps`) and separately
  granted.
- An operator **may refuse** a requested `hook.fork` — the hook fails to
  register if declared required, or is skipped with a status change if
  optional.
- An operator **may never force `Fork` onto a hook that declared `Spawn`**,
  and the runtime may never silently downgrade a declared `Fork` to `Spawn`
  and run it anyway. A hook that structurally needs the full conversation,
  given a spawned view instead, would answer a different question with
  apparent confidence rather than failing loudly — the same "never guessed
  at" standard the rest of this architecture holds permission decisions to.

**The precedent this reuses already ships**, outside any hook:
`crates/conway/src/intent.rs`'s classifier for the TUI's natural-language
`/fork`/`/spawn` command runs as an ephemeral, zero-tool
`SubagentMode::Spawn` child (`intent.rs:250`) — a judge with no ancestry and
no tool access, deciding one narrow question from a prompt alone. It is not
a hook and predates this design; it's cited because it's the identical shape
`Spawn` mode would reuse once a hook registration surface exists to attach
it to.

## Cost and bounds

An inference hook issues a **real LLM call**. Three things to know about
what that costs and who pays for it:

- **Bounding.** The same declared-`timeout_ms` clamp that already governs
  every inference-evaluated hook design applies uniformly — a 60 s default,
  operator-raisable to a configured maximum. This was never
  permission-specific; it's a property of issuing an LLM call from inside a
  hook, which an earlier decision confirmed applies to
  every inference-evaluated hook kind, not only a permission classifier.
- **Attribution — whose budget it spends.** For an in-process hook running
  the zero-tool judge shape above, the spend lands under the hook's own
  ephemeral child session, never folded into the calling agent's own
  `session_usage` (the same reason that accessor already excludes an
  inherited fork prefix — "the parent's own prior conversation, not this
  agent's," one level up: a hook's own inference call is not the calling
  agent's conversation either). The decision requires every
  inference-evaluated hook's spawn to carry a stable **role tag** (the
  `role: Some("guard")` shape `intent.rs`'s own precedent already uses,
  generalized) so "what did my guardrails cost me" is answerable separately
  from "what did my agents cost me," by grouping cost rollups on that tag.
  For a **remote** hook, there is and will be no conway-side accounting at
  all — the plugin spends its own credentials.
- **A decision-bearing call cannot stall the session by emitting progress
  forever, and this is the sharpest finding the settling decision made.**
  The general rule elsewhere in this architecture is that a call's deadline
  resets on progress notifications (`hooks.md`'s failure-semantics section).
  Applied naively to a decision-bearing hook, that rule has a hole: a hook
  emitting a liveness ping every couple of seconds while never actually
  deciding would be certified *healthy* forever — no timeout ever fires, no
  `on_failure` ever runs, and every tool call in the session stalls at that
  hook indefinitely, because it produces neither an allow nor a failure,
  just nothing, on and on. **Decision: decision-bearing calls are excluded
  from the progress-reset rule entirely.** A decision call's deadline is the
  clamped `timeout_ms`, flat, never extended by a progress notification on
  that call's own token — a hook may still emit progress (useful for a
  future liveness display), it simply carries no deadline-reset semantics
  for that call. The rejected alternative — a separate, smaller
  `max_total` for decision calls — was considered and dropped: it only
  delays the same failure to a later, arbitrarily-chosen point, and the
  smallest `max_total` that's actually correct for this call class already
  equals `timeout_ms`, so excluding progress-reset produces the right
  behavior without inventing a second number an operator has to reason
  about.

## The recursion rule

**A hook's own machinery does not re-enter that hook.** Stated generally:
no hook of any kind is invoked for a call made by an agent that hook's own
machinery spawned. Without this, an inference hook that (mistakenly or by
design) gives its judge agent tool access produces a child whose own tool
calls re-enter the same hook, which spawns another judge, which spawns
another child — a loop bounded by nothing but each individual agent's own
step budget, not by anything that recognizes the chain.

**Why the zero-tool judge pattern is the mechanism that actually closes
this, not merely a stylistic choice.** The reference shape — `ephemeral:
true`, `tools: Only(vec![])`, `keep_alive: false` — closes the loop two
ways at once: zero tools means the judge produces zero tool-call proposals,
so there's nothing to re-enter the hook chain with in the first place; and
`SubagentHost::start` is a host callback the judge's spawn goes through, not
a tool call, so the spawn itself never re-enters either. `intent.rs:254` is
the shipped precedent for this exact shape, even though — worth repeating —
it isn't itself a hook.

**But the zero-tool convention is a convention, not an enforced boundary,
which is exactly why an explicit invariant exists on top of it.** A policy
that spawns its judge with `tools: All` instead would produce a child whose
calls genuinely do re-enter `decide` — nothing about the zero-tool shape
*prevents* that, it just happens to be what every current instance chooses.
The settled invariant, checked on `agent_path` (already in scope wherever a
hook decision is made): **no hook evaluation happens for a call made by an
agent that hook's own spawn produced.** It's implemented once, generically,
keyed on `agent_path` the same way for every hook kind — not reimplemented
per kind, which is exactly the kind of duplication this project's own
sanitizer history shows drifts apart over time.

## When NOT to use one

An inference hook costs a real model call and adds real latency to every
decision it touches — everything in "Cost and bounds" above is a cost paid
on the path of a tool call actually running, not background work. **A
static rule that expresses the same intent is faster, cheaper, deterministic,
and works when the plugin is dead.** A pattern rule in `permissions.json`
(`hooks.md` point 6) that denies a specific command prefix costs nothing at
decision time, never times out, and keeps working if every plugin process on
the machine has crashed — an inference hook, by construction, cannot make
either of those last two claims.

Reach for inference only when the decision genuinely cannot be expressed
declaratively — when the judgment depends on reading and understanding
content, not matching a pattern against it. "Deny any `bash` call containing
`rm -rf /`" is a pattern match; write it as one. "Deny a commit message that
plausibly leaks a customer's data" is a judgment; that's the shape inference
hooks exist for.

## Where to go next

[`concepts.md`](concepts.md)'s "Fork vs spawn" section — the base
vocabulary this page assumes and builds on, and the value-class boundary
governing what any hook (inference-evaluated or not) may do to what it's
handed. [`hooks.md`](hooks.md) point 14 — the normative status row for
fork/spawn declaration, and point 8 for the composed permission-policy point
an inference hook would most often attach to. [`authoring.md`](
authoring.md) — the one path that's actually buildable today, and where
"Testing your hook"/"Debugging" apply just as much to a future
inference-evaluated hook as to the in-process ones you can write now.
