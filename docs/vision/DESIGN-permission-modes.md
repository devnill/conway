# Design: inference-gated permission modes

**Written 2026-08-25 against the tree at `4dcf79b`, version 0.9.0. Context:
[`INTENT.md`](INTENT.md) §7a/§7b, `docs/plugins/inference-hooks.md`,
[`DESIGN-plugin-dependencies.md`](DESIGN-plugin-dependencies.md).**

> **This is a design record, not a plan.** Per `INTENT.md` §8.8: a design
> document says what a feature *predicts* it will need. §7 names what would
> falsify this one.

---

## 0. The finding that reframes the whole thing

**The enforcement point already exists, and the capability already works in
configuration alone.**

`crates/conway-cli/src/main.rs:233` calls `builder.with_default_hook_runner()`,
so the shipped binary installs `ProcessHookRunner`. `PermissionBroker::decide`
invokes every enabled `pre_tool_use` hook at the **same tier as
`deny_matches`** — before the mode gate, the prompt-pattern step, the cache,
pattern allows, and `AutoAllow`. The broker's own comment states the
consequence: *"A denying hook beats every one of those, regardless of mode."*

The hook receives everything a classifier needs:

```json
{ "tool": …, "category": …, "arguments": …, "rendered": …,
  "agent_id": …, "agent_path": …, "session": …, "cwd": … }
```

— including the full argument object and the rendered command string. It
answers with `HookPermissionVerdict`, which is `NoOpinion | Deny { reason }`
and **has no `Allow` variant**.

So *auto mode with a local model vetoing dangerous calls* is expressible
today, with no code: `PermissionMode::AutoAllow` plus one
`[hooks].rules[]` entry on `pre_tool_use` whose command shells out to a
local model and returns a deny verdict. **Narrowing-only is the correct
algebra for it** — an inference call can never widen what the operator
authorised, which is the property that makes trusting a small local model
with this job reasonable at all.

**What this design is for, therefore, is not the mechanism.** It is the
difference between that shell script and a plugin: where the prompt lives,
how the model gets chosen, what stops the guard reaching a cloud provider,
what the status line says while it runs, and how the operator switches
modes.

---

## 1. What exists, precisely

| Piece | State |
| --- | --- |
| `PermissionMode` | `Prompt \| Plan \| AutoAllow` — closed enum, `conway-core/src/permission_mode.rs:39` |
| `Action::CyclePermissionMode` | exists, cycles all three, reachable **only** from a `/settings` menu row |
| `pre_tool_use` dispatch | live in the shipped binary, fail-closed, pre-everything |
| Hook payload | tool, category, arguments, rendered, agent, session, cwd |
| Hook verdict | `NoOpinion \| Deny { reason }` — narrowing only |
| Roles and chains | `[roles.<alias>].chain = ["backend/model"]`, first-eligible-wins |
| Ephemeral inference | `intent.rs::classify` — zero-tool `Spawn`, role-gated, purged; `Runtime::run_ephemeral_turn` is the reusable primitive |

`intent.rs` is the shape an inference guard would reuse: a classifier that
answers one narrow question from a prompt alone, with no tools and no
ancestry, gated on `[roles.intent]` being configured at all. It predates
any hook design and is cited by `docs/plugins/inference-hooks.md` as the
precedent the `Spawn` default would reuse.

## 2. What is missing, and which of it matters

**2a. `Plugin` has no `hooks()` method.** `ConwayBuilder` builds
`pre_tool_use_specs` exclusively from `config.hooks.rules[]` — each a
`command: Vec<String>` to spawn (`builder.rs:1740-1759`). **A plugin cannot
register a hook at all.** This is the single gap between "a shell script an
operator wires up" and "a plugin they enable."
`docs/plugins/hooks.md` point 13 has tracked it as the missing registration
surface; `docs/plugins/inference-hooks.md` is written entirely against it
and opens by admitting it has no code behind it.

**2b. A hook cannot use conway's own inference.** A spawned process gets a
JSON payload on stdin and nothing else — no handle to the router, the
backends, or `run_ephemeral_turn`. A script wanting to ask a model must
carry its own HTTP client, its own endpoint config, and its own model name,
**duplicating routing that already exists** and drifting from it silently.
This is the concrete cost of 2a, and the reason "make it a plugin" is a
capability question rather than a packaging preference.

**2c. `PermissionMode` is closed, so a gated mode cannot be named.**
"AutoAllow plus a guard" is not a mode; it is a mode and a coincidence of
configuration. This has a consequence sharper than taxonomy — see §3.

