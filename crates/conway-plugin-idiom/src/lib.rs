//! `conway.idiom`: a plugin that prepends a short, conway-specific
//! instruction fragment to a session (board item
//! `01M0VR3BKW5N3V3WS28H7FV8ZK`). The operator's own framing: *"this is a
//! plugin which prepends a custom system prompt. Currently we send minimal
//! data, and the purpose of this is to add a little extra if desired."*
//! **A little extra** -- see [`FRAGMENT_TEXT`]'s own line/word budget
//! before adding to it.
//!
//! # The absence this closes, re-verified (not assumed)
//!
//! `App::session_spec` (`crates/conway-cli/src/tui/app/startup.rs`) sets
//! `role`/`keep_alive`/`tools`/`model` on its `SessionSpec` and never sets
//! `agent_def` or `system_prompt_override`. `SessionSpec::
//! system_prompt_override`'s own doc (`crates/conway/src/session_handle.rs`)
//! states the consequence in as many words: `None` "preserves the
//! pre-existing behavior: the resolved `agent_def`'s own `system_prompt`,
//! or **no system-prompt segment at all** when `agent_def` is also `None`."
//! A bare interactive TUI session sets neither, so `ContextBuilder::build`'s
//! `[0] SystemPrompt` step (`crates/conway-runtime/src/context/builder.rs`)
//! pushes nothing: `input.system_prompt` is `None`, the `if let Some(...)`
//! guard never fires, and the assembled context carries tool schemas and
//! the conversation, with no segment telling the model what harness it is
//! in. Confirmed by reading both sites directly rather than trusted from
//! the board item's own citation; the premise holds.
//!
//! # Where the base fragment lands: genuinely first (process record
//! `01M1FQ36PCW2J19AP219GKZH3R`)
//!
//! `ContextBuilder::build` renders every `InstructionFragment` at one of two
//! positions relative to `[0] SystemPrompt` (an agent def's own prompt, or a
//! session's `system_prompt_override`/one-shot `--system-prompt`):
//! `BeforeSystemPrompt` or `AfterSystemPrompt` (the default), then `[1b]
//! SkillFragments*`, then tool schemas and the conversation --
//! `conway_core::ports::plugin::InstructionFragment`'s own doc, "Precedence",
//! has the full ordering argument. [`IdiomPlugin::instructions`]'s base
//! fragment ([`INSTRUCTION_NAME`]) declares `BeforeSystemPrompt` with
//! `order: -100`, so it renders AHEAD of `[0]` -- including ahead of a
//! curated `AgentDef`'s own deliberately-authored prompt.
//!
//! **This used to be argued the other way, and that argument no longer
//! holds.** An earlier version of this plugin placed the base fragment in
//! `[1]` (after `[0]`) on the theory that an agent def's own prompt -- "the
//! specific job this agent was built for" -- should stay the first thing the
//! model reads, with conway's own harness orientation following immediately
//! after. `InstructionFragment::position` did not exist yet, so `[1]`, not
//! `[0]`-adjacent-but-first, was the only slot this plugin could occupy at
//! all -- the prior doc's own "does not change `ContextBuilder::build`'s
//! assembly order" line named this limitation honestly rather than claim
//! more than the runtime could deliver. Process record
//! `01M1FQ36PCW2J19AP219GKZH3R` (a harness gap review, finding 2) argued the
//! operator's own framing at the top of this module -- "this is a plugin
//! which prepends a custom system prompt" -- means what it says: the harness
//! should be able to speak FIRST, before any agent-def-specific prompt, not
//! merely first among the fragments that follow one. `InstructionFragment::
//! position` is the runtime change that makes that true rather than merely
//! stated; conway's own orientation text now precedes an agent def's own
//! prompt, deliberately, the same way an operator's own `--system-prompt`
//! text at `[0]` still is NOT preceded (nothing about `[0]`'s own content
//! changes -- only what renders ahead of it). Multiple plugins' fragments at
//! the SAME position are still ordered by `with_plugin`/`install_selected`
//! install order (ties broken by that order, per `order: -100`'s own
//! precedence doc), so "first among `BeforeSystemPrompt` fragments" remains
//! not a property this plugin can guarantee for itself against a THIRD-PARTY
//! plugin that also declares `BeforeSystemPrompt` with a lower `order` -- see
//! `first_party_plugins::bundle`'s own doc for where this entry sits in that
//! list.
//!
//! # The `tool_ids` trap, and how per-part gating closes it (board item
//! `01M1FSRJJAB3ZYZXED4SVT2ZSF`)
//!
//! **Before this item, the whole fragment had exactly one `tool_ids` list,
//! and it had to stay empty.** `ContextBuilder::build`'s reachability check
//! withheld a fragment's text ENTIRELY (not just the offending sentence)
//! when any id in that list was not among the turn's announced tools -- so
//! [`IdiomPlugin::instructions`] left it `vec![]`, deliberately, even
//! though [`FRAGMENT_TEXT`] names `conway_fork`/`conway_spawn`/`report` in
//! prose throughout. That meant this fragment could never say something
//! genuinely actionable and tool-specific -- "verify with a tool call
//! before you claim done: run the relevant tests with `bash`" -- without
//! either (a) losing the whole paragraph on a `bash`-less session (an
//! interactive root SPECIFICALLY never has `report` either, `startup.rs`'s
//! own `ToolSelector::Except(vec!["report".into()])`, so naming it would
//! have made the fragment vanish from the one session type this plugin
//! exists for) or (b) staying purely descriptive forever ("`report` is how
//! a non-root agent ends a turn", never "call `report`").
//!
//! **Per-part gating is the fix.** [`InstructionFragment`] now carries an
//! unconditional `text` body plus zero or more [`InstructionPart`]s, each
//! independently gated on its own tool ids -- see that type's own doc.
//! [`FRAGMENT_TEXT`] (`fragments/idiom.md`) uses the markdown convention
//! [`parse_fragment_markdown`] implements: a paragraph immediately preceded
//! by an HTML comment `<!-- tools: id1, id2 -->` is a conditional part;
//! every other paragraph is body. Three sentences that used to have to stay
//! purely descriptive (or be left unwritten) are now real, actionable
//! parts: verify with `bash` before claiming done, call `report` to end a
//! turn as a child, fork to a child when the window is filling -- each
//! rendered only for a session that actually has the tool it names, and
//! withheld (recorded, never silently) otherwise. The always-true bullets
//! (fork vs. spawn exist as two primitives, tools are configuration-
//! dependent, context is scarce in general, permissions/budgets/steering)
//! stay in the unconditional body, exactly as before.
//!
//! [`IdiomPlugin::instructions`] parses [`FRAGMENT_TEXT`] through
//! [`parse_fragment_markdown`] at call time (cheap -- a handful of
//! paragraphs, called once per turn, not on a hot loop) and `.expect()`s
//! success: this crate's OWN shipped file must parse, and a build where it
//! does not is a bug in this crate, caught immediately by
//! `fragment_stays_within_budget` and every other test in this module
//! that exercises it, not a condition to handle gracefully at runtime. An
//! OPERATOR's own `instructions.md` goes through the identical parser in
//! `read_operator_fragment` below, but fallibly -- a malformed comment in
//! an operator's own file surfaces as `Err(FacadeError::Config)`, the same
//! tier every other "file the operator wrote and conway silently ignored"
//! failure already fails at (this module's own "Operator instructions"
//! doc, point 4) -- so an operator gating their own sentences gets the
//! identical convention and the identical honesty about a typo.
//!
//! # Reach: every agent, root or child (board item `01M0VSKA76NSEHDSH25XJGJ2J5`)
//!
//! At the time this plugin first shipped, `SubagentHost::start`
//! (`crates/conway-runtime/src/subagent.rs`) gave every forked or spawned
//! child `instructions: Vec::new()` unconditionally, and its sibling
//! `resolve_instructions` (`crates/conway-runtime/src/runtime/root.rs`) --
//! the function that forwards every installed plugin's fragments UNCHANGED
//! -- was root-only. That was disclosed as a caveat (`docs/plugins/
//! hooks.md` point 17: "If you author a fragment, assume a subagent will
//! not see it") but never *decided*: nobody had argued whether a child
//! SHOULD see it.
//!
//! Board item `01M0VSKA76NSEHDSH25XJGJ2J5` argued it and ruled a plugin
//! instruction fragment is harness configuration keyed to tool
//! reachability (the existing `tool_ids` gate), not transcript context --
//! so fork/spawn's "whole transcript vs. empty transcript" split does not
//! govern it, the same way it already does not govern `plugin_config`
//! (narrowed-and-inherited from the parent for spawn exactly as for fork,
//! predating this item). `SubagentHost::start` now calls the SAME
//! `resolve_instructions`/`resolve_skills` a root agent does, for both
//! fork and spawn, with no per-mode branch -- see that function's own doc
//! (`runtime/root.rs`) for the full argument. [`FRAGMENT_TEXT`] was written
//! knowing its "ending a turn"/"permissions"/"steering" bullets describe
//! how a *child* agent should behave; those agents now receive it, its
//! unconditional body always, and its `report`-gated part specifically
//! whenever THIS child's own tool set actually has `report` -- see "The
//! `tool_ids` trap" above for the per-part gate this composes through.
//!
//! # The environment block (board item `01M1FSTQT952QYM014G65EVW25`)
//!
//! A model running inside conway was never told what harness it is in, what
//! directory it is working from, what OS it is on, or what day it is --
//! [`FRAGMENT_TEXT`] talks ABOUT `conway_fork`/`report`/context scarcity but
//! never states the handful of session-static facts a model would otherwise
//! have to guess (or misremember from a general-purpose prior) from the
//! transcript alone. [`ENVIRONMENT_INSTRUCTION_NAME`] is a second, small
//! fragment closing that gap, declared `BeforeSystemPrompt` with
//! `order: -200` -- ahead of even [`INSTRUCTION_NAME`]'s own `order: -100`
//! (see "Where the base fragment lands" above) -- so it is genuinely the
//! FIRST thing in the assembled context, ahead of conway's own idioms
//! primer and ahead of an agent def's own prompt.
//!
//! **Computed exactly once, at [`IdiomPlugin::new`]/[`IdiomPlugin::
//! from_operator_files`] construction time, never re-derived per call to
//! [`IdiomPlugin::instructions`].** This is the entire point: an
//! `InstructionFragment` this early in the assembled context sits ahead of
//! everything a prompt cache keys on, so any line that changes between two
//! turns of the SAME session invalidates the cached prefix for the whole
//! rest of the context behind it. [`IdiomPlugin`] stores the rendered
//! fragment as a field, computed once in the constructor, rather than
//! re-deriving it the way [`FRAGMENT_TEXT`] is re-parsed on every call --
//! `environment_fragment_is_byte_identical_across_two_calls` (below) pins
//! this literally: two consecutive `instructions()` calls on the same
//! instance return the identical rendered text for this fragment.
//!
//! **Four facts, each named only when conway actually established it
//! (declaration honesty) -- see `build_environment_text`:**
//!
//! - `cwd` -- the constructor's own `cwd` argument, verbatim.
//! - `os`/arch -- `std::env::consts::OS`/`std::env::consts::ARCH`.
//! - `date` -- `chrono::Local::now()` at construction time, formatted
//!   `YYYY-MM-DD` and labeled "session start" IN the sentence itself, so
//!   the model is told, not left to assume, that this will not update if
//!   the session happens to cross midnight -- it is a snapshot, not a
//!   clock.
//! - `git` -- `.git/HEAD`, read directly (`std::fs`, no `git` subprocess,
//!   no `git2` dependency) and parsed by `resolve_head_path`/
//!   `parse_head_contents`: `ref: refs/heads/<branch>` names the branch;
//!   a raw 40-hex-character SHA (detached HEAD) is shortened to its first
//!   8 characters. A linked worktree's own `.git` is a FILE (`gitdir:
//!   <path>`, not a directory) naming where its REAL `HEAD` actually lives
//!   -- `resolve_head_path` follows that pointer, so a worktree checkout
//!   (like the one this very item was implemented in) reports the
//!   worktree's own current branch, not the main checkout's. Omitted
//!   entirely from the sentence -- never "git unknown", never a guess --
//!   when `.git` is absent, unreadable, or `HEAD`'s contents parse as
//!   neither shape.
//!
//! **Deliberately excludes three things**, each because including it would
//! break the one property this fragment exists for (byte-identical across
//! the plugin's whole lifetime) or duplicate a fact conway already states
//! elsewhere, more reliably:
//!
//! - **Clean/dirty git status.** Changes on nearly every turn (the model's
//!   own edits dirty the tree) -- the antithesis of session-static, and the
//!   single fact most likely to shred the cache prefix if it were included.
//! - **Model/window/headroom.** Not knowable at construction time --
//!   plugin construction precedes route resolution -- and a different,
//!   already-shipped mechanism (`conway_runtime::runway`, `docs/
//!   interactive.md`'s "runway notices") already carries per-turn window/
//!   budget information; restating it here would be a second, potentially
//!   stale source of truth for the same number.
//! - **A tool list.** The wire-level tool schema announcement already
//!   states exactly which tools this turn can call; a prose restatement
//!   here has nothing to add and every chance to go stale against it.
//!
//! No hostname, no username, no arbitrary environment variables -- none of
//! this fragment's four facts need either, and declaration honesty — say
//! only what is true and needed, nothing added "just in case" — argues
//! against naming a fact no consumer actually asked for.
//!
//! # Naming
//!
//! `conway.idiom` -- the exact id `conway_core::ports::plugin::Plugin::
//! instructions`'s own doc and `ContextBuilder::build`'s own "Precedence"
//! comment already use as their illustrative example of a base,
//! plugin-sourced fragment (`conway.idiom` "base" -> `conway.trim`/
//! `conway.memory` plugin-sourced -> `house-style` "(yours)"), so this
//! plugin fills a name the codebase's own comments already anticipated
//! rather than inventing a new one.
//!
//! # Operator instructions (board item `01M0VR4GMGSZ2682T908JCGVFG`)
//!
//! An operator has no other lever to add standing instructions to every
//! session -- `--system-prompt`/`--append-system-prompt`
//! (`crates/conway-cli/src/cli.rs`) reach `SessionSpec::
//! system_prompt_override` only on the one-shot path, and that field
//! REPLACES the whole `[0] SystemPrompt` segment rather than adding to it
//! (`crates/conway/src/session_handle.rs`'s own doc). This module gives an
//! interactive operator a file instead, following Pi's `AGENTS.md`/
//! `SYSTEM.md` precedent while staying additive: it contributes MORE
//! `InstructionFragment`s alongside [`FRAGMENT_TEXT`], never replacing
//! `system_prompt_override`, which stays the flag's job (report it here so
//! the next reader does not invent a second "replace" answer).
//!
//! **1. A short walk, bounded at the enclosing git repository root, plus
//! an `AGENTS.md` fallback.** `.conway/instructions.md` is no longer
//! resolved by a single direct `cwd`-join alone:
//! [`project_instructions_path`] walks from `cwd` up through each
//! ancestor directory, NEAREST FIRST, taking the first
//! `.conway/instructions.md` it finds, and stops climbing at the
//! enclosing git repository root -- the nearest ancestor (including
//! `cwd` itself) that contains a `.git` entry -- rather than continuing
//! all the way to the filesystem root. When `cwd` is not inside a git
//! repository at all, the walk is `cwd` alone, exactly the old,
//! single-directory behaviour. This doc used to say conway's project-file
//! convention "never walked up an ancestor chain" for `.conway/*`; that
//! was true when written and is no longer the rule this crate's own code
//! follows -- someone launching a session from a subdirectory of an
//! already-`.conway`-configured repository now sees the same project
//! instructions a launch from the repository root would have. `.conway/
//! agents/`/`.conway/skills/` are unaffected by this item and still
//! resolve directly against `cwd` alone
//! (`crates/conway-cli/src/first_party_plugins.rs`'s own `bundle`, `cwd.
//! join(".conway").join("skills")`).
//!
//! **When that walk finds no `.conway/instructions.md` anywhere, it tries
//! `AGENTS.md` on the identical directory list** -- same nearest-first
//! order, same git-root boundary -- before giving up. Most harnesses an
//! operator arriving at conway from elsewhere has likely used converge on
//! `AGENTS.md` as the filename a project already carries; an operator
//! should not have to re-author `.conway/instructions.md` before conway
//! reads anything a project already has. `.conway/instructions.md` wins
//! whenever both exist ANYWHERE on the walk, even when the `AGENTS.md`
//! that lost is nearer to `cwd` than the `.conway/instructions.md` that
//! won -- conway's own file is the more specific declaration. Both land
//! under the SAME fragment name, [`OPERATOR_PROJECT_INSTRUCTION_NAME`]: an
//! `AGENTS.md` source is a different file backing the existing fragment
//! slot, not a new one, and `Provenance::Operator`'s own `path` field --
//! already the mechanism naming which file backs an operator fragment --
//! names the exact file this session actually read, so `/context` and a
//! session's durable log already say which one without any new field.
//! `CLAUDE.md` is deliberately NOT read: ruled out as single-vendor,
//! unlike `AGENTS.md`'s multi-harness convergence. No `@import` directive
//! and no per-directory rule file either -- this plugin still contributes
//! at most one project-scope fragment per session. No concrete case named
//! here needs more than one project file -- an operator who wants to say
//! two different things says them in one file.
//!
//! **2. Project AND global, both additive.** conway's config discovery
//! already resolves a project layer (`conway::config::discovery::discover`,
//! upward-walking for `settings.json` specifically) and a user layer
//! (`conway::config::discovery::user_config_path`). Reuse is cheap for the
//! GLOBAL half: [`global_instructions_path`] below is one call to
//! `user_config_path` plus a filename swap, no new dependency, no schema
//! change. **It honours `CONWAY_CONFIG_DIR` exactly the way
//! `settings.json` itself does** (board item
//! `01M0W5Q569F0T97HSEP6F0MPCR`, closing the same isolation gap board item
//! `01M0VV6CVSZM4XH8J4G6EBV5E3` closed for `settings.json` -- an operator
//! or embedder relocating conway's user-config layer relocates THIS file
//! with it, not only `settings.json`) -- so `global_instructions_path`
//! takes the same explicit `env: &HashMap<String, String>` every other
//! `CONWAY_CONFIG_DIR`-aware resolver in this codebase takes, threaded
//! from `first_party_plugins::install`/`all_bundle_plugins`/
//! `installed_plugins` (`crates/conway-cli/src/first_party_plugins.rs`)
//! down through `resolve_idiom_plugin`/`resolve_operator_paths` to here --
//! never read from `std::env` directly at any point in that chain (see
//! `config_isolation_guard.rs`'s own doc for why an ambient read anywhere
//! along it would be the identical defect wearing a new file's name). The
//! PROJECT half does not reuse `discover` at all -- that function's
//! candidate is hardcoded to `settings.json` (`crates/conway/src/config/
//! discovery.rs`), not a generic file-discovery primitive, so "reusing"
//! it here would mean requiring a `settings.json` to already exist beside
//! `instructions.md` for the latter to be found, a strictly worse answer
//! than the direct `cwd`-join every other `.conway/*` convention already
//! uses. Both files are read when present; neither one's presence
//! disables the other, matching the additive shape `system_prompt_override`
//! deliberately does NOT have.
//!
//! **3. Two operator fragments, not merged into the base.** The shipped
//! [`FRAGMENT_TEXT`] and an operator's own text have different authors and
//! different lifetimes (one ships with this crate, one is the operator's
//! own file, editable with no rebuild) -- `/context` already renders
//! per-fragment token costs, so keeping them apart is what lets an
//! operator see what THEIR OWN text costs, separately from this plugin's.
//! The same argument extends one level further, from "shipped vs.
//! operator" to "project vs. global": both are operator-authored, but a
//! project file changes at a different rate than a global one (per-repo
//! conventions vs. house-wide preference an operator carries everywhere),
//! and an operator who has authored both wants to see both costs, not one
//! opaque combined number. [`OPERATOR_PROJECT_INSTRUCTION_NAME`]/
//! [`OPERATOR_GLOBAL_INSTRUCTION_NAME`] are therefore two more names
//! alongside [`INSTRUCTION_NAME`], each optional, each independently
//! absent when its file is absent. When `cwd` genuinely IS the operator's
//! home directory, the two paths name the same underlying file --
//! [`resolve_operator_paths`] collapses that case to the project fragment
//! alone rather than injecting the same text twice under two names.
//!
//! **4. Missing is silent; unreadable or malformed is not.** A
//! `NotFound` read error yields no fragment and no `Result::Err` -- an
//! operator who never wrote either file sees conway behave exactly as
//! before this item. An empty (or whitespace-only) file is treated the
//! same way: it communicates nothing, same as absent. Any OTHER read
//! failure -- a permissions error, the path naming a directory, invalid
//! UTF-8 -- is `Err(FacadeError::Config { .. })`, surfaced at
//! `ConwayBuilder::build` (the same failure tier a malformed `.conway/
//! skills/*/SKILL.md` already fails at, `crate::skills::load_skill_defs`),
//! never silently dropped: a file the operator wrote and conway silently
//! ignored is exactly the failure mode this project cares most about.
//!
//! # Provenance: the shipped fragment and an operator's own text are
//! tagged apart (board item `01M1FSNBRE5XJ0GQ04RT5HZ1PS`)
//!
//! Every fragment this plugin contributes is stamped by
//! `authored_by` (`InstructionFragment::authored_by`), and
//! `ContextBuilder::build` (`crates/conway-runtime/src/context/builder.rs`)
//! reads that field to choose the assembled segment's own
//! `conway_core::provenance::Provenance`:
//!
//! - [`FRAGMENT_TEXT`] ([`INSTRUCTION_NAME`]) leaves `authored_by` at its
//!   default, [`FragmentAuthor::Plugin`] -- this crate wrote it, so it is
//!   stamped `Provenance::PluginInstruction { plugin_id: "conway.idiom",
//!   name }`.
//! - An operator's own project/global text ([`OPERATOR_PROJECT_INSTRUCTION_NAME`]/
//!   [`OPERATOR_GLOBAL_INSTRUCTION_NAME`], built by `read_operator_fragment`
//!   below) sets `authored_by: FragmentAuthor::Operator { path }` -- the
//!   operator wrote it, in a file this plugin merely reads, so it is
//!   stamped `Provenance::Operator { name, path }` instead, naming the
//!   exact file (a project's `.conway/instructions.md`, its `AGENTS.md`
//!   fallback, or `<home>/.conway/instructions.md` at global scope) an
//!   operator can go edit.
//!
//! This used to be a single, uniform `Provenance::Skill { name }` stamp for
//! every fragment this plugin contributed -- the SAME stamp an
//! operator-authored `.conway/skills` body gets, even though a shipped
//! plugin fragment, an operator's own standing instructions, and a
//! directory-authored skill share nothing but the segment slot they render
//! in. Attribution used to live only in the parallel `ContextReport::
//! instruction_fragments` list, a side-channel that named which PLUGIN
//! declared a fragment but had no way to say a fragment's WORDS were never
//! the plugin's own. That made the durable log lie in both directions at
//! once: an operator's own prose read back as "a skill" it categorically
//! was not, and this crate's own shipped paragraph was indistinguishable
//! from operator prose in the one place -- per-segment `Provenance` -- a
//! session's durable record actually lives. Fixed here, board item
//! `01M1FSNBRE5XJ0GQ04RT5HZ1PS`; see `conway_core::provenance::Provenance`'s
//! own doc for the two new variants and their wire-format argument, and
//! `docs/plugins/idiom.md`'s "Seeing it in /context" section for the
//! operator-facing render this closes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use conway::plugin::{
    FragmentAuthor, FragmentPosition, FragmentScope, InstructionFragment, InstructionPart, Plugin,
    PluginDescription, PluginManifest, Tool, ToolName,
};

