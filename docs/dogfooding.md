# Dogfooding conway on conway

This page is the standing path from *using* conway to *work on the board*. It
exists because, for most of conway's life, nobody had actually used it — the
only recorded input was the string `"hello from the test"`, typed roughly
twenty times. Every review before that audited the harness against its own
documents, because no session had ever produced the kind of friction only
real use generates. See
[`vision/INTENT.md`](vision/INTENT.md) §7a–§7b for why this is a stated
priority rather than a nice-to-have, and board item
`01KZY8V4MYNZJABZR0X0SJ2G5Y` for the standing intake item this page supports.

**This is not a page telling you to try conway.** It is the mechanism that
turns friction encountered *while* trying it into a board item, in the same
number of keystrokes it would take to shrug and keep going. If recording is
more expensive than absorbing, the default outcome is always absorbing — so
that is the thing this page's tooling is built to beat.

## The two rungs

`vision/INTENT.md` §7b lays out the bar as a ladder, not a switch:

- **Rung one — supplement.** conway is used alongside the current harness,
  for real work, by choice, for some class of task. This is the rung that
  starts generating honest signal.
- **Rung two — no longer needed.** Feature coverage of what actually matters
  day to day, *and* output quality **better** than the incumbent tool
  produces today — not comparable, better. Feature parity with worse results
  convinces nobody, including us.

Every dogfooding session works rung one directly and feeds the "which
features actually matter" question rung two depends on.

## Before you start

You need a working provider config — see
[`getting-started.md`](getting-started.md) — and `ideate-work` /
`ideate-record` on `PATH` (they ship with the ideate plugin; if a command
below says one is missing, that plugin is not loaded in this session).

## Isolated config dirs start empty

