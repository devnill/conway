# Compatibility

What an author may rely on across conway versions, and what they may not.
This is contract documentation, not narrative — this project's ban on
version references in `docs/` forbids "added in 0.x" feature-history, not
stating the compatibility rules themselves, and this page is entirely the
latter.

Depends on [`concepts.md`](concepts.md) for vocabulary. Most of what this
page states concretely applies to the FULL wire protocol, which remains a
design spike beyond a thin, disclosed slice: `tool.spec/1`/`tool/1`
(one-shot exec) are real and enforced today — see
[`subprocess-plugins.md`](subprocess-plugins.md) — and the persistent NDJSON
transport is real and enforced too, now including an `initialize/1`
version-negotiation handshake at session open (see the disclosure below the
table). The remaining wire points (`permission.policy/1`,
`context.hook/1`, `observe/1`, `status/1`) are not yet implemented. Every
claim below says plainly whether it describes something enforced in the tree
today or a decided rule for the transport once it lands, the same labeling
discipline [`hooks.md`](hooks.md) uses throughout.

## Versioning

**The rule that makes forward compatibility work, stated once, for both
scopes below: unknown enum tags fail to the most restrictive value, never to
a default that happens to be permissive.** "Use versioning" is not this
page's content; the table under each scope is.

### Config files, as shipped today

A hand-authored file — `settings.json`, `permissions.json`, an agent def's
frontmatter, a provider profile — is read by conway *before* the model or
any plugin sees it, and the two directions of change need opposite
treatment:

- **An old file, a newer binary.** Every optional field carries
  `#[serde(default)]` with a conservative, documented default, so a file
  written before a field existed keeps parsing unchanged. `PermissionFile`'s
  own doc states this for `deny`/`rules`
  (`crates/conway-core/src/permission_pattern.rs`); `profile.rs`'s module
  doc states the identical reasoning for provider profiles
  (`crates/conway-plugin-backends/src/profile.rs`).
- **A newer file (or a typo), an older binary.** `#[serde(deny_unknown_fields)]`
  turns an unrecognized field into a loud, named error instead of a silently
  ignored one. This is deliberately the *opposite* choice from a wire frame
  (below): a hand-authored file is small, edited by a human, and a
  misspelled key defaulting to "off" and running anyway is worse than a
  refusal naming the field. `crates/conway/src/config/schema.rs`,
  `crates/conway/src/agents.rs`'s frontmatter, and
  `crates/conway-plugin-backends/src/profile.rs`'s `ProfileRaw` all set the
  attribute directly. As of a later item,
  `crates/conway-core/src/permission_pattern.rs`'s `RawPermissionFile` — the
  type `permissions.json` is actually loaded through — reaches the SAME
  observable outcome (a loud, named error for an unrecognized key) by a
  different mechanism instead: see below for why it uses a
  `#[serde(flatten)]` catch-all rather than the attribute itself. The
  crate's own `config_precedence.rs` test suite pins the `settings.json`
  behavior with named typo cases (`typo_d_key_is_rejected_by_deny_unknown_fields`,
  `typo_d_health_key_is_rejected_by_deny_unknown_fields`), and
  `permission_pattern.rs`'s own suite does the same for `permissions.json`
  (see below for the full account, including the one type in this file's
  own family — `PermissionFile` — that deliberately stays lenient, and why).

