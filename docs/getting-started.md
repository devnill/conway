# Getting started

conway is a Rust agent harness: a CLI/TUI (`conway`) and a library, both
built on the same facade. This page gets you from a checkout to a working
session — installing the binary, configuring a model provider, and running
your first prompt.

## Install

You need a recent stable Rust toolchain (see `rust-version` in
`Cargo.toml`). There is no published binary release; build from source:

```console
cargo build -p conway-cli --release
```

The binary lands at `target/release/conway`. The rest of this page assumes
it's on your `PATH`, or that you invoke it by that full path.

## Configure a provider

conway reads its configuration from `.conway/settings.json`, discovered by
walking up from your current directory (so a project-local `.conway/`
takes precedence over `~/.conway/settings.json`, which takes precedence
over conway's built-in defaults). At minimum, `settings.json` needs a
`default_role`, a `[backends.<id>]` entry for your provider, and a
`[roles.<alias>]` entry (named by `default_role`) whose model chain points
at that backend.

**A second file is required alongside it.** conway only routes to a model
if that exact `"backend/model"` pair has a capability entry in
`.conway/models.json` (the file named by `[models.metadata_path]`, which
defaults to `.conway/models.json`). This applies to every backend kind,
including Anthropic — it's not optional. Skip it and you'll see a routing
error before conway ever contacts your provider:

```text
routing error: no candidate for role coder (1 considered): anthropic/claude-sonnet-4-6: capability: capabilities: unknown (backend, model) pair
```

If you've set `[models].probe_on_startup` for an `openai-compat` backend,
that startup probe cannot fill this gap for you: it may only confirm and
narrow the capabilities of a pair `models.json` already lists, never add a
pair on its own — a server reporting a model it serves is not the same as
you declaring it, and conway never routes to a model on the strength of
the server's own say-so alone.

Each entry needs four fields; `max_context_tokens` and `reliability_tier`
affect routing and the TUI's context-window display, `tool_calling` and
`reasoning` are currently informational.

Create both files under `.conway/` in your project directory (or under
`~/.conway/` — or `$XDG_CONFIG_HOME/conway/` if that's set — for a config
that follows you across projects).

### Anthropic

```json
// .conway/settings.json
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

```json
// .conway/models.json
{
  "models": {
    "anthropic/claude-sonnet-4-6": {
      "max_context_tokens": 200000,
      "tool_calling": "yes",
      "reasoning": true,
      "reliability_tier": "verified"
    }
  }
}
```

`api_key_env` names an environment variable to read the key from at
startup, so the key itself never sits in the config file — export it
before running conway:

```console
export ANTHROPIC_API_KEY=sk-ant-...
```

(You can set a literal `api_key` in the file instead, but `api_key_env` is
the better default for anything you might commit.) conway does not inspect
the key's shape — any non-empty value is passed through as-is, which is
what lets an Anthropic-compatible third-party endpoint or a subscription
token work the same way a standard API key does.

### An OpenAI-compatible endpoint (Ollama, llama.cpp, vLLM, and others)

One adapter (`kind: "openai-compat"`) covers every OpenAI-compatible
server, selected by a `dialect`. The built-in dialects are `openai`,
`ollama`, `vllm-hermes`, `lm-studio`, and `llamacpp-server`; each captures
that server's actual wire quirks (streaming behavior, tool-call parsing,
context defaults) so you don't have to. A local Ollama server, reachable
at its OpenAI-compatible `/v1` path:

```json
// .conway/settings.json
{
  "default_role": "coder",
  "backends": {
    "local": {
      "kind": "openai-compat",
      "dialect": "ollama",
      "base_url": "http://localhost:11434/v1"
    }
  },
  "roles": {
    "coder": { "chain": ["local/qwen3:4b"] }
  }
}
```

```json
// .conway/models.json
{
  "models": {
    "local/qwen3:4b": {
      "max_context_tokens": 32768,
      "tool_calling": "yes",
      "reasoning": false,
      "reliability_tier": "community"
    }
  }
}
```

Swap `dialect`/`base_url` for llama.cpp's or vLLM's own address to point
at those instead. `api_key` is optional for a server that doesn't require
one. If your server needs behavior none of the five built-in dialects
capture — a custom `chat_path`, a different tool-call style — declare a
named profile in `.conway/profiles.toml` and reference its id as the
`dialect`; see [`providers.md`](providers.md#declarative-provider-profiles)
for the full profile schema.

## `--cwd` and `--root`

These two flags are easy to conflate, and mixing them up is the mistake
most likely to cost you real damage — read this before you set either one.

- **`--cwd <DIR>`** sets the process's (and the root agent's own) working
  directory: where the agent *works*, and where a relative tool argument
  starts from. It is **not** a security boundary. It never limits what a
  tool call can reach — an agent given `--cwd /home/alice/project` can
  still read or write `/etc/passwd` if a tool call names that absolute
  path.
- **`--root <DIR>`** confines the root agent — and, by inheritance, every
  subagent it forks or spawns — to that directory: any tool call whose
  path argument resolves outside it is denied before your permission gate
  is ever consulted. This **is** the security boundary. A subagent can
  only narrow its inherited root further, never widen it.

Omit `--root` and the agent is **unconfined**: it can reach anywhere your
user account can reach, exactly like every invocation before this flag
existed. Set `--root` whenever you want a hard guarantee that conway
cannot touch anything outside a directory tree, regardless of what a tool
call asks for or what permission you grant it.

**When you set `--root`, also pass `--cwd` as an absolute path.** conway
must be able to verify the agent's own working directory sits inside the
root before it will start; a relative `--cwd` (or no `--cwd` at all, which
leaves the working directory at its default) can't be checked against the
root and conway refuses to start rather than guess:

```console
conway --cwd /home/alice/project --root /home/alice/project
```

## Enabling bash (shell commands)

**bash is off by default.** conway's `fs` (read/write/edit/glob/grep/cd),
`subagent`, and `report` built-in tools are registered automatically; bash
— arbitrary shell command execution — is not, and requires a deliberate
opt-in. This applies to the interactive TUI and to a library embedder's own
`ConwayBuilder::build()`; it does *not* apply to `-p`/`--print` one-shot
mode, which already gated bash behind `--allowed-tools` (see "Running
non-interactively" below) and is unaffected by this default.

To turn bash on for the TUI (or any run that loads `settings.json`), add
`"conway.shell"` to `tools.builtin_plugins`:

```json
// .conway/settings.json
{
  "tools": {
    "builtin_plugins": ["conway.fs", "conway.subagent", "conway.report", "conway.shell"]
  }
}
```

This is the full replacement list, not an append — `builtin_plugins`
defaults to `["conway.fs", "conway.subagent", "conway.report"]` (every
built-in except bash), so the snippet above is that default plus
`"conway.shell"`. A library embedder can do the same thing without a config
file: `ConwayBuilder::with_builtin_plugins(PluginSelection::All)` (or an
`Only`/`AllExcept` selection naming `"conway.shell"`) before `.build()`.

## Your first session

Run `conway` with no arguments to start the interactive TUI:

```console
conway
```

You'll see an empty input box at the bottom of the screen (with the
placeholder text `Type a message, or / for commands`) and a status line
below it. Type a prompt — try something that needs a tool, like "what's
in this directory?" — and press `Enter`.

As the agent works, the status line's activity field shows what's
happening (`⠋ thinking…`, then the response streaming in), and any tool
call the model proposes appears in the transcript. If your permission mode
is the default (`prompt`), a tool call pauses for your decision. The
example below shows a `bash` call — see "Enabling bash" above if you
haven't opted in yet and want to follow along with a shell command
specifically; every other built-in tool (`read`, `write`, `glob`, …) is on
by default and prompts the same way:

```
┌ PERMISSION REQUIRED ────────────────────────────────────────────┐
│echo pong                                                        │
│[y] once  [a] always  [p] pattern  [n] deny  [Esc] deny w/ feedback│
└───────────────────────────────────────────────────────────────────┘
```

This is asking whether conway may run that exact command right now. `[y]`
allows it once; `[a]` allows it and remembers the decision for the rest of
the session; `[p]` grants a narrow, reusable pattern (a prefix match, so
"allow `git status`" doesn't also allow `git push`); `[n]` denies it;
`Esc` denies it and tells the model to try a different approach. Press `y`
to let it run and watch the tool call resolve in the transcript. See
[`interactive.md`](interactive.md) for the full TUI reference and
[`permissions.md`](permissions.md) for how pattern grants, project trust,
and persistence work.

## Running non-interactively

For scripting, `-p`/`--print` runs one prompt and exits:

```console
conway -p "what's in this directory? use bash" --allowed-tools bash
```

One-shot mode can't prompt you for a permission decision, so it fails
closed by default: with `--allowed-tools` empty, every tool call is
denied and you get an answer-only response with no side effects. List the
tools you want to allow explicitly, as above.

## Next steps

- [`interactive.md`](interactive.md) — driving the TUI: keys, slash
  commands, the status line, and what you see during a turn.
- [`permissions.md`](permissions.md) — permission modes, pattern grants,
  and project-file trust.
