# Configuring providers

This page covers pointing conway at the models you actually have access
to: what each provider kind needs, and how to add a provider conway
doesn't already know about without waiting for a code change. For the
five-minute path to a first working session (including where
`.conway/models.json` fits — optional on a default build, required once the
routing plugin is installed), see
[`getting-started.md`](getting-started.md); this page goes deeper on the
provider side. For controlling *which* configured model actually serves a
given request, see [`routing.md`](routing.md).

## Where a backend is declared

Every provider is a `backends.<id>` entry in `.conway/settings.json`,
discovered project-first then global — the nearest `.conway/settings.json`
walking up from your current directory, then `~/.conway/settings.json` (or
`$CONWAY_CONFIG_DIR/settings.json`), then conway's built-in defaults.
`<id>` is a name you choose, not a fixed provider identifier, so an
Anthropic-compatible third-party endpoint can be named for what it
actually is (`kimi`, `internal-proxy`) and sit alongside a real
`anthropic` backend in the same config.

**`kind` is an open name, not a closed set of two.** `conway` (the facade)
resolves it against every `BackendFactory` an embedder has registered
(`ConwayBuilder::with_backend_factory`) — and *only* those; the facade
itself compiles in no fallback adapter of its own, and its own resolution
path names neither `"anthropic"` nor `"openai-compat"`:

