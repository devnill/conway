# Configuring providers

This page covers pointing conway at the models you actually have access
to: what each provider kind needs, and how to add a provider conway
doesn't already know about without waiting for a code change. For the
five-minute path to a first working session (including the mandatory
`.conway/models.json` file every backend needs), see
[`getting-started.md`](getting-started.md); this page goes deeper on the
provider side. For controlling *which* configured model actually serves a
given request, see [`routing.md`](routing.md).

## Where a backend is declared

Every provider is a `[backends.<id>]` entry in `.conway/settings.json`,
discovered project-first then global — the nearest `.conway/settings.json`
walking up from your current directory, then `~/.conway/settings.json` (or
`$XDG_CONFIG_HOME/conway/settings.json`), then conway's built-in defaults.
`<id>` is a name you choose, not a fixed provider identifier, so an
Anthropic-compatible third-party endpoint can be named for what it
actually is (`kimi`, `internal-proxy`) and sit alongside a real
`anthropic` backend in the same config.

**`kind` is an open name, not a closed set of two.** `conway` resolves it
against every `BackendFactory` an embedder has registered
(`ConwayBuilder::with_backend_factory`), falling back to the two adapters
this facade still compiles in for any name they claim:

| `kind` | Adapter | Selects |
| --- | --- | --- |
| `"anthropic"` | The native Anthropic Messages API | any endpoint speaking that wire format |
| `"openai-compat"` | One adapter for every OpenAI-compatible server | a `dialect` (built-in or your own [profile](#declarative-provider-profiles)) |

**Which of these two you use is configuration, not a build option.** The
`conway` binary and library both ship with both adapters compiled in
always — there is no cargo feature to enable, and never was a supported
one for very long: an operator picks one of them by writing a
`[backends.<id>]` entry naming it, not by recompiling. **A third `kind` is
a library extension point, not a config typo.** An embedder registers a
`BackendFactory` under whatever kind name it wants
(`ConwayBuilder::with_backend_factory`, board item
01KZHF0RBKJZZC68F7GPFB347Q/01KZHF1E85MS1VF4YH8CDNCP9Z) and a
`[backends.<id>]` entry naming that kind is resolved to it — the same
extension surface a third-party plugin author already uses, not a
build-time switch on this crate. A `kind` neither a registered factory nor
the two built-ins above claims is a hard `build()` error naming the
offending value and listing every kind the running binary actually
recognises — never a silently ignored entry.

An entry's keys beyond `kind`/`api_key`/`api_key_env`/`base_url`/`dialect`/
`stream_tools` are not rejected: they are captured verbatim and handed to
whichever factory built that entry's backend, so a third-party kind can
carry its own configuration without this crate knowing its shape in
advance. The cost: a typo in one of the six named keys above (e.g.
`base_ur1`) is no longer caught at load time — it is silently captured
alongside any real custom key rather than erroring. Double-check spelling
against the fields named above; `conway` cannot catch that typo for you.

## Anthropic and Anthropic-compatible endpoints

```json
// .conway/settings.json
{
  "backends": {
    "anthropic": {
      "kind": "anthropic",
      "api_key_env": "ANTHROPIC_API_KEY"
    }
  }
}
```

`AnthropicBackend` isn't hardwired to `api.anthropic.com`: set `base_url`
to point it at any provider serving an Anthropic-shaped `/v1/messages`,
including one that lives under a path prefix rather than at the host root
(`https://host/coding/` resolves to `https://host/coding/v1/messages`,
with or without the trailing slash). conway does not inspect the shape of
the key — a metered API key, a coding-plan subscription key, or a
self-hosted shim's own token are all passed through as-is; an unusable key
surfaces as that provider's own auth error rather than a guess on
conway's part.

### Kimi coding plan

Kimi's coding plan is served over an Anthropic-compatible endpoint, so it
needs no dedicated adapter — just a `base_url` and its own env var:

```json
// .conway/settings.json
{
  "backends": {
    "kimi": {
      "kind": "anthropic",
      "base_url": "https://api.kimi.com/coding/",
      "api_key_env": "KIMI_API_KEY"
    }
  }
}
```

Two context variants ship in conway's bundled metadata (used when you
don't supply your own `models.json` entry):

| Model id | Context window |
| --- | --- |
| `k3-256k` | 262,144 tokens |
| `k3[1m]` | 1,048,576 tokens |

The `[1m]` suffix is literal — part of the id the provider expects.

## Credentials

| Field | Effect |
| --- | --- |
| `api_key_env` | Names an environment variable to read the key from at startup. The key itself never sits in the config file. |
| `api_key` | A literal key in the config file. Fine for a file you won't commit; `api_key_env` is the better default for anything you will. |

Set exactly one — `api_key` and `api_key_env` both non-empty on the same
backend is a config error naming the backend. An `api_key_env` naming a
variable that isn't set at startup is also a named, fail-loud error;
conway never silently falls back to "no credential."

## OpenAI-compatible endpoints

One adapter, `kind: "openai-compat"`, covers every OpenAI-compatible
server. `dialect` selects a [profile](#declarative-provider-profiles) —
either a built-in or one you've declared — that parameterizes that
adapter's request/response shaping: which fields it sends, how it parses
streamed tool calls, and the model's baseline capabilities before
`models.json` or a live probe override them.

| `dialect` | Server | Notable behavior |
| --- | --- | --- |
| `openai` | OpenAI's own API, or a fully-compatible clone | Streams usage, sends `parallel_tool_calls`, `max_completion_tokens` |
| `ollama` | Ollama | Flattens multi-block content, tolerant tool-call parsing |
| `vllm-hermes` | vLLM serving a Hermes-style tool-call template | Scans `delta.content` for an inline `<tool_call>` block some builds emit instead of a structured delta |
| `lm-studio` | LM Studio | No structured output, tolerant parsing |
| `llamacpp-server` | `llama-server` | Grammar-constrained (GBNF) structured output, not JSON-schema |
| `kimi` | Moonshot's platform API (see below) | Structured tool calls, array (not flattened) multi-block content |

A local Ollama server, reachable at its OpenAI-compatible path — note the
**`/v1` suffix**; omitting it 404s:

```json
// .conway/settings.json
{
  "backends": {
    "local": {
      "kind": "openai-compat",
      "dialect": "ollama",
      "base_url": "http://localhost:11434/v1"
    }
  }
}
```

Swap `dialect`/`base_url` for llama.cpp's or vLLM's own address to point
at those instead. `api_key` is optional for a server that doesn't require
one.

### Kimi (Moonshot platform API)

`kimi` names Moonshot's platform API (`https://api.moonshot.ai/v1`) — a
different product from the Kimi **coding plan** above (that one is
Anthropic-compatible; this one is OpenAI-compatible). Don't confuse the
two `base_url`s:

```json
// .conway/settings.json
{
  "backends": {
    "kimi": {
      "kind": "openai-compat",
      "base_url": "https://api.moonshot.ai/v1",
      "dialect": "kimi",
      "api_key_env": "MOONSHOT_API_KEY"
    }
  }
}
```

Three provider quirks are deliberately **not** worked around — conway
passes values through and lets the provider's own error or behavior be
loud rather than silently rewriting a request: temperature is accepted
outside Kimi's documented `[0, 1]` range and Kimi's own validation error
is what you see; `tool_choice: "required"` is rejected by k2.5/k2.6 but
accepted by k3 (a per-model, not per-provider, distinction); prompt
caching is automatic above a 256-token prompt, which the `kimi` profile
documents (`min_prefix_tokens = 256`, see [caching](routing.md#prompt-caching-economics-not-correctness))
but never gates on.

## Declarative provider profiles

A provider profile is data, not code: `conway_backends::profile::Profile`
parameterizes the one `openai-compat` adapter, so a new provider — or your
own local server's actual quirks — is added by writing a
`.conway/profiles.toml` file, never by waiting on a conway release.

### Discovery and precedence

Identical layering to permission rules: project-scoped first, then
global.

1. `.conway/profiles.toml` next to the `.conway/settings.json` conway
   discovered (or `<cwd>/.conway/profiles.toml` if no project config
   exists yet).
2. `~/.conway/profiles.toml` (or `$XDG_CONFIG_HOME/conway/profiles.toml`).
3. The six built-ins compiled into conway (`openai`, `ollama`,
   `vllm_hermes`, `lm_studio`, `llama_cpp_server`, `kimi`).

A profile file's `id` decides whether it's new or an override: a
project-scoped `[[profile]] id = "ollama"` **shadows** the built-in
`ollama` profile entirely for that process, field by field per the table
below (every field you omit falls back to that field's own conservative
default, not to the shadowed profile's value). A global file's profile is
in turn shadowed by a project one with the same id. This is a deliberate
override mechanism, not a collision error.

### The `Profile` fields

Every field is optional; a profile with only `id` set loads and behaves
like a maximally conservative, unfamiliar server. Verified against
`crates/conway-backends/src/profile.rs`'s `ProfileRaw`:

| Field | Default | Effect |
| --- | --- | --- |
| `id` | *(required)* | The name a backend's `dialect` selects this profile by. Never empty. |
| `chat_path` | `/chat/completions` | The chat-completions endpoint, relative to `base_url`. |
| `supports_stream_options` | `false` | Send `"stream_options":{"include_usage":true}` on a streamed request. |
| `flatten_multiblock_user` | `true` | Flatten a multi-block user message to one string (`true`) or keep it as an array (`false`). |
| `sends_parallel_tool_calls` | `false` | Emit the `"parallel_tool_calls"` request field. |
| `uses_max_completion_tokens` | `false` | Name the output-token-limit field `max_completion_tokens` (`true`) or `max_tokens` (`false`). |
| `sends_reasoning_effort` | `false` | Emit a caller-supplied `"reasoning_effort"` field. |
| `tool_call_style` | `"tolerant"` | `"structured"` (canonical deltas only), `"tolerant"` (also accepts a complete JSON-object `arguments` value, not just a string fragment), or `"hermes_text_fallback"` (also scans `delta.content` for an inline `<tool_call>` block). |
| `cache` | `{ kind = "none" }` | Baseline caching behavior — see [prompt caching](routing.md#prompt-caching-economics-not-correctness). `{ kind = "implicit_prefix", min_prefix_tokens = N }`, `{ kind = "explicit_breakpoints", max_breakpoints, ttls }`, or `{ kind = "none" }`. |
| `tool_calling` | `"non_streaming"` | Baseline tool-calling support: `"none"`, `"non_streaming"`, or `"streaming"` (with `structured_output`-style variants for validated streaming). |
| `max_context_tokens` | `32768` | Baseline context window, in tokens. A `models.json` entry for the model is authoritative and always wins; a live startup probe (`probe_on_startup`) only fills in a window for a model that neither `models.json` nor conway's bundled model metadata already describes. |
| `structured_output` | `"none"` | `"none"`, `"json_schema"`, or `"grammar"`. |
| `parallel_tool_calls` | `false` | Baseline "can an undescribed model of this provider make multiple tool calls in one turn" capability. |
| `reliability_tier` | `"unknown"` | `"unknown"`, `"community"`, or `"verified"`. Feeds routing's capability floor if a role sets one. |

A malformed profile — invalid TOML, an unrecognized field, an empty `id`
— is always a loud, typed error naming the file and the offending field,
never a silent fallback or a panic:

```console
conway routes explain coder
```

```text
conway: error: failed to load provider profiles from ./.conway/profiles.toml: ...
  unknown field `sends_paralel_tool_calls`, expected one of `id`, `chat_path`, ...
```

### Adding a provider

```toml
# .conway/profiles.toml
[[profile]]
id = "my-local-server"
supports_stream_options = true
tool_call_style = "tolerant"
max_context_tokens = 65536
reliability_tier = "community"

[profile.cache]
kind = "implicit_prefix"
min_prefix_tokens = 0
```

A backend selects it the same way it selects a built-in — `dialect`
names the profile's `id`:

```json
// .conway/settings.json
{
  "backends": {
    "local": {
      "kind": "openai-compat",
      "base_url": "http://localhost:8000/v1",
      "dialect": "my-local-server"
    }
  }
}
```

### Seeing what's loaded

`conway_backends::profile::ProfileStore::list()` is a real inspection
surface at the library level: every loaded profile, its origin
(`ProfileOrigin::BuiltIn` or `ProfileOrigin::File(path)`), and — when it
replaced an existing entry — what it shadowed. An override nobody can see
is a trap, and this is how a caller embedding conway avoids that trap.

**Verified: the `conway` binary itself does not expose this today.** No
subcommand lists loaded profiles or their origin, and no `-v`/`-vv`
tracing output names a resolved profile's id or source file — checked
directly against every profile-loading call site in `conway::builder`
and confirmed by running (a well-formed override that changed a
backend's wire behavior loaded silently even at `-vv`, indistinguishable
from no override at all). The one thing that *is* loud is a broken file: a typo'd
field name or empty `id` fails startup with the path and field named, as
shown above — that confirms a file is being read, but tells you nothing
about a *valid* override actually taking effect. Until a CLI surface for
this exists, your options are: know the precedence rules above and give
your override an `id` that unambiguously matches (or doesn't match) a
built-in's on purpose, or, if you're embedding conway as a library, call
`ProfileStore::list()` directly.

## How it fits together

A backend entry alone doesn't make a model routable. Every `(backend,
model)` pair a role's chain names also needs an entry in
`.conway/models.json` — see [`getting-started.md`](getting-started.md)
for the file and the exact routing error you get if you skip it — and
[`routing.md`](routing.md) for how conway picks among the backends you've
configured here.
