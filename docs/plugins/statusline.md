# `conway.statusline`: a shell command on the status line

The status-line plugin (board item `01M0X500861X9035QJEA82F94K`), shipped by
`crates/conway-plugin-statusline`. Depends on [`concepts.md`](concepts.md)
for vocabulary and on `hooks.md`'s `status.declare/1` row for what
`PluginStatusContribution` is and how the TUI renders it — this page covers
only what is specific to this one plugin: what it runs, how often, and what
it costs.

## What this is for

The operator's Claude Code config can carry `statusLine.type`/
`statusLine.command` — a shell command whose stdout becomes the status line.
conway's own status line is `[tui.status_line] fields` over a closed,
ten-variant vocabulary (`session`, `lineage`, `mode`, `model`, `ctx`,
`tokens`, `activity`, `hint`, `git`, `cwd`) — composable and safe, but not
extensible: you cannot show something conway did not anticipate. **This
plugin is the migration home for that gap.** Operator ruling, 2026-08-25:
"settings migrate only where they fit conway's philosophy; a plugin is the
right home where one exists." A shell-out status line is exactly that case —
opinionated, spawns a process on a UI cadence, and unarguably not core.

## What installing it costs

Two separate opt-ins, deliberately: linking the crate into the binary you
run is not enough on its own, and neither is naming a command without the
plugin being linked.

```json
{
  "plugins": {
    "statusline": {
      "command": ["git", "branch", "--show-current"],
      "key": "branch",
      "refresh_interval_ms": 5000,
      "timeout_ms": 2000
    }
  }
}
```

- `command` — argv-shaped (program, then its arguments), **never a single
  shell string** — the same shape and reasoning `[hooks].rules[].command`
  and `[plugins].subprocess[].command` already use: no shell-quoting
  ambiguity in config. If you are migrating an actual Claude Code
  `statusLine.command` shell one-liner, wrap it explicitly:
  `["sh", "-c", "git branch --show-current"]`.
- `key` — the `PluginStatusContribution::key` this result is filed under.
  Defaults to `"statusline"`. Matters only if you also run another
  status-contributing plugin and want to tell the two rows apart.
- `refresh_interval_ms` — milliseconds between the end of one run and the
  start of the next. Defaults to 5000 (12 spawns/minute). **Floored at
  1000ms regardless of what you write here** — see "Cadence" below.
- `timeout_ms` — milliseconds a single run is allowed before it is treated
  as failed. Defaults to 2000.

Absent `[tui.status_line_command]` entirely, or present with an empty `command`
(the default), **this plugin is a true no-op**: no process is ever spawned,
nothing is ever attached to the build, `status_contributions()` returns
nothing, forever. Naming a command is the whole of the opt-in — there is no
separate `[plugins].install = ["conway.statusline"]` entry to also write;
see "Why this is not a `[plugins].install` entry" below.

## Cadence: the number that actually matters here

A status line redraws constantly. This plugin never spawns a process per
redraw — process-spawn cadence is decoupled from render cadence entirely.
One background task runs the configured command, publishes the result to a
cache, sleeps for `refresh_interval_ms`, and repeats, for the life of the
process, regardless of how many times (or how rarely) anything actually
reads the cache.

**Worst case: 60 process spawns per minute**, no matter what you write into
`refresh_interval_ms` — the plugin floors the interval at 1000ms internally.
A command that runs long, or times out, only ever makes the cadence
*slower*: the next run does not start until the full interval has elapsed
*after* the previous one finished, so a hung command cannot make this
plugin spawn faster than its own configured pace, only more sluggish.

## A slow command never stalls the UI

Reading the status line (`Plugin::status_contributions()`) only ever reads
the background task's last cached result — a single, non-blocking lock. It
never touches the subprocess machinery itself, so a command that is
*currently* mid-run, however slow, cannot make a reader wait even one extra
millisecond. A single run is separately bounded by `timeout_ms`; past that,
the run is killed and recorded as failed. This is the same "lossy-with-
notice, the host turn never blocks on a slow plugin read loop" posture
`Plugin::observe_sink` already has for its own forwarding task
(`crates/conway-core/src/ports/plugin.rs`), applied here to a synchronous
cache read instead of a queue.

A 3-second command shows a stale (or, on the very first run, absent) value.
It never freezes anything reading this plugin.

