# D5 — Template-variable instrumentation for the status line and other UI surfaces

Status: design spec (board item 01KYNNCFG8HYRN5QRHHHF00WHZ). Written against
the working tree at `e240a55` + uncommitted containment work. Transport is D1,
extension points D2, wire vocabulary D3, trust D4. **Read
`.design/d2-extension-points.md` §11 case 4 first** — this document is
consistent with it and extends it; the one place it goes further is stated in
§5.3.

---

## 0. Two corrections to the framing

**The status line has ten fields, not eight.** `session` and `lineage` were
added after the brief was written (`status.rs:212-226`, default list at
`config/schema.rs:462-471`). `lineage` matters here because it is the one
existing field that embeds *arbitrary user-chosen text* — `agent_def` names,
rendered verbatim as `@{def}` (`agents.rs:95` `recipe_parts`) — so the tree
already has a precedent for non-host-authored text on this line, and it is
already the reason `ladder_width` measures display columns rather than chars
(`status.rs:407`, adversarial review finding 2).

**`settings.json` is discovered project-first.** `config/discovery.rs:10`
walks from cwd upward for `<dir>/.conway/settings.json`, and
`config/merge.rs:81` merges it *above* the XDG user file. So a cloned repo
can already supply `[tui.status_line]`. Today that is harmless — it can only
reorder a closed set of names, and `resolve_fields` (`status.rs:299`) forces
`mode` back in when the permission mode is non-default. **The moment a
template can carry literal text, a hostile repo can author words on the
operator's screen.** §2.3 closes this; it is the single most load-bearing
decision in the document.

---

## 1. The property, stated precisely, and what happens to it

Today's property, as the brief states it: *config supplies an enum selector
from a closed set, and no config value ever reaches the screen.* That is two
properties wearing one coat, and templating treats them differently.

- **P-A — the host owns every value.** No *datum* originating in config is
  ever displayed as if it were state. A `{ctx_pct}` reference resolves to a
  number the host computed; config cannot supply the number.
  **Preserved exactly, and it is the whole point of the template framing.**
  There is no construct in §2 that substitutes a config-supplied value.
- **P-B — no config-authored *text* reaches the screen.**
  **Deliberately broken.** Literal text between placeholders renders. This is
  the feature, not a side effect, and it is the entire cost of the design.

Everything that follows is the price of breaking P-B, paid explicitly:

| Consequence of breaking P-B | Answer |
|---|---|
| Config text can carry control bytes (`"\x1b[2J{model}"`) | Literal text goes through the same sanitizer as every other untrusted string (§5.2). Today config text is never sanitized because it never renders. |
| Config text can consume the width budget | Custom fields sit at the bottom of `drop_priority` and are capped there (§5.1). |
| Config text can *lie* (`"ready · prompt · {model}"` while `AUTO-ALLOW` is live) | Two defenses: the forced-`mode` invariant is preserved verbatim (§2.4), so the real label still renders; and project-scoped config may not author templates at all (§2.3). |
| A typo produces a silent hole | Unknown variables are detectable three ways (§2.2). |

**The one-sentence version to record:** *P-A survives untouched — config still
cannot supply a value; P-B is traded away for arrangement, and the trade is
paid for with a sanitizer at the literal-text boundary, a hard floor in the
width priority order, an unmodified forced-`mode` invariant, and a
user-scope-only rule for the key.*

---

## 2. The template language

### 2.1 Syntax — two constructs, two escapes, nothing else

```
{name}        variable reference; name is [a-z0-9_.]+
[ ... ]       group: renders only if EVERY variable inside it resolved
              non-empty; otherwise the group and its literals vanish
{{  }}        literal brace
[[  ]]        literal bracket
anything else literal text
```

That is the complete grammar. **No conditionals, no operators, no functions,
no width/precision specs, no nesting, no styling markup.** Groups do not
nest (a `[` inside a group is a syntax error). A group containing zero
variable references is a syntax error (it would be unconditionally on or
unconditionally off — ambiguous either way).