**2d. There is no keybinding registry.** `handle_key` (`tui/input.rs:156`)
is a hardcoded guard chain followed by a `match` on `Mode`. `BackTab`
(Shift+Tab) is **entirely unbound** — grep returns nothing. Binding it is a
small edit. Making binding *pluggable* is a different and much larger one,
and nothing in the operator's stated need requires the second.

**2e. Backends have no locality.** `BackendEntry` is
`{kind, api_key, api_key_env, base_url, dialect, stream_tools, #[serde(flatten)] extra}`.
Nothing distinguishes `http://localhost:11434/v1` from `api.openai.com`
except the string. Note the `extra` catch-all: a `local: true` key would be
**silently accepted into `extra` today** with no schema change and no
meaning — accepted and ignored, which is worse than rejected.

---

## 3. Two hazards this design exists to name

### 3a. Fail-closed plus a local model is a session that bricks

`pre_tool_use_hook_denial`'s posture is **fail-closed by deliberate
design**: a missing script, a timeout, or stdout that fails to parse is
treated as a denial. The broker's doc is explicit that this inherits from
the runner rather than being a second check layered on top.

That is correct for a hand-written policy script. **Applied to a guard
backed by a local model, it means every tool call is denied whenever Ollama
is not running** — and the failure presents as the agent being unable to do
anything, with a denial reason per call, rather than as "your guard is
down."

This is not an argument for fail-open; a permission guard that fails open is
not a guard. It is an argument that **the guard's own availability is a
distinct state from its verdict**, and the operator must be able to tell the
two apart. A guard that cannot reach its model should say so once, loudly,
and — this is the open question, §6a — either stop the session or fall back
to `Prompt`, which is the mode that asks rather than the mode that denies.

### 3b. The status line would lie by omission

`PermissionMode::label` returns `"AUTO-ALLOW"`, and its own doc explains
why the label is emphatic: *"An operator who has forgotten they are in it,
and believes they are still being asked, is the failure this mode most needs
to avoid."*

**What an inference-gated mode actually is — operator framing, 2026-08-25,
and it corrects an earlier draft of this section.** It is **full permission,
filtered by a model.** It is *less* safe than deciding each call by hand,
whether that decision is made once or turned into a standing rule. Its
justification is not safety — it is that **approval fatigue at volume
produces bad judgement**, and a filter that catches the genuinely dangerous
calls beats a human who has stopped reading the prompts.

That framing matters because an earlier draft had this backwards. It said a
gated mode "carries materially less risk than bare `AutoAllow`" and
concluded the operator was seeing "a warning harsher than their situation."
**Wrong.** Gated auto is still auto: every call proceeds unless the model
objects. `AUTO-ALLOW`'s emphatic label is *appropriate* for it, and softening
that warning because a classifier is running would be exactly the
false reassurance `PermissionMode::label`'s own doc was written to prevent.

**So the defect is narrower than the earlier draft claimed, and entirely on
one side:** a live guard and a dead guard look identical. An operator whose
model server died is in bare `AutoAllow` — genuinely unfiltered, full
permission, nothing vetting anything — and the status line says exactly what
it said five minutes earlier.

**The status line must distinguish "auto" from "auto, gated" from "auto,
gated, guard unreachable."**

### 3c. Resolving 3a: the root is a type, and `on_failure` is already designed

**`pre_tool_use_hook_denial` returns `Option<String>` — a verdict and an
outage are literally the same value.** The rendered text differs (the
failure branch appends "fail-closed"), but nothing structured does:
`decide` emits `PermissionDecisionKind::Denied` for both, identically.

That is why 3a felt like an unanswerable policy question. **You cannot set a
policy on a distinction the type cannot express.** The fix is to express it,
and the vocabulary for doing so is already specified rather than needing
invention:

- `docs/plugins/hooks.md`'s own status table, point 8:
  *"Design only: `on_failure`, default `Deny` — **never `Allow`**"*, and
  *"an unparseable verdict is treated as `on_failure`, never guessed at."*
- `Prompt` is **already a legitimate narrowing verdict** in this same
  system: `PluginPermissionVerdict` is `Deny | Prompt | Abstain`, where
  `Prompt` forces the operator's gate. It is not a widening, and the
  operator's own `Deny` and plan-mode refusal still outrank it.

So the resolution needs no new concept:

> **`on_failure: Deny | Prompt`, declared per hook registration, defaulting
> to `Deny`, never `Allow`.**

- **Default `Deny` means today's behaviour is unchanged** for every
  operator-authored `[hooks].rules[]` entry. A broken policy script still
  fails closed, which is right: the operator wrote it, and its breakage is
  theirs.
- **A guard declares `Prompt`.** A guard is a *narrowing on top of a mode*;
  when the narrowing is unavailable, the safe resting state is the mode that
  **asks** — which is also conway's own default mode. The operator ends up
  exactly where they would be had they never enabled the guard.