/// This plugin's published manifest id -- a config author (or a first-party
/// bundle's own linking module) resolves `[plugins].install` entries
/// against this constant.
pub const PLUGIN_ID: &str = "conway.idiom";

/// The bare name of this plugin's shipped idioms-primer [`InstructionFragment`]
/// -- see [`ENVIRONMENT_INSTRUCTION_NAME`] for the second shipped fragment,
/// this plugin's environment block.
pub const INSTRUCTION_NAME: &str = "conway.idiom.base";

/// The bare name of this plugin's shipped environment-block
/// [`InstructionFragment`] -- see this module's own doc, "The environment
/// block" (board item `01M1FSTQT952QYM014G65EVW25`), for what it says, why
/// it says only those four facts, and why it is computed exactly once.
pub const ENVIRONMENT_INSTRUCTION_NAME: &str = "conway.idiom.environment";

/// The name of the operator's project-scope instruction fragment, sourced
/// from the nearest `.conway/instructions.md` (or, when none is found, the
/// nearest `AGENTS.md`) found walking up from `<cwd>` to the enclosing git
/// repository root -- see [`project_instructions_path`] for the exact
/// walk/fallback rule, and this module's own doc, "Operator instructions",
/// points 1 and 3.
pub const OPERATOR_PROJECT_INSTRUCTION_NAME: &str = "conway.idiom.operator.project";

