# Plugin-declared permission modes

How an operator names a mode that is not one of conway's own three, cycles
to it, and what happens to the mode's own guarantees when its declaring
plugin comes and goes. Board item `01M0X4YDNVP7TZ0PVSRJ0388SS`; design
`docs/vision/DESIGN-permission-modes.md` §2c/§3b/§3d/§6b. Depends on
[`concepts.md`](concepts.md) for vocabulary and
[`hooks.md`](hooks.md) point 8 for the permission-policy point a declared
mode's own narrowing ultimately runs through.

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
Nothing here adds a fourth variant, and `PermissionBroker::decide` (the one
place a tool call's fate is decided) reads that same enum, unmodified, for
every session regardless of whether a declared mode is in play.

A plugin declares a mode by returning `PluginDeclaredMode { name, base }`
from `Plugin::permission_modes()` — a NAME (what the operator sees and picks
from the cycle) plus the ONE core mode it narrows. That is the whole
declaration. There is no second field for "which extra categories this mode
allows," no override list, nothing a plugin could use to make its own mode
more permissive than the `base` it names — see the next section for why
that omission is deliberate rather than an oversight.

Any *real* narrowing a plugin's mode wants — a classifier that vetoes
dangerous calls, a rule that denies a specific tool outright — is expressed
through the mechanisms that already exist for exactly that: a plugin's own
[`Plugin::permission_rules`](hooks.md) (already narrowing-only: its verdict
type has no `Allow`), and, once the general hook-registration seam
[`hooks.md`](hooks.md) point 13 tracks lands, a plugin's own `pre_tool_use`
hook. A declared mode is the NAME over that narrowing, not a second copy of
the enforcement.

## Why widening is structurally impossible, not merely rejected

`PluginDeclaredMode` carries exactly one field that anything permission-
related ever reads: `base`. `PermissionBroker::decide` never learns a
declared mode's name — only its base, via the one accessor
(`ModeCycleEntry::base`) every consumer of a declared mode goes through.
There is no field anywhere in the chain a plugin could populate with
something wider than `base` already permits, because nothing downstream
ever asks the mode anything except "what is your base."

This is the same shape two other types in this tree already use for the
identical reason: `HookPermissionVerdict` and `PluginPermissionVerdict` both
simply have no `Allow` variant — described in-tree as *"a property a plugin
cannot talk its way around."* `PluginDeclaredMode` applies that one level
up: instead of a verdict type with no `Allow` variant, it is a mode-
declaration type with no *field* that could carry one. A validation
function that rejected a widening declaration was considered and rejected —
that only proves a plugin's author remembered to write a legal manifest
this time; a type with nothing to widen proves it regardless of what any
future manifest says.

## The mode cycle: deterministic order, and what a collision does

Shift+Tab (and the `/settings` menu's `permission_mode` row) walks a cycle:
the three core modes first, always in the same order (`Prompt`, `Plan`,
`AutoAllow`), followed by every currently-installed plugin's declared modes,
sorted by name. A declared mode's position depends only on its own name —
never on which plugin happened to load first, and never on the order
plugins were installed in. With no mode-declaring plugin installed, the
cycle is exactly the three core modes, byte-identical to every build before
this capability existed.

**Two plugins declaring the identical name is a collision, and it is
handled by refusing both, never by picking one.** There is no principled
way to prefer one plugin's mode over another's same-named one, so neither
enters the cycle, and the collision is reported naming both plugins by id —
never a silent pick, and never a crash.

## The status line never softens the warning

`PermissionMode::label()` renders `AutoAllow` as the emphatic `"AUTO-ALLOW"`
on purpose — an operator who has forgotten they are in it, and believes they
are still being asked, is the failure that label exists to prevent. A
declared mode built on `AutoAllow` does not get a gentler label instead: its
display carries the base mode's own unmodified label alongside its own
name — `"auto-gated (AUTO-ALLOW)"`, never `"auto-gated"` alone and never a
paraphrase. This is checked directly, not left to convention: the display
composition is a single small function with no branch that can drop the
base's own label, and a test in `conway-runtime`'s `permission_mode` module
pins the exact rendered string for an `AutoAllow`-based declared mode.

A guard's own health — whether its classifier is actually reachable right
now, as opposed to silently down — is a separate question from which mode
is selected, and is reported the same way any plugin reports live status:
through `Plugin::status_contributions()`, rendered in the status line
alongside the mode field, never displacing it. See
`docs/vision/DESIGN-permission-modes.md` §3d for the three states that
surface distinguishes (auto / auto, gated / auto, gated, guard
unreachable) — this page does not repeat that table, only the guarantee
that the mode's own label is never the thing that goes quiet when a guard
dies.

## Uninstalling the declaring plugin

A session sitting in a declared mode when its declaring plugin is
uninstalled does not end up gating calls against a name nothing backs. The
mode a session actually enforces is always one of the closed three — a
declared mode is never anything but a display label over that real value,
so uninstalling the plugin changes nothing about what the session is
already permitted to do. What *does* change is the label: the cycle is
rebuilt without the uninstalled plugin's entry, and a session whose active
declared mode is no longer in that cycle falls back to a plain core label —
never a stale name, and never a crash. The same reconciliation answers it
whether the operator presses Shift+Tab again (landing on the first core
mode) or the uninstall is noticed passively (the display simply stops
showing the dangling name).

## Familiar, not identical

Claude Code's permission vocabulary (`default`, `acceptEdits`, `plan`,
`bypassPermissions`) is that tool's own model, and this capability
deliberately does not import it — see
[`claude-compat.md`](claude-compat.md) for what that compatibility layer
does and does not translate. Conway's three core modes plus whatever a
plugin declares should *feel* like the same gesture as switching modes
anywhere else — one key, a visible mode, a real difference in what gets
asked — without matching Claude Code's names or its semantics.

## Status: what is built, what is not

**Everything described on this page is built, unit-tested, and wired
end to end** — there is no remaining gap between the data model and the
keystroke. `Plugin::permission_modes()` (`conway-core`), the cycle-order/
collision/uninstall-reconciliation logic
(`conway_runtime::permission_mode::ModeCycle`), the broker-side bookkeeping
(`PermissionBroker::active_declared_mode`/`select_mode_cycle_entry`) that
keeps a selected declared mode's display identity separate from — and
never able to influence — the one enforcement field `PermissionBroker::
decide` actually reads, **and the startup wiring that gathers a real
plugin's declared modes and drives Shift+Tab through them**: `ConwayBuilder
::build` collects `Plugin::permission_modes()` across every installed
plugin at the last point they are reachable before `PluginRegistry`
consumes them (`crates/conway/src/builder.rs`), and
`Action::CyclePermissionMode` in the TUI resolves against that real cycle
— `Conway::cycle_permission_mode` — instead of a fixed three-way switch
(`crates/conway-cli/src/tui/app/run.rs`). Built by commit `db23a65`,
2026-08-25 ("Wire plugin-declared permission modes through to Shift+Tab
and the status line"). **2026-08-27 correction: this page previously said
the startup wiring was not yet built and that every build cycled exactly
the three core modes; both sentences were already false when read, having
gone stale the moment `db23a65` landed.**

**`conway.permissions`, the first-party plugin this whole capability was
designed for, is cancelled, 2026-08-27** (decision record
`01M128AP39WXE01BBZV4RENC4M`; see `DESIGN-permission-modes.md` §8): a local
model was not reliable enough to gate tool calls, and the failure was shown
not to be a model-size problem — it does not scale away with a bigger one.
`Plugin::hooks()` itself ([`hooks.md`](hooks.md) point 13) is now built
(commit `0a2fa76`, 2026-08-27), for an unrelated consumer (claude-compat,
board item `01M129QW0GV90QTQS6B3BY3DAR`, done) — not for a declared mode's
own classifier hook, and not for the reason design §6c gave. This page's
own mechanism — `Plugin::permission_modes()`, the cycle, the
collision/uninstall reconciliation — is unaffected by the cancellation and
remains fully available to any other plugin that wants to declare a mode.
It has no first-party plugin exercising it: with the designed-for consumer
cancelled and no replacement planned, a build with only stock plugins
installed cycles exactly the three core modes today — not because
anything is unbuilt, but because nothing has yet declared a fourth. This
is a statement about which plugins happen to be installed, not an open
implementation gap, so it names no board item.