**This gap is closed for `permissions.json`, and deliberately NOT closed the
same way for `trust.json`.** The
type that actually deserializes a permissions file being loaded for
installation is `RawPermissionFile`, the private struct inside
`conway_core::permission_pattern::parse_permission_file` — not the public
`PermissionFile` type, which is used only for the ROUND-TRIP writers
(`Conway`'s revoke rewrite, and `conway-cli`'s best-effort "always allow"
append). `RawPermissionFile` now carries a `#[serde(flatten)] extra:
serde_json::Map<String, serde_json::Value>` catch-all instead of
`deny_unknown_fields` itself (the two are mutually incompatible in serde):
a file naming an unrecognized top-level key (`"denys"` instead of `"deny"`,
or any other typo) is detected structurally, by that map being non-empty,
rather than by matching text inside a `serde_json::Error`'s message — text
that is neither serde's nor serde_json's own semver contract, so a future
dependency bump changing it could otherwise silently reopen the exact gap
this item closes. `permission_file_unknown_field_error` checks this BEFORE
any rule is parsed from the file, and `Conway::load_permission_files`/
`Conway::trust_permission_file` both refuse the whole file — allow, deny,
and prompt, none of it installs — surfacing a message that names the
offending key, rather than silently enforcing nothing, at BOTH entry
points, through the SAME `Entry::Error { fatal: false }` transcript
severity (`crates/conway-cli/src/tui/app.rs`). `permission_pattern.rs`'s
own test suite pins the typo case
(`a_misspelled_deny_key_is_reported_rather_than_silently_installing_nothing`)
and a CONTROL case with the key correctly spelled
(`a_correctly_spelled_deny_key_installs_the_rule_the_typo_would_have_dropped`)
so an empty result is evidence of the catch, not of an empty fixture; the
same pairing is repeated at the real production seam in
`crates/conway/tests/permission_trust_seam.rs`
(`a_misspelled_deny_key_in_a_project_file_installs_no_rule_and_is_reported_loudly`
/ `a_correctly_spelled_deny_key_in_a_project_file_does_refuse_the_call`).
`PermissionFile` itself stays lenient — its own doc comment states why: the
best-effort append path already treats ANY parse failure as "the file was
empty" and overwrites it with just the one new rule, so making that read
strict would mean a field a NEWER conway build added, read back by an OLDER
build appending one grant, silently destroys every other rule the file
held (its `deny` rules included) — a worse outcome than the gap this item
closes. Every load path reaches the file through the now-strict
`RawPermissionFile` first regardless, so this leniency in the round-trip
type does not reopen the hole.