**Why the group earns its place when nothing else does.** Bias-small says one
construct. But today's assembly gets separator elision for free: `model` is
omitted before the first `ModelDecision` and no dangling ` | ` appears
(`status.rs:357-367`, and the `missing_git_field_is_omitted_gracefully`
test). A flat one-construct template renders `" · ctx 0%"` with a leading
orphan separator in exactly that case. The pressure to fix that is precisely
the pressure that grows a template language into a programming language — the
next request after "silently wrong output" is `{model?}`, then `{if model}`,
then expressions. One non-nesting bracket pair absorbs that pressure
permanently and cannot be built on. It is the cheapest possible fixed point.

Worked example, reproducing today's `tokens` field exactly — which is the
acceptance test for the language being expressive enough:

```
{tokens_total} tok[ ({cache_pct}% cached)]
```

`cache_pct` is empty when its denominator is zero, so the parenthetical
elides — identical to `tokens_label`'s hand-written guard (`status.rs:687`).

### 2.2 Unknown variables: detectable three ways, never silent

Dropping silently matches `StatusLineField::parse`'s current behavior
(`status.rs:231`) and is rejected here. That behavior is defensible for a
*closed enum of ten names* where a typo produces a visibly missing field; it
is indefensible for an open namespace where `{ctx_pc}` produces a hole in the
middle of a line that otherwise looks fine. D2 §8 already established the
house rule — "three ways a non-match is detectable" — and D5 applies the same
three:

1. **Load-time validation.** Host variable names are a closed set, so
   `config::merge::validate` (`merge.rs:422`) checks every `{name}` against
   it and reports unknown names with the offending template named. `conway
   config validate` catches the typo before the TUI ever starts. Names in the
   `plugin.*` namespace are skipped here — plugins register after config
   load.
2. **Render-time marker.** An unresolvable name renders as `⟨?ctx_pc⟩` in
   `theme.error`, in place. Loud, local, self-documenting, and it does not
   destroy the rest of the line the way a whole-template rejection would.
3. **Inspectability.** `conway status-line explain` prints the resolved
   template, every variable it references, each one's current value, and its
   source (host / view / plugin id). Precedent and rationale are
   `PermissionBroker::active_patterns` — "a rule set nobody can inspect is a
   trap" (`permission.rs:418`) — and D2 §8's third detection route.

**Unknown is not the same as unavailable, and the difference is the whole
design.** `{plugin.foo.pr}` when the plugin is dead resolves to *empty*
(silent, its group elides — §5.4). `{ctx_pc}` resolves to `⟨?ctx_pc⟩`. A typo
is loud; an absent plugin is quiet. Conflating the two is what makes Claude
Code's silent matcher no-ops a documented complaint.

**Syntax errors are a different class.** An unbalanced `{` or a nested `[`
invalidates *that one custom field*, which is dropped from the resolved list
with a one-time diagnostic naming the field and the byte offset. The rest of
the line is unaffected. P-10 is satisfied without the collateral damage of
falling back to the whole default line for one bad character.

### 2.3 Where a template may be authored: user scope only

**Decision: `[tui.status_line]`'s template keys are accepted from the
XDG/home `settings.json` and from `CONWAY_TUI__*` env, and are *ignored with
a diagnostic* when they appear in a project-scoped `.conway/settings.json`.**

This is D4's adopted Claude Code lesson — project *allow* rules require
workspace trust, project *deny* rules apply immediately — applied to the one
key that can author screen text. A repo you clone should not be able to write
sentences into your status bar, and the fix costs one check in
`config/merge.rs` where the project source is already handled distinctly
(`merge.rs:90-94`). The `fields` list itself stays project-settable: it is
still a closed-set selector and `resolve_fields`' forced-`mode` rule already
neutralizes the only abuse.

Follow-on, not v1: project-scoped templates behind D4's workspace-trust
ceremony. Deliberately deferred so D5 does not depend on D4 landing.

### 2.4 Styling: not in the template

**A template contains no styling markup.** A *custom field declaration* may
name **one theme slot** for its entire text:

```json
"custom": { "cache": { "template": "…", "style": "notice" } }
```