/// The name of the operator's global-scope instruction fragment, sourced
/// from `<home>/.conway/instructions.md`. See this module's own doc,
/// "Operator instructions", points 2 and 3.
pub const OPERATOR_GLOBAL_INSTRUCTION_NAME: &str = "conway.idiom.operator.global";

/// The bare filename an operator writes their own instructions into, at
/// either scope -- `instructions.md` alongside `.conway/agents/`/
/// `.conway/skills/`'s own convention of a `.conway/`-relative name.
pub const OPERATOR_INSTRUCTIONS_FILENAME: &str = "instructions.md";

/// The fragment's text, sourced from a markdown file in this crate's own
/// `fragments/` directory (`crates/conway-plugin-path`/
/// `crates/conway-plugin-discover`'s own `include_str!` convention --
/// `Plugin::instructions`'s own doc, "Convention, not enforcement"), using
/// the `<!-- tools: ... -->` per-part convention [`parse_fragment_markdown`]
/// reads (board item `01M1FSRJJAB3ZYZXED4SVT2ZSF`) -- well under a
/// 40-line/400-word budget measured from Pi's own `system-prompt.ts` core
/// template (`docs/vision/INTENT.md`'s citation of Pi as conway's
/// extension-surface reference), RAW source, markers included -- see this
/// module's own doc, "Reach: every agent, root or child", for who actually
/// reads this text.
pub const FRAGMENT_TEXT: &str = include_str!("../fragments/idiom.md");

/// Builds [`ENVIRONMENT_INSTRUCTION_NAME`]'s body text for `cwd` -- called
/// exactly once, at [`IdiomPlugin::new`]/[`IdiomPlugin::from_operator_files`]
/// construction time, never per call to [`IdiomPlugin::instructions`]. See
/// this module's own doc, "The environment block", for the cache-economics
/// argument that single-call-site discipline exists for, and for why each
/// of the four facts below is included (or, for the three named there,
/// deliberately not).
///
/// Every clause names a fact this call actually established -- when
/// `resolve_head_ref` returns `None` (no `.git`, an unreadable one, or
/// `HEAD` contents that parse as neither a symbolic ref nor a raw SHA), the
/// `git` clause is omitted from the sentence entirely rather than replaced
/// with a guess or a placeholder like "git unknown" (declaration honesty,
/// GP-14).
fn build_environment_text(cwd: &Path) -> String {
    let cwd_display = cwd.display();
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let date = chrono::Local::now().format("%Y-%m-%d");
    let mut text = format!(
        "You are running in conway, an agent harness. Environment: cwd {cwd_display}; os {os} \
         {arch}; date {date}, session start"
    );
    if let Some(git_ref) = resolve_head_ref(cwd) {
        text.push_str("; git ");
        text.push_str(&git_ref);
    }
    text.push('.');
    text
}

/// Resolves `cwd`'s current git ref for `build_environment_text` -- the
/// short label (`parse_head_contents`) parsed out of `cwd`'s `HEAD` file,
/// wherever `resolve_head_path` finds it actually lives. `None` covers
/// every way this can fail to name a real fact: no `.git` at all (`cwd` is
/// not inside a git work tree), a `.git` that is neither a directory nor a
/// well-formed worktree pointer file, a `HEAD` that cannot be read, or
/// `HEAD` contents that parse as neither shape `parse_head_contents`
/// recognizes.
fn resolve_head_ref(cwd: &Path) -> Option<String> {
    let head_path = resolve_head_path(cwd)?;
    let contents = std::fs::read_to_string(head_path).ok()?;
    parse_head_contents(&contents)
}

/// Finds the actual `HEAD` file for `cwd`'s repository or worktree.
///
/// `cwd/.git` is ordinarily a DIRECTORY (an ordinary clone, or the primary
/// checkout of a repository that also has linked worktrees) -- in which
/// case `cwd/.git/HEAD` is the answer directly. For a LINKED WORKTREE
/// checkout, though, `cwd/.git` is instead a FILE containing one line,
/// `gitdir: <path>`, naming this worktree's own metadata directory
/// (typically `<main-repo>/.git/worktrees/<name>`) -- that directory
/// carries this worktree's OWN `HEAD`, distinct from the main checkout's
/// (each linked worktree can be on its own branch), so this function
/// follows the pointer rather than trying to read `cwd/.git/HEAD` as
/// though `.git` were a directory and failing. Returns `None` when
/// `cwd/.git` does not exist at all, or exists as neither a directory nor
/// a well-formed `gitdir:` pointer file -- callers treat that identically
/// to "no git ref to report" (see `resolve_head_ref`).
fn resolve_head_path(cwd: &Path) -> Option<PathBuf> {
    let dot_git = cwd.join(".git");
    let metadata = std::fs::symlink_metadata(&dot_git).ok()?;
    if metadata.is_dir() {
        return Some(dot_git.join("HEAD"));
    }
    let pointer = std::fs::read_to_string(&dot_git).ok()?;
    let gitdir = pointer.trim().strip_prefix("gitdir:")?.trim();
    if gitdir.is_empty() {
        return None;
    }
    let gitdir_path = PathBuf::from(gitdir);
    let resolved = if gitdir_path.is_absolute() {
        gitdir_path
    } else {
        // A relative `gitdir:` line is relative to the pointer FILE's own
        // parent directory (`cwd`), not this process's own cwd -- matching
        // how git itself resolves it.
        cwd.join(gitdir_path)
    };
    Some(resolved.join("HEAD"))
}

/// Parses a `HEAD` file's raw contents into the short git ref
/// `build_environment_text` reports. `ref: refs/heads/<branch>` names the
/// branch (the `refs/heads/` prefix is stripped; anything else after
/// `ref:` -- e.g. a ref outside `refs/heads/` -- is reported verbatim
/// rather than guessed at). A raw, exactly-40-hex-character SHA (a
/// detached `HEAD`) is shortened to its first 8 characters. Anything else
/// -- an empty file, a blank `ref:` line, bytes that are neither shape --
/// yields `None`: better to omit the `git` clause than report something
/// this function is not actually sure of.
fn parse_head_contents(contents: &str) -> Option<String> {
    let line = contents.lines().next()?.trim();
    if let Some(rest) = line.strip_prefix("ref:") {
        let ref_name = rest.trim();
        if ref_name.is_empty() {
            return None;
        }
        return Some(
            ref_name
                .strip_prefix("refs/heads/")
                .unwrap_or(ref_name)
                .to_string(),
        );
    }
    if line.len() == 40 && line.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(line[..8].to_string());
    }
    None
}

/// Board item `01M1FSRJJAB3ZYZXED4SVT2ZSF` -- see this module's own doc,
/// "The `tool_ids` trap", for the full argument this implements. Parses
/// `source` (conway.idiom's own markdown convention: a paragraph
/// immediately preceded, with no blank line between them, by a
/// `<!-- tools: id1, id2 -->` HTML comment is a conditional part gated on
/// those ids; every other paragraph is unconditional body) into
/// `(body, parts)`. Blocks are markdown's own paragraph boundary -- one or
/// more consecutive non-blank lines, so a multi-line bulleted list is ONE
/// block, matching how [`FRAGMENT_TEXT`] itself uses it. `body` is every
/// non-gated block, in source order, joined by a blank line (`"\n\n"`) --
/// the SAME separator `conway_runtime::context::builder`'s
/// `filter_reachable_parts` uses to join a reachable part onto it, so a
/// fragment with no `<!-- tools: -->` comment at all round-trips to
/// exactly its own source text (modulo each block's own leading/trailing
/// whitespace, trimmed).
///
/// **A malformed tools comment is a load error, never a silent fallback to
/// body.** A block whose first line begins `<!--` is read as an ATTEMPTED
/// tools directive -- there is no other reason for a standalone HTML
/// comment line in one of these files -- so it must match the exact
/// `<!-- tools: id[, id...] -->` shape (a `tools:` prefix immediately
/// after `<!--`, a closing `-->`, at least one non-empty comma-separated
/// id, nothing else on the line) AND be immediately followed, same block,
/// by the paragraph it gates. Anything else -- a missing `tools:` prefix,
/// an empty id list, an unterminated comment with no closing `-->`, a
/// comment with nothing after it -- is far more likely to be an author's
/// typo than deliberate prose, so this returns `Err`, naming the offending
/// line, rather than silently demoting the paragraph that follows to body
/// text no one asked to gate.
pub fn parse_fragment_markdown(source: &str) -> Result<(String, Vec<InstructionPart>), String> {
    let mut body_blocks: Vec<String> = Vec::new();
    let mut parts: Vec<InstructionPart> = Vec::new();

    for block in split_into_paragraph_blocks(source) {
        let mut lines = block.lines();
        let first_line = lines.next().unwrap_or("").trim();
        if first_line.starts_with("<!--") {
            let tool_ids = parse_tools_comment(first_line)?;
            let rest: String = lines.collect::<Vec<_>>().join("\n").trim().to_string();
            if rest.is_empty() {
                return Err(format!(
                    "a `<!-- tools: ... -->` comment must be immediately followed (no blank \
                     line) by the paragraph it gates; found none after: {first_line}"
                ));
            }
            parts.push(InstructionPart::new(rest, tool_ids));
        } else {
            body_blocks.push(block.trim().to_string());
        }
    }

    Ok((body_blocks.join("\n\n"), parts))
}

/// Splits `source` into blank-line-delimited blocks, each keeping its own
/// internal line breaks intact (a multi-line block is returned as one
/// multi-line string, newline-joined) -- markdown's own paragraph
/// boundary, and the unit [`parse_fragment_markdown`] classifies as either
/// a conditional part or unconditional body.
fn split_into_paragraph_blocks(source: &str) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in source.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                blocks.push(current.join("\n"));
                current.clear();
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        blocks.push(current.join("\n"));
    }
    blocks
}

