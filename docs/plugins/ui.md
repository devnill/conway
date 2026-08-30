# `conway.ui`: the `ui.form` capability other plugins call into

The first-party plugin for Edge B's `ui.form` capability (board item
`01M0WWPA70E8YAAN981EK10D3D`, `docs/vision/DESIGN-plugin-dependencies.md`
§0/§2/§7a), installed by `crates/conway-plugin-ui`. Depends on
[`concepts.md`](concepts.md) for vocabulary and on
[`hooks.md`](hooks.md) point 21 for the `ToolCtx::capabilities`/
`CapabilityCallHandle` contract this plugin's capability answers on.

**This page is not an operator's first stop.** `conway.ui` contributes no
tool a model calls and no command an operator types — it does nothing on
its own. It exists so ANOTHER installed plugin can pose a single-select
question and get one answer back, instead of every plugin author
reimplementing that round trip themselves. If you are deciding whether to
enable it, the honest answer is: enable it because SOME OTHER plugin you
also enabled says it needs `ui.form`, never on its own.

## What this is, in one sentence

A capability, `ui.form`, published at `1.0.0`: a request carrying a prompt
and a fixed, ordered list of options in; one selected option back — the
AskUserQuestion analogue this item's own spec names, over Edge B (plugin →
plugin), a blocking call with exactly one provider and one answer (the
*pull* shape `docs/vision/DESIGN-plugin-dependencies.md` §7c contrasts with
`conway.statusline`'s *push*).

## Why this exists

Before this item, the only plugin → screen path was
`PluginStatusContribution` — a fixed `{ key, status, value }` triple, one
widget, no composition (that design page's §1). A plugin wanting to ask an
operator a real question with options had nowhere to put it. `conway.ui` is
the toolkit end of the extensible declarative widget tree design §7a rules
in: feature plugins consume `ui.form`; `conway.ui` is what actually
publishes it.

**Built narrow, deliberately.** Design §7a's own operative half is a
sequencing constraint: ship only the primitives a real, shipped consumer
exercises, and let THAT consumer specify focus/input-routing/modal-stacking
rather than speculating ahead of one. This item's consumer
(`conway-plugin-skeleton`'s `skeleton_ask` tool) asks one fixed yes/no
question. Accordingly, `ui.form`'s v1 shape is exactly that: a prompt plus
an options list in, one selected option back — no checkbox, no
multi-select, no nested tree, and no live rendering primitive wired into
this pass at all (see "What is NOT built yet" below). A future consumer
that genuinely needs more is what should drive the next primitive, not a
speculative widget vocabulary added ahead of one.

## The request and answer shapes

```json
// request
{ "prompt": "proceed?", "options": ["yes", "no"] }

// answer
{ "selected": "yes" }
```

`options` must be non-empty; an empty list is refused before it ever
reaches an answering surface. `selected` echoes one of `options` verbatim
— never an index — so a caller never has to re-resolve an answer against
the request it sent.

## Calling it from your own plugin

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
degraded to a mismatched shape.

## What installing it costs

```json
{ "plugins": { "install": ["conway.ui"] } }
```

Uninstalled, nothing changes: no capability is registered, and a caller's
`call_versioned("ui.form", ...)` fails as `CapabilityCallError::NotProvided`
— an ordinary, expected outcome your own tool should degrade from, not a
crash. Opt-in, exactly like every other member of the first-party tier
(`docs/vision/DESIGN-plugin-dependencies.md` §0 ruling 2: bundled
liberally, enabled by nobody by default) — a build with no `[plugins]`
section installs it not at all, the same as every other bundle member.

## The degrade path is main-line, not an edge case

`conway.ui` **always installs** once named — its manifest declares no host
capability requirement at all, because whether a live drawing surface
exists is a property of the running process, not of `settings.json` (see
"What is NOT built yet" below). What varies is what happens on a CALL: a
host with a real, live answering surface returns a real answer; a host
without one refuses immediately, naming the reason
(`"code": "no_drawing_surface"` in `CapabilityError::detail`), rather than
blocking forever waiting for an answer nothing can ever produce. `conway
-p` (one-shot) is exactly the second case today — see the next section for
why it is, right now, the *only* case.

A caller that treats this refusal as a hard failure has mis-modelled what
`ui.form` promises. `PHILOSOPHY.md`'s opening states embedded, interactive,
and one-shot as equally native hosts, none available in only one of them; a
consumer plugin degrades and
announces (its own reply text, a `tracing::warn!`, whatever channel it
has), and the run completes. `conway-plugin-skeleton`'s `skeleton_ask` is
the worked example: whether `conway.ui` is absent, present with an
incompatible version, or present with no drawing surface, its reply always
says so in plain text and its tool call is never marked an error.

## What is NOT built yet

**No live, interactive answering surface ships in this pass.**
`ConwayUiPlugin` takes its answering mechanism (a `FormSurface`) as a plain
constructor argument — `Some(surface)` where a host can present a question
and collect a choice, `None` where it cannot — and
`crates/conway-cli/src/first_party_plugins.rs` passes `None` for EVERY
dispatch target today, the TUI included. This is a disclosed scope
decision, not an oversight: no shipped form yet needs a specific rendering,
and the TUI already owns one modal stack (built for the permission prompt,
since extended to the `/ask` modal, the intent-confirm card, and the
trust-preview card — `crates/conway-cli/src/tui/state/modal.rs`); wiring a
second, competing one for a proof-of-mechanism consumer with no real
on-screen need would be exactly the "designing on theory" INTENT §8.5
forbids. The seam (`conway_plugin_ui::FormSurface`) exists precisely so a
later item can plug a live surface in — mirroring
`crates/conway-cli/src/tui/gate.rs`'s `TuiGate`, the identical
"host-supplied answering mechanism, injected at construction, not derived
from config" shape — the day a real form needs one.

Practically: **every host today refuses every `ui.form` call.** That is
honestly declared (`ConwayUiPlugin::description`'s own `costs` text says
so) rather than hidden behind a mechanism that looks complete.

## Trust

`conway.ui` reads and writes nothing outside the process, spawns nothing,
and touches no file. See
[`trust-and-security.md`](trust-and-security.md)'s "Plugin-to-plugin
capability calls" section for what a capability call trusts in general —
short version: a capability provider runs with the SAME privileges any
other installed plugin has; `conway.ui` adds no new trust surface beyond
that.