Slot names are a closed set (the `Theme` struct's own field names,
`theme.rs:107-244`), validated at load, falling back to the base style on an
unknown name — the same P-10 shape `Theme::from_config` already uses for a
malformed color (`theme.rs:297`). This needs one new function,
`Theme::slot(&self, name: &str) -> Option<Style>`, living in `theme.rs`.

This resolves the T1 grep guard cleanly rather than dodging it. The guard is
a real unit test with an explicit file list
(`theme::tests::no_inline_style_default_fg_color_remains_in_view_files`,
`theme.rs:960-994`) asserting `Style::default().fg(Color::` appears only in
`theme.rs`. A template naming colors directly would need a color parser
outside `theme.rs` — a violation in spirit even if the needle string differed.
Naming a slot keeps every `Color` literal where it already is. **Any new view
file (`view/template.rs`) must be added to that test's file list**; a new file
silently outside the list is how that guard rots.

Want two colors on one line? Define two custom fields. That is the deliberate
cap, and it is the difference between "arrange host values" and "author a
rendering language."

---

## 3. Where the template plugs in: `fields` keeps its spine

**Decision: the template does not replace the field list. It defines new
*named* fields that the existing closed-set `fields` list selects.**

```json
"tui": {
  "status_line": {
    "fields": ["session", "mode", "model", "ctx", "cache", "activity", "hint"],
    "custom": {
      "cache": {
        "template": ["{tokens_total} tok[ ({cache_pct}% cached)]",
                     "{cache_pct}%c"],
        "style": "status_dim",
        "priority": "telemetry"
      }
    }
  }
}
```

The rejected alternative is the obvious one: replace `fields` with a single
whole-line template string. It is rejected because it destroys four working
properties at once, and every one of them was bought with an adversarial
review finding:

- **The ladder.** Every field is a list of *complete shorter phrasings*, and
  the assembly steps them down one at a time in a fixed priority order until
  the line fits (`status_line_spans:344-353`). A single flat string has no
  rungs, so a narrow terminal falls straight through to `clamp_to_width`'s
  pathological-width path (`status.rs:446`) — the explicit-truncation
  *last resort*, used as the normal case.
- **`mode`'s survival guarantee.** `drop_priority` puts `Mode` last (`:529`)
  and `mode_ladder` never ends in an empty rung (`:616`), so `AUTO-ALLOW` is
  the last thing on the line to lose space and is never removed.
- **The forced-`mode` rule.** `resolve_fields:299` pushes `mode` in when the
  permission mode is non-default and the configured list omits it. It
  operates on a list of *names*; keep `fields` a list of names and it keeps
  working, unmodified, from both the file and env paths.
- **The per-field omission semantics** that make `git`/`cwd`/`model` vanish
  cleanly today.

Under the recommended shape, a custom field is simply another
`Vec<Vec<Span<'static>>>` from `field_ladder` (`status.rs:543`) — a
template *list* is a ladder, most detailed first, and the author writes the
rungs. All four properties survive with no change to the assembly loop.

`StatusLineField` becomes `enum ResolvedField { Builtin(StatusLineField),
Custom(usize) }`; `resolve_fields` resolves a name against the builtin
`parse` first, then the `custom` table, and drops it otherwise — the same
"unknown names are dropped" rule, now with §2.2's load-time validation making
the drop detectable.

`priority` is a closed enum of band names mapping onto today's
`drop_priority` numbering: `ambient` (0), `telemetry` (2). **`orientation`
and above are not offerable** — see §5.1.

`/settings` (V4) shows the resolved template **read-only** and names the file
to edit. A free-text template editor inside a modal is a worse UI than an
editor, and V4's menu is a leaf-value menu, not a text editor.

---

## 4. The variable namespace

Two tiers, and the split is P-8's honest answer rather than a workaround.

- **Session variables** — meaningful for any consumer of a session. These
  live on the **facade**, as a `conway::instrument` module producing a
  `Vars` snapshot (an ordered `name -> String` map) from a `SessionHandle`
  plus a focused `AgentId`. Library embedders and the one-shot CLI reach the
  same values the TUI does.
- **View variables** — meaningful only while a TUI is running (`spinner`,
  `elapsed_s`, `mode`). These are TUI-owned and overlay the session snapshot.
  Putting `spinner_frame` on the facade to satisfy P-8 literally would be
  worse than stating the boundary.