/// Parses one whole-line HTML comment -- ALREADY confirmed to start with
/// `<!--` by [`parse_fragment_markdown`]'s own caller -- into the tool ids
/// it names, or an `Err` naming exactly what about the line does not match
/// the `<!-- tools: id[, id...] -->` shape. See [`parse_fragment_markdown`]'s
/// own doc for why any mismatch here is an error rather than a fallback.
fn parse_tools_comment(line: &str) -> Result<Vec<ToolName>, String> {
    let inner = line
        .strip_prefix("<!--")
        .and_then(|s| s.strip_suffix("-->"))
        .map(str::trim)
        .ok_or_else(|| {
            format!("malformed HTML comment (missing closing `-->` on its own line): {line}")
        })?;
    let rest = inner.strip_prefix("tools:").ok_or_else(|| {
        format!(
            "HTML comment on its own line is not a `tools:` directive (must start `<!-- \
             tools: `): {line}"
        )
    })?;
    let tool_ids: Vec<ToolName> = rest
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToolName::new)
        .collect();
    if tool_ids.is_empty() {
        return Err(format!(
            "`<!-- tools: ... -->` comment names no tool ids: {line}"
        ));
    }
    Ok(tool_ids)
}

/// Unit coverage for [`parse_fragment_markdown`] in isolation -- P-15's
/// "shown to fail" bar for acceptance 3 (board item
/// `01M1FSRJJAB3ZYZXED4SVT2ZSF`). `operator_file_tests` below exercises the
/// SAME parser through `read_operator_fragment`'s real call site; these
/// tests are the parser's own contract, argument by argument.
#[cfg(test)]
mod parse_fragment_markdown_tests {
    use super::*;

    /// The exact shape acceptance 3 names: one `<!-- tools: bash -->`
    /// comment immediately followed by a paragraph yields one part with
    /// `tool_ids == [bash]`, and text with no such comment anywhere is
    /// body.
    #[test]
    fn a_tools_comment_followed_by_a_paragraph_yields_one_gated_part() {
        let source = "General orientation, always true.\n\n<!-- tools: bash -->\nVerify with \
                       `bash` before you claim done.\n";
        let (body, parts) = parse_fragment_markdown(source).expect("must parse");
        assert_eq!(body, "General orientation, always true.");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].tool_ids, vec![ToolName::new("bash")]);
        assert_eq!(parts[0].text, "Verify with `bash` before you claim done.");
    }

    /// Multiple gated parts, and multiple body blocks interleaved with
    /// them, are each classified independently and body blocks are joined
    /// in source order.
    #[test]
    fn multiple_parts_and_body_blocks_interleave_correctly() {
        let source = "First body block.\n\n<!-- tools: bash -->\nBash part.\n\nSecond body \
                       block.\n\n<!-- tools: report, conway_fork -->\nMulti-tool part.\n";
        let (body, parts) = parse_fragment_markdown(source).expect("must parse");
        assert_eq!(body, "First body block.\n\nSecond body block.");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].tool_ids, vec![ToolName::new("bash")]);
        assert_eq!(parts[0].text, "Bash part.");
        assert_eq!(
            parts[1].tool_ids,
            vec![ToolName::new("report"), ToolName::new("conway_fork")]
        );
        assert_eq!(parts[1].text, "Multi-tool part.");
    }

    /// Text with no `<!-- tools: -->` comment at all is entirely body --
    /// the degenerate, most common case (every fragment before this item
    /// existed).
    #[test]
    fn text_with_no_tools_comment_is_entirely_body() {
        let source = "Just an ordinary paragraph.\n\nAnd another one.\n";
        let (body, parts) = parse_fragment_markdown(source).expect("must parse");
        assert_eq!(body, "Just an ordinary paragraph.\n\nAnd another one.");
        assert!(parts.is_empty());
    }

    /// Falsified before this item existed at all (the whole function is
    /// new): a comment missing the `tools:` prefix is a load error, not a
    /// silent fallback demoting the following paragraph to body.
    #[test]
    fn a_comment_missing_the_tools_prefix_is_an_error() {
        let source = "<!-- bash -->\nSome text.\n";
        assert!(parse_fragment_markdown(source).is_err());
    }

    /// An empty id list is malformed, not a vacuously-true "no ids."
    #[test]
    fn a_tools_comment_with_no_ids_is_an_error() {
        let source = "<!-- tools: -->\nSome text.\n";
        assert!(parse_fragment_markdown(source).is_err());
    }

    /// An unterminated comment (no closing `-->` on the same line) is
    /// malformed -- it must NOT silently fall through to being treated as
    /// an ordinary body paragraph just because it fails the "ends with
    /// `-->`" check.
    #[test]
    fn an_unterminated_comment_is_an_error_not_a_body_fallback() {
        let source = "<!-- tools: bash\nSome text.\n";
        assert!(parse_fragment_markdown(source).is_err());
    }

    /// A `<!-- tools: ... -->` comment with nothing after it (end of
    /// input, or a blank line before the next block) is an error -- there
    /// is no paragraph for it to gate.
    #[test]
    fn a_tools_comment_with_nothing_to_gate_is_an_error() {
        assert!(parse_fragment_markdown("<!-- tools: bash -->\n").is_err());
        assert!(parse_fragment_markdown("<!-- tools: bash -->\n\nSome text.\n").is_err());
    }
}

/// Unit coverage for the environment block's own text-building and git
/// parsing, in isolation from a real `IdiomPlugin`/`Conway` build -- board
/// item `01M1FSTQT952QYM014G65EVW25`. `operator_file_tests` (below,
/// existing) is the precedent this mirrors for `conway-plugin-idiom`'s own
/// file-reading logic; `tests/idiom_end_to_end.rs`'s
/// `environment_fragment_renders_first_ahead_of_the_base_fragment` is this
/// item's facade-level, acceptance-3 proof.
///
/// **Written before the implementation existed, and confirmed to fail
/// first** (this item's own stated convention): before
/// [`ENVIRONMENT_INSTRUCTION_NAME`]/`build_environment_text`/
/// `resolve_head_path`/`parse_head_contents` existed, every test in
/// this module failed to compile (the names it references did not exist);
/// after implementing them, every test below passes.
#[cfg(test)]
mod environment_tests {
    use super::*;

    /// Acceptance 1, the positive git half: a temp dir with a fabricated,
    /// non-worktree `.git/HEAD` of `ref: refs/heads/main` yields
    /// `git main`.
    #[test]
    fn a_plain_git_dir_on_a_branch_yields_the_branch_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dot_git = tmp.path().join(".git");
        std::fs::create_dir_all(&dot_git).expect("mkdir .git");
        std::fs::write(dot_git.join("HEAD"), "ref: refs/heads/main\n").expect("write HEAD");
        assert_eq!(resolve_head_ref(tmp.path()), Some("main".to_string()));
    }

    /// Acceptance 1, the negative half: a temp dir with no `.git` at all
    /// yields no git ref -- and the rendered environment text carries no
    /// `git` token anywhere.
    #[test]
    fn no_git_dir_at_all_yields_no_git_ref_or_token() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(resolve_head_ref(tmp.path()), None);
        let text = build_environment_text(tmp.path());
        assert!(
            !text.contains("git"),
            "no `.git` at all must omit the `git` clause entirely, never a placeholder: {text}"
        );
    }

    /// A detached `HEAD` (a raw 40-hex-character SHA, no `ref:` line) is
    /// shortened to its first 8 characters, not reported in full.
    #[test]
    fn a_detached_head_yields_an_eight_character_short_sha() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dot_git = tmp.path().join(".git");
        std::fs::create_dir_all(&dot_git).expect("mkdir .git");
        std::fs::write(
            dot_git.join("HEAD"),
            "3f786850e387550fdab836ed7e6dc881de230012\n",
        )
        .expect("write HEAD");
        assert_eq!(resolve_head_ref(tmp.path()), Some("3f786850".to_string()));
    }

    /// A linked worktree's `.git` is a FILE, not a directory -- `gitdir:
    /// <path>` names the worktree's own metadata directory, which carries
    /// its OWN `HEAD`, distinct from whatever the main checkout is on.
    /// This is the exact shape the repo this item was implemented in
    /// actually has (a linked worktree checkout) -- see this module's own
    /// doc, "The environment block".
    #[test]
    fn a_linked_worktrees_git_file_follows_the_gitdir_pointer_to_its_own_head() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let worktree_dir = tmp.path().join("worktree");
        let real_gitdir = tmp.path().join("main-repo/.git/worktrees/feature-x");
        std::fs::create_dir_all(&worktree_dir).expect("mkdir worktree");
        std::fs::create_dir_all(&real_gitdir).expect("mkdir real gitdir");
        std::fs::write(
            worktree_dir.join(".git"),
            format!("gitdir: {}\n", real_gitdir.display()),
        )
        .expect("write .git pointer file");
        // The worktree's OWN HEAD, at the resolved gitdir -- deliberately a
        // DIFFERENT branch than any HEAD `main-repo/.git` itself might
        // carry (none is fabricated here at all), so a test that
        // accidentally read the wrong file would fail loudly rather than
        // coincidentally pass.
        std::fs::write(real_gitdir.join("HEAD"), "ref: refs/heads/feature-x\n")
            .expect("write worktree HEAD");
        assert_eq!(
            resolve_head_ref(&worktree_dir),
            Some("feature-x".to_string()),
            "must follow the `gitdir:` pointer to the worktree's own HEAD, not fail because \
             `.git` is a file rather than a directory"
        );
    }

    /// A relative `gitdir:` line is resolved against the pointer file's own
    /// parent directory (`cwd`), matching git's own resolution rule.
    #[test]
    fn a_relative_gitdir_pointer_resolves_against_cwd() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let worktree_dir = tmp.path().join("worktree");
        std::fs::create_dir_all(&worktree_dir).expect("mkdir worktree");
        let real_gitdir = worktree_dir.join("../real-gitdir");
        std::fs::create_dir_all(&real_gitdir).expect("mkdir real gitdir");
        std::fs::write(worktree_dir.join(".git"), "gitdir: ../real-gitdir\n")
            .expect("write relative .git pointer file");
        std::fs::write(
            real_gitdir.join("HEAD"),
            "ref: refs/heads/relative-branch\n",
        )
        .expect("write HEAD");
        assert_eq!(
            resolve_head_ref(&worktree_dir),
            Some("relative-branch".to_string())
        );
    }

    /// Malformed `HEAD` contents (neither a `ref:` line nor a valid-length
    /// hex SHA) yield `None` -- declaration honesty: no guess, no
    /// placeholder.
    #[test]
    fn malformed_head_contents_yield_no_ref() {
        assert_eq!(parse_head_contents(""), None);
        assert_eq!(parse_head_contents("ref:\n"), None);
        assert_eq!(parse_head_contents("not-a-ref-or-a-sha\n"), None);
        // Too short to be a real SHA -- must not be misreported as one.
        assert_eq!(parse_head_contents("abc123\n"), None);
    }

    /// `build_environment_text`'s exact shape (acceptance 1): every one
    /// of `cwd`/`os`/`date`/`git` appears, in that reading order, when
    /// `.git` resolves cleanly.
    #[test]
    fn environment_text_names_cwd_os_date_and_git() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dot_git = tmp.path().join(".git");
        std::fs::create_dir_all(&dot_git).expect("mkdir .git");
        std::fs::write(dot_git.join("HEAD"), "ref: refs/heads/main\n").expect("write HEAD");

        let text = build_environment_text(tmp.path());
        assert!(
            text.contains(&format!("cwd {}", tmp.path().display())),
            "must name the exact cwd: {text}"
        );
        assert!(text.contains("os "), "must name the OS: {text}");
        assert!(text.contains("date "), "must name the date: {text}");
        assert!(
            text.contains("git main"),
            "must name the git branch: {text}"
        );
    }
}

