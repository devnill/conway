# Changelog

All notable changes to **conway** are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **A typed `Event::UserTurn` for the event stream.** A user's own prompt
  had no typed representation on the flat `Event` enum: replay fell back to
  `Event::AgentProgress { note: format!("user turn: {text}") }`, so the only
  way to recognize a prompt was matching a literal `"user turn: "` string
  prefix — fragile (a genuine notice could start with it too), and it meant
  a library consumer watching the bare `EventStream` could not tell "the
  user said this" from "the runtime noted this" the way the TUI could,
  because the TUI's transcript prompt bubble came from its own local push,
  not from the event stream at all. That was a real mode divergence: the
  TUI only *looked* correct because it kept its own copy alongside the
  facade, exactly the kind of renderer bug the interactive-first principle
  calls out.

  `Event::UserTurn { text, prov }` closes this, and is emitted **live**
  (`conway-runtime`'s `Runtime::prompt`/`start_root`, and `subagent.rs`'s
  `start` for a `Spawn` with a non-empty prompt — ordering-checked so it
  never precedes that agent's own `Event::AgentSpawned`), not only
  synthesized on replay. The TUI's `submit`/`deliver_first_message` no
  longer push `Entry::User` into the transcript locally; `AppState::apply`
  now builds it from this same event, so a prompt appears exactly once
  whether the TUI is live, replaying a focus switch, or a library embedder
  is watching the raw event stream — one path, every mode.
  `ForkDirective`/`ParentSteer` remain on the `AgentProgress` fallback for
  now, a disclosed scope decision, not an oversight.

### Fixed

- **The TUI's sticky scroll header showed the wrong thing.** T6's own
  problem statement was scroll-shaped — "you scroll and lose track of where
  you are" — but its binding decision put `session <id> · agent <id>[ via
  lineage] · model · ctx%` on it: application chrome answering "what
  session/agent/model am I in", not "what am I looking at". The tell was
  T6's own gating, which showed that line only while the transcript
  overflowed the viewport — nobody gates session/model/ctx on scroll
  position if they actually mean it as persistent chrome.

  The overlay now shows **only** the current turn's own prompt, and only
  while it has scrolled out of view — triggered by whether the nearest
  `Entry::User` at or before the viewport's topmost visible row is itself
  still (at least partly) on screen, never by "the transcript overflows"
  (T6's original test) or `!follow_tail` (the floating footer's own test,
  which would also wrongly fire for a short turn scrolled back only
  slightly). It no longer reserves a layout row either: T6's header used to
  claim a real `Constraint::Length` row whenever content overflowed,
  reflowing the transcript out from under the reader as that row
  appeared/disappeared; the overlay is now drawn straight onto the frame
  after the transcript, the same way the floating "jump to bottom" footer
  already was.

  `session`/`model`/`ctx%` were never removed — they always belonged in the
  persistent status line and stay there. The one field that genuinely
  needed a new home was V5's lineage breadcrumb, which T6 had misfiled onto
  the scroll overlay in the first place: it moved to two new
  `[tui.status_line]` fields, `session` and `lineage` (added to the default
  Lean line), carrying V5's width-degrade machinery and its
  fork/spawn-content trap with it unchanged. The status line's own `hint`
  field, which used to append `focused: <id>` off-root, now suppresses that
  note whenever `lineage` is part of the resolved field list, so the two
  never say the same thing twice — it survives only as a fallback for a
  pinned `fields` config from before this change, which will not
  automatically gain either new field.

- **The sticky prompt overlay re-wrapped the entire transcript, every
  entry, on every render.** `entry_row_starts` (used to find which entry
  governs the row currently at the top of the transcript viewport) built a
  fresh `Vec<Line>` + `Paragraph` and re-ran `line_count` for every
  transcript entry unconditionally, with no early exit — and it ran on
  every dirty render, which includes a 125ms animation tick throughout
  active streaming, exactly when the transcript is also growing. It now
  short-circuits at the row the caller actually asked about: entries whose
  own start row already exceeds that point are never turned into `Line`s or
  measured at all. A `state.follow_tail` skip was considered and explicitly
  rejected — the overlay legitimately shows while auto-following the tail
  of a single turn whose own response is taller than the viewport (a long
  streaming answer, the most common time this runs), so that gate would
  have silently hidden a correct overlay rather than been a safe no-op.

- **The status line could silently clip `hint` off narrow terminals.**
  Adding `session`/`lineage` to the default field order (see above) grew
  the line's full length to ~106 characters; every field but `lineage`
  rendered its full text unconditionally with no `.wrap()`, so anything
  past the render width was clipped by the terminal with no visible sign —
  at 80 columns `hint` lost roughly 26 characters versus ~18 before, and
  below ~40 columns it vanished entirely, along with the line's only
  pointer to `/help` and the `/agents` toggle. The status line now budgets
  its own width: each field degrades through a small ladder of
  shorter-but-still-complete phrasings (the same shape the floating scroll
  footer and `lineage`'s own Full → Compact → Bare degrade already used),
  giving up space in a fixed priority order (ambient chrome and telemetry
  first, then `session`/`lineage`, then `activity`, then `hint`, with
  `mode` never dropped) until the line fits or nothing more can be shrunk.
  `AUTO-ALLOW` — a genuine safety signal, not decoration — is the one
  thing on the line guaranteed to survive as long as anything does, down
  to the narrowest terminal that shows anything at all. See
  `docs/crates/conway-cli.md`'s `[tui.status_line]` section for the full
  give-up order and reasoning.

## [0.3.0] — 2026-07-28

### Added

- **Permission modes and pattern grants.** Approving every command
  individually does not scale — a real session can produce hundreds of
  prompts. Three modes now exist: `prompt` (the default, unchanged
  behavior), `plan` (non-mutating tools only), and `AUTO-ALLOW`. The mode
  is switchable from `/settings`, which is also the escape hatch out of an
  over-broad mode mid-session, and it is always visible in the status line.

  The underlying `AllowAlways` machinery already existed; the reason it
  never helped is that its cache key included a digest of the exact
  arguments, so `git status` and `git diff` were different entries and
  every distinct command re-prompted.

  Pattern grants fix that: `bash:git status` covers `git status --short`
  but not `git push`. Patterns are **prefixes matched on whole arguments,
  not regexes** — `bash:git .*` reads as tight, but `.` matches `;`, so it
  would authorize `git status; <anything>`.

  **The rule that makes prefixes safe:** a pattern applies only when the
  command contains no shell metacharacters. `git status && <anything>`
  starts with `git status`, so it always re-prompts regardless of any
  matching grant. The check runs before any prefix comparison, so nothing
  can bypass it. It is deliberately over-eager — a harmless pipe still
  re-prompts, because an unnecessary prompt costs a keystroke and a missed
  one costs arbitrary execution.

  Plan mode is defined on the tool's **declared category**, never on
  command text: `bash` declares `Execute` whatever it is handed, so
  `bash cat file` is blocked even though it only reads. Deciding otherwise
  would mean parsing shell. A category Conway does not have yet is blocked,
  not allowed.

  Grants inherit to subagents via the existing `AgentSubtree` scope. Rules
  persist to `.conway/permissions.json` (project-first, then global) as a
  human-readable list; a corrupt file **fails closed**, authorizing nothing.

  The permission prompt offers `[p]` to grant a pattern, and states what
  accepting would permit before you press it. The offered prefix is two
  tokens (`git status`, not `git`) — `git` alone would silently include
  `git push --force`. No offer is made for a command carrying shell
  metacharacters, since the gate would refuse to honor it anyway. Rules
  from the project and global files merge; new grants are written to the
  project file so they can be reviewed in a diff. Switch modes, review
  grants, and revoke them all from `/settings` (per-rule revocation is not
  implemented yet).

- **TUI: `/help` keybinding overlay (T7).** `/help` used to dump a static
  command list into the transcript as a pile of `Entry::Notice` lines,
  spamming the conversation with content that already lived in the `/`
  command palette, and there was no keybinding reference anywhere. `/help`
  now opens a read-only overlay (`tui/view/help.rs`) instead and pushes
  zero transcript entries.

  The overlay is keybindings-only: every genuine slash command stays
  exclusively in the `/` palette, so the two surfaces can never drift into
  duplicating each other (`/thinking`/`/timestamps` were the one deliberate
  exception at the time, since they functioned as keyboard-driven view
  toggles — both are since removed in favor of `/settings`, above). It
  groups every binding Conway actually has — input & editing, history &
  navigation, tools & display, the settings menu's own keys, the modal-only
  keys for the `/ask` modal / intent-confirm card / permission prompt, and
  the agent panel — plus a trailing note that mouse-wheel scrolling is
  deliberately not a Conway binding (it's your terminal's own scrollback;
  capturing the mouse would disable the terminal's native click-drag text
  selection). `Esc` closes it; no hotkey opens it, since Conway is always in
  input-typing mode.

  The overlay is not a `Mode` variant — it's a plain `AppState::help_open`
  flag, gated on `mode == Normal` at both draw and key-routing time — so it
  can never stack on top of an active permission prompt, `/ask` modal, or
  intent-confirm card (each of those is a decision the user owes an
  answer), and reappears on its own once one resolves. New theme slots:
  `help_border` (blue, bold) and `help_key` (green, bold).

- **TUI: input ergonomics — multi-line, persisted history, bracketed
  paste, and a cursor-clamp fix (T8).** The input line was
  single-line-only (`Enter` always submitted, `\n` could never land in
  it), had no memory of previous prompts, mangled a pasted block into a
  flood of individual keystrokes, and clamped a long line's cursor to the
  box's own width instead of scrolling — the cursor froze at the right
  edge while the text kept extending off-screen invisibly.

  `Alt-Enter` **and** `Shift-Enter` both insert a literal `\n` (some
  terminals encode Shift-Enter indistinguishably from a plain Enter, so
  only binding one would silently lose multi-line entry there); plain
  `Enter` still submits. The box's own height grows with the draft
  (capped at a third of the terminal height) without disturbing T6's
  header-overflow math, which now reads the same grown height.

  `Up`/`Down` recall a bounded, persisted history FIFO
  (`[tui.history_size]`, default 500) — oldest evicted once the cap is
  exceeded, `Down` past the newest entry restores whatever unsent draft
  you had going, and a recalled entry is editable inline before
  resubmit. History is contended with the `/` command palette, the
  `/agents` panel, and a multi-line draft's own interior lines, resolved
  in that fixed priority order so recall can never fire while another
  surface owns the arrow keys. It persists to `~/.conway/history`
  (alongside the global config, not the project checkout), one
  JSON-string-encoded entry per line so an embedded `\n` round-trips,
  written via a tmp-then-rename so a crash mid-write can't corrupt it. A
  missing/corrupt file degrades to an empty history (P-10) and a failed
  write never fails the submit that triggered it.

  Bracketed paste is now actually enabled on the terminal (it previously
  wasn't, so `Event::Paste` never even arrived) and inserts the whole
  pasted block as one edit at the cursor, not a per-character flood.

  The cursor-clamp bug is fixed: the box's cursor line now scrolls
  horizontally (and, for a tall multi-line draft, the box scrolls
  vertically) so the cursor is always genuinely at the character it
  claims to be at, instead of visually pinned to `width - 2` regardless
  of the draft's true length.

- **TUI: sticky context header, End/Home jump keys, and a scrolled-back
  indicator (T6).** Scrolling back through a long conversation gave no
  sense of position and no way home but paging. Three keyboard-only
  affordances now cover it.

  A **sticky header** (`session · agent · model · ctx%`) sits above the
  transcript, but only while the transcript actually overflows — content
  that fits on screen never gives up a row. `agent` shows only off-root
  and `model` only once routing has happened, so the single-agent case
  stays uncluttered. The `ctx%` figure reuses `status::ctx_label` rather
  than recomputing the percentage, so header and status line cannot
  disagree.

  **End** snaps to the tail and re-engages auto-follow; **Home** jumps to
  the top and disengages it. Both apply only when the input box is empty
  — with text present they keep their ordinary cursor-movement meaning,
  so the jump never steals a key mid-edit.

  A **floating footer** (`↓ N lines above tail — End to jump to bottom`)
  overlays the transcript's bottom row while scrolled up, with a live
  count, and disappears when auto-follow re-engages. On a narrow terminal
  it degrades to a shorter complete form rather than clipping mid-word,
  since a truncation would cut the `End` hint off first.

  Neither widget joins the transcript's `Paragraph` (the header gets its
  own `Rect`; the footer is a `Clear` overlay), so the clean-copy
  guarantee is unchanged. New theme slots `header` and `scroll_footer`.

  Mouse-wheel scrolling remains deliberately unimplemented: capturing the
  wheel would disable the terminal's native click-drag text selection,
  which clean-copy exists to protect. Native terminal scrollback is
  unaffected — it scrolls the emulator's buffer, not Conway's, which is
  why it cannot drive the indicator.

- **Kimi coding-plan support, and Anthropic-compatible endpoints
  generally.** Kimi's coding plan is served over an Anthropic-shaped
  `/v1/messages`, so it needs no dedicated adapter — point `base_url` at
  `https://api.kimi.com/coding/` with `kind = "anthropic"`. Endpoints
  under a path prefix now have that prefix preserved (`.../coding/` →
  `.../coding/v1/messages`), pinned by a test so a future refactor cannot
  silently drop it.

  `AnthropicConfig` gained an optional `id`, defaulting to `"anthropic"`
  so existing configs are unaffected. Previously `AnthropicBackend::id()`
  was hardcoded, which forced any Anthropic-kind backend to occupy the
  config key `"anthropic"` — you could not name a backend `kimi`, and you
  could not configure Kimi and Anthropic at the same time. Both now work;
  the build-time key check that enforced the old constraint is removed.

  Bundled model metadata gains `k3-256k` (262,144 tokens) and `k3[1m]`
  (1,048,576 tokens), so the status line's `ctx%` is accurate rather than
  falling back to raw counts. The literal brackets in `k3[1m]` are part of
  the provider's model id; a test pins that they survive TOML parsing.

  See `docs/crates/conway-backends.md` for a copy-pasteable config, which
  is itself pinned by a test that loads it through the real config loader.