### 4.1 Exists today — zero plumbing, read straight off `AppState`

| Variable | Source | Tier |
|---|---|---|
| `mode` | `mode_label(&state.mode)` (`status.rs:579`) | view |
| `permission_mode` | `state.permission_mode.label()` | session |
| `model` | `state.focused_model` | session |
| `ctx_pct` | `focused_ctx_tokens` / `focused_model_max_context`, clamped 100 (`ctx_label:664`) | session |
| `ctx_remaining_pct` | `100 - ctx_pct` | session |
| `ctx_tokens`, `ctx_max` | `focused_ctx_tokens`, `focused_model_max_context` | session |
| `tokens_total` | `spent_tokens(&usage)` (`status.rs:928`) | session |
| `tokens_input`/`_output`/`_cache_read`/`_cache_write`/`_reasoning` | `Usage` fields (`content.rs:174-180`) | session |
| **`cache_pct`** | `cache_read / (input + cache_read + cache_write)` (`tokens_label:690`) | session |
| `activity` | `activity_phrase(&state.activity)` (`:911`) | view |
| `spinner` | `SPINNER_FRAMES[state.spinner_frame % len]` | view |
| `elapsed_s` | `turn_started_at.elapsed()` | view |
| `turn_tokens` | `state.turn_running_tokens` | view |
| `agent`, `agent_short` | `focused_agent`, `short_agent_id` (`agents.rs:144`) | session |
| `session`, `session_short` | `root_agent()` | session |
| `git_branch` | `state.git_branch` | session (see §4.4) |
| `cwd` | `state.cwd_display` | session |
| `agent_count` | `state.tree.nodes.len()` | session |
| `grants` | `state.permission_grants.len()` | session |
| `queued` | `state.queued_prompts.len()` | view |

**`cache_pct` is the one to lead with.** The line already renders
`tokens (n% cached)`, the arithmetic already exists, cache economics are
central to conway's O(1)-fork design (`ports/session.rs:49`), and no
competitor surfaces it. It costs nothing and it is the variable that makes
the feature feel designed rather than generic.

### 4.2 Needs plumbing, and worth it (small)

- **`cwd_short`** — tilde-abbreviated, or last two components. `cwd` is the
  single most likely variable to blow the width budget; a short form is one
  pure function and belongs beside the full one.
- **`version`** — `env!("CARGO_PKG_VERSION")` in `conway-cli`. Trivial; the
  Claude Code catalogue has it for a reason (bug reports).
- **`git_repo`** — the repo directory name. One more `git rev-parse` at the
  same startup call site as `read_git_branch` (`app.rs:1209`).
- **`conway::instrument::Vars`** — the facade module itself (§4). This is the
  real plumbing cost of the item, and it is what P-8 actually asks for.

### 4.3 Out, with reasons

- **Cost in USD** — nothing in the workspace prices a token; `grep -rn "cost"
  crates/conway-core/src` finds only prose. It needs a per-model price table,
  a currency, a staleness policy, and an answer to "priced when." `{tokens_*}`
  is the honest analogue. A price table is its own board item, not a status
  line feature.
- **Lines added / removed** — conway has no diff ledger and no edit-tool
  instrumentation for one. Inventing one to fill a status variable is a
  feature described by its display.
- **Rate limits and reset timestamps** — `BackendError::RateLimit {
  retry_after_secs }` exists (`conway-backends/src/error.rs:110`) but is
  per-request error classification, not a persistent quota model. "N% of
  quota remaining" needs a provider quota API conway does not call.
- **Vim mode** — the input box has no modal editing. Same rule D2 §4 used to
  reject `ConfigChange`: a variable naming a feature that does not exist
  documents a feature that does not exist.
- **Output style** — no analogue in conway.
- **PR number and review state** — needs an authenticated network call. This
  is the *canonical* `status/1` plugin, and saying so is more useful than
  half-implementing it in the host: it is the clean example of where the
  plugin path earns its keep (§5).
- **Repo host / owner / worktree** — same as PR: plugin territory. Adding
  four more startup shell-outs to the host for values most users will not
  reference is the wrong default.