/// The `conway.idiom` plugin: contributes no tool, two shipped instruction
/// fragments -- [`ENVIRONMENT_INSTRUCTION_NAME`] (a session-static
/// environment block, board item `01M1FSTQT952QYM014G65EVW25`, rendered
/// genuinely first) then [`INSTRUCTION_NAME`] (conway's own short idioms
/// primer, immediately behind it), both ahead of even an `AgentDef`'s own
/// system prompt (see this crate's own module doc, "Where the base
/// fragment lands") -- plus up to two more, optional fragments sourced
/// from an operator's own `instructions.md` (see this module's own doc,
/// "Operator instructions"), which stay positioned AFTER `[0]` like every
/// ordinary fragment.
///
/// [`IdiomPlugin::new`] carries no operator fragments -- the shape every
/// caller with no `instructions.md` on disk gets.
/// [`IdiomPlugin::from_operator_files`] is the constructor that actually
/// reads an operator's files; `first_party_plugins::bundle` is this
/// binary's one production call site. Both constructors take `cwd`, used
/// to seed [`ENVIRONMENT_INSTRUCTION_NAME`]'s body
/// (`build_environment_text`) -- computed exactly once, here, and
/// stored as `Self::environment`, never re-derived by
/// [`IdiomPlugin::instructions`] (see this module's own doc, "The
/// environment block", for why that single-computation discipline is the
/// whole point).
pub struct IdiomPlugin {
    environment: InstructionFragment,
    operator_project: Option<InstructionFragment>,
    operator_global: Option<InstructionFragment>,
}

impl IdiomPlugin {
    /// No operator fragments -- the shape every caller with no
    /// `instructions.md` on disk gets. `cwd` seeds
    /// [`ENVIRONMENT_INSTRUCTION_NAME`]'s body, computed once, here (see
    /// `environment_fragment`).
    pub fn new(cwd: &Path) -> Self {
        Self {
            environment: environment_fragment(cwd),
            operator_project: None,
            operator_global: None,
        }
    }

    /// Reads `project_path`/`global_path` (each `None` when that scope has
    /// no resolvable location at all -- see [`resolve_operator_paths`]) into
    /// this plugin's two optional operator fragments, and builds
    /// [`ENVIRONMENT_INSTRUCTION_NAME`] from `cwd` the same way
    /// [`IdiomPlugin::new`] does. Point 4 of this module's own "Operator
    /// instructions" doc for the missing/empty/unreadable/malformed policy
    /// this implements.
    pub fn from_operator_files(
        cwd: &Path,
        project_path: Option<&Path>,
        global_path: Option<&Path>,
    ) -> conway::Result<Self> {
        let operator_project =
            read_operator_fragment(project_path, OPERATOR_PROJECT_INSTRUCTION_NAME)?;
        let operator_global =
            read_operator_fragment(global_path, OPERATOR_GLOBAL_INSTRUCTION_NAME)?;
        Ok(Self {
            environment: environment_fragment(cwd),
            operator_project,
            operator_global,
        })
    }
}

/// Builds [`ENVIRONMENT_INSTRUCTION_NAME`] itself from
/// `build_environment_text`'s rendered body -- pulled out as its own
/// function so both [`IdiomPlugin::new`] and [`IdiomPlugin::
/// from_operator_files`] construct it identically. `position`/`order`/
/// `scope`/`authored_by` are all spelled out explicitly, even though
/// `scope`/`authored_by` equal `InstructionFragment::new`'s own defaults,
/// so this reads as a deliberate choice -- see this module's own doc, "The
/// environment block", for why `order: -200` (ahead of even
/// [`INSTRUCTION_NAME`]'s own `order: -100`).
fn environment_fragment(cwd: &Path) -> InstructionFragment {
    InstructionFragment::new(ENVIRONMENT_INSTRUCTION_NAME, build_environment_text(cwd))
        .with_position(FragmentPosition::BeforeSystemPrompt)
        .with_order(-200)
        .with_scope(FragmentScope::All)
        .with_authored_by(FragmentAuthor::Plugin)
}

/// The bare filename this crate falls back to when no `.conway/
/// instructions.md` is found anywhere on [`project_instructions_path`]'s
/// walk -- see this module's own doc, "Operator instructions", point 1.
/// `CLAUDE.md` is deliberately not a second fallback filename here: ruled
/// out as single-vendor, unlike this name's multi-harness convergence.
pub const AGENTS_FALLBACK_FILENAME: &str = "AGENTS.md";

/// The project-scope operator file for `cwd`: the NEAREST
/// `.conway/instructions.md` found walking from `cwd` up through each
/// ancestor directory (nearest first), stopping at -- and including -- the
/// enclosing git repository root (this module's own doc, "Operator
/// instructions", point 1). When that walk finds no `.conway/
/// instructions.md` anywhere, the NEAREST [`AGENTS_FALLBACK_FILENAME`]
/// found on the identical directory list is returned instead --
/// `.conway/instructions.md` wins whenever both exist anywhere on the
/// walk, even one nearer to `cwd` than a farther `.conway/instructions.md`
/// that wins. When `cwd` is not inside a git repository at all
/// (`find_enclosing_git_root` returns `None`), the walk is `cwd` alone --
/// no walk above a repository boundary that does not exist, so a
/// home-directory file is never picked up as a project file.
///
/// When NEITHER file is found anywhere on the walk, the direct
/// `cwd`-join is returned regardless (the exact path the pre-walk-up
/// version of this function always returned) -- `read_operator_fragment`'s
/// own `NotFound` handling already treats that path exactly like every
/// other absent file, missing silently (this module's own doc, "Operator
/// instructions", point 4), so returning it here rather than an `Option`
/// keeps every existing caller's shape unchanged.
pub fn project_instructions_path(cwd: &Path) -> PathBuf {
    let dirs = candidate_project_directories(cwd);
    if let Some(found) = dirs
        .iter()
        .map(|dir| dir.join(".conway").join(OPERATOR_INSTRUCTIONS_FILENAME))
        .find(|path| path.exists())
    {
        return found;
    }
    if let Some(found) = dirs
        .iter()
        .map(|dir| dir.join(AGENTS_FALLBACK_FILENAME))
        .find(|path| path.exists())
    {
        return found;
    }
    cwd.join(".conway").join(OPERATOR_INSTRUCTIONS_FILENAME)
}

/// The directories [`project_instructions_path`] searches, NEAREST FIRST:
/// `cwd` itself, then each ancestor up to and including
/// [`find_enclosing_git_root`]'s result -- or `[cwd]` alone when that
/// returns `None` (no enclosing git repository). Pulled out as its own
/// function so the directory LIST and the two filename searches that
/// consume it stay separately testable.
fn candidate_project_directories(cwd: &Path) -> Vec<PathBuf> {
    let git_root = find_enclosing_git_root(cwd);
    let mut dirs = Vec::new();
    let mut current = cwd.to_path_buf();
    loop {
        dirs.push(current.clone());
        if git_root.as_deref() == Some(current.as_path()) {
            break;
        }
        match &git_root {
            Some(_) => match current.parent() {
                Some(parent) => current = parent.to_path_buf(),
                // Defensive only: `git_root`, when `Some`, is by
                // construction an ancestor of `cwd` (or `cwd` itself)
                // reached by this identical `.parent()` chain, so this
                // arm is unreachable in practice -- stop rather than loop
                // forever if that invariant is ever wrong.
                None => break,
            },
            // No enclosing git repository: the walk is `cwd` alone. A
            // walk with no repository boundary to stop at must not run
            // all the way to the filesystem root -- a home-directory
            // `AGENTS.md` (or `.conway/instructions.md`) above `cwd` is
            // not a project file.
            None => break,
        }
    }
    dirs
}

/// Walks up from `cwd`, INCLUSIVE, looking for a `.git` entry -- directory
/// OR file. A plain repository's `.git` is a directory; a linked
/// worktree's or a submodule's `.git` is a FILE containing a `gitdir:`
/// pointer elsewhere, but its mere presence still marks the directory
/// that holds it as that worktree's or submodule's own project root for
/// this walk's purposes -- this function does not resolve where the
/// pointer leads, only whether one exists. Returns `None` when no such
/// entry exists anywhere above `cwd`, including the ordinary case of `cwd`
/// not being inside a git repository at all.
///
/// **Not handled:** a bare-repository checkout, where `cwd` sits inside
/// the bare repository's own directory (`HEAD`/`objects`/`refs` directly
/// present) rather than under a nested `.git`. Nothing marks that
/// directory as a boundary here, so the walk falls back to `cwd` alone in
/// that case, the same as a plain non-repository directory.
fn find_enclosing_git_root(cwd: &Path) -> Option<PathBuf> {
    let mut current = cwd.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => return None,
        }
    }
}

/// The default global-scope operator file: alongside conway's user-scoped
/// `settings.json` (`conway::config::discovery::user_config_path`'s own
/// directory) -- `None` under the exact condition that function returns
/// `None` (no home directory discoverable on this platform/environment,
/// and `CONWAY_CONFIG_DIR` unset or empty in `env`). **Honours
/// `CONWAY_CONFIG_DIR`** (board item `01M0W5Q569F0T97HSEP6F0MPCR`): unlike
/// the raw, override-independent `home_settings_path`, `user_config_path`
/// relocates to `$CONWAY_CONFIG_DIR/settings.json` whenever that variable
/// is set and non-empty in `env`, and this function follows it there --
/// see this module's own doc, "Operator instructions", point 2, for why
/// that parity with `settings.json` matters.
pub fn global_instructions_path(
    env: &std::collections::HashMap<String, String>,
) -> Option<PathBuf> {
    conway::config::discovery::user_config_path(env)
        .and_then(|settings| settings.parent().map(Path::to_path_buf))
        .map(|dir| dir.join(OPERATOR_INSTRUCTIONS_FILENAME))
}

/// Resolves both operator-file locations for `cwd`, collapsing the global
/// path to `None` when it names the SAME underlying file as the project
/// path (an operator whose project genuinely lives at `$HOME`) -- see this
/// module's own doc, "Operator instructions", point 3, for why that case
/// must not inject the same text twice under two fragment names.
///
/// `env` is the same explicit map every `CONWAY_CONFIG_DIR`-aware resolver
/// in this codebase takes -- forwarded to [`global_instructions_path`],
/// never read from `std::env` here or anywhere downstream of this call.
pub fn resolve_operator_paths(
    cwd: &Path,
    env: &std::collections::HashMap<String, String>,
) -> (PathBuf, Option<PathBuf>) {
    let project = project_instructions_path(cwd);
    let global = collapse_global_onto_project(project.clone(), global_instructions_path(env));
    (project, global)
}

/// The collapse rule itself, pulled out as a pure function so it is
/// testable with two arbitrary paths rather than requiring a real,
/// process-global home directory to coincide with a test's own `cwd`
/// (which would not be test-isolated). `None` in, `None` out; `Some`
/// naming the same underlying file as `project` collapses to `None`;
/// anything else passes through unchanged.
fn collapse_global_onto_project(project: PathBuf, global: Option<PathBuf>) -> Option<PathBuf> {
    match global {
        Some(candidate) if same_file_lenient(&candidate, &project) => None,
        other => other,
    }
}

