# Plugin-declared permission modes (ABANDONED)

**STATUS: ABANDONED, 2026-09-02. This page is a retired design record, not
a plan** — nothing in the tree is being built against it, and nothing is
scheduled to be. It is kept, rather than deleted, because a materially
different follow-up may one day want the mode-cycle reasoning below and
should read it before re-deriving it from nothing. Do not read anything
past this notice as forthcoming work.

**Why.** The mechanism this page describes — a plugin naming a mode over
one of the three closed core modes, cycled by Shift+Tab alongside them —
was fully built and wired end to end (board item
`01M0X4YDNVP7TZ0PVSRJ0388SS`) for exactly one designed-for consumer:
`conway.permissions`, a `pre_tool_use` hook that judged a tool call by
calling a local model. That consumer was cancelled outright, decision
record `01M128AP39WXE01BBZV4RENC4M` (2026-08-27; see
`docs/vision/DESIGN-permission-modes.md`): tested against a 48-case corpus,
a local model missed the paradigm case — telling a scratch `git reset
--hard` from a real one via `cwd` — 100% of the time at both tested sizes,
and the finding was explicit that scaling does not fix it. With the one
consumer gone and grep over the whole workspace finding no other producer,
ever, the mechanism itself was removed rather than left standing for a
consumer that never arrived — the same "a hook that silently never runs is
worse than an absent one" reasoning `conway_core::ports::plugin::Plugin`'s
own trait doc already applies to its removed `on_init` method, applied one
level up: not unreachable code, but reachable code with categorically no
reachable caller. Operator ruling, harness gap review 2026-09-01 finding 9:
"delete the mode-declaration mechanism." See that trait's own doc for the
exact removal note (what it was, why it went, what to build instead if a
producer ever appears).

**What this does NOT mean.** The three closed core modes
(`Prompt`/`Plan`/`AutoAllow`), Shift+Tab, and `PermissionBroker::decide`
are entirely untouched — every session cycles the exact same three modes,
in the exact same order, through the exact same field `decide()` has
always read. Only the layer that let a plugin name a fourth, narrower
display identity over one of them is gone.

---

## What this page recorded, kept for history

Everything below is the design as it stood before abandonment, unedited
except for this notice and the renaming of two removed Rust identifiers to
plain prose (so this page does not itself show up as a live reference to
code that no longer exists). It described a **first-class supported
shape**, not a workaround, and was built, tested, and wired to a real
keystroke before its one consumer was cancelled.

## The framing this page exists to state plainly

**An inference-gated mode is not a safer mode.** It is full permission,
filtered by a model — *less* safe than deciding each call by hand, whether
that decision is made once or turned into a standing rule. Its
justification is not safety. It is that **approval fatigue at volume
produces bad judgement**, and a filter that catches the genuinely dangerous
calls beats an operator who has stopped reading the prompts.

If you take one thing from this page, take that sentence. A mode named
`auto-gated` (or whatever a guard plugin calls itself) is `AutoAllow` with a
classifier watching — not `AutoAllow` with a safety net under it. Every tool
call still proceeds automatically. The classifier is a filter that can veto
a call before it runs, not a review that has to approve one before it does.
Those are opposite defaults, and conflating them is exactly the mistake an
earlier draft of the design behind this page made and then corrected: it
argued a gated auto mode "carries materially less risk" and concluded its
warning was too harsh. It does not, and it is not. Nothing described below
should ever present a declared `AutoAllow`-based mode with a softer label
than bare `AUTO-ALLOW` — see "The status line never softens the warning"
below for the mechanical guarantee behind that sentence.

## The shape: a name plus a narrowing, on a closed base