You can point conway at a throwaway settings directory instead of your real
`~/.conway/` — set `CONWAY_CONFIG_DIR` to any directory and, per board item
`01M0VV6CVSZM4XH8J4G6EBV5E3`, conway treats it as the user-scope config
location *and* stops a project directory under your real `$HOME` from
reaching your real `~/.conway/settings.json` instead (see
[`embedding.md`'s "Loading config without the ambient user
layer"](embedding.md#loading-config-without-the-ambient-user-layer) for the
full precedence mechanics). This is the right way to dogfood a change you
don't fully trust yet — it's cheap, and it can't touch your real setup.

**Nothing carries over from your real config.** A brand-new
`CONWAY_CONFIG_DIR` has no `settings.json` at all, so conway falls back to
its own built-in defaults. Those defaults do cover housekeeping — session
limits, permissions, headroom — but they give you no `[backends]` at all,
and only an empty `default` role whose chain names no model. So nothing can
route, and you write a complete-enough config from scratch. At minimum
that's three keys, in one file:

```json
// $CONWAY_CONFIG_DIR/settings.json
{
  "default_role": "coder",
  "backends": {
    "anthropic": {
      "kind": "anthropic",
      "api_key_env": "ANTHROPIC_API_KEY"
    }
  },
  "roles": {
    "coder": { "chain": ["anthropic/claude-sonnet-4-6"] }
  }
}
```

`api_key_env` names an environment variable to read the key from — export
it before running conway; never put a literal key in this file. (No
Anthropic key handy? Swap the `backends`/`roles` entry for a local server
the same way
[`getting-started.md`'s Ollama example](getting-started.md#an-openai-compatible-endpoint-ollama-llamacpp-vllm-and-others)
does — no key needed. Copy that example's backend block whole rather than
editing `kind` and `base_url` in the one above: `openai-compat` also
requires a `dialect` naming your server, and refuses to build without it
(`backend 'x': kind 'openai-compat' requires 'dialect'`).)

Miss this step and conway fails loud and fast: `no backends configured: add
a [backends.<id>] entry to config`. Annoying, but instantly diagnosable —
this is the **good failure**.

**The tools list is the trap, because it fails quiet.** conway's `fs`
(read/write/edit/glob/grep/cd), `subagent`, and `report` built-ins are
registered by default — `tools.builtin_plugins` defaults to `["conway.fs",
"conway.subagent", "conway.report"]`, every built-in except bash — so
leaving `[tools]` out of the file above entirely is *not* the problem; a
bare isolated config already gets those three for free, same as a real one.
The problem starts the moment you add a `[tools]` block yourself (to opt
into bash for the TUI, say) and don't restate the full list:
`builtin_plugins` is a whole-array *replacement*, not something that adds to
the default the way you'd expect from an opt-in field — see
[`getting-started.md`'s "Enabling bash (shell
commands)"](getting-started.md#enabling-bash-shell-commands) for the
canonical statement of that gotcha. Write only `{"tools": {"builtin_plugins":
["conway.shell"]}}` because bash is what you wanted, and you've just taken
`fs`/`subagent`/`report` away too. This gate only applies to the interactive
TUI, for what it's worth — one-shot (`-p`) always registers every built-in
regardless of `[tools]`, gating them instead with
`--permission-mode`/`--allowed-tools`.

**The symptom to search for is "the agent replies but never acts."** An
agent with no registered tools can still talk — asked to edit a file or run
a command, it answers by describing what it would have done, in prose that
reads exactly like a real result. At a glance that is indistinguishable from
a genuinely broken feature (a slash command whose translation silently drops
the underlying tool call, say), and chasing the wrong one costs real time —
that's exactly what happened staging a dogfooding session on
`01M0X3AMASEJGHZ6ZDMDFWCHSE`: a `/beepboop:config` command that appeared to
do nothing was diagnosed as a broken translation path before the real cause
(no tools) was found. If a session in an isolated config dir seems to "do
nothing" — no file changed, no command ran, but the model's own narration
sounds like it did — check `[tools]` in that dir's `settings.json` before
you go looking anywhere else.

## The loop

1. **Use conway for something real.** A change to conway itself is ideal —
   it is the stated long-term goal of cutting over from the harness you are
   reading this page in. `conway` for the interactive TUI, `conway -p` for a
   scripted one-shot; see [`GUIDE.md`](../GUIDE.md) and
   [`scripting.md`](scripting.md).
2. **The moment something is awkward, record it — do not reconstruct it
   later.** Reconstructed friction is the same failure mode as prose checked
   against prose: it reports what you remember deciding, not what actually
   happened. Open a second terminal, or a second pane, and run:

   ```console
   scripts/dogfood-note.sh friction \
     --title "denied bash calls give no reason" \
     --body "Five permission-denied warnings for bash/glob before the model
   settled on read. It adapted, but I never saw what the allow-list actually
   was." \
     --human "$(git config user.name || echo dan)"
   ```

   If the friction is about something already on the board, attach it there
   instead of filing a new item:

   ```console
   scripts/dogfood-note.sh comment \
     --id 01KZYM81YFE08ASM225A1R5H5X \
     --note "Hit this today: a long session on conway's own tree hard-refused
   on admission with no compaction plugin to fall back to, well before the
   work was done."
   ```

3. **At the end of the session, append the required session note** — what
   was attempted, what worked, what stopped you:

   ```console
   scripts/dogfood-note.sh session \
     --note "Used conway to draft this page. Provider routing and /ask were
   solid. Hit two friction points, filed both. Compaction never came up --
   session stayed well under the window."
   ```

Each of those is one command. That is the whole mechanism: `scripts/
dogfood-note.sh` resolves the repository root from its own location on disk
(so it refuses to run — loudly — against the wrong project no matter where
your shell's current directory happens to be), and calls the same
`ideate-work` / `ideate-record` tooling every other agent in this repository
uses. Run `scripts/dogfood-note.sh --help` for the full flag reference, and
add `--dry-run` to any of the three commands above to see exactly what it
would file without filing it.

## Known friction already on record

Seeded 2026-08-13, from `vision/INTENT.md`'s own history. One of the four has
since shipped — checked against the board while writing this page, not
assumed — which is itself the point: stale friction claims are exactly the
kind of thing this loop exists to catch and correct.

- Denied tool calls are noisy and unexplained in one-shot mode: the model
  sees a reason (via `DenyWithFeedback`), the operator does not. **Still
  open** as of this page's last check.
- Two `read` calls returned `error` in the same run before one succeeded,
  with no operator-visible explanation — worth checking whether it's
  relative-path resolution. **Still open** as of this page's last check.
- No `/rewind` from a bad turn without quitting the TUI. **Shipped** —
  `01KZY8Q1CMMNVSF54CTC270N3H` is `done`; see [GUIDE.md's "When a turn goes
  wrong"](../GUIDE.md#when-a-turn-goes-wrong) for `/conway.history.rewind`.
- No compaction, by design (`PHILOSOPHY.md` §6) — a long session on conway's
  own tree will hard-refuse on admission rather than degrade, and the
  compaction plugin that would let you keep going does not exist yet. Expect
  to hit this on a long session and record where. **Still open.**

## How the falsifiable check works

"We dogfooded conway" is not a checkable claim. This is:

- Every board item `scripts/dogfood-note.sh friction` creates has a title
  prefixed `[dogfooding] `. Count them:

  ```console
  ideate-work list --json | python3 -c '
  import json, sys
  items = json.load(sys.stdin)["items"]
  print(sum(1 for i in items if i["title"].startswith("[dogfooding] ")))
  '
  ```

- Every record entry it appends — both `comment` and `session` — carries
  `--scope dogfood` (session notes use `dogfood-session`). Read them back:

  ```console
  ideate-record read --scope dogfood --json
  ```

Zero on both, session over session, means the path exists but nobody is
walking it — which is itself a useful, honest signal, and a reason to ask why
rather than to declare the item done anyway.

## What this deliberately does not do

- It does not judge whether your friction was worth filing. It removes the
  ceremony of filing it; the judgment stays yours.
- It does not touch conway's own configuration, source, or the operator's
  `~/.conway/settings.json`. It only calls the board tooling.
- It does not close `01KZY8V4MYNZJABZR0X0SJ2G5Y`. That item is a standing
  one by design — claim it, work a session, release it. See the item's own
  spec for the claim-lease note (renew explicitly; the 4h default silently
  auto-releases under a long session).