### 4.4 A defect this work surfaces: `git_branch` and `cwd_display` never refresh

`state.git_branch` is set once in `App::new` (`app.rs:252`) and its own doc
says "No polling" (`state.rs:742-746`). After a `git checkout` in a `bash`
call the status line is simply wrong. `cwd_display` is worse: v0.6.0 shipped
`cd` and a mutable `CwdHandle` (`ports/plugin.rs:246`), so the displayed cwd
can be stale *by design* now, and D2 §4 adds `CwdChanged` partly for this.

Recommendation: the **cached-value-plus-async-refresh channel that §5.3
requires for plugin variables is the same channel that fixes these**.
Refresh `git_branch` on `ToolCallFinished` for `bash` (debounced) and
`cwd_display` on `CwdChanged`, both on the event loop, never from the render
path. File as its own defect; it is independently valuable and it is what
makes the async-refresh shape pay for itself twice.

---

## 5. Variable sources

### 5.0 Three sources, one rejected permanently

1. **Host-computed** — §4. The floor, zero I/O in the render path by
   construction.
2. **Plugin-supplied** — D2's `status/1`, namespaced `{plugin.<id>.<key>}`.
   The namespace prefix is mandatory and non-collidable with host names,
   which is why host names are `[a-z0-9_]` and only plugin names contain `.`.
3. **Shell command** — **rejected, and this time permanently rather than
   deferred.**

The v0.3.0 record (`01KYJFBQDEFAWSSXACXFQFYWP2`) *deferred* the shell hook.
D5 closes it, because the two things a shell command bought are now served
separately and better: **arrangement** by the template, **arbitrary data** by
`status/1`. A `status/1` plugin whose implementation happens to be a script
delivers the identical capability while being registered, named, versioned,
TTL-bounded, trust-gated by D4, and supervised by D1 (restart counts,
`next_retry_at`, a stderr ring buffer — `d1-transport.md:230-234`). A config
string that execs has none of that. Re-adding it would be strictly redundant
*and* strictly less safe, which is a rare enough combination to be worth
writing down so it is not re-proposed.

### 5.1 Obligation 1 — width budgeting

Build on v0.5.0's accounting; do not duplicate it. `ladder_width`
(`status.rs:407`) sums `Span::width()` — real display columns, backed by
`unicode-width` inside ratatui — and `clamp_to_width` (`:446`) makes the
pathological case explicit with a trailing `…`. Two layers on top:

- **Ingest cap (registry side).** A `status/1` contribution declares
  `max_len` at registration; the registry truncates on store, default 24,
  hard cap 64. **This cap is in `char`s, not columns** — a deliberate,
  stated compromise: `conway-runtime` has no `unicode-width` and adding one
  would violate C-04. Char-capping bounds memory and pathological input;
  column-accurate accounting happens at render, where ratatui is present.
- **Render floor (view side).** A custom field's `drop_priority` defaults to
  `ambient` (0) — it gives up space **before `cwd`**. A declaration may raise
  it to `telemetry` (2) and **no higher**. `orientation` (5-6), `activity`
  (7), `hint` (8) and `mode` (9) are unreachable from config.

**The invariant a test must pin:** *no configuration of custom fields can
cause `mode` to lose a rung earlier than it does today, or `hint` to be
dropped earlier than it does today.* That is the statement that keeps
`AUTO-ALLOW`'s survival guarantee true in the presence of arbitrary
config-authored text, and it follows mechanically from the priority ceiling.

### 5.2 Obligation 2 — control-byte sanitization

The rule is already written and already correct; it is in the wrong place
and there are three copies of it. `sanitize_rendered`
(`runner.rs:409`) maps every `char::is_control()` to U+FFFD, with tests at
`:567-590`. `permission_pattern.rs:246` documents a **hand-copy** of it
because of a crate-dependency edge. The status registry would be the third.

**Recommendation: hoist it to `conway-core` as
`conway_core::text::sanitize_control`, move the existing tests with it, and
have `runner.rs`, `permission_pattern.rs`, and the status registry all call
the one function.** Three hand-copies of a security-relevant transform is how
they drift.

Apply it at three boundaries:

