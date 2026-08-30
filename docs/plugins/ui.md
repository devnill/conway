# `conway.ui`: ask the operator a question, directly or from another plugin

A standalone operator-facing feature (board item `01M19NH39AE2D5AMJK0RZRQY86`,
operator decision `01M19NF1C8E8CA8Y3X653Q3R23`) that also publishes the
`ui.form` capability over Edge B (board item `01M0WWPA70E8YAAN981EK10D3D`,
`docs/vision/DESIGN-plugin-dependencies.md` §0/§2/§7a), installed by
`crates/conway-plugin-ui`. Depends on [`concepts.md`](concepts.md) for
vocabulary and on [`hooks.md`](hooks.md) point 21 for the
`ToolCtx::capabilities`/`CapabilityCallHandle` contract the capability half
answers on.

**The licensing consumer is the model, not another plugin.** The operator's
own ruling: *"conway.ui should work as a standalone feature, making the
consumer rule moot. I need to be able to prompt a model to be able to
interact with me in an interview format."* The path is model → tool →
operator: `ask_question` is the tool a model calls directly to interview
you. `ui.form` still exists — another installed plugin can call into it too
— but it is no longer the reason this plugin ships.

## What this is, in one sentence

One declarative shape, reachable two ways: a prompt and a fixed, ordered
list of options in; one selected option back — the AskUserQuestion analogue
this item's own spec names, a blocking call with exactly one answerer and
one answer (the *pull* shape `docs/vision/DESIGN-plugin-dependencies.md` §7c
contrasts with `conway.statusline`'s *push*). `ask_question` is the tool a
model calls; `ui.form` (published at `1.0.0`) is the identical shape another
plugin's own code calls into over Edge B.

## Why this exists

Before board item `01M0WWPA70E8YAAN981EK10D3D`, the only plugin → screen
path was `PluginStatusContribution` — a fixed `{ key, status, value }`
triple, one widget, no composition (that design page's §1). That item built
`ui.form` as toolkit infrastructure, on the theory that a feature plugin
would eventually call into it. **The operator's own ruling replaced that
theory with a standing need**: rather than wait for a plugin-to-plugin
consumer to show up, `conway.ui` now answers the model directly — an
operator who wants a model to run an interview, not just a chat, has
somewhere for that model to put the question.

**Built narrow, deliberately, and still narrow after this item.** Design
§7a's own operative half is a sequencing constraint: ship only the
primitives a real, shipped consumer exercises, and let THAT consumer specify
focus/input-routing/modal-stacking rather than speculating ahead of one.
This item's spec named the first falsifier explicitly as a live risk — "if
the declarative shape cannot express what a real interview needs... STOP and
report" — and it did not fire: `ask_question`'s v1 shape is exactly a prompt
plus an options list in, one selected option back — no checkbox, no
multi-select, no free-text answer alongside the options, no
answer-conditioned follow-up, no nested tree. A future consumer that
genuinely needs more is what should drive the next primitive, not a
speculative widget vocabulary added ahead of one.

## The request and answer shapes

```json
// request
{ "prompt": "proceed?", "options": ["yes", "no"] }

// answer
{ "selected": "yes" }
```

`options` must be non-empty; an empty list is refused before it ever reaches
an answering surface — for `ask_question`, as a genuine tool error
(`ToolError::InvalidArguments`, a caller mistake); for `ui.form`, as a
`CapabilityError`. `selected` echoes one of `options` verbatim — never an
index — so a caller never has to re-resolve an answer against the request it
sent.

## Calling `ask_question` (the model's own path)

A model calls `ask_question` exactly like any other tool, with `prompt` and
`options`. In the TUI it renders as a modal card; the operator picks with
`Up`/`Down` and answers with `Enter` (or cancels with `Esc`), and the tool
call returns `"operator selected: <choice>"`. Under a host with no
interactive surface (every host except the TUI today — see "Where a live
surface exists" below), the tool degrades in plain text rather than hanging:
`"no answer available: no interactive surface is available in this host to
ask the operator"`. Neither case is ever `ToolOutput::is_error`; asking a
question nobody could answer interactively is not a tool error, the same
main-line degrade posture `conway-plugin-skeleton`'s `skeleton_ask` already
established for the capability half.

## Calling `ui.form` from your own plugin (the plugin-to-plugin path)

Edge B needs no shared type between a provider and a consumer — a caller
names the capability by its bare string and builds its own JSON payload by
hand, exactly the shape an out-of-process (subprocess) plugin would have to
use anyway:

```rust
let required = semver::VersionReq::parse("^1").expect("valid literal");
let payload = serde_json::json!({ "prompt": "proceed?", "options": ["yes", "no"] });
match ctx.capabilities.call_versioned("ui.form", &required, payload).await {
    Ok(answer) => { /* answer["selected"] is a String */ }
    Err(e) => { /* degrade -- see below; never fail the tool call for this alone */ }
}
```

`^1` is the ordinary floor; `=1.0.0` pins a hard match if you need one
(`semver::VersionReq` gives both for free — see decision
`01M189XS6Z9VKYENAHNY1B54CM`). A version your requirement does not accept
is **refused**, naming both the requirement and the version actually
installed (`CapabilityCallError::VersionMismatch`) — never silently
degraded to a mismatched shape. `conway-plugin-skeleton`'s `skeleton_ask`
tool remains the worked example of this path end to end, unchanged by this
item.

## What installing it costs

```json
{ "plugins": { "install": ["conway.ui"] } }
```

Uninstalled, nothing changes: `ask_question` is not among the announced
tools, and a caller's `call_versioned("ui.form", ...)` fails as
`CapabilityCallError::NotProvided` — an ordinary, expected outcome your own
tool should degrade from, not a crash. Opt-in, exactly like every other
member of the first-party tier (`docs/vision/DESIGN-plugin-dependencies.md`
§0 ruling 2: bundled liberally, enabled by nobody by default) — a build
with no `[plugins]` section installs it not at all, the same as every other
bundle member.

## Where a live surface exists — and where it does not

`conway.ui` **always installs** once named — its manifest declares no host
capability requirement at all, because whether a live drawing surface
exists is a property of the running process, not of `settings.json`.
**The interactive TUI wires a real, live `FormSurface` in** (`crates/
conway-cli/src/tui/form.rs`'s `TuiFormSurface`, mirroring `crates/
conway-cli/src/tui/gate.rs`'s `TuiGate` shape exactly): `ask_question`
renders as a modal card in the TUI's own never-stack modal queue (see "Never
a second modal stack" below), and a call into `ui.form` from another
installed plugin reaches the identical live surface. **Every other dispatch
target** — one-shot `-p`, `sessions`, `routes`, a plugin subcommand —
still constructs `ConwayUiPlugin::default()` (no surface): a call there
refuses immediately, naming the reason (`"code": "no_drawing_surface"` in
`CapabilityError::detail` for `ui.form`; a plain sentence for
`ask_question`), rather than blocking forever waiting for an answer nothing
under that host could ever produce.

A caller that treats this refusal as a hard failure has mis-modelled what
`ui.form`/`ask_question` promise. `PHILOSOPHY.md`'s opening states embedded,
interactive, and one-shot as equally native hosts; a consumer degrades and
announces (its own reply text, a `tracing::warn!`, whatever channel it has),
and the run completes. `crates/conway-cli/tests/
ui_ask_question_one_shot.rs` drives this end to end through the real
compiled `conway -p` binary; `skeleton_ask`'s own sibling coverage
(`ui_form_degrades_under_one_shot.rs`) is unchanged.

## Never a second modal stack

The TUI already owns one modal-bearing surface stack (`crates/conway-cli/
src/tui/state/modal.rs`'s `Mode` enum and its `promote_next_surface`
park/promote queue, built for the permission prompt and since extended to
the `/ask` modal, the intent-confirm card, and the trust-preview card).
`ask_question`'s modal (`Mode::UiForm`) is the FIFTH surface in that same
queue, at the lowest priority: if it arrives while another modal is
showing, it parks and is promoted once the surface ahead of it clears,
exactly like every surface already there — it does not build, and does not
need, a competing stack of its own.

A model-raised question is not one of `docs/vision/
DESIGN-surface-coherence.md`'s three operator-invoked surface kinds
(ACTION/VIEW/CONFIGURATION) — that page's own inventory is `/`-command
surfaces an operator *types*. `ask_question`'s modal is reactive, exactly
like the permission prompt and the three other modal-bearing cards it now
sits beside, none of which that page's six rules govern either.

## Trust

`conway.ui` reads and writes nothing outside the process, spawns nothing,
and touches no file — true of both `ask_question` and `ui.form`. See
[`trust-and-security.md`](trust-and-security.md)'s "Plugin-to-plugin
capability calls" section for what a capability call trusts in general —
short version: a capability provider runs with the SAME privileges any
other installed plugin has; `conway.ui` adds no new trust surface beyond
that. `ask_question` is an ordinary `PermissionClass::Safe` tool: no
operator approval gate stands between the model and asking a question, only
the operator's own answer stands between the question and a reply.