[`PermissionMode`](../vision/DESIGN-permission-modes.md) stays exactly what
it always was — a closed, three-variant enum (`Prompt | Plan | AutoAllow`).
Nothing here added a fourth variant, and `PermissionBroker::decide` (the one
place a tool call's fate is decided) read that same enum, unmodified, for
every session regardless of whether a declared mode was in play.

A plugin declared a mode by returning a small struct pairing a NAME (what
the operator sees and picks from the cycle) with the ONE core mode it
narrowed. That was the whole declaration. There was no second field for
"which extra categories this mode allows," no override list, nothing a
plugin could use to make its own mode more permissive than the base it
named — see the next section for why that omission was deliberate rather
than an oversight.

Any *real* narrowing a plugin's mode wanted — a classifier that vetoes
dangerous calls, a rule that denies a specific tool outright — was expressed
through the mechanisms that already exist for exactly that: a plugin's own
[`Plugin::permission_rules`](hooks.md) (already narrowing-only: its verdict
type has no `Allow`), and a plugin's own `pre_tool_use` hook via
[`Plugin::hooks`](hooks.md). A declared mode was the NAME over that
narrowing, not a second copy of the enforcement.

## Why widening was structurally impossible, not merely rejected

The declared-mode type carried exactly one field that anything permission-
related ever read: its base. `PermissionBroker::decide` never learned a
declared mode's name — only its base, via the one accessor every consumer
of a declared mode went through. There was no field anywhere in the chain a
plugin could populate with something wider than the base already permitted,
because nothing downstream ever asked the mode anything except "what is
your base."

This was the same shape two other types in this tree already use for the
identical reason: `HookPermissionVerdict` and `PluginPermissionVerdict` both
simply have no `Allow` variant — described in-tree as *"a property a plugin
cannot talk its way around."* The declared-mode type applied that one level
up: instead of a verdict type with no `Allow` variant, it was a mode-
declaration type with no *field* that could carry one. A validation
function that rejected a widening declaration was considered and rejected —
that only proves a plugin's author remembered to write a legal manifest
this time; a type with nothing to widen proves it regardless of what any
future manifest says.

## The mode cycle: deterministic order, and what a collision did

Shift+Tab (and the `/settings` menu's `permission_mode` row) walked a
cycle: the three core modes first, always in the same order (`Prompt`,
`Plan`, `AutoAllow`), followed by every currently-installed plugin's
declared modes, sorted by name. A declared mode's position depended only
on its own name — never on which plugin happened to load first, and never
on the order plugins were installed in. With no mode-declaring plugin
installed, the cycle was exactly the three core modes, byte-identical to
every build before this capability existed, and byte-identical to every
build after its removal.

**Two plugins declaring the identical name was a collision, handled by
refusing both, never by picking one.** There was no principled way to
prefer one plugin's mode over another's same-named one, so neither entered
the cycle, and the collision was reported naming both plugins by id —
never a silent pick, and never a crash.

## The status line never softened the warning

`PermissionMode::label()` renders `AutoAllow` as the emphatic `"AUTO-ALLOW"`
on purpose — an operator who has forgotten they are in it, and believes they
are still being asked, is the failure that label exists to prevent. A
declared mode built on `AutoAllow` did not get a gentler label instead: its
display carried the base mode's own unmodified label alongside its own
name — `"auto-gated (AUTO-ALLOW)"`, never `"auto-gated"` alone and never a
paraphrase. This was checked directly, not left to convention: the display
composition was a single small function with no branch that could drop the
base's own label, pinned by a unit test for the exact rendered string of an
`AutoAllow`-based declared mode.

A guard's own health — whether its classifier is actually reachable right
now, as opposed to silently down — was a separate question from which mode
is selected, and was reported the same way any plugin reports live status:
through `Plugin::status_contributions()`, rendered in the status line
alongside the mode field, never displacing it. See
`docs/vision/DESIGN-permission-modes.md` §3d for the three states that
surface distinguishes (auto / auto, gated / auto, gated, guard
unreachable) — this page never repeated that table, only the guarantee
that the mode's own label was never the thing that went quiet when a guard
died.

## Uninstalling the declaring plugin

A session sitting in a declared mode when its declaring plugin was
uninstalled did not end up gating calls against a name nothing backed. The
mode a session actually enforced was always one of the closed three — a
declared mode was never anything but a display label over that real value,
so uninstalling the plugin changed nothing about what the session was
already permitted to do. What *did* change was the label: the cycle was
rebuilt without the uninstalled plugin's entry, and a session whose active
declared mode was no longer in that cycle fell back to a plain core label —
never a stale name, and never a crash. The same reconciliation answered it
whether the operator pressed Shift+Tab again (landing on the first core
mode) or the uninstall was noticed passively (the display simply stopped
showing the dangling name).

## Familiar, not identical

Claude Code's permission vocabulary (`default`, `acceptEdits`, `plan`,
`bypassPermissions`) is that tool's own model, and this capability
deliberately did not import it — see
[`claude-compat.md`](claude-compat.md) for what that compatibility layer
does and does not translate. Conway's three core modes plus whatever a
plugin declared were meant to *feel* like the same gesture as switching
modes anywhere else — one key, a visible mode, a real difference in what
gets asked — without matching Claude Code's names or its semantics.

## Status: built, tested, wired, then removed

**Everything described above was built, unit-tested, and wired end to
end** — there was no remaining gap between the data model and the
keystroke by the time it was removed. The core-mode-enum-stays-closed
mechanism (`conway-core`), the cycle-order/collision/uninstall-
reconciliation logic, and the broker-side bookkeeping that kept a selected
declared mode's display identity separate from — and never able to
influence — the one enforcement field `PermissionBroker::decide` actually
reads, were all real and tested, alongside the startup wiring that
gathered a real plugin's declared modes and drove Shift+Tab through them.
Built by commit `db23a65`, 2026-08-25.

**`conway.permissions`, the first-party plugin this whole capability was
designed for, was cancelled, 2026-08-27** (decision record
`01M128AP39WXE01BBZV4RENC4M`; see `DESIGN-permission-modes.md` §8): a local
model was not reliable enough to gate tool calls, and the failure was shown
not to be a model-size problem — it does not scale away with a bigger one.
With the one designed-for consumer cancelled and no other plugin, ever,
declaring a mode, the mechanism itself was removed on 2026-09-02 (operator
ruling, harness gap review 2026-09-01 finding 9) rather than kept standing
for a consumer that never arrived. `Plugin::hooks()` remains fully built
and wired — for the unrelated, already-shipped claude-compat consumer
(board item `01M129QW0GV90QTQS6B3BY3DAR`), not for a declared mode's own
classifier hook, and not for the reason design §6c gave.

If a plugin ever again wants to contribute a NAME over a core mode, the
narrowing itself still has a home that was never removed:
`Plugin::permission_rules` today, and a plugin's own `pre_tool_use` hook via
`Plugin::hooks`. A resurrected version of this mechanism should not be
built speculatively — it should be built against a real, shipping consumer
that needs the display-name layer specifically, not merely the narrowing.