## A failing command is visible, never a silent blank

A non-zero exit, a missing binary, a timeout, or a *successful* run that
produced no output at all — every one of these renders
`status: failed` with a legible reason (the exit code and the first line of
stderr, "timed out after Nms", "failed to spawn ...", or "produced no
output"), never an empty string. An empty string would be visually
indistinguishable from a healthy command that legitimately has nothing to
say — the exact silent-success trap this plugin is built to avoid.

## It never displaces `mode`

`view/status.rs`'s `mode` field — in particular the `AUTO-ALLOW` label — is
"a genuine safety signal: an operator who forgets they're in it is the exact
failure this guards against" (that module's own doc). This plugin produces
ordinary `PluginStatusContribution`s through the same mechanism every
status-contributing plugin already uses; the non-displacement guarantee
lives entirely in the render path (`drop_priority` ranks the `plugins`
field strictly below `mode`, and every contribution is shrunk to its own
empty floor before `mode` is ever asked to give up a single column), not in
this plugin. There is nothing plugin-specific to configure or verify here —
the existing render-path test already covers every contribution source
uniformly, this plugin's included.

## Trust: read this before writing a command

`[tui.status_line_command].command` runs with **your own process privileges** —
no sandboxing, no digest check, no confirmation prompt. This is the
identical footing `[hooks].rules[].command` and `[plugins].subprocess[]`
already have: an operator who would not paste an unfamiliar command into
`[hooks]` should not paste one into `[tui.status_line_command].command` either.
Unlike a hook (fired on an event) or a one-shot subprocess call (fired on a
tool call), this command runs **repeatedly, unattended, on a fixed
schedule** — see "Cadence" above for the bound.

One further, narrower limitation, disclosed rather than silently accepted:
the spawned process is killed (`kill_on_drop`) if it outlives `timeout_ms`,
but only the *immediate* child — a shell command that itself forks a
background job is not chased down through a full process-group kill. The
heavier machinery for that (`conway::plugin::kill_group`) is shaped for a
persistent, long-lived RPC child, not a fire-and-forget probe; a status-line
command that spawns its own background children is unusual enough that this
plugin does not carry that extra weight for it.

## Why this is not a `[plugins].install` entry

Every one of the ten plugins `first_party_plugins::bundle()` resolves
(`docs/plugins/README.md`'s "Ten shipped first-party plugins") is named in
`[plugins].install` against a closed candidate set this binary happens to
link, and ships with **no `settings.json` field of its own** — naming the id
is the whole of the opt-in.

`conway.statusline` does not fit that shape, because there is nothing to
*name*: naming a command in `[tui.status_line_command].command` is already the
complete opt-in signal, structurally identical to `[plugins].subprocess[]`/
`[plugins].mcp[]`/`[plugins].claude_compat[]` — "an operator names a
command directly; that naming alone is what makes it run." It is resolved
by its own choke point, `crates/conway-cli/src/statusline_plugin.rs`, a
fifth sibling to those three. `"conway.statusline"` never appears in
`[plugins].install`.

## What this plugin found, for `DESIGN-plugin-dependencies.md` §7c

This plugin is the second consumer of the status-contribution seam, and the
first that is not a permission guard — and the first to exercise the
**push** half (`PluginStatusContribution`) rather than the pull half, which
that design's §7c leaves as an open question: does one mechanism serve
both?

The concrete answer, found while building this plugin rather than assumed
going in: `PluginStatusContribution` the *type* needed nothing new — it
already carries a value, a success/failure verdict, and a legible failure
reason, everything a status-line command produces. What is missing is
entirely on the host side. `Conway::plugin_status_contributions()` is a
**build-time snapshot, read exactly once**, at `ConwayBuilder::build` —
before this plugin's own background loop has any reliable chance to have
produced a value. A plugin that refreshes every five seconds is invisible
to the TUI after that one read, for the life of the process. Proven, not
merely argued, by this crate's own `tests/statusline_end_to_end.rs`: the
identical plugin and command reach the real facade's snapshot when given a
head start before `build()` runs, and produce nothing in that same snapshot
with no head start — while the plugin's own live state shows the value
arriving moments later. The gap is a missing live poll on the host side (or
the pull half §7c already names as the alternative), not a type that needs
another field.