- **`Allow` remains impossible**, so no failure path can ever widen what the
  operator authorised. This is the property that makes the whole arrangement
  safe to reason about.

**And the outage must be legible as an outage**, not as a stream of per-call
denials or per-call prompts. That is 3d's job.

### 3d. Resolving 3b: the type for this already exists too, and is unrendered

`PluginStatusContribution` is `{ key, status: ResultStatus, value }`. **The
`status` field already expresses healthy-versus-failed** — it was built for
exactly this shape of thing. A guard can push `{ key: "guard", status, value }`
and cover all three of §3b's states with no new mechanism and **no fourth
`PermissionMode` variant**:

| State | Contribution |
| --- | --- |
| auto | *(none — the plugin is not installed)* |
| auto, gated | `status: completed`, value naming the guard model |
| auto, gated, guard unreachable | `status: failed`, value naming why |

**A fourth enum variant is the tempting move and the wrong one.**
`PermissionMode` is core, closed, and `allows_category` matches on it — and
a mode cannot express a *failure* state without inventing a mode for a
failure, which is a category error. Composition beside the mode expresses
all three; a widened enum expresses two and lies about the third.

**The one blocker is real and is not this design's fault.**
`Conway::plugin_status_contributions()` collects and exposes contributions
on the facade, and **the TUI status view never reads them**
([`DESIGN-plugin-dependencies.md`](DESIGN-plugin-dependencies.md) §1
documents this as a live built-but-unreachable defect). The mitigation for
3b is therefore *rendering a thing that already exists* — small,
independent, and blocked on nothing.

---

## 4. Local-only routing

The operator's requirement is that the guard's inference **never leaves the
machine**. Three shapes, in ascending cost:

- **Convention.** Ship a documented `[roles.permission-guard]` chain
  pointing at a local backend. Uses only what exists. Nothing prevents a
  misconfiguration — or a chain fallthrough on failure — from sending the
  tool call, its arguments, and the cwd to a cloud provider.
- **Enforced by the plugin.** The guard inspects its resolved chain and
  refuses to run if a candidate is not local, falling back per §3a rather
  than silently reaching outward. Needs a definition of "local" — realistically
  a URL heuristic, which is a heuristic.
- **Backends declare locality.** A typed property on `BackendEntry`, making
  "local" a checkable fact rather than a naming convention. Reusable by
  every future privacy-sensitive inference, and it makes *"which of my
  backends can see my file contents"* answerable at all — a question this
  tree currently cannot answer.

**Recommendation: backends declare locality.** The plugin-side heuristic and
the typed property cost about the same to build; only one of them is true
afterwards. And §2e's `extra` catch-all means the untyped version is
*already* silently accepted, so leaving it unmodelled is the option that
degrades quietly.

Note this is a **defence in depth** question, not a correctness one: the
narrowing-only verdict means a compromised or wrong guard cannot widen
permissions. What leaks is the *prompt* — the command, the arguments, the
paths — which is exactly the material the operator wants kept local.

---

## 5. What the plugin is, given all of the above

`conway.permissions`, an opinionated first-party bundled plugin
([`DESIGN-plugin-dependencies.md`](DESIGN-plugin-dependencies.md) §0's
ruling 2 — bundled liberally, enabled never):

- **Registers a `pre_tool_use` hook** (needs §2a) that classifies via
  conway's own inference (needs §2b), against a configured guard role
  constrained to local backends (§4).
- **Ships a rigid default prompt, operator-overridable** — which needs
  per-plugin operator configuration, blocked on `[S1.5]`, already gated in
  `DESIGN-plugin-dependencies.md` §6.
- **Declares an `optional` dependency on `conway.ui`** for interactive mode
  selection — optional under §4a's criterion, because without a UI the
  classifier still runs and the gate still narrows; only the picker is lost.
- **Binds Shift+Tab** to mode cycling (§2d).

**The dependency ordering is real, not ceremonial**: this plugin cannot be
built as a plugin at all until `Plugin::hooks()` exists, and cannot be
configured until `[S1.5]` is ruled.

---

## 6. Open

**6a — RESOLVED, see §3c. What remains open is narrow:** may an operator
override a plugin's declared `on_failure`? A plugin declaring `Prompt` is
declaring its own degradation; an operator wanting strictness may
legitimately want `Deny` instead. Since `on_failure` can never be `Allow`,
an override in either direction stays within the narrowing-only envelope, so
this is a UX question rather than a safety one.

**6b — RESOLVED, see §3d.** The guard badge belongs in the **status line**,
beside the mode, because that is where the mode already lives and a badge
somewhere else answers a question nobody asked there. Nothing further open.