- **TUI: tool output folding + expand (T5).** A settled tool entry's
  preview in the transcript now renders **folded** by default: the first
  `[tui.tool_preview_lines]` physical lines (default 3) plus a dim
  `… (+M lines, Ctrl-E to expand)` affordance naming how many lines are
  hidden, instead of spilling the entire preview inline with no bound.
  `Ctrl-E` flips `expanded` on **every** tool entry at once (MVP — no
  transcript-cursor/selection state, so expand/collapse is all-at-once);
  an expanded entry renders its full preview. The stored `preview` is
  never truncated — the cap is render-time only, so toggling never loses
  data. The toggle is pure state mutation: `Ctrl-E` does NOT touch
  `scroll` or `follow_tail`; the next render's existing clamp
  (`state.scroll.min(max_scroll)`) re-clamps to the nearest valid
  position without snapping the viewport. `Ctrl-E` is a control key
  (not a bare `e`, which stays ordinary text input for the always-on
  input box), bound directly to
  `AppState::toggle_all_tool_entries_expanded` (mirroring the `v`
  visibility-filter key's direct-mutation pattern — no `Action` variant,
  no facade side effect). Settled tool output honors the clean-copy
  invariant: no box-drawing, no `Block` — the entry ends with a blank
  line + a dim plain `-` rule as a non-box separator. New config key
  `[tui.tool_preview_lines]` (optional integer, default 3, clamped to
  `1..=200` with a fallback to 3 on bad input — P-10, never a panic);
  `CONWAY_TUI__TOOL_PREVIEW_LINES=10` overrides via env. The
  `Entry::Tool::expanded` flag and the `tool_lines` collapsed/expanded
  render branch are generic for T4's tool-args reuse. The status-line
  `hint` field now advertises `Enter submit · Ctrl-E expand` (reconciling
  the earlier `Ctrl-E submit` hint — Enter was always the actual submit
  key; T8 will move submit to Alt/Shift-Enter). See
  `docs/crates/conway-cli.md`'s "Tool output folding + expand (T5)"
  section.