/// Whether `a` and `b` name the same underlying file -- canonicalizes both
/// sides when possible, falling back to a lexical comparison when either
/// side does not exist (the ordinary case for a file an operator has not
/// written yet), mirroring `conway::config::discovery`'s own
/// `same_settings_file` comparison one level up rather than re-exporting
/// it (that helper is private to its own module).
fn same_file_lenient(a: &Path, b: &Path) -> bool {
    fn normalize(path: &Path) -> PathBuf {
        let mut out = PathBuf::new();
        for component in path.components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    out.pop();
                }
                other => out.push(other.as_os_str()),
            }
        }
        out
    }
    let resolve = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| normalize(p));
    resolve(a) == resolve(b)
}

/// Reads one operator instruction file at `path` into a named
/// [`InstructionFragment`], implementing this module's own "Operator
/// instructions" point 4:
///
/// - `path` is `None` (no resolvable location for this scope): `Ok(None)`,
///   silently -- there is nothing to read.
/// - The file does not exist (`io::ErrorKind::NotFound`): `Ok(None)`,
///   silently (P-13: absence is the normal, unremarkable case).
/// - The file exists and is whitespace-only: `Ok(None)` -- an empty file
///   communicates nothing, the same as an absent one.
/// - The file exists, has content, and reads cleanly: parsed through the
///   SAME [`parse_fragment_markdown`] convention [`FRAGMENT_TEXT`] itself
///   uses (board item `01M1FSRJJAB3ZYZXED4SVT2ZSF`) -- an operator can gate
///   their own sentences on a tool exactly the way this crate does for its
///   own -- and becomes `Ok(Some(fragment))`.
/// - Any OTHER read failure (permission denied, the path names a
///   directory, the bytes are not valid UTF-8) OR a malformed `<!-- tools:
///   ... -->` comment in an otherwise-readable file: `Err(FacadeError::
///   Config)`, naming `path` and the underlying problem -- surfaced at
///   `ConwayBuilder::build`, never dropped. A file the operator wrote and
///   conway silently ignored (or silently mis-parsed) is the failure mode
///   P-13 exists to prevent; a malformed comment is exactly as much "the
///   operator's own words, misread" as an encoding error is, so it fails
///   at the identical tier rather than a softer one.
fn read_operator_fragment(
    path: Option<&Path>,
    name: &str,
) -> conway::Result<Option<InstructionFragment>> {
    let Some(path) = path else {
        return Ok(None);
    };
    match std::fs::read_to_string(path) {
        Ok(text) => {
            if text.trim().is_empty() {
                Ok(None)
            } else {
                let (body, parts) = parse_fragment_markdown(&text).map_err(|reason| {
                    conway::FacadeError::Config {
                        path: Some(path.to_path_buf()),
                        message: format!(
                            "operator instructions file {} uses the `<!-- tools: ... -->` \
                             convention incorrectly: {reason}",
                            path.display()
                        ),
                    }
                })?;
                // `position`/`scope` are spelled out explicitly, even
                // though both equal `InstructionFragment::new`'s own
                // defaults, so this reads as a deliberate choice rather
                // than an accident of construction order: an operator's
                // own standing instructions follow an agent def's own
                // prompt, never precede it, and reach every agent -- see
                // this module's own "Where the base fragment lands" doc.
                //
                // `authored_by: FragmentAuthor::Operator { path }` -- board
                // item `01M1FSNBRE5XJ0GQ04RT5HZ1PS`: this text is the
                // OPERATOR'S OWN, not this plugin's, so `ContextBuilder::
                // build` must stamp the resulting segment
                // `Provenance::Operator { name, path }`, never
                // `Provenance::PluginInstruction { plugin_id: "conway.idiom",
                // .. }` -- see this module's own doc, "Provenance: the
                // shipped fragment and an operator's own text are tagged
                // apart".
                Ok(Some(
                    InstructionFragment::new(name, body)
                        .with_parts(parts)
                        .with_position(FragmentPosition::AfterSystemPrompt)
                        .with_scope(FragmentScope::All)
                        .with_authored_by(FragmentAuthor::Operator {
                            path: path.to_path_buf(),
                        }),
                ))
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(conway::FacadeError::Config {
            path: Some(path.to_path_buf()),
            message: format!(
                "could not read operator instructions file {}: {err}",
                path.display()
            ),
        }),
    }
}

impl Plugin for IdiomPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PLUGIN_ID.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            tools: vec![],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn description(&self) -> PluginDescription {
        PluginDescription {
            summary: "prepends a session-static environment block (cwd, OS, date, git branch) \
                       and a short conway-idioms primer to a session's system-prompt segment, \
                       plus an operator's own project/global instructions.md when either exists"
                .to_string(),
            you_get: "two shipped instruction fragments. First, a small session-static \
                      environment block (board item 01M1FSTQT952QYM014G65EVW25) -- cwd, OS/arch, \
                      the session's start date, and (when cwd is inside a git work tree) the \
                      current branch or a short commit sha -- computed once, at install time, \
                      and rendered genuinely first, ahead of everything else including this \
                      plugin's own idioms primer. Second, that idioms primer itself (fork vs. \
                      spawn, how an agent ends, configuration-dependent tools, context scarcity, \
                      permissions, budgets, steering), immediately behind the environment block, \
                      both ahead of even an agent def's own system prompt -- and ahead of the \
                      whole context when no agent def supplies one, which is the ordinary \
                      interactive-TUI case this plugin exists for. Both reach every forked or \
                      spawned child too, not the root alone (board item \
                      01M0VSKA76NSEHDSH25XJGJ2J5's ruling: an instruction fragment is harness \
                      configuration, not transcript context, so fork/spawn's inheritance split \
                      does not govern it) -- the permissions/steering bullets, and the \
                      `report`-gated part specifically, describe how a *child* agent should \
                      behave, and now reach exactly that agent. Also up to two more fragments, \
                      additive alongside the two shipped ones, \
                      read from an operator's own `.conway/instructions.md` (project scope, \
                      found by walking up from cwd to the enclosing git repository root, or \
                      -- when that file is absent everywhere on the walk -- the nearest \
                      `AGENTS.md` instead) and `<home>/.conway/instructions.md` (global scope) \
                      when either file exists -- \
                      reaching a forked/spawned child exactly the same way, for the same reason"
                .to_string(),
            you_lose: "nothing else".to_string(),
            costs: "one system-prompt segment's worth of tokens per turn for the environment \
                    block (a single short sentence, byte-identical across the whole session -- \
                    the same stable prefix every turn re-sends, so it costs nothing extra on a \
                    prompt cache after the first turn) plus the idioms primer's own \
                    unconditional body plus whichever of its bash/report/conway_fork-gated \
                    sentences this turn's own tool set makes reachable (board item \
                    01M1FSRJJAB3ZYZXED4SVT2ZSF) -- never the full ~360-word raw source at once, \
                    only the parts that actually apply -- plus whatever an operator's own \
                    instructions.md file(s) cost -- /context's preamble section names \
                    conway.idiom.environment, conway.idiom.base, conway.idiom.operator.project, \
                    and conway.idiom.operator.global separately, each with its own exact token \
                    cost"
                .to_string(),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![]
    }

    fn instructions(&self) -> Vec<InstructionFragment> {
        // `FRAGMENT_TEXT` is THIS crate's own shipped file, parsed once per
        // call (board item `01M1FSRJJAB3ZYZXED4SVT2ZSF` -- see this
        // module's own doc, "The `tool_ids` trap"). It must parse: a
        // malformed shipped fragment is a bug in this crate, not a
        // condition to handle at runtime -- `.expect()`, backed by
        // `fragment_stays_within_budget` and every other test below that
        // exercises this exact call.
        let (body, parts) = parse_fragment_markdown(FRAGMENT_TEXT)
            .expect("FRAGMENT_TEXT is this crate's own shipped file and must parse");
        // `self.environment` was computed exactly once, at construction
        // time (`IdiomPlugin::new`/`IdiomPlugin::from_operator_files`) --
        // cloned here, never re-derived, so two calls to `instructions()`
        // return byte-identical text for it (see this module's own doc,
        // "The environment block", and
        // `environment_fragment_is_byte_identical_across_two_calls` below).
        // `order: -200` renders it ahead of even the base fragment's own
        // `order: -100` immediately below.
        let mut fragments = vec![self.environment.clone()];
        fragments.push(
            InstructionFragment::new(INSTRUCTION_NAME, body)
                .with_parts(parts)
                // `BeforeSystemPrompt`, `order: -100` -- see this module's
                // own doc, "Where the base fragment lands": conway's own
                // harness orientation now precedes even an agent
                // definition's own carefully-authored prompt, the position
                // this plugin's own premise (a bare interactive session with
                // no `[0]` at all) never needed but a curated `AgentDef`
                // always did.
                .with_position(FragmentPosition::BeforeSystemPrompt)
                .with_order(-100),
        );
        fragments.extend(self.operator_project.clone());
        fragments.extend(self.operator_global.clone());
        fragments
    }
}

#[cfg(test)]
mod plugin_tests {
    use super::*;

    /// The plugin browser's own read surface (board item
    /// `01M0KARX71A64NTSYTDBVANVPF`): a real description, never the
    /// trait's empty default.
    #[test]
    fn description_is_non_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let description = IdiomPlugin::new(tmp.path()).description();
        assert!(!description.summary.is_empty());
        assert!(!description.you_get.is_empty());
        assert!(!description.you_lose.is_empty());
        assert!(!description.costs.is_empty());
    }

    /// Board item `01M0VSKA76NSEHDSH25XJGJ2J5` ruled a plugin instruction
    /// fragment reaches a forked/spawned child too, not the root alone --
    /// `you_get` (not `you_lose`, now that this is a capability rather than
    /// a limitation) must say so verbatim, not merely in this crate's own
    /// doc comment -- GP-14: a declaration site is one artifact, and an
    /// operator deciding whether to install this plugin reads the
    /// description, not the source.
    #[test]
    fn you_get_names_the_subagent_reach() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let description = IdiomPlugin::new(tmp.path()).description();
        assert!(
            description.you_get.to_lowercase().contains("subagent")
                || description.you_get.to_lowercase().contains("child"),
            "you_get must state that the fragment reaches a forked/spawned child: {:?}",
            description.you_get
        );
    }

    /// Exactly two fragments (the environment block, then the idioms
    /// primer), contributing no tool -- `manifest().tools` is empty, so
    /// the reachability check's "same plugin also provides the tool"
    /// shortcut never applies here; every id any of the primer's parts
    /// names (board item `01M1FSRJJAB3ZYZXED4SVT2ZSF` -- `bash`, `report`,
    /// `conway_fork`) has to be reachable through a DIFFERENT installed
    /// plugin. Both fragments' bodies stay unconditional; only the
    /// primer's own extra sentences are gated.
    #[test]
    fn contributes_exactly_two_fragments_and_no_tool() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plugin = IdiomPlugin::new(tmp.path());
        assert!(plugin.tools().is_empty());
        assert!(plugin.manifest().tools.is_empty());
        let instructions = plugin.instructions();
        assert_eq!(instructions.len(), 2);
        assert_eq!(
            instructions[0].name, ENVIRONMENT_INSTRUCTION_NAME,
            "the environment block must be the FIRST fragment"
        );
        assert!(
            !instructions[0].text.is_empty(),
            "the environment block's body must be non-empty"
        );
        assert_eq!(instructions[1].name, INSTRUCTION_NAME);
        assert!(
            !instructions[1].text.is_empty(),
            "the base fragment's body must be non-empty"
        );
        assert!(
            !instructions[1].parts.is_empty(),
            "the base fragment must declare at least one conditional part"
        );
    }

    /// Declaration honesty for this module's own "Where the base fragment
    /// lands" doc: the base fragment must actually declare
    /// `BeforeSystemPrompt`/`order: -100`, not merely claim to in prose.
    /// `crates/conway/tests/builder.rs`'s
    /// `a_before_system_prompt_fragment_renders_ahead_of_a_real_agent_defs_prompt`
    /// proves the RENDERED effect through a real `AgentDef` and a real
    /// `ContextBuilder::build` pass; this is the declaration-level pin.
    #[test]
    fn base_fragment_declares_before_system_prompt_with_negative_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plugin = IdiomPlugin::new(tmp.path());
        let instructions = plugin.instructions();
        let base = instructions
            .iter()
            .find(|f| f.name == INSTRUCTION_NAME)
            .expect("base fragment present");
        assert_eq!(base.position, FragmentPosition::BeforeSystemPrompt);
        assert_eq!(base.order, -100);
    }

    /// The environment block declares `BeforeSystemPrompt`/`order: -200` --
    /// ahead of even the base fragment's own `order: -100` -- see this
    /// module's own doc, "The environment block".
    /// `tests/idiom_end_to_end.rs`'s
    /// `environment_fragment_renders_first_ahead_of_the_base_fragment`
    /// proves the RENDERED ordering effect through a real facade build
    /// (acceptance 3); this is the declaration-level pin.
    #[test]
    fn environment_fragment_declares_before_system_prompt_with_a_more_negative_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plugin = IdiomPlugin::new(tmp.path());
        let instructions = plugin.instructions();
        let environment = instructions
            .iter()
            .find(|f| f.name == ENVIRONMENT_INSTRUCTION_NAME)
            .expect("environment fragment present");
        assert_eq!(environment.position, FragmentPosition::BeforeSystemPrompt);
        assert_eq!(environment.order, -200);
        let base = instructions
            .iter()
            .find(|f| f.name == INSTRUCTION_NAME)
            .expect("base fragment present");
        assert!(
            environment.order < base.order,
            "the environment block's order ({}) must be more negative than the base \
             fragment's ({}), so it renders first",
            environment.order,
            base.order
        );
    }

    /// Acceptance 2: two consecutive `instructions()` calls on the SAME
    /// `IdiomPlugin` instance return byte-identical text for the
    /// environment fragment specifically -- the core cache-prefix-
    /// stability property this fragment exists to preserve (see this
    /// module's own doc, "The environment block"). Also pins that the
    /// fragment's declared position/order/scope/authored_by are identical
    /// across both calls, not merely its text.
    #[test]
    fn environment_fragment_is_byte_identical_across_two_calls() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dot_git = tmp.path().join(".git");
        std::fs::create_dir_all(&dot_git).expect("mkdir .git");
        std::fs::write(dot_git.join("HEAD"), "ref: refs/heads/main\n").expect("write HEAD");

        let plugin = IdiomPlugin::new(tmp.path());
        let first = plugin
            .instructions()
            .into_iter()
            .find(|f| f.name == ENVIRONMENT_INSTRUCTION_NAME)
            .expect("environment fragment present on first call");
        let second = plugin
            .instructions()
            .into_iter()
            .find(|f| f.name == ENVIRONMENT_INSTRUCTION_NAME)
            .expect("environment fragment present on second call");
        assert_eq!(
            first, second,
            "two consecutive instructions() calls must return a byte-identical environment \
             fragment"
        );
    }

    /// The operator project/global fragments, by contrast, must stay at the
    /// `AfterSystemPrompt` default -- an agent def's own prompt still
    /// precedes an operator's own standing instructions.
    #[test]
    fn operator_fragments_stay_after_system_prompt() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("instructions.md");
        std::fs::write(&path, "Always run tests.\n").expect("write");
        let plugin =
            IdiomPlugin::from_operator_files(tmp.path(), Some(&path), None).expect("read ok");
        let operator = plugin
            .instructions()
            .into_iter()
            .find(|f| f.name == OPERATOR_PROJECT_INSTRUCTION_NAME)
            .expect("operator fragment present");
        assert_eq!(operator.position, FragmentPosition::AfterSystemPrompt);
        assert_eq!(operator.order, 0);
    }

    /// Board item `01M1FSNBRE5XJ0GQ04RT5HZ1PS`: the shipped base fragment
    /// declares no `authored_by` override, so it stays at
    /// `InstructionFragment::new`'s own default, `FragmentAuthor::Plugin`
    /// -- `ContextBuilder::build` stamps it `Provenance::PluginInstruction`,
    /// never `Provenance::Operator`, since this crate (not an operator)
    /// wrote every word of it.
    #[test]
    fn base_fragment_is_authored_by_the_plugin() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plugin = IdiomPlugin::new(tmp.path());
        let instructions = plugin.instructions();
        let base = instructions
            .iter()
            .find(|f| f.name == INSTRUCTION_NAME)
            .expect("base fragment present");
        assert_eq!(base.authored_by, FragmentAuthor::Plugin);
    }

    /// The operator project/global fragments, by contrast, must declare
    /// `FragmentAuthor::Operator { path }` naming exactly the file this
    /// plugin read the text from -- `ContextBuilder::build` stamps these
    /// `Provenance::Operator`, so an operator's own words are never
    /// durably attributed to `conway.idiom` itself.
    #[test]
    fn operator_fragments_are_authored_by_the_operator_naming_their_own_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project_path = tmp.path().join("project-instructions.md");
        let global_path = tmp.path().join("global-instructions.md");
        std::fs::write(&project_path, "Project convention.\n").expect("write");
        std::fs::write(&global_path, "House-wide preference.\n").expect("write");
        let plugin =
            IdiomPlugin::from_operator_files(tmp.path(), Some(&project_path), Some(&global_path))
                .expect("read ok");
        let instructions = plugin.instructions();

        let project = instructions
            .iter()
            .find(|f| f.name == OPERATOR_PROJECT_INSTRUCTION_NAME)
            .expect("operator project fragment present");
        assert_eq!(
            project.authored_by,
            FragmentAuthor::Operator {
                path: project_path.clone()
            }
        );

        let global = instructions
            .iter()
            .find(|f| f.name == OPERATOR_GLOBAL_INSTRUCTION_NAME)
            .expect("operator global fragment present");
        assert_eq!(
            global.authored_by,
            FragmentAuthor::Operator {
                path: global_path.clone()
            }
        );
    }

    /// Budget pin (acceptance 3): fails loudly if a future edit grows the
    /// fragment past the item's stated 40-line/400-word cap, rather than
    /// letting the cap drift unnoticed.
    #[test]
    fn fragment_stays_within_budget() {
        let line_count = FRAGMENT_TEXT.lines().count();
        let word_count = FRAGMENT_TEXT.split_whitespace().count();
        assert!(
            line_count <= 40,
            "fragment grew to {line_count} lines, past the 40-line budget"
        );
        assert!(
            word_count <= 400,
            "fragment grew to {word_count} words, past the 400-word budget"
        );
    }
}