**6c. Whether `Plugin::hooks()` should be this item's job at all.** It is
the registration surface `docs/plugins/hooks.md` point 13 has tracked for a
long time and that `inference-hooks.md` is written entirely against. Doing
it *for* the permissions plugin risks shaping a general seam around one
consumer; doing it separately risks building it against no consumer, which
is how this tree gets built-but-unreachable capability. **They should be one
item's design and two items' delivery**, with the general seam landing first
and the plugin as its first caller.

**6d. Prompt rigidity.** The operator's stated want is a *fairly rigid*
default prompt that is nonetheless configurable. Rigid how — a fixed
scaffold with an operator-supplied policy paragraph slotted in, or a wholly
replaceable string? The first is safer against prompt injection reaching the
classifier; the second is honest about the operator's authority over their
own machine. Unresolved.

---

## 7. What would falsify this

- **The shell-script version turns out to be pleasant enough.** If an
  operator runs `AutoAllow` plus a hand-wired hook for a week and wants
  nothing more, §5's plugin is packaging, and `Plugin::hooks()` should be
  justified by a different consumer.
- **A small local model is not good enough at the classification.** The
  entire design assumes a ~4B-class local model can usefully distinguish
  dangerous from routine tool calls with the payload it gets. If it cannot,
  no amount of plumbing helps, and the honest answer is pattern rules.
  **This is testable today, before any of the code below is written** — and
  it should be.
- **Fail-closed turns out to be right after all.** If §6a's `Prompt`
  fallback produces a session that nags constantly whenever the model is
  slow, the bricking failure may be the more honest one.
- **Locality cannot be determined usefully.** If "is this backend local" has
  no answer better than a URL heuristic, §4's recommendation collapses back
  to the plugin-side check it was preferred over.

---

## 8. Revisions

Corrections are appended here dated, never absorbed upward.

**2026-08-25 — §3a and §3b resolved in place (new §3c, §3d); §6a and §6b
narrowed to what actually remains open.** Operator-raised, against the
same-day first draft, which had deferred both hazards to an operator ruling.

**Both were already answered in the design corpus, and the first draft
failed to look.** §3a read as a policy question only because
`pre_tool_use_hook_denial` returns `Option<String>` for both a verdict and
an outage — a distinction the type cannot express, so no policy could be
set on it. `docs/plugins/hooks.md` point 8 had already specified
`on_failure` with a `Deny` default and an explicit `never Allow`, and
`PluginPermissionVerdict` already carried `Prompt` as a legitimate narrowing
verdict. §3b likewise: `PluginStatusContribution::status` already expresses
healthy-versus-failed, and the only obstacle is that nothing renders it.

**The correction is not that the recommendations changed** — both landed
where the first draft weakly guessed. It is that they were presented as
open questions for the operator when the vocabulary to close them already
existed and was reachable by reading. Deferring a decision that is already
made is the same defect as leaving a stale limitation note: it costs the
next reader a round-trip to re-derive something the tree already knew.

**2026-08-25 — §3b's framing of what a gated mode *is* was wrong, and is
corrected in place. §6b closed.** Operator-raised.

The draft argued a gated auto mode "carries materially less risk than bare
`AutoAllow`" and therefore that its emphatic warning was too harsh. The
operator's framing is the correct one: **an inference-gated mode is full
permission filtered by a model. It is less safe than deciding calls by hand
— and its justification is approval fatigue, not safety.** A filter that
catches the dangerous calls beats an operator who has stopped reading the
prompts, and that argument stands on its own without pretending the mode is
safer than it is.

The consequence is that §3b's defect is **half what the draft claimed**. The
warning is appropriate and stays. What is genuinely broken is only that a
live guard and a dead guard are indistinguishable — and an operator whose
guard died is in bare `AutoAllow` with no indication of it. §3d's three-state
contribution still answers that; the reasoning behind it is now honest.

**Also closed: §6b's remaining question.** The badge goes in the status line,
beside the mode.

---

## 9. Standing rulings recorded elsewhere

Two settings-migration questions this page's neighbours raised, ruled
2026-08-25, recorded here so they are not re-opened:

- **A global `env` config key: declined.** It misaligns with conway. The
  explicit-`env`-threading discipline (`ae318f7`, and
  `crates/conway/tests/config_isolation_guard.rs`, which exists because the
  ambient-environment hazard already broke a suite) is the position; a
  global env-injection key would reintroduce what that work removed.
- **A `SessionEnd` hook event: declined**, and not a candidate to grow. The
  earlier "file an item if the absence hurts" framing is withdrawn — this is
  settled, not deferred.