- **TUI: transcript provenance — speaker markers, reasoning variant,
  timestamps, tool args/progress (T4).** The transcript now surfaces
  per-entry provenance: reasoning traces (`Event::ThinkingDelta` ->
  `Entry::Reasoning`, dim+italic with a `thinking> ` prefix, **expanded
  by default**; `/thinking` toggles `show_reasoning` and hides them
  entirely while off, but entries are still stored so toggling back on
  restores them without replay); assistant speaker markers
  (`Entry::Assistant` carries the serving model name, rendered as a
  `[modelname]> ` prefix in `theme.assistant_marker`; omitted on replay
  where no model provenance is available); tool args + progress
  (`Entry::Tool` stores `Event::ToolCallProposed::args` as a compact
  JSON string, rendered as a one-line truncated `args: …` preview while
  collapsed and pretty-printed while expanded — both args and output
  expand/collapse together via T5's `Ctrl-E` toggle; accumulated
  `Event::ToolProgress { call_id, note }` notes — previously dropped —
  append to the matching in-flight tool entry by `call_id` and render as
  dim `-> {note}` lines); per-entry timestamps (`/timestamps` toggles
  `show_timestamps`, default off, prepending an `HH:MM ` prefix styled
  with `theme.timestamp` to each entry's first rendered line); and a
  turn-end summary (`Event::TurnFinished` stamps `{elapsed} · {tokens}
  ({n%} cached)` — e.g. `1m 6s · 1.4k tok (88% cached)` — onto the last
  assistant/reasoning block, rendered as a final dim line). The
  streaming cursor (T2) extends to the live reasoning line while
  `activity == Thinking`. New theme slots `assistant_marker`,
  `reasoning`, and `timestamp` (defaults: magenta+bold, dark_gray+italic,
  dark_gray) are configurable via `[tui.theme]`. `/thinking` and
  `/timestamps` are intercepted in `app.rs::submit` (state-only toggles,
  never sent to the model), listed in `/help`, the command palette, and
  the status-line hint. Settled `entry_lines` output honors the
  clean-copy invariant (no box-drawing glyphs). See
  `docs/crates/conway-cli.md`'s "Transcript provenance (T4)" section.

- **Facade lifecycle ops for ephemeral `/ask` children** —
  `Conway::promote` (the one-way ephemeral→persistent flip: durable header
  rewrite, live-tree flip, and an `Event::AgentPromoted` for UIs, in that
  failure-ordered sequence), `Conway::pull_in` (merge the child's question
  and answer into the parent's log — the question re-stamped
  `Provenance::MergedAsk`, assistant records verbatim — then purge the
  child), `Conway::purge` (discard a terminal ephemeral child), and
  `Conway::sweep_stale_modal_asks` (crash-residue reaper). Ephemeral `/ask`
  children now attach as proper fork children of the asker, so they appear
  in `/agents` marked `(ephemeral)` while running.

- **`conway_ask` model-facing tool**: runs a prompt in an ephemeral fork of
  the calling agent and returns the child's full reply text (not a truncated
  summary), so the model can compose it into a `conway_subagent` spawn and
  keep curation/context-drafting inference out of the orchestrator's context
  window. Fork-only (`prompt` + optional `budget`); the child is marked
  ephemeral (shown in the TUI `/agents` panel with an `(ephemeral)` marker
  while running, and under the `v`-cycled all/finished views once done;
  excluded from default session listings; still attached to the live agent
  tree for provenance). Composes
  `conway_subagent` per the "exactly two subagent primitives" principle —
  `ask` is fork+await-text, not a third primitive.