/// Unit coverage for the operator-file reading/resolution logic itself,
/// in isolation from a real `Conway` build -- `tests/idiom_end_to_end.rs`
/// is the end-to-end proof the resulting fragments reach a real wire
/// request (acceptance 1); this module is P-15's "shown to fail" proof
/// for the missing/empty/unreadable/malformed policy (acceptance 2).
#[cfg(test)]
mod operator_file_tests {
    use super::*;

    /// P-13: a file the operator never wrote must not be an error, and
    /// must not contribute a fragment either -- silent and normal.
    #[test]
    fn missing_file_is_silent_and_contributes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("does-not-exist.md");
        let plugin = IdiomPlugin::from_operator_files(tmp.path(), Some(&path), None)
            .expect("a missing file must not be an error");
        assert_eq!(
            plugin.instructions().len(),
            2,
            "environment block and base fragment only"
        );
    }

    /// An empty (or whitespace-only) file communicates nothing, same as an
    /// absent one -- not an error, and no fragment.
    #[test]
    fn empty_file_is_silent_and_contributes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("instructions.md");
        std::fs::write(&path, "   \n\n\t\n").expect("write");
        let plugin = IdiomPlugin::from_operator_files(tmp.path(), Some(&path), None)
            .expect("a whitespace-only file must not be an error");
        assert_eq!(
            plugin.instructions().len(),
            2,
            "environment block and base fragment only"
        );
    }

    /// The positive path: a real, non-empty file becomes a real fragment,
    /// named and reachable, its exact text intact.
    #[test]
    fn present_file_becomes_a_named_reachable_fragment() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("instructions.md");
        std::fs::write(&path, "Always run `cargo test` before reporting done.\n").expect("write");
        let plugin =
            IdiomPlugin::from_operator_files(tmp.path(), Some(&path), None).expect("read ok");
        let instructions = plugin.instructions();
        assert_eq!(instructions.len(), 3);
        let operator = instructions
            .iter()
            .find(|f| f.name == OPERATOR_PROJECT_INSTRUCTION_NAME)
            .expect("operator project fragment present");
        assert!(operator.text.contains("Always run `cargo test`"));
        assert!(
            operator.parts.is_empty(),
            "a file with no `<!-- tools: ... -->` comment must parse to body-only, no parts"
        );
    }

    /// An operator can gate their own sentence on a tool too, through the
    /// identical `<!-- tools: ... -->` convention the shipped fragment
    /// uses (board item `01M1FSRJJAB3ZYZXED4SVT2ZSF`).
    #[test]
    fn operator_file_with_a_tools_comment_yields_a_gated_part() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("instructions.md");
        std::fs::write(
            &path,
            "House convention: prefer small commits.\n\n<!-- tools: bash -->\nAlways run \
             `cargo test` before reporting done.\n",
        )
        .expect("write");
        let plugin =
            IdiomPlugin::from_operator_files(tmp.path(), Some(&path), None).expect("read ok");
        let instructions = plugin.instructions();
        let operator = instructions
            .iter()
            .find(|f| f.name == OPERATOR_PROJECT_INSTRUCTION_NAME)
            .expect("operator project fragment present");
        assert!(operator.text.contains("prefer small commits"));
        assert_eq!(operator.parts.len(), 1);
        assert_eq!(operator.parts[0].tool_ids, vec![ToolName::new("bash")]);
        assert!(operator.parts[0].text.contains("Always run `cargo test`"));
    }

    /// P-15's "shown to fail" bar, for the parser's own error path
    /// (acceptance 3): a malformed `<!-- tools: ... -->` comment in an
    /// operator's file surfaces as a build-time-adjacent `Err`, never a
    /// silent fallback that demotes the paragraph to body text nobody
    /// asked to gate. Falsified by temporarily changing
    /// `read_operator_fragment` to ignore `parse_fragment_markdown`'s
    /// `Err` and fall back to `InstructionFragment::new(name, text)`
    /// unparsed -- confirmed to make this test fail (the call returns
    /// `Ok` instead of `Err`) before restoring the real `map_err` propagation.
    #[test]
    fn operator_file_with_a_malformed_tools_comment_is_a_load_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("instructions.md");
        std::fs::write(
            &path,
            // Missing the `tools:` prefix -- an author's typo, not a
            // deliberate comment.
            "<!-- bash -->\nAlways run `cargo test` before reporting done.\n",
        )
        .expect("write");
        let result = IdiomPlugin::from_operator_files(tmp.path(), Some(&path), None);
        assert!(
            result.is_err(),
            "a malformed `<!-- tools: ... -->` comment must surface as an error, not be \
             silently demoted to body text"
        );
    }

    /// Acceptance 2, the negative half, and P-15's "shown to fail" bar:
    /// a file that exists but cannot be read as UTF-8 text (a directory,
    /// standing in for "the read fails for a reason other than absence")
    /// must surface as an `Err`, never be silently dropped. Falsified by
    /// temporarily deleting the `Err(err) => Err(...)` arm's guard (i.e.
    /// treating every read failure as `Ok(None)`, the fail-open shape this
    /// test exists to reject) -- confirmed to fail before restoring it.
    #[test]
    fn unreadable_file_surfaces_as_an_error_not_a_silent_drop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // A directory named `instructions.md`: `fs::read_to_string` fails
        // with `ErrorKind::IsADirectory` (or an OS-specific equivalent),
        // never `NotFound` -- so this exercises the exact "exists but
        // cannot be read" branch this test is named for, without relying
        // on a platform-specific permissions setup.
        let path = tmp.path().join("instructions.md");
        std::fs::create_dir(&path).expect("mkdir");
        let result = IdiomPlugin::from_operator_files(tmp.path(), Some(&path), None);
        assert!(
            result.is_err(),
            "an unreadable operator file must surface as an error, not be silently dropped"
        );
    }

    /// The malformed case: bytes that are not valid UTF-8 must surface too,
    /// on the same footing as the directory case above -- `parse_skill_def`'s
    /// own precedent (`crate::skills::load_skill_defs` fails loudly on a
    /// malformed `SKILL.md`) is the shape this mirrors.
    #[test]
    fn malformed_utf8_file_surfaces_as_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("instructions.md");
        std::fs::write(&path, [0xff, 0xfe, 0x00, 0xff]).expect("write invalid utf-8");
        let result = IdiomPlugin::from_operator_files(tmp.path(), Some(&path), None);
        assert!(
            result.is_err(),
            "invalid UTF-8 in an operator file must surface as an error"
        );
    }

    /// `None` for a scope (no resolvable location -- e.g. no home directory
    /// discoverable) is treated exactly like "file absent": silent, no
    /// fragment, no error.
    #[test]
    fn none_path_is_silent_and_contributes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plugin = IdiomPlugin::from_operator_files(tmp.path(), None, None).expect("ok");
        assert_eq!(plugin.instructions().len(), 2);
    }

    /// Both scopes present at once: two additional, independently named
    /// fragments, neither one displacing the other.
    #[test]
    fn project_and_global_both_present_yield_two_independent_fragments() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project = tmp.path().join("project-instructions.md");
        let global = tmp.path().join("global-instructions.md");
        std::fs::write(&project, "Project-specific convention.\n").expect("write");
        std::fs::write(&global, "House-wide preference.\n").expect("write");
        let plugin = IdiomPlugin::from_operator_files(tmp.path(), Some(&project), Some(&global))
            .expect("read ok");
        let instructions = plugin.instructions();
        assert_eq!(instructions.len(), 4);
        assert!(instructions
            .iter()
            .any(|f| f.name == OPERATOR_PROJECT_INSTRUCTION_NAME
                && f.text.contains("Project-specific")));
        assert!(instructions
            .iter()
            .any(|f| f.name == OPERATOR_GLOBAL_INSTRUCTION_NAME && f.text.contains("House-wide")));
    }

    /// [`resolve_operator_paths`]: an ordinary project/global split names
    /// two distinct paths -- driven through an isolated `CONWAY_CONFIG_DIR`
    /// (never a real, ambient home directory: this crate's own tests must
    /// stay parallel-safe and must never touch the invoking user's real
    /// `$HOME`).
    #[test]
    fn resolve_operator_paths_names_two_distinct_paths_in_the_ordinary_case() {
        let project_tmp = tempfile::tempdir().expect("tempdir");
        let global_tmp = tempfile::tempdir().expect("tempdir");
        let mut env = std::collections::HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            global_tmp.path().display().to_string(),
        );
        let (project, global) = resolve_operator_paths(project_tmp.path(), &env);
        assert_eq!(
            project,
            project_tmp.path().join(".conway").join("instructions.md")
        );
        assert_eq!(global, Some(global_tmp.path().join("instructions.md")));
    }

    /// Fixture-tree case (a): `repo/.conway/instructions.md` is read when
    /// launched from `repo/a/b`. Catches an implementation that never
    /// walks up at all (the direct `cwd`-join this crate used before this
    /// item) -- such an implementation resolves to
    /// `repo/a/b/.conway/instructions.md`, a path that does not exist, and
    /// silently sees no project instructions even though the repository
    /// genuinely has one.
    #[test]
    fn walk_up_finds_the_repo_root_dot_conway_file_from_a_nested_cwd() {
        let repo = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(repo.path().join(".git")).expect("mkdir .git");
        let dot_conway_instructions = repo.path().join(".conway").join("instructions.md");
        std::fs::create_dir_all(dot_conway_instructions.parent().unwrap()).expect("mkdir .conway");
        std::fs::write(&dot_conway_instructions, "Project convention.\n")
            .expect("write instructions.md");
        let nested = repo.path().join("a").join("b");
        std::fs::create_dir_all(&nested).expect("mkdir nested");

        assert_eq!(project_instructions_path(&nested), dot_conway_instructions);
    }

    /// Fixture-tree case (b): `repo/AGENTS.md` is read when
    /// `.conway/instructions.md` is absent everywhere on the walk, as
    /// [`OPERATOR_PROJECT_INSTRUCTION_NAME`] with its source noted via
    /// `authored_by`'s own `path`. Catches an implementation that walks
    /// correctly for `.conway/instructions.md` but never implements (or
    /// never walks for) the `AGENTS.md` fallback at all -- such an
    /// implementation resolves to the same nonexistent path case (a)
    /// rules out and sees nothing, even though the repository genuinely
    /// has standing instructions under the other name.
    #[test]
    fn walk_up_falls_back_to_the_repo_root_agents_md_when_dot_conway_is_absent() {
        let repo = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(repo.path().join(".git")).expect("mkdir .git");
        let agents_md = repo.path().join("AGENTS.md");
        std::fs::write(&agents_md, "Project convention.\n").expect("write AGENTS.md");
        let nested = repo.path().join("a").join("b");
        std::fs::create_dir_all(&nested).expect("mkdir nested");

        let resolved = project_instructions_path(&nested);
        assert_eq!(resolved, agents_md);

        let plugin =
            IdiomPlugin::from_operator_files(&nested, Some(&resolved), None).expect("read ok");
        let fragment = plugin
            .instructions()
            .into_iter()
            .find(|f| f.name == OPERATOR_PROJECT_INSTRUCTION_NAME)
            .expect("AGENTS.md must contribute the project-scope fragment");
        assert!(fragment.text.contains("Project convention"));
        match fragment.authored_by {
            FragmentAuthor::Operator { path } => assert_eq!(path, agents_md),
            other => panic!("expected FragmentAuthor::Operator, got {other:?}"),
        }
    }

    /// Fixture-tree case (c): both `repo/.conway/instructions.md` and a
    /// NEARER `repo/a/AGENTS.md` exist -- `.conway/instructions.md` wins
    /// even though it is farther from `cwd`. Catches a walk that is too
    /// eager to prefer `AGENTS.md`: an implementation that merges the two
    /// searches into one single nearest-first pass over BOTH filenames
    /// together (rather than exhausting the `.conway/instructions.md`
    /// search before ever trying `AGENTS.md`) would return
    /// `repo/a/AGENTS.md` here, the wrong file, silently overriding
    /// conway's own more specific declaration with a nearer but less
    /// specific one.
    #[test]
    fn dot_conway_wins_over_a_nearer_agents_md() {
        let repo = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(repo.path().join(".git")).expect("mkdir .git");
        let dot_conway_instructions = repo.path().join(".conway").join("instructions.md");
        std::fs::create_dir_all(dot_conway_instructions.parent().unwrap()).expect("mkdir .conway");
        std::fs::write(&dot_conway_instructions, "Project convention.\n")
            .expect("write instructions.md");
        let nearer_agents_md = repo.path().join("a").join("AGENTS.md");
        std::fs::create_dir_all(nearer_agents_md.parent().unwrap()).expect("mkdir a");
        std::fs::write(&nearer_agents_md, "A different, nearer file.\n").expect("write AGENTS.md");
        let nested = repo.path().join("a").join("b");
        std::fs::create_dir_all(&nested).expect("mkdir nested");

        assert_eq!(
            project_instructions_path(&nested),
            dot_conway_instructions,
            ".conway/instructions.md must win even though AGENTS.md is nearer to cwd"
        );
    }

    /// Fixture-tree case (d): no enclosing git repository -- the walk is
    /// `cwd` alone, never reaching a PARENT directory's `AGENTS.md`
    /// (deliberately placed just one level above `cwd`, with no `.git`
    /// anywhere). Catches a walk that is too greedy in the other
    /// direction: an implementation that walks to the filesystem root
    /// whenever it finds nothing nearby (ignoring the "stop at the git
    /// root, or don't walk at all" rule) would return the parent's
    /// `AGENTS.md` here -- exactly the home-directory-leak case this item
    /// explicitly rules out.
    #[test]
    fn no_repository_means_no_walk_a_parent_agents_md_is_never_read() {
        let outside = tempfile::tempdir().expect("tempdir");
        std::fs::write(outside.path().join("AGENTS.md"), "Not a project file.\n")
            .expect("write AGENTS.md");
        let cwd = outside.path().join("project");
        std::fs::create_dir_all(&cwd).expect("mkdir project");

        assert_eq!(
            project_instructions_path(&cwd),
            cwd.join(".conway").join("instructions.md"),
            "with no enclosing git repository, the walk must be cwd alone"
        );
    }

    /// Board item `01M0W5Q569F0T97HSEP6F0MPCR`, at the unit level:
    /// [`global_instructions_path`] must track wherever `CONWAY_CONFIG_DIR`
    /// points -- two distinct values must resolve to two distinct paths,
    /// never a single ambient location neither one names.
    #[test]
    fn global_instructions_path_honours_conway_config_dir() {
        let dir_a = tempfile::tempdir().expect("tempdir");
        let dir_b = tempfile::tempdir().expect("tempdir");
        let mut env_a = std::collections::HashMap::new();
        env_a.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            dir_a.path().display().to_string(),
        );
        let mut env_b = std::collections::HashMap::new();
        env_b.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            dir_b.path().display().to_string(),
        );
        assert_eq!(
            global_instructions_path(&env_a),
            Some(dir_a.path().join("instructions.md"))
        );
        assert_eq!(
            global_instructions_path(&env_b),
            Some(dir_b.path().join("instructions.md"))
        );
        assert_ne!(
            global_instructions_path(&env_a),
            global_instructions_path(&env_b)
        );
    }

    /// The collapse case (point 3 of this module's own doc): when the
    /// global path would name the SAME file as the project path (the
    /// operator's project genuinely lives at their resolved global
    /// location), it must collapse to `None` rather than double-inject the
    /// same text under two fragment names.
    #[test]
    fn collapse_global_onto_project_drops_a_coincident_global_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project = tmp.path().join(".conway").join("instructions.md");
        let same_path_different_spelling =
            tmp.path().join(".").join(".conway").join("instructions.md");
        assert_eq!(
            collapse_global_onto_project(project.clone(), Some(same_path_different_spelling)),
            None,
            "a global path naming the same file as project must collapse to None"
        );
    }

    /// The ordinary case: a genuinely different global path passes through
    /// untouched.
    #[test]
    fn collapse_global_onto_project_keeps_a_distinct_global_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project = tmp.path().join(".conway").join("instructions.md");
        let global = tmp.path().join("elsewhere").join("instructions.md");
        assert_eq!(
            collapse_global_onto_project(project, Some(global.clone())),
            Some(global)
        );
    }

    /// `None` in, `None` out -- no home directory resolvable is not an
    /// error and does not accidentally conjure a global path.
    #[test]
    fn collapse_global_onto_project_passes_through_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project = tmp.path().join(".conway").join("instructions.md");
        assert_eq!(collapse_global_onto_project(project, None), None);
    }
}