1. **`status/1` ingest** — before the value is stored.
2. **Template literal text** — at template parse. This is new and it is the
   direct cost of breaking P-B: config text now renders, so config text is
   now untrusted input.
3. **Host variables that are not host-authored** — `focused_model` arrives
   off the wire in `Event::ModelDecision`; `cwd_display` comes from CLI args;
   `lineage` embeds `agent_def` names. **Finding: `focused_model` is rendered
   today with no sanitizer** (`status.rs:560`). A hostile or buggy provider
   returning a model name containing `\x1b[` reaches a ratatui `Span` as raw
   terminal control bytes. Small, pre-existing, real, and worth fixing
   regardless of this item.

**The 0.5.0 laundering lesson, stated for this context.** That bug was
sanitizing *before a security check*, so the sanitizer laundered the evidence
(`permission.rs:328-331`). Sanitizing at ingest is safe **here precisely
because nothing downstream of a status variable makes a security decision on
it** — it is display-only, by construction, in every one of the three
sources. That is the discriminating property, and it must be restated at the
call site or a future reader will "helpfully" move the sanitizer to match the
permission path's rule and get it backwards.

### 5.3 Obligation 3 — latency

D2 §11 case 4 says "the render path never calls a plugin and never blocks on
one." **D5 makes that structural rather than a rule to remember.**

`status_line_spans(&AppState, &Theme, u16)` is pure and takes no handle to
anything (`status.rs:322`). The plugin registry's last-known values are
**snapshotted into `AppState` by the event loop** — the same place
`focused_model` and `git_branch` already land — as one field:

```
pub plugin_status_vars: HashMap<String, String>,
```