- **`conway_ask` gains an optional `tools` arg**: narrows the ephemeral fork
  child's tool set to the named tools (`ToolSelector::Only`, the same
  selector `conway_subagent`'s `tools` arg produces) — e.g.
  `{"prompt": "summarize the diff", "tools": ["read"]}` restricts the child
  to read-only inspection. Narrowing-only: it can restrict, never widen, the
  tool set the child would otherwise inherit.

- **NL intent on `/fork` and `/spawn` with a mandatory confirmation card.**
  Free text after `/fork` or `/spawn` that does NOT start with explicit
  `@<agent_def>` syntax is classified by the facade's `intent` role
  (`Conway::classify_agent_intent`, C1) BEFORE any agent is created, and
  the classified result is shown in a confirmation card
  (`[enter]` confirm / `[e]` edit / `[esc]` manual) so inference can never
  silently choose (P-10). `[enter]` runs the classified recipe as-is
  (possibly cross-classified); `[e]` drops the classified prompt into the
  input line for editing; `[esc]` falls back to today's pre-classification
  manual flow with the raw text untouched. The verbatim passthrough
  (unconfigured `[roles.intent]` role, unparseable reply, invalid recipe,
  empty prompt) still shows the card with the raw text; a hard
  `ConwayError::IntentClassification` does NOT show the card and falls back
  to the manual flow with a notice. Explicit `@<agent_def>` syntax and bare
  invocations are unchanged. Oneshot (`-p`) `/fork`/`/spawn` paths are
  unchanged (deferred).

### Changed

