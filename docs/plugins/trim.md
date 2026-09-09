# `conway.trim`: age out old tool call/result round-trips

The first-party curator plugin that keeps a long session's context small by
dropping tool call/result round-trips once they fall out of a sliding
window (board item `01M0EMAC4CCDQ8QJYM21RXPKRY`), installed by
`crates/conway-plugin-trim`. Its window is operator-configurable (board item
`01M1YVM9CHFCJ6112XDYHCFS84`). Depends on [`concepts.md`](concepts.md) for
vocabulary (plugin, curator, `[plugins].install`).

## What it is for

A session that runs long accumulates tool output the model already acted
on and no longer needs verbatim: the contents of a file it read three
turns ago, the output of a command it already responded to. Left in
context, that output still costs tokens on every subsequent request. This
plugin drops it, on a rolling basis, once it falls more than `keep_turns`
turns behind the turn currently in progress.

It never reorders anything ([`concepts.md`](concepts.md)'s vocabulary: this
is an omission, never a move) and it never touches a lone tool result
without also dropping the call that produced it — a tool call and its
answering result age out together, as one unit, so the model is never shown
a call with no result or a result with no call. One real cost this forces:
a model response that mixes prose with a tool call in the same turn loses
that prose too once the round-trip it belongs to ages out, because omission
works at whole-record granularity and cannot split the two apart.

## Install it

```json
{ "plugins": { "install": ["conway.trim"] } }
```

Not installed by default. Every first-party plugin in the shipped bundle is
opt-in (see [`README.md`](README.md) and
`crates/conway-cli/src/first_party_plugins.rs`).

## Configuring the window

`keep_turns` defaults to **8** if you never set it. Set your own value under
`[plugins.config.conway.trim]`:

```json
{
  "plugins": {
    "install": ["conway.trim"],
    "config": { "conway.trim": { "keep_turns": 3 } }
  }
}
```

- `keep_turns` must be a JSON integer **`>= 1`**. `0` is refused rather than
  silently clamped to `1` — this plugin always keeps at least the
  most recent round-trip, so a configured `0` cannot mean what it says and
  fails the load loudly instead of being quietly reinterpreted.
- Any OTHER key under `conway.trim`'s own table — a typo (`keep_trns`), a
  renamed field, a key that belongs to a different plugin pasted into the
  wrong table — is refused **by name**. It is never silently ignored: a
  config value conway cannot honor stops the build rather than pretending
  to have applied it.

**Smaller vs. larger.** A smaller `keep_turns` frees more context sooner, at
the cost of the model losing sight of tool output further back — right for
a small context budget or a session doing many short, independent steps. A
larger `keep_turns` keeps more history in view, at the cost of the context
it occupies — right for a session where an early tool result stays relevant
for a long time (a spec read once at the start, referenced throughout).
There is no universally correct value; this is exactly the operator
judgment `[plugins.config.conway.trim]` exists to let you state.

**How the window is applied.** `keep_turns` counts *turns*, not tool calls
or records: `threshold = current_turn - keep_turns`, and a round-trip whose
own turn falls below that threshold is dropped, in whichever order the
session's own path visits it. A turn that made no tool call is not a
round-trip at all and is never a candidate for omission — only an
`Assistant` record that issued a tool call, and the `ToolResultRecord`(s)
answering it, are ever dropped, and always together.

**Where the active window is visible.** `Plugin::description().you_get`
reports the CURRENTLY active window, not a fixed string: it reads the same
`keep_turns` the curator itself uses. `conway plugin list`
(`first_party_plugins::installed_plugins`, which applies
`[plugins.config.<id>]` before rendering) shows the configured value
immediately, with no separate query needed. **The interactive TUI's
`/settings` plugin browser does not, today:** it reads
`first_party_plugins::all_bundle_plugins`, the unfiltered "every compiled-in
candidate, on or off" scan the browser needs to show what is
available-but-off — that scan constructs every candidate with its plain
default and does not apply `[plugins.config.<id>]` to any of them, so its
row for `conway.trim` always reads the unconfigured default (`8`)
regardless of what `settings.json` actually says. This is a disclosed gap,
not an oversight: closing it means threading an operator's config table
through a TUI-owned call site this item did not touch.

## This is the mechanism's worked example, not a special case

`[plugins.config.<id>]` and `Plugin::configure` are general — any
first-party or third-party plugin may adopt them for its own persistent
settings. `conway.trim` is the FIRST plugin to do so, proving the seam with
one real key (`keep_turns`) rather than as a hypothetical. See
[`authoring.md`](authoring.md)'s "Configuration" section for the mechanism
itself, and this page for what one real implementor of it looks like end to
end.

**What is NOT built by this item:** a TUI editor for plugin settings. File
configuration (editing `settings.json` directly) is the whole of what
exists today — an acceptable, disclosed first slice, not a smaller version
of a TUI surface that already exists elsewhere.

## Uninstalled, nothing changes -- with one disclosed exception

With `"conway.trim"` absent from `[plugins].install`, no curator runs and no
tool call/result round-trip is ever dropped.

**Its `[plugins.config.conway.trim]` table is still validated, even then.**
`first_party_plugins::apply_plugin_config` walks every candidate the CLI's
linked bundle constructs and applies `[plugins.config.<id>]` to any one an
operator's table names, BEFORE `[plugins].install` filters down to the
selected subset — so a malformed value (an unrecognized key, `keep_turns:
0`) still fails the build even for a `conway.trim` you never installed. This
is deliberate, not a bug: it is the same "validate every candidate this
table names, whether or not it ends up selected" posture every other
"selected but broken" resolver in `first_party_plugins.rs` already takes,
and it means a typo under `[plugins.config.conway.trim]` is caught even by
an operator who added the config block before adding the install entry.