| `kind` | Adapter | Selects |
| --- | --- | --- |
| `"anthropic"` | The native Anthropic Messages API | any endpoint speaking that wire format |
| `"openai-compat"` | One adapter for every OpenAI-compatible server | a `dialect` (built-in or your own [profile](#declarative-provider-profiles)) |

**Where these two `BackendFactory`s live, and how they reach a build.**
Both are `conway-plugin-backends`, a first-party plugin crate (like
`conway-plugin-routing`, see [`embedding.md`](embedding.md#first-party-plugin-tier))
under `crates/`, published as `conway_plugin_backends::AnthropicBackendFactory`/
`OpenAiCompatBackendFactory` (kind ids `ANTHROPIC_KIND`/`OPENAI_COMPAT_KIND`
— the same two strings above). Getting from "this crate exists" to "your
`settings.json` resolves `kind = "anthropic"`" differs by which binary or
embedding path you're on:

- **The shipped `conway` binary** links `conway-plugin-backends` and
  attaches both factories by default — no `plugins.install` entry
  required, unlike every other first-party plugin. `plugins.
  default_backends` (default: `["anthropic", "openai-compat"]`) is the
  declarative key that makes this happen (owner): a backend, unlike a router or a tool
  plugin, has no honest degenerate fallback — an install with no backend
  attached cannot reach a model at all — so this one pair ships on rather
  than opt-in. An operator declines a specific kind by removing its id
  from `default_backends` in `settings.json`.
- **A library embedder using `conway` alone** (no `conway-cli`) depends on
  `conway-plugin-backends` directly and calls `ConwayBuilder::
  with_backend_factory(Arc::new(conway_plugin_backends::
  AnthropicBackendFactory))` (and/or the OpenAI-compatible one) before
  `build()` — the identical mechanism a third-party kind uses, since a
  first-party backend gets exactly the surface a third party gets.
  `conway` itself never does this for you.

**Declining a shipped dialect means something observable, not just a
smaller install.** Removing a kind
id from `default_backends` — say, dropping `"openai-compat"` — does two
things: no factory for that kind attaches (the install mechanism above,
unchanged), *and* the CLI tells `build()` that kind was deliberately
declined rather than simply never installed. If a `backends.<id>` entry
still names it, `build()` still fails — a build with zero backends can
never reach a model, so there is no silent smaller-but-working outcome to
fall back to (`PluginsConfig::default_backends`'s own doc) — but the
message reads as a decline, not a typo:

```json
// .conway/settings.json
{
  "plugins": { "default_backends": ["anthropic"] },
  "backends": {
    "mock": { "kind": "openai-compat", "base_url": "https://example.invalid/" }
  }
}
```

```text
conway: error: backend 'mock': kind 'openai-compat' was declined, not
installed for this build. This is a DIFFERENT diagnosis than a kind this
build has never heard of at all: 'openai-compat' is a recognised dialect
that plugins.default_backends/plugins.install no longer names (or that an
embedder chose not to attach via ConwayBuilder::with_backend_factory).
Installed kinds: [anthropic]. Add 'openai-compat' back to
plugins.default_backends (or plugins.install), or call
ConwayBuilder::with_backend_factory for it, before build().
```

A kind this build has genuinely never heard of at all — a typo, or a
third-party kind nobody ever registered a factory for — still gets the
plain **unknown kind** message (`recognised kinds: [...]`, no mention of a
decline). The two are deliberately distinguishable text, because they are
different situations: one is "you turned this off," the other is "conway
doesn't know what this is."

A library embedder gets the identical diagnosis by calling
`ConwayBuilder::with_declined_backend_kinds(vec!["openai-compat".into()])`
before `build()` — the builder-method equivalent of removing an id from
`default_backends`, reaching the same code path `conway-cli` uses
internally. It is purely diagnostic: it never attaches or removes a
factory itself, only changes which of the two messages an unresolved
`kind` gets.

**A third `kind` is a library extension point, not a config typo.** An
embedder registers a `BackendFactory` under whatever kind name it wants
(`ConwayBuilder::with_backend_factory`) and a
`backends.<id>` entry naming that kind is resolved to it — the same
extension surface `conway-plugin-backends`'s own two factories use, not a
privileged, build-time switch on this crate. A `kind` no registered factory
claims is a hard `build()` error naming the offending value and listing
every kind the running binary actually recognises — never a silently
ignored entry.

An entry's keys beyond `kind`/`api_key`/`api_key_env`/`base_url`/`dialect`/
`stream_tools`/`local` are not rejected: they are captured verbatim and
handed to whichever factory built that entry's backend, so a third-party
kind can carry its own configuration without this crate knowing its shape in
advance. Concretely: they land in `BackendBuildContext::extra` (a
`BTreeMap<String, serde_json::Value>`), the exact argument
`BackendFactory::build` receives — see ["Writing your own
adapter"](#writing-your-own-adapter) below for a worked example that reads
one back out and lets it change the backend's own behaviour. The cost: a
typo in one of the seven named keys above (e.g. `base_ur1`) is no longer
caught at load time — it is silently captured alongside any real custom key
rather than erroring. Double-check spelling against the fields named above;
`conway` cannot catch that typo for you.

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

### Wire version and header overrides

The `"anthropic"` kind reads two keys out of its own `extra` (the same
catch-all map described [above](#where-a-backend-is-declared) — every key
beyond `kind`/`api_key`/`api_key_env`/`base_url`/`dialect`/`stream_tools`/
`local`):

| `extra` key | Effect |
| --- | --- |
| `anthropic_version` | Overrides the `anthropic-version` header sent with every request. Defaults to `2023-06-01` when unset — unchanged from every config written before this key existed. |
| `headers` | An object of header name/value string pairs, sent alongside (never in place of) the two headers `AnthropicBackend` always sets (`x-api-key`, `anthropic-version`) — for a provider-specific header like `anthropic-beta`. |

```json
// .conway/settings.json
{
  "backends": {
    "anthropic": {
      "kind": "anthropic",
      "api_key_env": "ANTHROPIC_API_KEY",
      "anthropic_version": "2024-01-01",
      "headers": { "anthropic-beta": "some-feature-flag" }
    }
  }
}
```

**Any other key under this kind's `extra` is a rejected, named build
error, not a silently ignored one.** A typo in one of these two names (or
any key this kind does not understand) fails `build()` naming the offending
key, rather than accepting a configuration that quietly does nothing —
the same "an operator who bought a behaviour deserves more than silence"
reasoning `AnthropicBackendFactory::resolve_fields` documents in its own
doc comment.

**These two keys are also selectable as a reusable, named profile** — the
same [declarative provider profiles](#declarative-provider-profiles)
mechanism `"openai-compat"` uses, generalized so `"anthropic"` can name a
bundle once and reuse it across several backend entries rather than
repeating `anthropic_version`/`headers` in every one:

```toml
# .conway/profiles.toml
[[profile]]
id = "acme-gateway"
anthropic_version = "2024-01-01"

[profile.headers]
"anthropic-beta" = "some-feature-flag"
```

```json
// .conway/settings.json
{
  "backends": {
    "acme": {
      "kind": "anthropic",
      "base_url": "https://gateway.example.com",
      "api_key_env": "ACME_API_KEY",
      "dialect": "acme-gateway"
    }
  }
}
```

Unlike `"openai-compat"`, where `dialect` is *required* (it selects the
whole wire-behavior profile the adapter needs to function at all), it is
*optional* here — the Messages API has exactly one wire shape, so an
`"anthropic"` entry naming no profile is a complete, valid configuration on
its own, exactly as before this mechanism existed. `"anthropic"` ships one
built-in profile of its own (`kimi-code`, unlike `"openai-compat"`'s six —
see [Kimi coding plan](#kimi-coding-plan) below for what it does and does
not set, and why); every other `"anthropic"` profile is one you declare.
When both a selected profile and
this entry's own inline `extra` set the same key, **`extra` wins** — the
one precedence rule both kinds' profile support share, see
[Precedence: profile vs. `extra` vs. defaults](#precedence-profile-vs-extra-vs-defaults)
below.

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

**`"anthropic"`'s one shipped built-in profile names this same
endpoint.** `dialect: "kimi-code"` resolves out of the box, with no
`.conway/profiles.toml` required — equivalent to the entry above (today
it sets neither `anthropic_version` nor `headers`; conway has no verified
evidence Kimi's gateway needs either overridden, so it ships none rather
than guessing — see `crates/conway-plugin-backends/src/factory.rs`'s
`ANTHROPIC_BUILT_IN_PROFILES` for the other endpoints considered and left
out of this set for the same reason). Naming it buys one thing the
inline form doesn't: a single place a future real Kimi-specific override
would land, inherited by every backend entry that selects it rather than
copied into each one:

```json
// .conway/settings.json
{
  "backends": {
    "kimi": {
      "kind": "anthropic",
      "base_url": "https://api.kimi.com/coding/",
      "api_key_env": "KIMI_API_KEY",
      "dialect": "kimi-code"
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

## Managing providers from the TUI

Everything above is a `settings.json` edit. The interactive TUI's
`/settings` menu also has its own **providers** section — the same data,
a second way to reach it, not a second implementation of it:

- **Every configured provider is listed**, by id and `kind`, with its
  current status: `working`, `not working: <reason>` (the reason names
  the actionable cause — an unset `api_key_env` variable by name, or the
  URL that refused a connection), `undetermined: <reason>` (conway
  genuinely cannot tell — a local server that needs no credential, or a
  probe that hasn't answered yet — and this is deliberately never shown
  as a failure), or `checking...` while the very first classification is
  still in flight. Opening this section probes every endpoint live
  (including remote ones, unlike the quieter check conway runs at
  startup), on the understanding that you opened it because you wanted
  to know right now.
- **Adding a provider** offers the same three hosted choices the first-run
  flow offers when conway starts with nothing configured (Anthropic,
  OpenAI, or Ollama Cloud) — reusing your already-set credential
  environment variable in one keystroke if it's there, or prompting for
  the key (never echoed, never logged) if it isn't. It writes the same
  `backends.<id>` entry a hand-edit would, to your user-scope
  `settings.json`.
- **Removing a provider** checks whether any `roles.*.chain` entry still
  names it first. If one does, the removal is refused and the affected
  role(s) are named — nothing is written — rather than silently leaving a
  role pointing at a provider that no longer exists.

This section doesn't do model pinning or fallback-chain editing — see
[`routing.md`](routing.md) for that.

## Locality

`backends.<id>.local` (bool, default `false`) declares whether a backend's
inference never leaves the machine conway runs on. This exists for exactly
one reason: some consumers — a permission guard classifying a tool call
before it runs is the motivating one, see
[`docs/vision/DESIGN-permission-modes.md`](../docs/vision/DESIGN-permission-modes.md)
§2e/§4 — need to know *"which of my backends can see my file contents"*
before they hand a prompt to one, and nothing before this existed to ask.

```json
// .conway/settings.json
{
  "backends": {
    "ollama": {
      "kind": "openai-compat",
      "dialect": "ollama",
      "base_url": "http://localhost:11434/v1",
      "local": true
    }
  }
}
```

**This is declared, not inferred — say it plainly, because the other option
was on the table and was rejected.** `local` is exactly the boolean you
write. Nothing in conway reads `base_url` and *guesses* at this field: a
`base_url` of `http://localhost:11434/v1` sets nothing by itself, and a
backend with no `local` key at all defaults to `false` even if its
`base_url` says `localhost`. Two backend ids in the [worked
example](routing.md#worked-example-a-fallback-chain-across-a-cloud-and-a-local-provider)
are both named things — `anthropic`, `local` — but only one of the two
carries `"local": true`; the *id* `local` was always a name an operator
chose, never something conway read meaning from, and that has not changed.

**Why not infer it from `base_url` — the case that breaks every
alternative.** `localhost`, `127.0.0.1`, `::1`, and a bare `.local` hostname
all look like clean predicates until an SSH tunnel is in the picture:
`ssh -L 11434:localhost:11434 some-remote-box` makes a server on
`some-remote-box` answer at `http://localhost:11434/v1` from this machine's
point of view, and no string-shaped test on `base_url` can tell that apart
from a real local Ollama install — the bytes on the wire are identical
either way. An explicit `local` field does not make that ambiguity
disappear (an operator can mis-declare a tunneled backend `local: true` just
as wrongly as a heuristic could infer it), but it stops conway from
manufacturing false confidence out of a URL string nobody asked it to
interpret, and it means a wrong answer is traceable to one line an operator
wrote rather than to logic buried in a router.

**This is defence in depth, not a correctness guarantee, and the scope of
what it protects is narrower than "safe."** No routing behaviour changes
because of this field — `conway`'s router does not prefer, skip, or reorder
a chain on locality, and a chain that falls through from a local candidate
to a non-local one keeps doing so; refusing that fallthrough is a
*consumer's* policy, not something this field or the router enforces on its
own. A permission guard checking a role's chain (`role_is_local` in
`conway::config`, keyed by the backends this page describes) can refuse to
run when a candidate isn't local — but if it is wrong, what it exposes is
the *prompt* handed to the non-local candidate: the command, its arguments,
the paths involved. It cannot widen what an operator has already
authorised (`HookPermissionVerdict` has no `Allow` variant), so a
misdeclared `local` is a privacy leak of the prompt, never a permission
escalation.

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
| `ollama` | Ollama, local or [Cloud](#ollama-cloud) — both are the same project's wire behavior | Flattens multi-block content, tolerant tool-call parsing |
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

### Ollama Cloud

Ollama's hosted offering, served over an OpenAI-compatible endpoint —
`https://ollama.com/v1`, note the **`/v1` suffix** and **not**
`/api/v1` (that path is Ollama's native, non-OpenAI-compat surface).
Also one of the three choices the guided first-run flow and `/settings`'
providers section both offer directly, needing no hand-edit at all:

```json
// .conway/settings.json
{
  "backends": {
    "ollama_cloud": {
      "kind": "openai-compat",
      "dialect": "ollama",
      "base_url": "https://ollama.com/v1",
      "api_key_env": "OLLAMA_API_KEY"
    }
  },
  "roles": {
    "coder": { "chain": ["ollama_cloud/glm-5.2"] }
  }
}
```

**`dialect` is `"ollama"`, the same profile the local server above uses —
not `"openai"`.** The two servers are different deployments of the same
project and share the same tool-call/content-shaping quirks, which is what
`dialect` actually selects.

`https://ollama.com/v1` is confirmed against the live server (2026-08-30):
`GET /v1/models` returns 200 with a real model roster; an unauthenticated
`POST /v1/chat/completions` returns 401. **This is not stated anywhere in
`docs.ollama.com`'s own prose** — those pages document only the local
`http://localhost:11434/v1` form — so don't "correct" this back to a
documented-looking value without re-checking the live server first.

`glm-5.2` above is a deliberate choice, not the smallest or cheapest model
in Ollama Cloud's roster (`gpt-oss:20b` is smaller): it is the model
conway's `openai-compat` wire layer has actually been debugged against — see
the `content: ""` (never `null`) comment in
`crates/conway-plugin-backends/src/openai_compat/wire.rs`'s tool-call
message builder, which exists specifically because Ollama Cloud's `glm-5.2`
rejects a `null` content field on a tool-call-only turn. Model ids on
Ollama Cloud are expected to age — `docs.ollama.com/cloud` states Ollama
"will occasionally deprecate and retire older cloud models" — so treat this
one as a snapshot, not a permanent recommendation.

**`glm-5.2`'s context window ships bundled, so this exact config — the
guided first-run flow's own output, with no `models.json` at all — already
declares a real window, not the `"ollama"` profile's 32,768-token floor.**
`ollama.com/library/glm-5.2` documents "a truly usable 1M-token context
window"; conway's bundled per-model metadata
(`crates/conway-plugin-backends/src/model_metadata.rs`'s `DEFAULTS`) carries
`glm-5.2` at `1,000,000` tokens for exactly this reason — see ["Where a
context ceiling comes from"](#where-a-context-ceiling-comes-from) below for
the full precedence story and why this matters even before you've written a
`models.json` entry of your own.

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

A provider profile is data, not code: `conway_plugin_backends::profile::Profile`
parameterizes the `openai-compat` adapter, so a new provider — or your
own local server's actual quirks — is added by writing a
`.conway/profiles.toml` file, never by waiting on a conway release.
`"anthropic"`'s own, much smaller profile vocabulary
(`anthropic_version`/`headers` — [above](#wire-version-and-header-overrides))
is selected the same way, through the same file-discovery/precedence rules
below, but is a genuinely different set of fields — see
[One facility, two vocabularies](#one-facility-two-vocabularies) for why a
single `.conway/profiles.toml` cannot mix entries for both kinds.

### Discovery and precedence

Identical layering to permission rules: project-scoped first, then
global.

1. `.conway/profiles.toml` next to the `.conway/settings.json` conway
   discovered (or `<cwd>/.conway/profiles.toml` if no project config
   exists yet).
2. `~/.conway/profiles.toml` (or `$CONWAY_CONFIG_DIR/profiles.toml`).
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
`crates/conway-plugin-backends/src/profile.rs`'s `ProfileRaw`:

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
| `max_context_tokens` | `32768` | **A last-resort floor, not a routine per-model answer** — see ["Where a context ceiling comes from"](#where-a-context-ceiling-comes-from) immediately below. A context window is a property of a *model*, not a wire dialect (Ollama alone serves 4K- to 1M-token models under this identical `"ollama"` profile), so this number is reached only when nothing more specific — a `models.json` entry, conway's bundled per-model metadata, or a `probe_on_startup` discovery hint — says anything about the model in question. |
| `structured_output` | `"none"` | `"none"`, `"json_schema"`, or `"grammar"`. |
| `parallel_tool_calls` | `false` | Baseline "can an undescribed model of this provider make multiple tool calls in one turn" capability. |
| `reliability_tier` | `"unknown"` | `"unknown"`, `"community"`, or `"verified"`. Feeds routing's capability floor if a role sets one. |

### Where a context ceiling comes from

A `max_context_tokens` conway actually uses for a `(backend, model)` pair
is resolved through three layers, checked in this order — the first one
that says anything about the model wins:

1. **A `models.json`/config-level override.** `.conway/models.json`'s
   `max_context_tokens` for this exact `"backend/model"` key
   ([getting-started.md](getting-started.md)), or a `.conway/settings.json`
   `backends.<id>.models.<model>.max_context_tokens` entry. This is a
   statement an operator made about a specific model — always authoritative.
2. **conway's per-model metadata.** Bundled at compile time
   (`crates/conway-plugin-backends/src/model_metadata.rs`'s `DEFAULTS` —
   e.g. `glm-5.2` at `1,000,000`, the two Kimi K3 variants
   [above](#kimi-coding-plan)) and/or your own
   `.conway/models.json`-adjacent `metadata_path` TOML file (`[models]`
   config, defaults to none). A `probe_on_startup` discovery hint, if
   enabled, is folded in at this same layer for a model this metadata
   doesn't already cover — see below.
3. **The dialect's `max_context_tokens` profile field** — the table above,
   `32768` for every built-in profile except `openai` (`128000`). Reached
   only when NEITHER layer above says anything about this exact model. This
   is a floor, not a fact about the model in front of you.

**Whether that floor is actually governing is discoverable, not silent.**
This is the direct fix for the incident that motivated this section: an
operator's Ollama Cloud session against `glm-5.2` was refused at 36,288
tokens ("accepts at most 32768") while the same model, on the same
endpoint, was independently recorded accepting 61,667 input tokens minutes
earlier — the floor was governing silently because `glm-5.2` had no
bundled metadata entry at all, and a rejection citing "32768" read exactly
like a real model limit rather than what it actually was.
`conway_plugin_backends::capabilities::{ContextTokensSource,
max_context_tokens_source}` is the pure primitive that answers "did a real
source declare this, or is the floor governing?" for any given resolution;
`OpenAiCompatBackend::capabilities` emits a `tracing::debug!` naming the
backend, model, and dialect whenever it resolves the floor for exactly this
reason — run with `RUST_LOG=conway_plugin_backends=debug` (or the CLI's own
`-vv`; see [`scripting.md`'s flag reference](scripting.md#flag-reference)
for the other log-level knobs) to see it. Fix it the same way you'd fix any
other under-declared model: add a `models.json` entry, or a `metadata_path`
entry, naming the real window.

**`probe_on_startup` (`[models]` config, default `false`) stays opt-in.**
Considered and rejected: flipping the default to `true` so every session
verifies its own context windows live. Rejected because a live network
call at every startup contradicts this crate's own "never a hard network
dependency" boundary (`model_metadata.rs`'s module doc) and adds startup
latency an operator on a fast local loop would feel every single time, for
a check that fixing the SOURCE data (bundled metadata) makes unnecessary
for every model conway ships an entry for. When enabled, the `"ollama"`
dialect's own discovery step queries `POST /api/show` for each observed
model's `model_info["<arch>.context_length"]` — the model's own declared
ceiling — deliberately NOT `GET /api/ps`'s per-instance *loaded* runtime
window (which can be smaller than the model's real capacity, and requires
the model to already be resident to answer at all); see
`crates/conway-plugin-backends/src/probe.rs`'s module doc for the full
reasoning and the accepted risk (a server deliberately configured with a
smaller runtime window than its model's ceiling gets a real, loud
provider-side refusal instead of a silent conway-side one — preferred,
under this same declaration-honesty principle).

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

### Adding a provider variant

One procedure adds a provider variant to *either* family — the field
vocabulary differs (`openai-compat`'s nine wire-behavior fields
[above](#the-profile-fields) vs. `anthropic`'s two,
`anthropic_version`/`headers`, [above](#wire-version-and-header-overrides)),
but the mechanism to declare and select one does not:

1. **Pick your kind, and look up ONLY that kind's own field table** — the
   two tables just named. A field from the other kind's vocabulary in your
   entry is a rejected, named build error (`deny_unknown_fields`/the
   exhaustive `extra` match, per kind), not a silently ignored one.
2. **Write a `[[profile]]` entry** — `id` plus whichever of that kind's
   fields you're overriding — in a `.conway/profiles.toml` conway
   discovers (project- or global-scoped; [above](#discovery-and-precedence)).
   **One physical file per kind**: `Profiled::parse_source` parses every
   entry in a discovered file as ONE kind's shape, so a file mixing an
   `openai-compat`-shaped entry with an `anthropic`-shaped one fails to
   load for whichever kind reads it second — see [One facility, two
   vocabularies](#one-facility-two-vocabularies) for why this is a
   recorded property of the facility staying kind-agnostic, not a bug.
3. **Select it** — set the corresponding `backends.<id>.dialect` to the
   profile's `id`, the same way you'd name a built-in. `openai-compat`
   *requires* `dialect`; `anthropic` accepts one but doesn't
   ([above](#wire-version-and-header-overrides)).
4. **Verify.** A malformed profile fails `build()` loudly, naming the file
   and the offending field, as shown above. A well-formed but
   *misspelled* `id` does not — see [Seeing what's loaded](#seeing-whats-loaded),
   immediately below, for today's actual limits on confirming a valid
   override took effect.

**Worked, executed example — an `openai-compat` variant.** Every backend
and expectation below is driven through a real `BackendFactory::build` in
`crates/conway-plugin-backends/tests/providers_doc_walkthrough.rs`, against
a local `wiremock` server standing in for the real endpoint — this is not
a description, it is a transcript of code that runs in CI:

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

The test asserts on the real outgoing request body: `"stream_options"` is
present on a streamed call (`supports_stream_options = true`), and a
tool-call reply shaped as a complete JSON-object `arguments` value (not
just a string fragment) is accepted (`tool_call_style = "tolerant"`) —
both properties this profile's fields, and no others, control.

**Worked, executed example — an `anthropic` variant.** The same test file
proves the second family end to end, against a *different* profile file
(the one-file-per-kind constraint from step 2, above):

```toml
# .conway/profiles.toml
[[profile]]
id = "my-anthropic-gateway"
anthropic_version = "2024-10-22"

[profile.headers]
"anthropic-beta" = "my-feature-flag"
```

```json
// .conway/settings.json
{
  "backends": {
    "gateway": {
      "kind": "anthropic",
      "base_url": "https://gateway.example.com",
      "api_key_env": "GATEWAY_API_KEY",
      "dialect": "my-anthropic-gateway"
    }
  }
}
```

The test asserts on the real outgoing request's HTTP headers (never the
JSON body — Anthropic's wire format carries both fields as headers, not
body keys): `anthropic-version: 2024-10-22` and `anthropic-beta:
my-feature-flag` both reach the wire, alongside (never in place of) the
`x-api-key` header `AnthropicBackend` always sets.

**The two-family proof is the point of this section, not a bonus.** A
procedure that only actually works for the family its author was testing
is worse than no procedure — it reads as authoritative for both and is
correct for one. `cargo test -p conway-plugin-backends --test
providers_doc_walkthrough` runs both worked examples above on every CI
run.

### Seeing what's loaded

`conway_plugin_backends::profile::ProfileStore::list()` is a real inspection
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

### One facility, two vocabularies

Both kinds resolve a selected profile through the same underlying
mechanism (`conway_plugin_backends::profile_store::ProfileStore<T>` — file
discovery, project/global precedence, shadow-tracking, and "unknown profile
name" error reporting are ONE implementation, not one per kind). That
mechanism never reads a field beyond a profile's `id`, so it has no opinion
on what `T` is — `openai-compat` resolves to its own nine-field
`Profile`; `anthropic` resolves to a small, opaque bundle of just
`anthropic_version`/`headers`.

**One consequence worth knowing before you hit it:** a single physical
`.conway/profiles.toml` cannot mix `"openai-compat"`-shaped entries with
`"anthropic"`-shaped ones. Each kind parses *every* `[[profile]]` entry in a
discovered file as its own shape, and rejects any field it doesn't
recognize (the same loud-failure-over-silent-typo policy [above](#the-profile-fields)) — so a file containing an
`anthropic_version` key fails to load for `"openai-compat"`, and vice
versa. If you use both kinds, put their profiles in different files (a
project-scoped one and a global one, say, or just two differently-named
files if your setup discovers both).

### Precedence: profile vs. `extra` vs. defaults

One rule, the same for both kinds: **an explicit key in a backend entry's
own `extra` wins over the same key in a selected profile; a key neither
sets falls back to that kind's own hardcoded default.** A profile is a
reusable preset; `extra` is inherently per-instance, so a per-instance
override winning over a shared preset is what lets one backend entry
deviate from a profile without forking it.

`"openai-compat"` does not read `extra` today (its own profile fields are
resolved as one whole bundle via `dialect`, not overlaid key-by-key), so
this rule is currently observable through `"anthropic"` alone: a selected
profile setting `anthropic_version = "2023-01-01"` and that entry's own
`extra.anthropic_version = "2024-06-01"` resolves to `"2024-06-01"` — and a
`headers` entry the profile sets but `extra` never mentions still applies.

## Writing your own adapter

A [declarative profile](#declarative-provider-profiles) and a new adapter
solve different problems, and reaching for the wrong one costs you either a
config file you didn't need or code you didn't need to write. A profile
parameterizes the one `openai-compat` adapter for a server that speaks (a
variant of) OpenAI's own wire protocol — no code, just a
`.conway/profiles.toml` entry, merged by
`crates/conway-plugin-backends/src/profile.rs`'s `merge_file`. A new adapter is for
a genuinely different wire protocol — not Anthropic's Messages API, not an
OpenAI-compatible chat-completions endpoint. If your provider needs its own
request/response shaping from the ground up, this is that path.

**Read [`docs/plugins/trust-and-security.md`](plugins/trust-and-security.md#backends-and-routers-the-same-install-pass-and-one-hands-over-more)
before you write one.** A `BackendFactory` installs through the identical
mechanism as a tool plugin and runs with the identical unsandboxed
privileges — and, unlike a tool plugin, is additionally handed the
operator's resolved `api_key` by the harness, without asking for it. That
page states the privilege and the exposure in full; this page is the
mechanics and the worked example.

**The crate boundary.** Depend on `conway` — the public facade — alone; no
`conway-core` dependency is needed or, for a third party, available.
`conway::backend` is a curated module re-exporting the `Backend` trait and
every type its five methods (`id`, `capabilities`, `generate`, `stream`,
`probe`, plus the overridable `admit`) name; `conway::{BackendFactory,
BackendBuildContext, ConwayBuilder, CoreConwayError}` (root re-exports) are
the installation surface. The full, name-by-name breakdown of what's in
each is [`embedding.md`'s "Writing a `Backend`"](embedding.md#writing-a-backend)
and ["Installing a backend:
`BackendFactory`"](embedding.md#installing-a-backend-backendfactory); this
page's job is the crate you depend on and a worked example, not restating
that list a second time.

**Publishing and naming a kind id.** `BackendFactory::id()` returns the
*kind* string — the same open name `backends.<id>.kind` (see ["Where a
backend is declared"](#where-a-backend-is-declared) above) resolves against
every registered factory, with no privileged set: the two shipped dialects
resolve through the identical mechanism. Publish your own kind as a `pub
const`, the way the shipped dialects publish `ANTHROPIC_KIND`/
`OPENAI_COMPAT_KIND`, so a consumer of your crate names it instead of
retyping a string literal that only works by coincidentally matching yours.

**Attaching it, as a library embedder.**
`ConwayBuilder::with_backend_factory(Arc::new(YourFactory))` before
`build()` — the identical channel `conway-plugin-backends`'s own two
factories attach through. This is the only mechanism there is: this tree
has no dynamic-loading path at all (no `dlopen`/`libloading`/`dylib`
anywhere in it), so a third-party adapter is always a Rust dependency an
embedder links and registers in code, never a crate name the shipped
`conway` binary can pick up from `settings.json` on its own.

### What conway cannot enforce

Implementing `Backend` means agreeing to rules nothing in this tree checks
for a crate it doesn't compile. Stated here because unstated, they bind
nobody:

- **A cache hint must not change request bytes.** Caching is economics,
  never correctness: the request your adapter sends with every
  cache hint stripped must be byte-identical to the request it sends with
  hints applied. `conway-plugin-backends`'s own Anthropic dialect carries a
  test for exactly this —
  `body_with_hints_stripped_equals_body_with_hints_minus_every_cache_control_key`
  (`crates/conway-plugin-backends/tests/anthropic_cache_mapping.rs`) — and
  that test is required of every first-party adapter. For a third-party
  one, conway cannot run your crate's tests and cannot assert this from
  outside it: it is a contract you carry, not one conway verifies for you.
  If your adapter's `capabilities()` reports a `CacheMode` other than
  `CacheMode::None`, this is the invariant that claim is worth nothing
  without.
- **`admit` must be honest about what it can actually serve.** Override
  `Backend::admit` by calling `check_admission` (`conway::backend`'s
  re-export of `crates/conway-core/src/ports/backend.rs`'s function) for
  the fits/shortfall arithmetic — never restate
  `est_tokens + headroom_tokens <= max_context_tokens` by hand. This is the
  one piece of the contract conway's own test suite states in words rather
  than only in code: `crates/conway/tests/backend_parity.rs`'s own doc
  comment calls out "an author who cannot name `check_admission` cannot
  honour `Backend::admit`'s contract," and the [worked example
  below](#a-complete-worked-example) does exactly this. An `admit` that
  fabricates a fit conway didn't actually verify surfaces as a
  `BackendError::ContextTooLarge` the caller never gets to react to in
  time — the provider's own truncation or rejection, not conway's.
- **Untrusted input yields a typed error, never a panic.** Model-supplied
  values reaching your adapter — tool-call arguments, sampling parameters,
  anything decoded off the wire — are untrusted input the same way a tool
  argument is. A range check that fails must produce `BackendError`
  (`conway::backend`'s re-export), not an `unwrap`/`expect`/arithmetic
  overflow. A panic here takes down the turn calling it, not merely fails
  the one request.

None of this is enforced by the trait, by `build()`, or by any test
`conway` runs against a crate it doesn't own. It is a contract discharged
by you following it, the same way [`docs/plugins/trust-and-
security.md`](plugins/trust-and-security.md) states what a plugin author
is trusted with rather than what conway checks for them.

### A complete worked example

The strongest claim this page can make is that its example is the same
code a test compiles, not a fresh retelling. It is: every snippet below is
lifted verbatim from `crates/conway-thirdparty-backend/src/lib.rs`, a real
workspace member built for exactly this purpose — a third-party-shaped `Backend` +
`BackendFactory` whose own `[dependencies]` name exactly one workspace
crate, `conway`, so `use conway_core::...` anywhere in it is a hard
`error[E0433]: failed to resolve`, not a convention a reviewer has to
police. `crates/conway-thirdparty-backend/tests/end_to_end.rs` and
`src/bin/thirdparty_backend_demo.rs` both build a real `Conway` from this
code and run a real turn through it — the former as a library call,
asserting on the turn's own returned text directly; the latter as a
genuinely separate compiled binary that prints that text to stdout, which
`tests/binary.rs` then asserts on via `assert_cmd`. Every Rust block below
is byte-for-byte what the file contains, omitting only the struct field
declarations, a hand-rolled `Stream` impl (`VecStream`) `Backend::stream`'s
return type needs, doc comments, and the file-writing tail of the private
`write_settings_with_backend_entry` helper (the `std::fs::write` calls that
serialize `settings.json` and `.conway/models.json` to disk, unchanged by
this item); the `.conway/models.json` block near the end is shown rendered
as the JSON it produces, not as the `serde_json::json!` call that builds it
(that call IS shown verbatim for `settings.json`, immediately above it) —
nothing below diverges in substance from what the file contains.

The imports — everything needed to implement `Backend`, and nothing from
`conway-core`:

```rust,ignore
use conway::backend::{
    async_trait, check_admission, Admission, Backend, BackendError, BackendId, BoxStream,
    CacheMode, Capabilities, ContentBlock, GenerateRequest, GenerateResponse, ModelId, ProbeReport,
    ReliabilityTier, StopReason, StreamChunk, StructuredOutput, ToolCallSupport, Usage,
};
use conway::{BackendBuildContext, BackendFactory, CoreConwayError, ModelOverrides};
```

The published kind id, alongside the other ids this fixture uses:

```rust,ignore
/// The `backends.<id>` JSON key `fixture::write_settings` renders, and
/// the `Backend::id()` `ThirdPartyBackendFactory::build` gives back.
pub const BACKEND_ID: &str = "thirdparty";
/// The `kind` `fixture::write_settings` names -- an open string
/// (`ThirdPartyBackendFactory::id()` returns the identical value), never a
/// closed enum variant.
pub const BACKEND_KIND: &str = "thirdparty-stub";
```

`REPLY_TEXT` and `GREETING_KEY` — the reply this backend gives when the
entry sets no custom key, and the custom key it reads back out of
`BackendBuildContext::extra` when the entry does:

```rust,ignore
pub const REPLY_TEXT: &str =
    "hello from the third-party backend, installed through settings.json alone";
pub const GREETING_KEY: &str = "greeting";
```

`respond()` is where `greeting` — read once at construction time by
`ThirdPartyBackendFactory::build`, below — becomes an observable difference
in what the backend says, not merely a field that arrived populated:

```rust,ignore
impl ThirdPartyBackend {
    fn respond(&self) -> GenerateResponse {
        let text = match &self.greeting {
            Some(greeting) => format!(
                "hello, {greeting} -- from the third-party backend, installed through \
                 settings.json alone, with a custom `greeting` key read from \
                 BackendBuildContext::extra"
            ),
            None => REPLY_TEXT.to_string(),
        };
        GenerateResponse {
            content: vec![ContentBlock::Text { text: text.clone() }],
            tool_calls: vec![],
            stop: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 0,
                output_tokens: u32::try_from(text.len()).unwrap_or(u32::MAX).div_ceil(4),
                ..Usage::default()
            },
        }
    }
}
```

The `Backend` implementation — all five methods, `admit` overridden and
calling `check_admission` rather than restating the fits/shortfall
arithmetic itself, which conway keeps in exactly one place:

```rust,ignore
#[async_trait]
impl Backend for ThirdPartyBackend {
    fn id(&self) -> BackendId {
        self.id.clone()
    }

    fn capabilities(&self, _model: &ModelId) -> Capabilities {
        Capabilities {
            tool_calling: ToolCallSupport::Streaming { validated: true },
            cache: CacheMode::ImplicitPrefix {
                min_prefix_tokens: 0,
            },
            parallel_tool_calls: false,
            structured_output: StructuredOutput::None,
            max_context_tokens: self.max_context_tokens,
            reasoning: false,
            reliability_tier: ReliabilityTier::Community,
        }
    }

    async fn generate(&self, _req: GenerateRequest) -> Result<GenerateResponse, BackendError> {
        Ok(self.respond())
    }

    async fn stream(
        &self,
        _req: GenerateRequest) -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError> {
        let response = self.respond();
        let first_delta = match response.content.first() {
            Some(ContentBlock::Text { text }) => StreamChunk::TextDelta(text.clone()),
            _ => StreamChunk::TextDelta(String::new()),
        };
        let items = vec![Ok(first_delta), Ok(StreamChunk::Done(response))];
        Ok(Box::pin(VecStream {
            items: items.into_iter().collect(),
        }))
    }

    async fn probe(&self) -> Result<ProbeReport, BackendError> {
        Ok(ProbeReport {
            ok: true,
            latency_ms: 0,
            models: vec![ModelId::new(MODEL_ID)],
            detail: None,
            at: chrono::Utc::now(),
        })
    }

    fn admit(
        &self,
        req: &GenerateRequest,
        headroom_tokens: u32) -> Result<Admission, BackendError> {
        let est_tokens = estimate_tokens(req);
        check_admission(
            req.model.clone(),
            est_tokens,
            headroom_tokens,
            self.max_context_tokens)
    }
}
```

`admit`'s own `estimate_tokens` helper (a facade-only crate cannot reach
`conway-core`'s bundled `default_estimate_tokens`, so it writes a small,
dialect-neutral one instead — a real adapter is free to size a request
however its own provider's tokenizer actually works):

```rust,ignore
fn estimate_tokens(req: &GenerateRequest) -> u32 {
    let mut total: u32 = 0;
    for segment in &req.segments {
        for block in &segment.content {
            if let ContentBlock::Text { text } = block {
                total =
                    total.saturating_add(u32::try_from(text.len()).unwrap_or(u32::MAX).div_ceil(4));
            }
        }
    }
    total.saturating_add(
        u32::try_from(req.tools.len())
            .unwrap_or(u32::MAX)
            .saturating_mul(16))
}
```

The `BackendFactory` — `id()` names the published kind, `build()` reads
`BackendBuildContext::models` (the same `.conway/models.json`-derived table
a config-derived backend's own capabilities are projected from) for a
per-model override, exactly the way `conway-plugin-backends`'s own two
shipped factories do, and reads `BackendBuildContext::extra` for
`GREETING_KEY` the identical way — the same context field, read the same
way, proving the catch-all channel a third-party kind's own configuration
travels through is genuinely reachable, not merely nameable:

```rust,ignore
pub struct ThirdPartyBackendFactory;

impl BackendFactory for ThirdPartyBackendFactory {
    fn id(&self) -> &str {
        BACKEND_KIND
    }

    fn build(&self, ctx: BackendBuildContext) -> Result<Arc<dyn Backend>, CoreConwayError> {
        let max_context_tokens = ctx
            .models
            .get(MODEL_ID)
            .and_then(|overrides: &ModelOverrides| overrides.max_context_tokens)
            .unwrap_or(32_000);
        // `extra` is the entry's own keys beyond `kind` and the six typed
        // fields `BackendEntry` recognizes -- absent when the entry sets no
        // `greeting`, in which case `ThirdPartyBackend::respond` gives back
        // `REPLY_TEXT` unchanged.
        let greeting = ctx
            .extra
            .get(GREETING_KEY)
            .and_then(|value| value.as_str())
            .map(str::to_string);
        Ok(Arc::new(ThirdPartyBackend {
            id: ctx.id,
            max_context_tokens,
            greeting,
        }))
    }
}
```

Installing it and building a real `Conway` from a real, on-disk
`settings.json` — `conway::config::load` is the same five-source loader a
real `conway` invocation uses, and `with_backend_factory` is the same
channel every embedding path in this doc uses:

```rust,ignore
pub fn build_conway(dir: &Path, config_path: &Path) -> conway::Result<conway::Conway> {
    let mut env = std::collections::HashMap::new();
    env.insert(
        "CONWAY_CONFIG_DIR".to_string(),
        dir.to_string_lossy().into_owned());
    let outcome = conway::config::load(conway::config::LoadOptions {
        cwd: dir.to_path_buf(),
        explicit_path: Some(config_path.to_path_buf()),
        env,
        cli_overrides: conway::config::CliOverrides::default(),
        model_metadata_refresh: false,
    })?;
    conway::ConwayBuilder::from_parts(outcome.config)
        .with_backend_factory(Arc::new(ThirdPartyBackendFactory))
        .build()
}
```

And the `settings.json`/`.conway/models.json` pair this factory resolves
against — the same `backends.<id>.kind` shape every dialect on this page
uses, naming a kind no built-in factory claims. `write_settings_with_
greeting` is the variant that also sets `GREETING_KEY` — the custom key
beyond `kind` this section exists to demonstrate — leaving everything else
to the shared `write_settings_with_backend_entry` helper so the two never
drift apart on anything but the one entry that differs:

```rust,ignore
pub fn write_settings_with_greeting(dir: &std::path::Path, greeting: &str) -> PathBuf {
    write_settings_with_backend_entry(
        dir,
        serde_json::json!({ "kind": BACKEND_KIND, (GREETING_KEY): greeting }))
}

fn write_settings_with_backend_entry(
    dir: &std::path::Path,
    backend_entry: serde_json::Value) -> PathBuf {
    let chain = format!("{BACKEND_ID}/{MODEL_ID}");
    let settings = serde_json::json!({
        "default_role": "coder",
        "cwd": dir.to_string_lossy(),
        // `permissions.mode = "allowlist"` requires a non-empty
        // `allowed_tools` list (`config::merge::validate`) even though
        // `ThirdPartyBackend` never issues a tool call and this gate is
        // therefore never actually consulted -- `"*"` is a real,
        // syntactically valid `AllowListGate` glob entry (matches any
        // tool name), not a magic sentinel this fixture invented.
        "permissions": { "mode": "allowlist", "allowed_tools": ["*"] },
        "backends": {
            BACKEND_ID: backend_entry
        },
        "roles": {
            "coder": { "chain": [chain] }
        }
    });
    // (elided here, unchanged by this item: the std::fs::write calls that
    // serialize `settings` to `settings.json` and a `.conway/models.json`
    // entry to disk -- shown rendered as JSON, not as the code that builds
    // it, immediately below.)
}
```

A `write_settings_with_greeting(dir, "friend")` call therefore renders a
`backends.thirdparty` entry of `{"kind": "thirdparty-stub", "greeting":
"friend"}` — the `greeting` key is not one of `BackendEntry`'s six typed
fields, so it lands in that entry's own `extra` map, which is exactly what
`ThirdPartyBackendFactory::build` (above) reads back out. `fixture::
write_settings` (used everywhere else on this page and in `tests/end_to_
end.rs`) sets no such key, so its `ThirdPartyBackend` gives back
`REPLY_TEXT` unchanged — the two fixtures differ only in that one entry.

```json
// .conway/models.json
{
  "models": {
    "thirdparty/stub-model": {
      "max_context_tokens": 200000,
      "tool_calling": "streaming_validated",
      "reasoning": false,
      "reliability_tier": "community"
    }
  }
}
```

Run it yourself: `cargo test -p conway-thirdparty-backend` builds this
exact code, installs it through the exact mechanism above, completes one
real turn, and asserts the returned text is the adapter's own hand-written
reply — credential-free and network-free throughout, since this particular
stand-in adapter never makes an outbound call at all.
`tests/custom_key.rs` is the same proof one step further: it renders two
entries differing only in their `greeting` value and asserts the two
resulting turns come back with two different, `greeting`-naming replies —
the observable evidence that a custom key genuinely reaches the factory,
not merely that `BackendBuildContext::extra` is non-empty. A dialect that
talks to a real provider does the identical `Backend`/`BackendFactory`/
`with_backend_factory` dance; only `generate`/`stream`/`probe`'s own
bodies differ, the same way `conway-plugin-backends`'s two shipped
factories differ from this one.

## How it fits together

A backend entry alone doesn't make a model routable. Every `(backend,
model)` pair a role's chain names also needs an entry in
`.conway/models.json` — see [`getting-started.md`](getting-started.md)
for the file and the exact routing error you get if you skip it — and
[`routing.md`](routing.md) for how conway picks among the backends you've
configured here.