The render path therefore cannot *name* the registry, let alone lock it. This
is where D5 goes slightly further than D2: D2 places the TTL on the
contribution; D5 additionally requires that **TTL expiry is evaluated at
snapshot time, on the event loop, not at render time.** Otherwise
`status_line_spans` becomes clock-dependent for TTL purposes and two renders
of the same `AppState` could differ — which would break the existing
`status_line(&state)` test seam (`status.rs:195`) that the whole test module
is built on. (It already reads a clock for `elapsed_s`; the rule is "no *new*
clock reads that change which variables exist," not "no clock reads.")

`git_branch` is the existing model for this shape — read once off the render
path, cached in `AppState` (`app.rs:252`) — and §4.4 is its bug: the model is
right, the refresh half was never built.

### 5.4 Obligation 4 — failure

Failure is structurally impossible today. The explicit rule:

> **A variable that is known but unavailable resolves to the empty string.
> Its enclosing group elides. A field whose every rung renders empty is
> omitted, exactly as `git` is when there is no repo. Never a panic, never a
> block, never a placeholder that could be mistaken for a value.**

Concretely: plugin absent, plugin dead, TTL expired, contribution never sent,
`focused_model` before the first `ModelDecision` — all one behavior, the one
`field_ladder` already implements for `Git`/`Cwd`/`Model` (`status.rs:559-574`).

The distinction from §2.2 is the load-bearing part and bears restating:
**unavailable is silent, unknown is loud.** A plugin that is not running is a
normal state; a variable name that does not exist is a mistake.

---

## 6. Which other UI surfaces open

**Recommendation: open exactly two. The status line, and the one-shot summary
line. Close everything else; close the transcript permanently.**

Every surface opened is a surface to sanitize, width-budget, priority-order,
and test — and the sanitize/budget machinery above is per-surface work, not
free.

### Open

- **The status line** — the whole of §2-§5.
- **The one-shot summary line** (`crates/conway-cli/src/render/`). This is
  where P-8 stops being a slogan: the *session* tier of the namespace (§4) is
  facade-level, and a `--status-format "<template>"` flag proves it with one
  renderer and one variable set. It needs no width budget, no theme, and no
  ladder — the constraints that make the status line expensive are all
  TUI-specific.
  **Constraint: it writes to stderr, never stdout.** `render/text.rs:1-6` is
  explicit that stdout carries only the assistant's raw text, verbatim, so
  `conway -p "…" > out.txt` yields clean content. A summary line on stdout
  would break that guarantee for a cosmetic feature. View-tier variables
  (`spinner`, `elapsed_s`, `activity`) are unavailable there and resolve
  empty — a natural, explainable consequence of the two-tier split rather
  than a special case.

### Closed

- **Transcript entry prefixes** (`transcript.rs:303`, `entry_lines`) —
  **closed permanently, and this is the strongest "no" in the document.**
  The transcript is the surface whose entire job is attribution: who said
  what. Making `you>` and the assistant marker config-authored turns
  provenance labeling into a spoofing surface, on the one surface where
  spoofing matters, and (per §0) project config participates in that. It is
  also the surface the clean-copy invariant protects with a test
  (`entry_lines_never_contain_box_drawing_glyphs`, `transcript.rs:688`), and
  a config-authored prefix lands in the user's clipboard.
- **The sticky prompt breadcrumb** (`header.rs:112`) — its content is one
  thing (the governing prompt) and its whole design is about not lying about
  *which* prompt (`header.rs:54-77`). There is nothing to arrange.
- **The scroll footer** (`header.rs:275`, `footer_text:310`) — one number and
  its own four-rung ladder. Templating buys renaming `↓ N lines above tail`,
  and it is the affordance that tells a lost user how to get back to the
  tail. Cosmetic-only change to a recovery affordance: not worth the
  sanitize/budget cost.
- **`/agents` panel rows** (`agents.rs:95` `recipe_parts`, `:128`
  `hop_label`) — the most *tempting* one, because it is genuinely tabular.
  Closed for v1 for a specific reason: `hop_label` is deliberately shared
  with the status line's `lineage` field so "the breadcrumb and the panel can
  never disagree about how a given agent came to exist" (`status.rs:826-837`).
  Templating one side reintroduces exactly the drift that sharing was built
  to prevent. Revisit only if a user asks, and then template *both* from one
  definition.
- **Input box, `/help`, `/settings`, modals** — fixed chrome with nothing to
  instrument.

---

## 7. Open questions

1. **Does `conway::instrument::Vars` belong on the facade or in
   `conway-core`?** §4 puts it on the facade because it is assembled from a
   `SessionHandle`. If D3 wants status variables on the wire (a `status/1`
   plugin *reading* host variables, not just writing them), the type moves to
   core and becomes a wire commitment. D3's call.
2. **Does a `status/1` plugin get to read host variables?** D2 gives it
   `observe/1` (events in) and `status/1` (values out) but no way to ask "what
   is `ctx_pct` right now." Everything it needs is reconstructible from the
   event stream, so v1 says no — but it is a real ergonomic cost and D6
   should record it as a disclosed asymmetry the way D2 §3 discloses the
   `SubagentHost` one.
3. **Env-var override shape for a template.**
   `CONWAY_TUI__STATUS_LINE__FIELDS` works because the value is a list of
   simple tokens. A `custom` table is nested, and cramming a template through
   an env var re-opens the "is this env var project-controlled" question §2.3
   just closed for files. Recommend: `fields` stays env-overridable, `custom`
   does not.
4. **§2.3's project-scope rule needs D4 to define the trust ceremony** that
   would later unlock project templates. D5 does not depend on it landing.

---

## 8. Findings in the current tree (independent of this design)

- **`focused_model` reaches a ratatui `Span` unsanitized** (`status.rs:560`);
  it originates in `Event::ModelDecision`, i.e. off the wire. §5.2.
- **`sanitize_rendered` exists in three places** — the original
  (`runner.rs:409`), a documented hand-copy (`permission_pattern.rs:246`),
  and a third needed here. §5.2.
- **`git_branch` never refreshes** (`app.rs:252`, `state.rs:742`), and
  **`cwd_display` is now stale by design** since v0.6.0 shipped `cd` and a
  mutable `CwdHandle`. §4.4.
- **Project-scoped `.conway/settings.json` outranks the user's own file**
  (`discovery.rs:10`, `merge.rs:81-94`). Harmless today for
  `[tui.status_line]`; not harmless the moment it can carry text. §0, §2.3.
- **The T1 guard is a fixed file list** (`theme.rs:967-985`); a new view file
  is silently outside it. §2.4.
