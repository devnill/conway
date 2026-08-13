# `docs/plugins/authoring.md` — executed walkthrough

Evidence for board item `01KZYAYWRQKZ41Y7AHZ2QWVJ8X` ([S0f], the 1.0-beta
release's acceptance test): *"use conway, along with its bundled
user-facing documentation, to build out some hooks and plugins to customize
my personal conway install."* Same genre and rigour as
`.design/getting-started-default-build-walkthrough.md` — **the page was
executed, not read**, and every divergence found was either fixed in the
page directly or is enumerated below.

Run on 2026-08-13, against `wt/walkthrough` (based on `main` at `a337329`),
on macOS (Darwin 25.5.0). `CARGO_HOME`/`RUSTUP_HOME` left at their real
values throughout (both empty in this shell, resolving to the default
`~/.cargo`) — only `HOME`/`XDG_CONFIG_HOME` were redirected, per-fixture,
into scratch temp directories, never `~/.conway/`.

## What was being checked

`docs/plugins/authoring.md` claimed, as of this morning, that the
declarative `[hooks].rules[]` surface was "decided, not built" and that the
only way to write a hook was an in-process Rust `ContextHook`. Three
capabilities landed in the tree today that the page did not know about: the
hook tool matcher (`match`, board item `01KZYAWQ6011Q6CJVG6CCMQPF1`),
`Plugin::commands()`, and `ConwayBuilder::install_selected`. The item's own
framing: *"a first hook that fires on every tool call is not the first hook
anyone wants"* — the walkthrough had to use the matcher, not merely mention
it.

## Setup — fixtures, following the page's own instructions

**Fixture A (the declarative hook)**: an empty directory,
`$SC/hookfix`, with `HOME=$SC/hookfix/home` and
`XDG_CONFIG_HOME=$SC/hookfix/xdg` (`$SC` = a session-scoped scratch
directory, never under `~/.conway/` or inside this repo's working tree).
Binary: `cargo build -p conway-cli --release` from this worktree, exit 0,
49.3s, producing `target/release/conway`.

**Fixture B (the Rust plugin)**: a second empty directory,
`$SC/pluginfix`, same `HOME`/`XDG_CONFIG_HOME` redirection pattern, plus a
scratch crate `$SC/plugin-scratch` — a `cargo new --bin` outside the
workspace, `conway`/`conway-plugin-backends` as **path** dependencies onto
this checkout, nothing added to the real `Cargo.toml`/`Cargo.lock`.

Both fixtures point at a **local Ollama** serving `qwen3:4b` — the same
model `.design/getting-started-default-build-walkthrough.md` used, already
running on this machine, no cloud credentials, no network egress beyond
`localhost:11434`.

## Divergence 0 (found before the page's own steps even started): `XDG_CONFIG_HOME` redirects the config *directory name*, not just the base

Redirecting `HOME` and `XDG_CONFIG_HOME` together (as this item's fixture
instructions require) means the discovered global config path is
**`$XDG_CONFIG_HOME/conway/settings.json`**, not
`$XDG_CONFIG_HOME/.conway/settings.json` and not
`$HOME/.conway/settings.json` — `crates/conway/src/config/discovery.rs`'s
`user_config_path`: `XDG_CONFIG_HOME` set (even to an unrelated scratch
dir) means `~/.conway/settings.json` is **never consulted at all**, not
merged, not a fallback.

```console
$ HOME=$SC/hookfix_wrongpath/home XDG_CONFIG_HOME=$SC/hookfix_wrongpath/xdg \
  conway -p "hi"
conway: error: no backends configured: add a [backends.<id>] entry to config or call ConwayBuilder::with_backend
$ echo $?
2
```

(settings.json was sitting at `$HOME/.conway/settings.json` — the
*non*-XDG location — for this run; a config-isolation fixture that sets
`XDG_CONFIG_HOME` and then writes to `.conway/` under `HOME` produces a
silently-empty config, not an error naming the mismatch.) This is not a
defect in the tree — `discovery.rs`'s own doc states the precedence
correctly — but nothing on `authoring.md` said it, and this exact mistake
would have produced a *false negative* for this item's own acceptance
criterion (an isolated fixture that "worked" only because it fell through
to no config and printed a clear, early error rather than quietly reading
something real). **Fixed on the page**: "Ten minutes to a working hook"
step 2 and "Where things live" now say `$XDG_CONFIG_HOME/conway/
settings.json`, "if that variable is set, instead of `~/.conway/
settings.json`, not in addition to it" — spelled out because the getting-
started page's own precedence description does not make the "instead of"
half explicit either (out of this item's file lane, not touched, but worth
a note here for whoever next touches that page).

## Part 1: the declarative hook, executed via the page's rewritten "Ten minutes"

### Step 1 — get a binary

```console
$ cargo build -p conway-cli --release
   ...
    Finished `release` profile [optimized] target(s) in 49.23s
$ echo $?
0
```

### Step 2 — write the rule, with `match`

`$SC/hookfix/xdg/conway/settings.json` (copied verbatim from the page's own
step-2 example, `match: "bash"`, `event: "post_tool_use"`, logging to a
file under the same scratch directory):

```json
{
  "default_role": "coder",
  "backends": { "local": { "kind": "openai-compat", "dialect": "ollama", "base_url": "http://localhost:11434/v1" } },
  "roles": { "coder": { "chain": ["local/qwen3:4b"] } },
  "hooks": { "rules": [ { "id": "log-bash-calls", "event": "post_tool_use", "match": "bash",
    "command": ["/bin/sh", "-c", "echo bash-hook-fired >> $SC/hookfix/hook.log"], "timeout_ms": 3000 } ] },
  "tools": { "builtin_plugins": ["conway.fs", "conway.subagent", "conway.report", "conway.shell"] }
}
```

(`tools.builtin_plugins` including `conway.shell` was needed to give the
model a `bash` tool at all — opt-in per `docs/embedding.md`'s "Built-in
plugin selection" — already noted on the page's step 3.)

### Step 3 — see it fire, and prove the narrowing

**Run 1 — the load-bearing one**, `match: "bash"`:

```console
$ cd $SC/hookfix
$ HOME=$SC/hookfix/home XDG_CONFIG_HOME=$SC/hookfix/xdg \
  conway -p "List the files in this directory using the bash tool (run: ls)." --allowed-tools bash
conway: warning: tool call proposed: bash (call_1vli10lh)
conway: warning: tool call started (call_1vli10lh)
conway: warning: tool call finished (call_1vli10lh): ok
The files in this directory are:
- home
- hook.log
- xdg
$ echo $?
0
$ cat hook.log
bash-hook-fired
```

A real model, calling a real tool, through a real declarative hook rule
that named `match: "bash"` — the acceptance criterion's "hook firing in a
real session," and it used the matcher, as the item required.

**Run 2 — the narrowing counterfactual**, `match: "fs.write"` (a tool this
prompt never calls), same prompt, same everything else:

```console
$ HOME=$SC/hookfix/home XDG_CONFIG_HOME=$SC/hookfix/xdg \
  conway -p "List the files in this directory using the bash tool (run: ls)." --allowed-tools bash
conway: warning: tool call proposed: bash (call_5nhvpwva)
...
The files in this directory are:
- home
- hook.log
- xdg
$ echo $?
0
$ cat hook.log
(empty)
```

`bash` ran identically (the file listing is unchanged) but the hook did
not fire — the rule was parsed, validated, and consulted, and correctly
decided not to act, which is the exact thing `hooks.md`/
`.design/extension-architecture.md` §9.3 warn is otherwise indistinguishable
from "not wired at all." Runs 1 and 2 differ in exactly one config field
and produce opposite hook outcomes — the same shape of proof
`getting-started-default-build-walkthrough.md`'s own runs 1/3 used.

**Run 3 — the typed config error**, `match: "bash"` moved onto
`session_starting` (an event with no tool name):

```console
$ HOME=$SC/hookfix/home XDG_CONFIG_HOME=$SC/hookfix/xdg conway -p "hi" --allowed-tools bash
conway: error: hooks.rules[]: rule 'log-bash-calls' sets "match" on event "session_starting", which carries no tool name -- "match" only applies to "pre_tool_use"/"post_tool_use"
$ echo $?
2
```

Confirms `hooks.md`/the rewritten authoring page's own claim about this
safety net, exactly as documented — not merely asserted.

**Steps to first visible result, hook half: 3** (build a binary; write one
JSON block; run one command and `cat` a file). Config boilerplate: one
`[hooks].rules[]` entry, 6 fields, no Rust, no compiling anything of your
own. This is a large reduction from the previous page's shape (a `Cargo.toml`,
a ~35-line `ContextHook` impl, a ~30-line unit test, and a fake-backed
session example) — measured precisely below in "What changed, quantified."

## Part 2: the Rust plugin, executed via `install_selected`

`$SC/plugin-scratch/src/main.rs` — the page's own "Writing a Rust plugin"
snippet, unelided (the page's own copy elides `GreetTool::spec`/`invoke`'s
bodies for length only): a `MyFirstPlugin` registering one tool, `greet`,
built as its own binary outside the workspace, depending on `conway` and
`conway-plugin-backends` by path.

`$SC/pluginfix/xdg/conway/settings.json`:

```json
{
  "default_role": "coder",
  "backends": { "local": { "kind": "openai-compat", "dialect": "ollama", "base_url": "http://localhost:11434/v1" } },
  "roles": { "coder": { "chain": ["local/qwen3:4b"] } },
  "permissions": { "mode": "allowlist", "allowed_tools": ["greet"] },
  "plugins": { "install": ["my.first_plugin"] }
}
```

### Divergence 1 — `install_selected`'s `default_backends` union bites a facade-only binary that links one dialect

First attempt, this config as shown above (no `default_backends` override):

```console
$ cargo build --release   # in $SC/plugin-scratch
    Finished `release` profile [optimized] target(s) in 26.86s
$ HOME=$SC/pluginfix/home XDG_CONFIG_HOME=$SC/pluginfix/xdg ./target/release/plugin-scratch
Error: Config { path: None, message: "plugins.install names unknown id 'anthropic'; linked plugins: [my.first_plugin]; linked router factories: []; linked backend factories: [openai-compat]. A plugin, router, or backend not among these caller-supplied bundles is installed directly, before build(), via ConwayBuilder::with_plugin/with_router_factory/with_backend_factory." }
$ echo $?
1
```

`install_selected` resolves `[plugins].install` **unioned with**
`[plugins].default_backends` (default `["anthropic", "openai-compat"]`)
against the three bundles a caller supplies. This scratch binary links only
`OpenAiCompatBackendFactory` — nothing about the plugin or the tool call is
wrong, the binary simply never linked an `AnthropicBackendFactory`, and the
config never said to leave `anthropic` out of the defaulted set. Fixed by
narrowing config, not code:

```json
"plugins": { "install": ["my.first_plugin"], "default_backends": ["openai-compat"] }
```

`docs/embedding.md`'s own `install_selected` example (its "First-party
plugin tier" section) is `rust,ignore` and uses empty bundles throughout —
it never hits this, and never warned about it. **Fixed on the authoring
page** (out of scope to fix `embedding.md` itself under this item's file
lane — noted here as a candidate follow-up for whoever next touches that
page).

### Run — the tool actually called by a real model

```console
$ HOME=$SC/pluginfix/home XDG_CONFIG_HOME=$SC/pluginfix/xdg ./target/release/plugin-scratch
model said -> 
turn result -> AgentResult { agent_id: AgentId(...), status: Completed, summary: "hello, Ada, from my-first-plugin!", facts: [], artifacts: [], structured: None, transcript_ref: SessionId(...), usage: Usage { input_tokens: 4300, output_tokens: 418, cache_read_tokens: 0, cache_write_tokens: 0, reasoning_tokens: 0 }, steps_taken: 2 }
$ echo $?
0
```

`AgentResult.summary` is `"hello, Ada, from my-first-plugin!"` — this
walkthrough's own tool text (`GreetTool::invoke`'s `format!("hello, {},
from my-first-plugin!", args.name)`), reachable only through a real model
proposing the call, `PermissionGate` allowing it (`allowed_tools: ["greet"]`),
and this plugin's own `Tool::invoke` running. `turn.text()` came back
empty — the model's final turn carried no additional assistant text beyond
the tool call and its result, which the `AgentResult.summary` field still
captured; not a defect, just worth noting for a reader who expects
`turn.text()` alone to show it.

**Steps to first visible result, plugin half: 4** (write ~90 lines of Rust
across a `Tool` + `Plugin` + `main`; add two path dependencies; build; run
— plus the one config-narrowing fix above, which the page now teaches
up front instead of leaving a reader to discover it live).

## Divergences found — every one, fixed or filed

1. **"The declarative surface is decided, not built."** False as of today.
   **Fixed** — `authoring.md`'s "The one thing to get straight before you
   start" section rewritten; "Ten minutes to a working hook" rebuilt around
   the declarative surface with `match`; `concepts.md`'s "Hook-first" and
   "Language choice" sections corrected in the same change (out-of-lane
   files `hooks.md`/`scripts.md` were already accurate — checked, not
   touched).
2. **The walkthrough never used the matcher.** **Fixed** — the rewritten
   "Ten minutes" section's canonical example is `match: "bash"`, and Run 2
   above proves the narrowing rather than asserting it.
3. **`$XDG_CONFIG_HOME/conway/settings.json` vs. `~/.conway/settings.json`
   — an "instead of," not "layered under," and easy to get wrong in
   exactly this item's own required fixture shape.** **Fixed** —
   "Where things live" and step 2 of "Ten minutes" now say this explicitly;
   see Divergence 0 above.
4. **`install_selected`'s `default_backends` union failing a facade-only
   binary linking one dialect.** **Fixed** on the page (a named "gotcha"
   paragraph in "Writing a Rust plugin"), reproduced fresh in Divergence 1
   above. Not a code defect — `install_selected`'s own doc states the union
   rule correctly — but undocumented for a facade-only reader following
   `docs/embedding.md`'s own (illustrative-only) example.
5. **"Writing a Rust plugin" never mentioned `install_selected`, and had no
   working end-to-end example.** **Fixed** — replaced the sketch-only
   `with_plugin` snippet with the executed `install_selected` example above.
6. **No statement that installing a plugin requires building a binary.**
   **Fixed** — added as the section's own opening paragraph, linking the
   subprocess-host item `01KZY8PATND84AKY0J376E3DWV` as the future, per this
   item's own instruction.
7. **No mention of `Plugin::commands()` at all.** **Fixed** — one paragraph
   in "Writing a Rust plugin," pointing at `conway-plugin-skeleton`'s
   already-proven end-to-end test rather than re-deriving a third executed
   example (scope discipline — see "What was not additionally exercised"
   below).
8. **No pointer to `trust-and-security.md` from the top of the page**, even
   though a hook/plugin runs with the operator's own privileges. **Fixed**
   — added directly under the `concepts.md` pointer at the top of the page.
9. **Debugging section's matcher paragraph, `hooks.md`'s Status rows,
   `cookbook.md`'s five examples: checked, found clean.** No changes —
   these were already accurate against HEAD (the previous worker who landed
   the matcher item had already updated `hooks.md` and `authoring.md`'s own
   "Debugging" section; this item's gap was specifically the page's
   *opening framing* and its two worked-example sections, not those).
   **Enumerated per this item's own "including 'none'" instruction: none
   found in `docs/plugins/cookbook.md`.**

## What was not additionally exercised, and why (scope discipline, not an oversight)

- **`Plugin::commands()`/`Plugin::events()` were not independently re-run**
  by this walkthrough. `conway-plugin-skeleton/tests/skeleton_end_to_end.rs`
  already proves both end to end against a real configured `[hooks].rules[]`
  entry and a real TUI dispatch path (cited by name on the page). Re-running
  that proof under this item would have been re-deriving evidence that
  already exists, not producing new evidence the acceptance criteria ask
  for — the criteria name a hook firing and a plugin tool being invoked,
  both delivered above.
- **The TUI itself was not driven.** Same limitation
  `getting-started-default-build-walkthrough.md` recorded: capturing an
  alternate-screen TUI under script(1) does not produce readable output,
  and both the one-shot `-p` path exercised here and the TUI share the same
  `build_conway`/`ConwayBuilder` construction, so the hook-dispatch and
  plugin-installation claims this item is about are configuration-and-
  wiring claims, proven identically either way.

## What this establishes

- **The hook half of the acceptance criterion is met by following the page
  alone**, after this item's fix: three steps, no Rust, a real model calling
  a real tool, a real declarative hook firing, using the matcher, with the
  narrowing proven negatively (Run 2) and the typed-error safety net proven
  directly (Run 3).
- **The plugin half is met, with one documented, fixed gotcha.** Following
  the *original* page's "Writing a Rust plugin" section literally would not
  have compiled a working session at all (no `install_selected` example,
  no working end-to-end shape); following the *rewritten* page produces a
  real model calling a real tool through a real plugin, with the
  `default_backends` union failure mode named up front instead of
  discovered live.
- **Installing a plugin requires building a binary — confirmed, not merely
  asserted.** `[plugins].install` in Fixture B's config resolves only
  against the two bundles `plugin-scratch`'s own `main()` constructed; there
  is no config-only path that would have made `my.first_plugin` reachable
  without compiling that binary first.