`TrustFile`/`TrustedRecord` (`crates/conway/src/config/trust.rs`)
deliberately keep NO `deny_unknown_fields` at all, established rather than
assumed: nobody hand-types a key into `trust.json` — it is written
exclusively by `TrustStore::trust` and read back exclusively by
`TrustStore::load`, both this crate, across whatever two conway builds an
operator happens to run before and after an upgrade. Its realistic failure
mode is therefore version skew (a future build adds a field; an older
build reads the file back), not an operator's typo, and
`TrustStore::load_from_path` already treats ANY parse error as "trust.json
is corrupt" — zeroing every recorded trust decision in the file at once,
not just the one entry carrying the new field. That regression has no
matching security upside the way `permissions.json`'s strictness does: an
untrusted-by-mistake record degrades toward MORE prompting (the same
direction this module's whole failure posture already takes on purpose),
it never lets anything unenforced through the way a silently-dropped
`deny` rule does. `trust.rs`'s own test suite pins that the leniency
actually holds
(`an_unrecognized_key_in_trust_json_does_not_prevent_a_recorded_decision_from_matching`),
not just that it was decided.

**The general fallback pattern — explicit match, deny by default — already
ships, just not yet at the config-file-typo layer above.**
`PermissionMode::allows_category` (`crates/conway-core/src/
permission_mode.rs`) matches plan mode's allowed set explicitly and denies
everything else, including a `ToolCategory` variant that doesn't exist yet;
the module's own doc states why the match is spelled allow-list-then-deny
rather than the inverse: *"a future `ToolCategory::Deploy` is blocked in
plan mode the day it is added, without anyone remembering to update this
file."* This is the shipped instance of "unknown fails to the most
restrictive value" the wire-level rule below generalizes.

### The wire protocol — decided design, partially honored by the one-shot slice

the wire-vocabulary design settles the rules an out-of-process plugin
transport must follow. The `tool.spec/1`/`tool/1` slice now honors the
per-enum degradation rows for `ToolCategory`, `PermissionClass`, and
`ContentBlock` (see the disclosure below the table). `ToolCategory` and
`PermissionClass` degrade on the **discovery** path (`tool.spec/1`, which is
always one-shot by design); `ContentBlock` degrades on the **`tool/1`
answer** path, which is shared by the one-shot and the persistent NDJSON
transports (both route a `tool/1` answer through the same `classify`), so an
unknown block type is dropped, counted, and surfaced on either transport.
The remaining rows and the version-negotiation handshake are decided design
for the wider transport that lands later. Concrete, and cited here so a
future implementation is built against a decision, not a vibe:

| Enum | Unknown tag means, per the design |
|---|---|
| `ToolCategory` | `execute` — the category plan mode already denies |
| `PermissionClass` | `dangerous` |
| `TruncationPolicy` | the host's default policy, never `none` |
| `ContentBlock` | drop the block, count it, surface it via a status change |
| `Event` | ignore — the one point where "ignore" is correct, because an observer changes nothing by construction |
| `ResultStatus` | `failed`, never `completed` |
| `ToolSelector` | `only([])` — selects nothing; narrowing, never widening |

And the version-negotiation table for the handshake itself:

| Condition | Outcome |
|---|---|
| `plugin.major != host.major` | Refuse to load, naming both |
| `plugin.minor_min > host.minor` | Refuse — the plugin needs a feature this host does not have |
| `plugin.minor_min <= host.minor` | Accept, whatever the plugin's own minor; unknown fields ignored-and-counted, never rejected |
| Unknown/unsupported version of a **participant** point (`tool`, `permission.policy`, `context.hook`) | Refuse to load — a policy that silently never runs is the worst outcome |
| Unknown/unsupported version of an **observer** point (`observe`, `status`) | Degrade: load without that point, warn |

`major` covers the frame vocabulary and envelope semantics (method names,
error-code ranges); `minor` is additive only — new methods, new optional
fields, new capability names, new `Event` variants. Conway's own version
appears in a future handshake as informational only (`host: { name,
version }`), never branched on — a TUI-only release does not have to move
the protocol, and nothing here is size-of-conway-version-shaped.

**The wire-level table above is NOW honored by the `tool.spec/1`/`tool/1`
slice that exists today**
([`subprocess-plugins.md`](subprocess-plugins.md)), as of board item
`01M03VJPRT8629CYR8JK4A8JPF`. Previously that slice failed CLOSED on any
unknown tag or malformed field (a hard parse error, the same uniform
failure every other malformed answer produces); it now degrades unknown
ENUM TAGS per this table — an unknown `ToolCategory` -> `execute`, an
unknown `PermissionClass` -> `dangerous`, an unknown `ContentBlock` type
in a `tool/1` answer -> drop the block, count it, and surface it (a
summary `ContentBlock::Text` naming the dropped count and the unknown type
tags is appended to the kept blocks, and `is_error` is set so the host
knows the output is incomplete). Each degradation NAMES the unknown tag
via `tracing::warn!`, so the convergence is auditable; `#[serde(other)]`
is deliberately NOT used on the `#[non_exhaustive]` enums because it would
silently capture future variants this host SHOULD refuse, widening rather
than narrowing. The line, stated in the code at each custom deserializer:
an unknown ENUM TAG degrades to the most restrictive value; a missing or
structurally-invalid FIELD (a non-string where a string was expected, a
missing required `ok`, an `ok:false` with no `error`, an empty manifest
id, a non-compiling schema) STILL fails closed. A future implementation
that widens past `tool.spec/1`/`tool/1` (the `permission.policy/1`/
`context.hook/1`/`observe/1` points) is the one the REMAINING table rows
(`TruncationPolicy`, `Event`, `ResultStatus`, `ToolSelector`) are written
for.

**The version-negotiation handshake table above is NOW ENFORCED for the
persistent NDJSON transport** (board item
`01M03VK7MRPSAVWMW7YNYPRPGT`), as of that item. A persistent-transport
subprocess plugin is greeted with one `initialize/1` request/response at
session open, BEFORE any `tool/1` call: the host sends its
`wire_major`/`wire_minor` and the points it speaks (today `["tool/1"]`);
the plugin answers its own `major`, the minimum `minor` it requires
(`minor_min`), and the per-point versions it declares. The host then
applies this table — refuse on `major` mismatch or unsatisfied `minor_min`
(a typed `HandshakeRefused` naming both versions); accept otherwise;
unknown FIELDS in the plugin's answer ignored-and-counted (surfaced via
`tracing::debug!`), never rejected (the table's accept branch / forward-
compat rule: a newer plugin's extra field does not break an older host).
A structurally-invalid answer (missing `ok`, `ok:false` with no error, a
non-number where a number was expected) fails closed as `HandshakeMalformed`;
a plugin that closes without answering fails closed as `SessionDied` within
`timeout_ms`, never hangs. The plugin's declared per-point versions are
recorded on the session for the later wire-point items
(`permission.policy/1`, `observe/1`, `status/1`, `context.hook/1`) to
consult WITHOUT re-negotiating — those points themselves remain FUTURE
items, not yet implemented. `host.version` (the conway crate version) is
put on the wire for the plugin to read but NEVER branched on by the
negotiation — informational only, per the paragraph above.

**One-shot discovery (`tool.spec/1`) stays handshake-free** — the
handshake is a persistent-transport concern, and `tool.spec/1` discovery
remains a one-shot exec under both transports (see `subprocess-plugins.md`).
The `WireManifest` that `tool.spec/1` answers DOES exist in the tree (it
carries `required_host_caps` as of board item
`01M03VJXARFHSDAGHFXGCWKJTY`, mapped into `PluginManifest::required_host_caps`
and gated at the builder seam); the `initialize` handshake exists for the
persistent transport as of this item; per-point version NEGOTIATION records
are produced by the handshake and held on the session, but the per-point
wire points themselves (`permission.policy/1`, `observe/1`, `status/1`,
`context.hook/1`) and the REMAINING table rows (`TruncationPolicy`,
`Event`, `ResultStatus`, `ToolSelector`) are still future work.
`concepts.md`'s "Hook-first" section and [`hooks.md`](hooks.md) point 1
both state the same thing from their own angles. It is documented here,
concretely, because a reader building against this reference needs the
rule the same way `hooks.md` documents `permission.policy/1`'s contract
ahead of its implementation: labeled, decided, and precise enough to
implement without re-litigating.

**Decision** (settling the hook-first
redirect's four open questions) bears on this page only through what it
already settled elsewhere in the set: `concepts.md`'s "Fork vs spawn"
section states the resulting per-registration `subagent_mode` declaration
and its `hook.fork` capability gate. It adds nothing further to the
versioning rules above; restated here only so the check is on record rather
than silently skipped.

## The promise

- **`conway-core`'s port surface** — `Plugin`, `Tool`, `ContextHook`,
  `PermissionGate`, and every type named in their method signatures — is
  what `ARCHITECTURE.md` §2/§3.8 calls out by name: *"`conway-core` is
  deliberately small and slow-moving under strict semver discipline...
  because it is the surface third-party plugins depend on."* This is the
  surface an in-process plugin author is actually building against, and it
  is the one this project is most deliberate about not letting churn.
- **`conway::plugin`'s export set** (landed as F8,
  `crates/conway/src/lib.rs`) is a curated re-export, not a second surface
  with its own promise: every name in it is sourced from `conway-core`
  (`conway_core::ports`, `conway_core::content`, `conway_core::error`,
  `conway_core::agent::Fact`, `conway_core::segment::PromptSegment`,
  `conway_core::provenance::Provenance`) plus the external `async_trait`
  macro re-export that makes `use conway::plugin::*` sufficient to write an
  implementation. It is covered by the *same* discipline as the types it
  re-exports, not a weaker one invented for the module — `crates/conway/
  tests/plugin_surface.rs` and `plugin_builtin_parity.rs` pin the set by
  compiling against it, so a regression here is a build failure, not a
  silent drift.
- **conway is pre-1.0, and that is a real, stated limit on this promise, not
  a footnote.** `CHANGELOG.md` states the project "adheres to Semantic
  Versioning" — and semver's own text is explicit that a pre-1.0 major
  (`0.x`) carries no compatibility guarantee at all; anything may change in
  any `0.x` release. `docs/embedding.md`'s own statement about a first-party
  plugin crate generalizes past that one case: *"Pre-1.0, [an API] can
  change in any workspace release, same as everything else here."*
  `conway::plugin` is "everything else" in that sentence — nothing in this
  page promises a *version-number*-level compatibility guarantee. What the
  strict-semver-discipline language above actually promises is narrower and
  real: `conway-core`'s port surface is *edited* more carefully and changes
  less often than the rest of the tree, because the project has said so
  explicitly and treats a change there as costing every plugin author, not
  because a `0.x` bump is contractually barred from breaking it.
- **Everything not named above is explicitly unstable**: `conway-cli`'s
  flags, `conway-runtime`'s internals (never re-exported at all —
  `crates/conway/src/lib.rs`'s own doc states "no type from `conway-runtime`
  is re-exported here"), and any first-party plugin crate's own API
  (`conway-plugin-skeleton`, `conway-plugin-routing`) — all versioned with
  the workspace, all free to change in any release pre-1.0.

## Config file strictness

Stated fully under "Versioning" above; restated here in one line because
it's the sentence an author skimming for "can I add a field to my own
plugin's config and not break existing installs" wants without reading the
whole section: **a hand-authored file rejects a field it does not
recognize; a wire frame (once one exists) tolerates one, silently but
countedly, because the file in front of a human punishes a typo and the
frame between two programs must survive a newer peer.**

## Deprecation

**No point, field, or capability in this set has actually been retired
yet**, and nothing rules a formal deprecation *procedure*
(a warning period, a required runtime notice, a minimum number of minor
releases before removal) — searched and confirmed absent. What follows is
the policy this page can state with confidence from what's already ruled
elsewhere, not an exercised procedure:

- **Retiring a point, a field, or a capability name is a breaking change by
  definition**, and the versioning rules above put a breaking change behind
  a major bump only — never a minor, regardless of how unused the thing
  being removed appears to be. A minor release only ever adds.
- **A retirement is announced the same way any user-facing change is:
  a `CHANGELOG.md` entry under the section for the
  release that removes it.** That rule is explicit that version references and
  "added in 0.x"-shaped history do not belong in `docs/` — `CHANGELOG.md` is
  release metadata, the one place that framing is correct, and it stays the
  single place a reader looks for "what changed and when," never inline in
  a page like this one.
- **The closest live precedent for a surface moving out from under a
  consumer** is not a deprecation but a relocation: `conway-routing`'s
  engine moved from a mandatory workspace crate into the installable
  first-party plugin tier, and
  `conway_plugin_backends::config::Dialect`'s five-variant convenience enum was kept
  working for every existing call site rather than replaced outright when
  provider profiles became data (`crates/conway-plugin-backends/src/profile.rs`'s
  own doc: *"it is not deprecated, but it can no longer name a provider this
  crate doesn't already ship code for"*) — the shipped instinct, in both
  cases, is to keep an old surface compiling and route it onto the new
  mechanism rather than break it outright, even without a formally ruled
  deprecation window forcing that choice.

If a stronger commitment — a stated minimum warning period, a runtime
deprecation notice a plugin author can detect programmatically — is wanted,
it needs its own decision; this section states what already follows from
that rule and the versioning rules above, not a ruling this page is positioned to
make on its own.