- **TUI: palette audit — what each color means, and a few defaults tighten
  up (V7).** The request was "a little more visual polish," which usually
  means "more color" — the audit went the other way and found the palette
  was already mostly restrained; the real gaps were a couple of colors
  spent on things that don't carry meaning, and one real safety signal that
  had none.

  **Defaults change appearance** for three reasons, each narrow:

  - `timestamp`, `reasoning`, and `agent_cancelled` move off a fixed
    `Color::DarkGray` to a relative `Modifier::DIM`. `DarkGray` is an
    absolute dark color, and a dark-background terminal's own "bright
    black" frequently renders it nearly indistinguishable from the
    background; `DIM` asks the terminal to dim its *own* foreground
    instead, which stays legible on both a dark and a light scheme.
  - `help_key` (the `/help` overlay's key/chord column) drops its green —
    green already means "success" (`tool_done`/`agent_finished`) elsewhere
    in the palette, and reusing it for a plain column split blurred that
    meaning for no reason. It stays bold.
  - The status line's `AUTO-ALLOW` indicator — every tool call
    auto-approved with no prompt, a genuine safety-relevant state — now
    renders with `theme.fatal_error` (red + bold) instead of the plain
    `theme.emphasized` (bold, no color) it shared with the much lower-risk
    `plan` mode. `plan` keeps the unstyled-but-bold treatment; it only ever
    restricts what runs.

  If you had pinned any of these via `[tui.theme.timestamp]`,
  `[tui.theme.reasoning]`, `[tui.theme.agent_cancelled]`, or
  `[tui.theme.help_key]`, your override still applies unchanged — only the
  built-in defaults moved.

  **Removed:** the `agent_marker` theme slot (and its `[tui.theme.
  agent_marker]` config key) never had a call site anywhere in `view/*.rs`
  — a key a user could set that would silently do nothing, the same
  failure V6 already ruled out for `spinner_b`/`spinner_c`. It is now an
  unrecognized key rather than a no-op; if you had it set, remove it. No
  functional behavior changes either way, since it never rendered anything.

  **Considered and not done:** collapsing the `tool_*`/`agent_*` status-tag
  families (five duplicated color pairs) into one semantic set. The
  duplication is real but not a rendered-UI problem — the two families
  never draw side by side — and collapsing would have meant either breaking
  configs that already set one of the ten names or a real aliasing
  precedence risk, for a problem that is presently invisible on screen. See
  `docs/crates/conway-cli.md`'s new "Palette rationale (V7)" section for
  the full reasoning, the color-meaning rules, and what a future slot
  addition should follow.

- **TUI: `/thinking` and `/timestamps` are replaced by a single `/settings`
  menu (V4).** Two standalone slash commands, each owning exactly one
  boolean, don't scale — every future display preference would mean another
  command competing for footer/palette space. Both are now REMOVED (not
  aliased): `/settings` opens a menu, built on V1's shared modal/tree
  primitives (`tui/view/menu.rs`, its first real caller), covering "show
  reasoning traces", "show timestamps", and a THIRD setting new to runtime
  entirely — `tool_preview_lines` (T5's tool-output fold cap, previously
  config-only). The one non-boolean setting is a `Left`/`Right` stepper
  (±1, floor/cap at `1..=200`) rather than a cycled preset list — there's no
  natural "meaningfully different" preset set for a fold-cap the way a
  theme picker would have.

  Settings are **session-only**, exactly as the two commands they replace
  already were: Conway's config load is a five-source layered read with no
  writer anywhere outside test fixtures, and inventing one raises "which
  layer gets written" with no good default answer — out of this item's
  scope. A footer note says so on every render; the one setting with a real
  backing config key (`[tui.tool_preview_lines]`) names it inline, and the
  two that have no config-key equivalent today carry no such claim.

  `/settings` is gated exactly like `/help` — a plain `AppState::
  settings_open` flag, never a `Mode` variant, so it can't stack on an
  active permission prompt / `/ask` modal / intent-confirm card — and, new
  for this item, `/settings` and `/help` are also mutually exclusive with
  EACH OTHER (opening one closes the other), since both are informational
  overlays gated the identical way.

- **The status line no longer pulses, and the footer no longer lists slash
  commands.** The spinner's braille frames still advance — motion is the
  liveness cue — but the color is now steady. Cycling it on every 125ms
  tick read as strobing in the corner of the eye rather than as a signal,
  and competed with the frame animation already doing that job.

  The `spinner_b` and `spinner_c` theme slots are removed along with their
  `[tui.theme]` config keys. A config key that silently does nothing is
  worse than no key at all. If you had set either, the spinner now uses
  `spinner` alone.

  The footer read `Enter submit · Ctrl-E expand · Ctrl-P/N history ·
  PgUp/PgDn · /help · /thinking · /timestamps · /agents…`. It now names
  keys rather than commands: `Enter submit · Ctrl-E expand · /help ·
  /agents…`. Nothing became undiscoverable — `/help` is the keybinding
  overlay, which is where the rest already lives.

- **Config no longer inspects the shape of an API key.** The
  `sk-ant-oat*` prefix rejection is removed from all three layers that
  enforced it (`AnthropicConfig::validate`, `config::merge::validate`, and
  `ConwayBuilder`'s `api_key_env` resolution), along with the
  `ConfigError::SubscriptionTokenRejected` variant. Any non-empty key is
  now passed through to the configured `base_url` as-is.

  Policing which credentials look legitimate is an opinion that does not
  belong in the core, and it blocked a real use case: an
  Anthropic-compatible third-party endpoint (a coding-plan subscription, a
  self-hosted shim) could not be configured, and the resulting error
  misdirected the user to `console.anthropic.com` — the wrong vendor
  entirely. Whether a key works is the provider's answer to give, and its
  auth error is more accurate than any prefix match Conway could perform.

  **Unchanged:** an empty or whitespace-only `api_key` is still rejected
  (`ConfigError::MissingApiKey`), and an `api_key_env` naming an unset
  variable is still a hard error that names the variable. Those describe a
  missing credential, which Conway can identify precisely, rather than
  judging one it has.

- **TUI: status line rework — model + ctx% + cwd + git + field config
  (T3).** The bottom status line is now an ordered, configurable set of
  fields driven by a new `[tui.status_line]` `settings.json` section
  (schema: `conway::config::schema::StatusLineConfig`). The default Lean
  line is `mode | model | ctx | tokens | activity | hint`; `git` and
  `cwd` are also available as orderable fields. Each field renders only
  when listed in the configured `fields` order AND has data to show
  (`model` is omitted before the first turn routes; `git` is omitted
  outside a repo; etc.). Unknown field names are dropped at render time
  — never a panic (P-10). New fields: `model` (the focused agent's
  serving model display name from `Event::ModelDecision`); `ctx`
  (context-window occupancy — `ctx 42%` when the focused model's max
  context is known from `[models.metadata_path]`, else the raw
  cumulative `Event::ContextSegmentAdded` token estimate, compact-
  suffixed as `ctx 12.3k`; capped at `ctx 100%`); `tokens` formalizes
  the cumulative spend slot as `<total> tok (<n%> cached)`, where
  `total` is every `Usage` field summed and `n%` is the cache hit rate
  `cache_read / (input + cache_read + cache_write)` (the parenthetical
  is omitted when the denominator is 0 — divide-by-zero guarded); `git`
  (the current `git rev-parse --abbrev-ref HEAD` branch, read once at
  startup, best-effort, no polling, no new deps); `cwd` (from `--cwd` or
  `config.cwd`). The `activity` field IS T2's working indicator
  (spinner + pulse + elapsed + `+{n} tok`), unchanged; the `hint` field
  is a persistent keybinding/affordance hint (`Ctrl-E submit · ↑↓
  history · PgUp/PgDn · /help · /agents to {view|hide}`, plus
  `focused: <id>` off-root). `AppState::apply`'s previously-dropped
  `ModelDecision` arm now captures the focused model + max context;
  `ContextSegmentAdded` now also accumulates a session-wide cumulative
  context-token estimate (distinct from T2's per-turn
  `turn_running_tokens`). See `docs/crates/conway-cli.md`'s
  `[tui.status_line]` section for the full field table, the
  `tokens (n% cached)` format, and reordering/hiding instructions.

- **TUI: activity spinner + animation tick (T2).** The status line's
  "is it working?" slot now renders a braille spinner glyph plus the
  activity word plus live elapsed seconds plus the new context tokens
  added this turn (`⠋ thinking… 12s · +45 tok`) while the focused agent
  is working. The spinner glyph and the activity word pulse together
  through a small theme palette (`spinner`/`spinner_b`/`spinner_c`,
  defaulting to yellow/light_yellow/white) on a new 125ms (8 TPS)
  animation tick, additive to the existing 16ms redraw cap. The tick is
  gated by `should_animate(activity)` so an idle terminal never pays for
  animation — the counters don't advance and no redraw is forced while
  idle. The elapsed clock starts at `Event::TurnStarted`; the `+{n} tok`
  figure accumulates from `Event::ContextSegmentAdded` deltas —
  session-deduped new-segment tokens added this turn (NOT total context
  occupancy: the runtime emits `ContextSegmentAdded` only for segments
  new to a never-reset `seen_segments` set, so the figure is large on
  turn 1 then small on turn 2+ for the same conversation). The leading
  `+` signals "added this turn" and distinguishes it from the
  cumulative `| {tokens} tok |` slot; the authoritative turn-end token
  total lands via the turn-end summary (T4). New theme slots
  `spinner_b` and `spinner_c` join the existing `spinner` slot to form
  the pulse palette. While `activity == Responding`, the live,
  in-progress assistant line in the transcript also gets a block `▌`
  streaming cursor appended at render time only — never baked into the
  stored `Entry::Assistant` text or into `entry_lines` output for
  settled entries (clean-copy invariant relaxed only for the
  actively-streaming line). See `docs/crates/conway-cli.md`'s "Activity
  spinner + animation tick" section for the full mechanism.

- **TUI: central theme module + named styles (T1).** The TUI's render
  pass now reads colors/styles from a single `Theme` struct threaded
  through `view::draw` and each per-view `draw` fn as `&Theme`, replacing
  the per-call-site `Style::default().fg(Color::…)` the five view files
  used to hand-roll inline. The theme is configurable from the start via
  a new `[tui.theme]` `settings.json` section (per-named-style `fg`/`bg`/
  `modifiers` overrides; defaults match the pre-T1 exact
  `(Color, Modifier)` pairs, so an unconfigured TUI renders identically).
  Malformed overrides fall back to the affected slot's default — never a
  panic (P-10). New accent styles `assistant_marker`, `reasoning`,
  `agent_marker`, `fatal_error`, `status_dim`, and `spinner` are defined
  for later v0.3.0 polish items to consume. See
  `docs/crates/conway-cli.md`'s `[tui.theme]` section for the full named-
  style table and accepted color/modifier spellings.

- **`/ask` is now a single-turn modal with three forced fates.** Asking
  forks an ephemeral child (visible in `/agents` marked `(ephemeral)`),
  runs one turn, and opens a modal over the child's answer. Closing the
  modal forces exactly one choice: `[f]` fork (promote the child to a
  persistent session), `[p]` pull in (merge the question and answer into
  the parent's own transcript, then purge the child), or `[esc]` discard
  (purge outright). Quitting with the modal open discards. A failed fate
  keeps the modal open with the error shown. The 0.2.0 rendering — a
  dimmed aside inline in the transcript — is gone. On startup the TUI
  sweeps modal-`/ask` residue left behind by a crashed process; a new
  `ask_origin` tag on the session header distinguishes these from
  `conway_ask` tool children, which are never swept.

- **TUI `/agents` panel is now the single agent surface.** Every row shows
  the agent's recipe label — `fork @seq N` for forks (with the inherited
  fork point), `@<agent_def>` for spawns with a named agent definition,
  `(inherit)` for spawns that inherited the parent's role/model — and
  ephemeral `/ask` forks are now visible in the tree with an `(ephemeral)`
  marker instead of being omitted. While the panel is open, `v` cycles row
  visibility (active-only by default, all, finished-only) as a draw-time
  filter that never mutates the tree. `/tree` is demoted to a hidden
  alias: it still parses and renders, but its output is derived from the
  same panel tree — the same nodes and recipe labels, shown unfiltered as
  plain-text transcript lines — and it no longer appears in `/help` or the
  command palette.

### Fixed

- **`Esc` no longer discards the agent you just focused.** Forking, opening
  `/agents`, focusing the new child, then pressing `Esc` to dismiss the
  panel bounced you straight back to the root — so "focus a child and get
  the panel out of the way" was not expressible at all.

  Two changes had independently bound `Esc` (one to close the panel, one to
  return to the root) and both fired on a single press. Only the
  panel-close half was ever documented in `/help`, which is what made this
  a bug rather than a shortcut.

  `Esc` now does one thing per press, innermost surface first: it closes
  the panel if open and keeps your focus; a second press returns to the
  root. With the panel already closed it returns to the root immediately,
  so no keypress is wasted.

- **The `/agents` panel no longer appears to randomly lose agents, and
  focusing a subagent now shows where it sits in the tree.** Two dogfooding
  reports, one root cause each:

  The panel's visibility filter defaulted to active-only, so a finished
  agent's row vanished the instant it finished — with `v` (the filter
  cycle key) undiscovered, that reads as agents disappearing at random. The
  default is now **all**: the list's *shape* stays stable regardless of
  status, and the existing per-row marker (`v`/`x`/`-` vs `*`/`o`/`?`)
  already conveys "still running" at a glance. `v` still cycles
  all → finished-only → active-only → all; only the starting point moved.

  Focusing a subagent used to clear the transcript down to that agent's own
  log with no indication of how it got there. The sticky context header
  (T6) now grows a lineage breadcrumb off-root — `agent <id> via root →
  fork @seq 3 → @reviewer` — built from the same per-node provenance text
  the panel row already shows (`fork @seq N`, `@agent_def`, `(inherit)`),
  so it can never disagree with the panel. It is metadata only, never the
  ancestor's actual transcript content: a fork child truly inherited its
  parent's log up to a fixed point and showing that would be accurate, but
  a spawn child inherited nothing, and showing parent content next to it
  would display information the agent never saw. A deep chain degrades to
  a shorter complete form (`…(N)` collapsing the middle) rather than
  clipping mid-word, the same shape the T6 floating footer already uses.

- **Two-finger scroll works again.** In v0.3.0 the mouse wheel recalled
  input history instead of scrolling the transcript. Bare `Up`/`Down` now
  scroll one line; history recall moved to `Ctrl-P`/`Ctrl-N`.

  The cause is worth stating, because the earlier documentation had it
  wrong. Conway does not capture the mouse — doing so would disable the
  terminal's click-drag text selection, which the transcript's clean-copy
  guarantee protects. The previous notes concluded from this that the wheel
  never reached Conway at all. It does: terminals implement *alternate
  scroll* (DECSET 1007), translating wheel events into `Up`/`Down` cursor
  keys while the alternate screen is active. So when v0.3.0 bound those
  arrows to history, it silently took the wheel with them.

  Conway cannot distinguish a wheel-driven arrow from a typed one — that
  distinction is precisely what mouse capture would provide — so the arrows
  go to the more frequent interaction, and history takes the readline
  chord. `PageUp`/`PageDown` and `Home`/`End` are unchanged.

- **TUI: modals no longer eat the whole screen (V1).** The permission
  prompt's own comment used to read *"claim nearly the whole transcript
  area"* — which was the bug: a modal that always filled the screen
  regardless of how little it had to say. The permission prompt, the
  `/ask` modal, the NL intent-confirm card, and `/help` now share one
  primitive (`tui/view/modal.rs`): bottom-anchored, sized to their own
  content, capped at a maximum, with the transcript still visible above
  them. A long command/answer/prompt that exceeds the cap now **scrolls**
  (`PageUp`/`PageDown`, a single shared `AppState::modal_scroll` field —
  the old permission-only `permission_scroll`, generalized) instead of
  either truncating silently or filling the screen. `/agents` stays a
  panel rather than becoming a fifth modal on this primitive — it's meant
  to be browsed while still composing, sharing the screen with a live
  input line, which a modal (drawn *over* the transcript) cannot do.

  A new tree/menu navigation primitive (`tui/view/menu.rs`) is layered on
  the modal for a later settings surface (V4) to fill in — nested,
  collapsible groups with keyboard navigation, not wired to anything yet
  but fully exercised by its own tests, so that surface can build on a
  finished primitive rather than a half one. See `docs/crates/conway-cli.md`
  for the cap-fraction measurement and the full reasoning.

## [0.2.0] — 2026-07-23

### Changed

- **License: relicensed from Apache-2.0 to AGPL-3.0-only.** conway is now
  covered by the GNU Affero General Public License v3.0 — running a modified
  conway as a network service requires making the modified source available to
  its users. This is a deliberate choice for an agent harness and means conway
  is not intended for use as a permissively-licensed library dependency inside
  closed-source software. See [LICENSE](LICENSE).
- **Unified the two model-capability systems** into a single source of truth:
  the router's context-fit gate and `Backend::capabilities()` now resolve
  through the same path, so a `models.json` value has one predictable routing
  effect instead of silently diverging.
- **Redesigned the TUI** to a single-column, copy-paste-friendly layout
  (conversation stream, input box, status line) with a live `/`-command
  palette and an on-demand agent-tree panel, replacing the always-on paned
  layout that dragged UI chrome into the clipboard.

### Added

- **`ContextHook`** — a pluggable per-call context/tool-curation port: mask
  records, edit the system prompt, filter the announced tool set, or react to
  context overflow. No built-in curation policy and no automatic compaction;
  with no hook registered, behavior is unchanged.
- **Out-of-context record mask** — mark log records to exclude from LLM calls
  while keeping them in the append-only log (reversible).
- **Reasoning support at the wire layer** — extended-thinking budget /
  reasoning-effort request params per dialect, Anthropic thinking-block
  signature round-trip across tool loops, and `redacted_thinking` handling.
- **TUI keyboard navigation** — arrow-select + autofill in the `/`-command
  palette, and arrow-scroll + Esc-to-close in the agent panel.
- **`/ask`** — an ephemeral forked question rendered as a dimmed aside; it
  inherits the session's context but never pollutes the transcript.
- **Failure observability** — backend and routing errors are surfaced to
  stderr (including the reasons a candidate was rejected).
- **OSS front door** — a README and a runnable offline example
  (`cargo run -p conway --example minimal_session`).

### Fixed

- **Multi-turn tool use no longer loops.** Assistant records now persist the
  tool calls they made, so a follow-up turn sees the tool result instead of
  re-calling the tool indefinitely; tool-call-only assistant turns serialize an
  empty string rather than `null` (which some OpenAI-compatible servers, e.g.
  Ollama Cloud, reject).
- **Dialect-aware health probe** — the probe now uses a liveness endpoint the
  target dialect actually serves, and an unsupported liveness path is no longer
  counted as a health failure that opens the circuit breaker.
- **`--model`** is now wired to a facade pin (previously accepted by the CLI
  parser but inert — a 0.1.0 known limitation).

## [0.1.0] — 2026-07-22

First release. conway is a Rust agent harness for agentic coding, built around
one library that serves three consumption modes equally, first-class
hierarchical forking, and a strict ports-and-adapters architecture where every
capability is a plugin.

### Architecture

- **8-crate Cargo workspace** with strictly downward dependencies
  (ports-and-adapters):
  - `conway-core` — domain types and port traits only; no I/O.
  - `conway-backends` — provider adapters (Anthropic native, OpenAI-compatible).
  - `conway-routing` — declarative role→model routing, health, and failover.
  - `conway-session` — append-only session persistence and transcript resolution.
  - `conway-tools` — the built-in tool/plugin implementations.
  - `conway-runtime` — the agent loop, supervision, and orchestration.
  - `conway` — the public facade (the single supported embedding surface).
  - `conway-cli` — the `conway` binary; depends only on the `conway` facade.
- The `SubagentHost` port breaks the tools↔runtime cycle so tools can spawn
  sub-agents without an upward dependency.

### Three consumption modes (one library)

- **Embeddable Rust library** — the primary surface. A `Conway` builder plus
  `SessionHandle` API: fully async, event-streamed, designed to be driven from a
  host application (e.g. a Tauri IDE).
- **Interactive TUI** — a terminal shell with live token streaming, an agent-tree
  pane, in-UI permission prompts, an editable input line, and slash commands
  (`/steer`, `/tree`, `/context`, `/why`, `/fork`, `/spawn`, `/resume`, `/help`,
  `/quit`).
- **`-p` / `--print` one-shot** — a clean, scriptable non-interactive mode:
  prompt from argv or stdin, streamed output, `--output-format text|json|jsonl`,
  strict stdout purity (only model output on stdout; all diagnostics on stderr),
  stable exit codes, and SIGINT handling.

### Hierarchical forking and spawning (distinct primitives)

- **Fork** — a child inherits the forker's *entire* effective context at the fork
  point as a literal, immutable, cache-friendly prefix, plus an added directive.
  Storage is O(1): one header line, zero records copied. Siblings forked at the
  same point share a single memoized prefix allocation.
- **Spawn** — a clean-slate child that requires an agent definition; it inherits
  no parent context. Fork and spawn are genuinely separate primitives — there is
  no partial-inheritance knob.
- **Copy-on-fork snapshot semantics** — after a fork the two sessions are
  independent append-only logs. Prompting the parent never reaches the child, and
  prompting the child never reaches the parent; the inherited prefix is bounded
  at the fork sequence and is byte-identical to a snapshot.
- **Bidirectional messaging** — parents steer children (applied at turn
  boundaries) and can soft- or hard-cancel them; children report progress and
  terminal results back. This is explicit, addressed messaging — separate from
  context inheritance.
- **Aggregate** — a parent can fork, spawn N differently-prompted children, and
  collect their results. A parent's `await` on a child can never hang: the
  supervisor synthesizes a terminal result on panic, budget exhaustion, or
  cancellation.

### Session persistence and continuity

- Append-only JSONL session store, one file per session, with crash-tolerant
  reads.
- **Resume** a persisted session and continue it — from both the library and the
  CLI (`--resume <id>`).
- **Fork-from** a persisted session at any sequence — from both the library and
  the CLI (`--fork-from <id>[@<seq>]`); the child genuinely inherits the parent's
  context, transitively across multi-level fork chains.
- Caller-chosen session ids (`--session <id>`), with honest usage errors on
  collision (directs the user to `--resume`).
- `Runtime::resume_root` re-registers a persisted agent as live, gated so it idles
  until its first prompt rather than racing a spurious turn against the old
  transcript.
- Full session inspection surface: `conway sessions list | show | tree | export`.

### Provider routing and backends

- **Backends**:
  - Anthropic native (Messages API), with cache-breakpoint mapping.
    API-key authentication only.
  - OpenAI-compatible, with per-dialect adapters for **Ollama**, **vLLM / Hermes**,
    **LM Studio**, and **llama.cpp server**.
- **Declarative routing** — per-role model aliases with explicit fallback chains;
  every response can be traced to which model served it and why
  (`conway routes explain`). No content inspection, no learned classifiers.
- **Health and failover** — dual circuit breakers per endpoint (transport and
  probe), a background prober, and an attempt/fallback loop that records health
  observations and fails over on transport, server, and rate-limit errors.
- **Capability-aware** — per-model tool-calling reliability, streaming behavior,
  and prompt-caching support are first-class. Prompt caching is used
  opportunistically but is never correctness-bearing (verified by byte-identity
  tests).

### Tools and extensibility

- Built-in plugins, all implemented on the *same* public Plugin/Tool API that
  third parties use (no privileged core tools):
  - Filesystem: read, write, edit, glob, grep.
  - Shell: bash execution with process-group termination on cancel.
  - Fork/subagent tools.
  - Explicit report/finalization tools.
- **Permission gate model** — allow-list, deny-all, and interactive-prompt gates,
  with a callback surface for the embedder. One-shot mode defaults fail-closed
  (an empty allow-list denies every tool) because it cannot prompt an operator.
- Sandboxing and worktree isolation are left to an agent's own tools rather than
  imposed by the harness.

### Reliability (multi-agent failure-mode mitigations)

- Full context provenance: every context segment records where it came from
  (a 9-variant provenance model).
- Literal prefix inheritance — no lossy summarization of inherited context.
- Repeated-step detection.
- Mandatory budgets (token and deadline), enforced as hard ceilings.
- Result-contract schema validation with a single retry, then an explicit
  refusal rather than a silent bad result.

### Security

- Anthropic OAuth subscription tokens (`sk-ant-oat…`) are rejected at three
  layers — conway is metered-API-key only, by design.
- Cross-session agent access is rejected (`AgentNotInSession`); an agent handle
  cannot drive a session it does not belong to.

### Known limitations (deliberate for 0.1.0)

- No Claude Pro/Max subscription authentication — metered API keys only.
- `--model` is accepted by the CLI parser but not yet wired to a facade pin field.
- Cross-*backend-kind* failover has unit coverage but no end-to-end integration
  test yet.
- No bundled example third-party plugin, and OSS-release docs (README,
  plugin-author guide) are not yet written.

<!-- No published remote yet; add a compare/tag link here once the repository is
     hosted, e.g. [0.1.0]: https://<host>/<owner>/conway/releases/tag/v0.1.0 -->

