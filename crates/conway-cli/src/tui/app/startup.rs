//! `App::session_spec`/`App::new` -- the interactive session's construction
//! path, extracted out of `app.rs` verbatim. `run`, the
//! dispatch loop `new` hands off to, lives in [`super::run`]; the four
//! pre-parser slash-command interceptions stay in `app.rs` itself (a guard
//! in `crates/conway/tests/architecture_invariants.rs` greps that exact
//! file's source text for them).

use conway::{
    Conway, ForkSpec, ModelRef, RoleAlias, SessionFilter, SessionHandle, SessionId, SessionMeta,
    SessionSpec, ToolSelector,
};
use tokio::sync::mpsc;

use crate::cli::Cli;

use super::App;
use crate::tui::state::AppState;
use crate::tui::view::Theme;

impl App {
    /// Builds the `SessionSpec` [`Self::new`] passes to `Conway::
    /// new_session` -- factored out into its own associated function,
    /// rather than left inline, so it is directly testable without a live
    /// `Conway`/terminal (`tests/tui_model_pin.rs` drives THIS function --
    /// `App::new`'s own construction path, not one-shot's
    /// `oneshot::resolve_session`, which has no equivalent for the TUI to
    /// share).
    ///
    /// `--model` used to be accepted by
    /// the parser and then never read here at all, despite the same flag being
    /// genuinely wired in one-shot mode -- a renderer-only gap -- every
    /// capability lands in the facade, so a difference between modes is a
    /// renderer bug, closed by reusing one-shot's own `--model` parser
    /// (`crate::model_pin::parse_model_pin`) rather than a second one that
    /// could fail a malformed value a different way.
    ///
    /// **Superseded, in part, by board item `01M1YS4FMJH004D1Y619MTBY7A`:**
    /// `--resume`/`--fork-from` used to be a decided non-goal for the TUI
    /// alongside `--session` (the paragraph this replaces argued building a
    /// TUI-flavored `oneshot::resolve_session` was out of scope). That item
    /// built exactly that -- [`Self::resolve_handle`], immediately below,
    /// now the TUI's own per-flag continuity logic (existence-probe-free,
    /// since `Conway::resume_with`/`Conway::fork_from` reattach rather than
    /// create) -- plus a fourth flag, `--continue`, one-shot has no
    /// equivalent of. `session_spec` itself keeps its original, narrower
    /// job: building the FRESH-session `SessionSpec`, called only from
    /// `resolve_handle`'s own flag-free arm. `--session` alone is still
    /// refused (see that flag's own doc in `cli.rs` for why it did not
    /// graduate too) -- this function keeps guarding exactly that one flag,
    /// so `tests/tui_model_pin.rs` can still drive the refusal
    /// synchronously, with no live `Conway` needed.
    pub fn session_spec(cli: &Cli) -> conway::Result<SessionSpec> {
        if cli.session.is_some() {
            return Err(crate::model_pin::usage_error(
                "--session is not supported when starting the interactive TUI; use one-shot \
                 mode (-p) for --session, or --resume/--fork-from/--continue for TUI session \
                 continuity",
            ));
        }
        let model = crate::model_pin::parse_model_pin(cli)?;
        Ok(SessionSpec {
            role: cli.role_override.clone().map(RoleAlias::new),
            // The TUI drives one `SessionHandle::prompt` call per chat
            // message on the same handle/session for the app's whole
            // lifetime (`App::submit`, below) -- without this, the root
            // agent's task terminates after the FIRST message's turn and
            // every later message silently runs no turn (the confirmed
            // keep-alive bug; see `SessionSpec::keep_alive`'s own doc).
            keep_alive: true,
            // The interactive root has no parent to `report` an
            // `AgentResult` to (: the
            // "pure and light" tool profile for interactive chat sessions)
            // -- excluding `report` makes the model answer plain chat
            // questions in text instead of hitting the permission gate for a
            // tool call nothing downstream ever unblocks. `conway_fork`/
            // `conway_spawn` and every other builtin tool stay available.
            tools: Some(ToolSelector::Except(vec!["report".into()])),
            model,
            ..SessionSpec::default()
        })
    }

    /// Board item `01M1YS4FMJH004D1Y619MTBY7A`: resolves the live
    /// [`SessionHandle`] [`Self::new`] drives -- the fresh path
    /// ([`Self::session_spec`] + `Conway::new_session`, unchanged), or one
    /// of the TUI's three now-supported continuity flags: `--resume`,
    /// `--fork-from`, `--continue`. Mirrors `oneshot::resolve_session`'s
    /// own shape (same up-front guards, same per-flag arms) deliberately --
    /// see that function's own doc comment for the reasoning each guard
    /// below restates -- but is its own, TUI-scoped function rather than a
    /// shared one: `oneshot::resolve_session` is private to `oneshot.rs`
    /// and wires flags (`--agent`, `--output-schema`, `--allowed-tools`,
    /// budgets) the TUI does not read even on the flag-free path, so a
    /// shared function would need its own TUI-vs-one-shot branching
    /// throughout rather than at the one seam that already exists (two
    /// call sites, one per binary mode).
    ///
    /// **Every error here is a usage error** (`ExitCode::Usage`), matching
    /// `oneshot::resolve_session`'s own documented contract: in every arm
    /// that returns `Err`, no interactive session has started, so there is
    /// no partially-open TUI to tear back down.
    pub async fn resolve_handle(cli: &Cli, conway: &Conway) -> conway::Result<SessionHandle> {
        if cli.session.is_some()
            && (cli.resume.is_some() || cli.fork_from.is_some() || cli.continue_session)
        {
            // Unreachable through the real CLI parser (`cli.rs`'s
            // `conflicts_with_all` quad already refuses this combination
            // before `Cli` is ever handed to this function) -- kept as an
            // explicit, named refusal anyway, matching `oneshot::
            // resolve_session`'s own identical defensive catch-all, rather
            // than trusting a hand-built `Cli` (as several of this
            // function's own tests build) to always respect a parser-level
            // invariant it never actually runs through.
            return Err(crate::model_pin::usage_error(
                "--session, --resume, --fork-from, and --continue are mutually exclusive",
            ));
        }

        let continuing = cli.resume.is_some() || cli.fork_from.is_some() || cli.continue_session;
        if continuing {
            if cli.system_prompt.is_some() || cli.append_system_prompt.is_some() {
                return Err(crate::model_pin::usage_error(
                    "--system-prompt/--append-system-prompt are not supported with \
                     --resume/--fork-from/--continue: a continued session's system prompt is \
                     fixed by the session it continues, not by this invocation",
                ));
            }
            if cli.max_turns.is_some() || cli.max_tokens.is_some() || cli.max_seconds.is_some() {
                return Err(crate::model_pin::usage_error(
                    "--max-turns/--max-tokens/--max-seconds are not supported with \
                     --resume/--fork-from/--continue in this release: neither facade path \
                     accepts a caller-supplied budget override yet",
                ));
            }
            if cli.output_schema.is_some() {
                return Err(crate::model_pin::usage_error(
                    "--output-schema is not supported with --resume/--fork-from/--continue in \
                     this release: neither facade path accepts a caller-supplied \
                     result-contract override yet",
                ));
            }
            // Unlike `oneshot::resolve_session` (where `--agent` genuinely
            // wires onto `--fork-from`), the TUI refuses `--agent`
            // regardless of which continuity flag it is paired with: the
            // TUI's own flag-free path does not read `--agent` at all
            // today (`Self::session_spec` never names `cli.agent`), so
            // wiring it onto exactly one of three new flags would be a
            // narrower, more confusing surface than refusing it uniformly
            // -- consistent with every OTHER TUI-unsupported one-shot flag
            // in this same guard block.
            if cli.agent.is_some() {
                return Err(crate::model_pin::usage_error(
                    "--agent is not supported with --resume/--fork-from/--continue: a \
                     continued session's agent definition is fixed by the session it continues",
                ));
            }
        }

        if let Some(id) = &cli.resume {
            let names =
                crate::session_names::NamesStore::load(&crate::session_names::session_root(conway))
                    .map_err(|e| crate::model_pin::usage_error(e.to_string()))?;
            let sid = crate::session_names::resolve(id, &names)
                .map_err(|e| crate::model_pin::usage_error(e.to_string()))?;
            return Self::resume_sid(cli, conway, sid).await;
        }

        if let Some(r) = &cli.fork_from {
            return Self::fork_from_ref(cli, conway, r).await;
        }

        if cli.continue_session {
            let sid = Self::most_recently_written_session(conway).await?;
            return Self::resume_sid(cli, conway, sid).await;
        }

        let spec = Self::session_spec(cli)?;
        conway.new_session(spec).await
    }

    /// The shared tail of `--resume <id|name>` and `--continue` (which
    /// resolves to a `SessionId` first, via
    /// [`Self::most_recently_written_session`], then reattaches through
    /// this exact same path) -- `--role-override`/`--model` wire here
    /// exactly as they do on `oneshot::resolve_session`'s own `--resume`
    /// arm (`Conway::resume_with`, not the bare `Conway::resume` neither
    /// flag could reach).
    async fn resume_sid(
        cli: &Cli,
        conway: &Conway,
        sid: SessionId,
    ) -> conway::Result<SessionHandle> {
        let role = cli.role_override.clone().map(RoleAlias::new);
        let model = crate::model_pin::parse_model_pin(cli)?;
        conway
            .resume_with(sid, role, model)
            .await
            .map_err(|e| crate::model_pin::usage_error(format!("resuming session {sid}: {e}")))
    }

    /// `--fork-from <id|name>[@seq]`: mirrors `oneshot::resolve_session`'s
    /// own `--fork-from` arm (same `--cwd` refusal, same seq-less ->
    /// current-head resolution via `Conway::session_head`), narrowed to
    /// what the TUI's flag-free path already supports: `--role-override`
    /// wires through; `--agent`/`--output-schema`/tool-selector flags do
    /// not (see `Self::resolve_handle`'s own guard block for `--agent`,
    /// and this crate's own one-shot-only stance on `--allowed-tools`/
    /// `--deny-tools`/`--permission-mode`, neither of which the TUI's
    /// flag-free path reads either).
    async fn fork_from_ref(cli: &Cli, conway: &Conway, r: &str) -> conway::Result<SessionHandle> {
        if cli.cwd.is_some() {
            return Err(crate::model_pin::usage_error(
                "--cwd is not supported with --fork-from: the forked child always inherits the \
                 parent session's cwd",
            ));
        }
        let names =
            crate::session_names::NamesStore::load(&crate::session_names::session_root(conway))
                .map_err(|e| crate::model_pin::usage_error(e.to_string()))?;
        let (parent, seq) = crate::session_names::resolve_fork_ref(r, &names)
            .map_err(|e| crate::model_pin::usage_error(e.to_string()))?;
        let at = match seq {
            Some(seq) => seq,
            None => conway
                .session_head(parent)
                .await
                .map_err(|e| crate::model_pin::usage_error(format!("--fork-from {parent}: {e}")))?,
        };
        let mut spec = ForkSpec::new(String::new());
        if let Some(role) = cli.role_override.clone().map(RoleAlias::new) {
            spec = spec.role(role);
        }
        // Same "pure and light" tool profile `Self::session_spec` gives
        // every fresh TUI session (see that field's own comment there) --
        // a forked child opened directly in the TUI is driven
        // interactively too, with no parent to `report` an `AgentResult`
        // to.
        spec = spec.tools(ToolSelector::Except(vec!["report".into()]));
        conway.fork_from(parent, at, spec).await.map_err(|e| {
            crate::model_pin::usage_error(format!("--fork-from {parent}@{}: {e}", at.0))
        })
    }

    /// `--continue`: the most recently WRITTEN-TO session for this
    /// project. Excludes ephemeral sessions (`Conway::sessions`'s own
    /// default) -- an `/ask` scratchpad is never a `--continue` target.
    async fn most_recently_written_session(conway: &Conway) -> conway::Result<SessionId> {
        let candidates = conway.sessions(SessionFilter::default()).await?;
        let root = crate::session_names::session_root(conway);
        Self::most_recently_written(&root, &candidates).ok_or_else(|| {
            crate::model_pin::usage_error(
                "--continue: no sessions found for this project (run `conway sessions list` \
                 to check, or start a fresh session without --continue)",
            )
        })
    }

    /// The pure half of [`Self::most_recently_written_session`]: no
    /// `Conway`, no async, no store call -- just a directory to stat files
    /// under and the candidate list already fetched. Lets `mod
    /// continue_tests` (below) drive the actual ranking decision against a
    /// plain tempdir, with no real session store or `CONWAY_CONFIG_DIR`
    /// involved at all -- the same hermetic-testing idiom `Self::
    /// resolve_default_mode` (further below in this file) already
    /// establishes for this module.
    ///
    /// Ranked by each candidate's own `<id>.jsonl` log file's mtime under
    /// `root` (`session_names::session_root`'s own doc: "the SAME `root`
    /// `ConwayBuilder::build` resolved `JsonlSessionStore::open` against",
    /// and `conway-session`'s own `<ulid>.jsonl` naming convention -- see
    /// `session_names.rs`'s module doc, "`root/<session-id>.jsonl`") --
    /// deliberately NOT `SessionMeta::created` (that session's BIRTH): an
    /// older session chatted in five minutes ago must outrank a brand-new,
    /// still-empty one, and `created` cannot tell the two apart. A
    /// candidate whose file cannot be stat'd (removed mid-race, or this
    /// convention drifting from the real store's own -- never expected in
    /// practice) is skipped rather than treated as infinitely old/new, so
    /// one bad candidate cannot silently win OR silently veto every other
    /// one.
    fn most_recently_written(
        root: &std::path::Path,
        candidates: &[SessionMeta],
    ) -> Option<SessionId> {
        candidates
            .iter()
            .filter_map(|meta| {
                let path = root.join(format!("{}.jsonl", meta.id));
                let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
                Some((meta.id, mtime))
            })
            .max_by_key(|(_, mtime)| *mtime)
            .map(|(id, _)| id)
    }

    /// Creates the interactive session. `plugins` is the SAME plugin list the caller
    /// installed into `conway` -- this is what lets [`commands::
    /// CommandRegistry::build`] resolve exactly the plugin commands that
    /// were actually installed this run (never a plugin merely LINKED into
    /// the binary but not selected via `[plugins].install`), without
    /// `App`/`conway-cli` reaching past `conway`/`conway-core`'s own
    /// layering to ask the already-built `Conway`/`Runtime` which plugins it
    /// holds (no such accessor exists, and building one is out of this
    /// item's file lane -- see this crate's `first_party_plugins::
    /// installed_plugins`, the one production caller, for how the caller
    /// resolves this list from the same `[plugins].install` config
    /// `install_selected` itself reads).
    pub async fn new(
        cli: &Cli,
        conway: &Conway,
        plugins: &[std::sync::Arc<dyn conway::plugin::Plugin>],
    ) -> conway::Result<Self> {
        let command_registry = std::sync::Arc::new(
            crate::tui::commands::CommandRegistry::build(plugins).map_err(|e| {
                conway::FacadeError::Config {
                    path: None,
                    message: e.to_string(),
                }
            })?,
        );
        // Board item `01M1YS4FMJH004D1Y619MTBY7A`: `resolve_handle` covers
        // the flag-free path (`Self::session_spec` + `Conway::new_session`,
        // unchanged) AND the three continuity flags (`--resume`,
        // `--fork-from`, `--continue`) in one place -- see its own doc.
        let handle = Self::resolve_handle(cli, conway).await?;
        let mut state = AppState::new(handle.root());
        // Board item `01M1YS4FMJH004D1Y619MTBY7A`: backfill this session's
        // own history into the transcript BEFORE any of the startup
        // notices below are pushed, so a resumed conversation's past reads
        // as the past and this run's own notices ("no first-party plugins
        // installed", a skipped permission rule, ...) still read as the
        // present, appended after it -- not interleaved.
        //
        // Unconditional, not gated on "was this a --resume/--fork-from/
        // --continue launch": `handle.transcript(handle.root())` is
        // ancestry-resolved (the SAME read `conway sessions show` already
        // performs) and empty for a genuinely fresh session (nothing has
        // been appended to it yet at this point in `new`), so backfilling
        // unconditionally is a no-op for the common case rather than a
        // second, parallel "is this a continuation" branch to keep in
        // sync with `resolve_handle`'s own four-way match. Best-effort: a
        // failed fetch (store I/O) leaves the transcript exactly as
        // `AppState::new` left it -- empty -- rather than failing the
        // whole session open over a display-only backfill.
        if let Ok(records) = handle.transcript(handle.root()).await {
            state
                .transcript
                .extend(crate::tui::state::backfill_entries(&records));
        }
        // the initial, authoritative
        // read of this session's own head -- see `AppState::
        // session_head_seq`'s own doc for why this is the FIRST of several
        // refresh points, not the only one. Best-effort: a failed fetch
        // just leaves the field `None` until the next successful refresh
        // (`Self::refresh_session_head`).
        state.session_head_seq = conway.session_head(handle.id()).await.ok();
        state.plugin_commands = std::sync::Arc::new(command_registry.palette_entries());
        // Board item `01M0XC1GF73Z9GTE7TN65TRW4A`: populate the status
        // line's `plugins` field from the one build-time snapshot
        // `Conway::plugin_status_contributions()` holds -- the same
        // "populate once, outside the render path" shape `plugin_commands`
        // just above and `agent_names` (`with_agent_names`, below) already
        // use. **This is a snapshot, not a live poll**: `conway.
        // plugin_status_contributions()` was collected once, in
        // `ConwayBuilder::build`, before this session's own `status/1`
        // notifications (if any) had arrived -- see that accessor's own
        // doc. Copying it here closes the "renders but nothing feeds it"
        // gap for a plugin that already had a contribution at build time;
        // it does NOT make a later, mid-session health change (a guard
        // dying, a build finishing) show up -- that is a genuinely live
        // per-session poll, a separate and larger piece, deliberately not
        // built here. See `AppState::plugin_status_contributions`'s own
        // doc for the same caveat spelled out at the read side.
        //
        // This one `App::new` copy is also the ONLY place the value is
        // ever produced -- `commands::execute`'s `Resume` arm (board item
        // `01M0XDEDBR5YDF71Q7ZRXYMT85`) carries the already-populated field
        // across a `/resume`'s `AppState::new` reset rather than re-reading
        // `conway.plugin_status_contributions()` a second time, matching
        // `plugin_commands`/`agent_names`'s own carry-across exactly: the
        // snapshot taken here is still the one an operator sees after any
        // number of `/resume`s in the same process.
        state.plugin_status_contributions = conway.plugin_status_contributions().to_vec();
        // Stage 2a: `[tui]` no longer lives in `conway::config::ConwayConfig`
        // at all (`conway.config()` has no `.tui` field any more) -- this
        // crate reads it back via its OWN separate, layered load, using the
        // SAME settings.json discovery/precedence/env sources `build_conway`
        // used to build `conway` itself. See `crate::tui::config`'s own doc
        // for why, and for the `#[serde(deny_unknown_fields)]` typo
        // protection this crate keeps for its own presentation schema even
        // though the facade no longer can.
        let tui_config = crate::tui::config::load(cli)?;
        // T1: build the theme once from the loaded `[tui.theme]` config
        // (defaults when the section is absent; malformed values fall back
        // to per-slot defaults -- untrusted input, never a panic). `Theme::from_config`
        // is infallible by construction.
        let theme = Theme::from_config(&tui_config.theme);
        // T3: status-line field order/visibility from `[tui.status_line]`
        // (defaults to the Lean line when absent; unknown field names are
        // dropped at render time -- untrusted input, never a panic).
        state.status_line_config = tui_config.status_line.clone();
        // T5: collapsed tool-preview line cap from
        // `[tui.tool_preview_lines]` (default 3). The config is untrusted
        // input -- `clamp_tool_preview_lines` clamps to `1..=200` and
        // falls back to the default of 3 on a missing/out-of-range value.
        // Never a panic, no `unwrap`/`expect`/indexing on the config value.
        state.tool_preview_lines =
            crate::tui::state::clamp_tool_preview_lines(tui_config.tool_preview_lines);
        // T8: input-history cap from `[tui.history_size]` (default 500,
        // clamped the same way as `tool_preview_lines` just above),
        // then load whatever history already exists on disk -- best-effort
        // (`history::load` degrades to an empty history on a missing,
        // unreadable, or corrupt file, never a panic/startup failure -- see
        // that function's own doc). `history_file_path` itself can return
        // `None` (no resolvable home directory); the session still runs
        // with in-memory-only history in that case.
        state.history_cap = crate::tui::state::clamp_history_size(tui_config.history_size);
        let history_path = conway::config::discovery::history_file_path(
            &std::env::vars().collect::<std::collections::HashMap<_, _>>(),
        );
        if let Some(path) = &history_path {
            state.history = crate::tui::history::load(path);
        }

        // Board item `01M1YVJ4RA5V7FF95MFRQMTQW3`: the rebindable keymap --
        // `state.keybindings` (already `Keymap::defaults()` from
        // `AppState::new`) is what `input::handle_key`'s dispatcher and
        // `/help`'s overlay both resolve against for the rest of the
        // session -- see that module's own "one table" doc. A malformed
        // `keybindings.json` is refused WHOLESALE (never a partial load,
        // and `state.keybindings` is left at its plain-defaults value) and
        // surfaced through the SAME `Entry::Error { fatal: false }` channel
        // the permission-file loader below uses -- a broken keymap should
        // cost a visible notice, never a silently-wrong binding table and
        // never a refusal to start. `keybindings_file_path` itself can
        // return `None` (no resolvable home directory, mirrors
        // `history_file_path` just above); a missing/unreadable file is NOT
        // an error either (`Keymap::load`'s own doc) -- only a file that
        // EXISTS but fails to parse reaches the `Err` arm below.
        let keybindings_path = crate::tui::keybindings::keybindings_file_path(
            &std::env::vars().collect::<std::collections::HashMap<_, _>>(),
        );
        if let Some(path) = &keybindings_path {
            match crate::tui::keybindings::Keymap::load(path) {
                Ok(keymap) => state.keybindings = keymap,
                Err(err) => {
                    state.transcript.push(crate::tui::state::Entry::Error {
                        text: err.to_string(),
                        fatal: false,
                    });
                }
            }
        }

        // V2b: load persisted permission rules from both scopes, project
        // first then global, and MERGE them.
        //
        // Merge rather than override: the two answer different questions.
        // A global rule is "I always allow this, everywhere" (`read:*`);
        // a project rule is "this checkout's build command is fine"
        // (`bash:cargo test`). Having the project file silently discard a
        // global grant would surprise an operator who set one deliberately,
        // and the union is still bounded by the metacharacter gate, which
        // applies to every rule regardless of where it came from.
        //
        // Every failure here is silent and narrowing: a missing file is
        // normal, and `parse_rules`/`parse_deny_rules` already fail closed
        // on a corrupt one (returning no rules rather than erroring).
        // Deliberately NOT surfaced as a startup error — a broken rules
        // file should cost extra prompting, never a refusal to start.
        //
        // `Conway::load_permission_files`
        // is the real production seam -- it decides trust (global files are
        // trusted by authorship; a project file's `allow` half installs
        // only with a matching recorded trust decision; its `deny` half
        // applies from ANY file, trusted or not) and is the SAME method
        // `crates/conway/tests/permission_trust_seam.rs` drives directly,
        // so this loader can never silently diverge from what that test
        // proves. See `conway::config::trust`'s own doc for the full
        // reasoning.
        let env_vars: std::collections::HashMap<String, String> = std::env::vars().collect();
        let root_agent = state.root_agent();
        let report = conway.load_permission_files(
            cli.cwd.as_deref().unwrap_or(&conway.config().cwd),
            &env_vars,
            // The interactive TUI's file-loaded rules are session-scoped:
            // they are the operator's own standing grants, not tied to one
            // agent of the tree. The scope parameter exists for embedders
            // loading a file on behalf of a single agent/subtree.
            conway::PermissionScope::Session,
            root_agent,
        );
        for notice in report.notices {
            state
                .transcript
                .push(crate::tui::state::Entry::Notice { text: notice });
        }
        // A rule that fails registration (today: `command_prefix`
        // against a Structured-render tool, a rule that can never match
        // reliably) is the silent-inert rule these typed errors exist to
        // flag -- producing the error and then dropping it would recreate
        // exactly that failure. Surface each as a transcript `Error` with
        // `fatal: false` (conway keeps running, but a rule the operator
        // wrote was refused), rendered through the existing `Entry::Error`
        // path so it renders in `theme.error` and cannot be skimmed past
        // as a routine cyan notice -- an error camouflaged as a notice is
        // the defect the `fatal: false` severity exists to close. Every
        // registration-error variant added later is operator-visible the
        // moment the loader produces it.
        for err in report.registration_errors {
            state.transcript.push(crate::tui::state::Entry::Error {
                text: format!(
                    "permission rule not installed: {} -- {}",
                    err.rule.describe(),
                    err.reason.describe()
                ),
                fatal: false,
            });
        }
        // a permissions file naming
        // an unrecognized top-level key (`"denys"` for `"deny"`) installed
        // NOTHING from that file -- allow, deny, AND prompt. Surfaced
        // through the SAME `Entry::Error { fatal: false }` channel
        // `report.registration_errors` uses just above, for the same
        // reason: a silently-unenforced `deny` rule is a security outcome,
        // not a routine notice, and must not be camouflaged as one.
        for err in report.parse_errors {
            state.transcript.push(crate::tui::state::Entry::Error {
                text: err,
                fatal: false,
            });
        }
        // `Conway::warnings()`
        // (currently only `WarningCode::HeadroomExceedsContext`, pushed by
        // `config::merge::validate` when a role's effective headroom is `>=`
        // the smallest context window reachable through its chain -- every
        // request routed to that model would be rejected outright by the
        // context-window gate) had zero callers workspace-wide before this.
        // `main.rs` prints the SAME warnings to stderr for every
        // non-interactive target, but a stray stderr write here would land on
        // top of the drawn UI once the terminal is in raw/alternate- screen
        // mode -- so the TUI's own surface is the transcript instead, exactly
        // the two-surfaces answer the
        // interactive-first-but-every-mode-reachable rule asks for. Rendered
        // through the SAME `Entry::Error { fatal: false }` channel
        // `report.registration_errors` uses just above, not `Entry::Notice`: a
        // misconfigured headroom is not a routine notice, it is a standing
        // routing failure for that role, and `fatal: false`'s whole purpose is
        // to keep exactly that kind of message from being camouflaged as
        // ordinary cyan chatter.
        for warning in conway.warnings() {
            state.transcript.push(crate::tui::state::Entry::Error {
                text: format!("config warning: {}", warning.message),
                fatal: false,
            });
        }
        // Board item 01M1YVP3FDPHY4WZ72SXMWAN2D: the mode THIS session
        // STARTS in -- see `resolve_default_mode`'s own doc for the
        // precedence/trust contract. `report.paths`' first entry is
        // always project scope (`crate::config::discovery::
        // permission_file_paths`'s own construction); its parent
        // directory is where that scope's `settings.json` lives too.
        let project_settings_path = report
            .paths
            .first()
            .and_then(|p| p.parent())
            .map(|dir| dir.join("settings.json"));
        let user_settings_path = conway::config::discovery::user_config_path(&env_vars);
        let (effective_default_mode, default_mode_notice) = Self::resolve_default_mode(
            cli.default_permission_mode,
            project_settings_path.as_deref(),
            report.project_permissions_trusted,
            user_settings_path.as_deref(),
            conway.config().permissions.default_mode,
        );
        if let Some(text) = default_mode_notice {
            state
                .transcript
                .push(crate::tui::state::Entry::Notice { text });
        }
        conway.set_permission_mode(effective_default_mode);
        state.default_permission_mode = effective_default_mode;
        state.permission_mode = conway.permission_mode();
        state.permission_paths = report.paths;
        // T3: cwd display -- prefer the CLI `--cwd` override, fall back to
        // the config's `cwd`. Both are `PathBuf`; render the display string
        // via `display()` (lossy for non-UTF8).
        state.cwd_display = cli
            .cwd
            .as_ref()
            .or(Some(&conway.config().cwd))
            .map(|p| p.display().to_string());
        // Board item `01M0WB5W5DX844HSJQG3JP23X0` (Q1): the SAME
        // `--cwd`-then-`conway.config().cwd` fallback just above, parked
        // as an owned `App` field rather than only a display string --
        // `App::apply_marketplace_install`/`apply_marketplace_uninstall`
        // need a real `Path` for their own project-config-layer honesty
        // check. See `App`'s own field doc for why this is resolved here,
        // once, rather than as a new `App::new` parameter or an ambient
        // read inside command dispatch.
        let cwd = cli
            .cwd
            .clone()
            .unwrap_or_else(|| conway.config().cwd.clone());
        // Board item 01M1ZJ796E0YP6Y8QWS8HB0AVB (context-window-provenance,
        // status-line half): the known `"backend/model"` key set still
        // comes from `Conway::model_metadata` (unchanged -- that map is
        // still the one place `models.json` names which pairs exist at
        // all), but the WINDOW NUMBER and its PROVENANCE now both come from
        // `Conway::capability_index()` -- the SAME resolved index `routes
        // explain`/the runway notice/the admission gate already read,
        // instead of `ModelMetadataEntry::max_context_tokens` read
        // directly. The superseded comment here used to invoke a "one load,
        // one source of truth" rule against re-reading `models.json` from
        // disk a second time -- that rule is still honored (this reads
        // `conway.capability_index()`, an in-memory accessor over the
        // SAME builder-time resolution, not a second file read); it never
        // named a reason THIS number had to come from `model_metadata`
        // specifically, only that it must not be re-parsed from disk on its
        // own. `capability_index().get(&model_ref)` has no entry for a pair
        // whose backend was never configured/injected -- that pair is
        // simply omitted here too, same as before (the renderer then falls
        // back to raw tokens, no percentage). `context_window_source`
        // alongside it is what lets the status line's `ctx` field mark an
        // `Unverified` (assumed-floor) window, the same "floor (assumed)"
        // wording `conway routes explain` already uses.
        let capability_index = conway.capability_index();
        let mut model_max_context = std::collections::HashMap::new();
        let mut model_max_context_source = std::collections::HashMap::new();
        for key in conway.model_metadata().models.keys() {
            let Ok(model_ref) = key.parse::<ModelRef>() else {
                continue;
            };
            if let Some(caps) = capability_index.get(&model_ref) {
                model_max_context.insert(key.clone(), caps.max_context_tokens);
            }
            if let Some(source) = capability_index.context_window_source(&model_ref) {
                model_max_context_source.insert(key.clone(), source);
            }
        }
        state.model_max_context = model_max_context;
        state.model_max_context_source = model_max_context_source;
        // T3: read the current git branch once at startup (best-effort,
        // no polling). On any failure (not a repo, git absent, non-UTF8
        // output) -> `None`, and the status line's `git` field is omitted.
        // Run on the blocking pool so it never stalls the async startup
        // path -- `git rev-parse` is fast, but the spawn isolates us from
        // a hung `git` or a slow filesystem.
        state.git_branch = read_git_branch().await;
        // The plugin browser's own read surface (board item
        // `01M0KARX71A64NTSYTDBVANVPF`): every compiled-in first-party
        // plugin candidate, not only the `plugins` param's already-
        // filtered, installed-only subset -- a browser must show what is
        // available-but-off too. Derived from `crate::first_party_plugins
        // ::all_bundle_plugins` directly (same crate, same single bundle
        // `installed_plugins` itself filters -- never a second,
        // independently-derived candidate list) rather than threaded in as
        // a new `App::new` parameter, which would have forced every one of
        // this crate's ~30 existing `App::new` call sites to name one.
        //
        // A throwaway `InMemoryMemoryStore` backs the `conway.memory`
        // candidate here, deliberately -- this scan only ever calls
        // `.manifest()`/`.description()` on each candidate, never a method
        // that touches the store, so opening the REAL durable store again
        // here would violate `first_party_plugins::resolve_memory_store`'s
        // own "exactly one `FsMemoryStore::open` call site" invariant for
        // no benefit. Mirrors that module's own fallback for an unselected
        // build ("unused, cheap, no I/O").
        let browse_memory_store: std::sync::Arc<dyn conway::plugin::MemoryStore> =
            std::sync::Arc::new(conway_plugin_memory::InMemoryMemoryStore::new());
        let install_ids = &conway.config().plugins.install;
        state.plugin_browser = crate::first_party_plugins::all_bundle_plugins(
            &conway.config().cwd,
            browse_memory_store,
            &env_vars,
        )
        .iter()
        .map(|p| {
            let manifest = p.manifest();
            crate::tui::state::PluginBrowserEntry {
                installed: install_ids.contains(&manifest.id),
                id: manifest.id,
                version: manifest.version,
                description: p.description(),
            }
        })
        .collect();
        // Board item `01M0VR5RCCB8NDGG2JEQW8X7XR`: the `/plugin` listing's
        // OTHER two sources -- read straight from config, never spawned
        // (`view/plugins.rs`'s own doc: "no candidate set to browse, so
        // nothing more than identity is available without a live
        // handshake this listing deliberately never performs"). Both
        // config Vecs already carry every field this listing needs (`id`,
        // `command`), so this is a field-by-field copy, not a lookup
        // against anything already resolved -- the ACTUAL subprocess/MCP
        // plugin objects `subprocess_plugins::install`/`mcp_plugins::
        // install` attached to `conway` earlier in this same startup path
        // are never consulted here.
        state.subprocess_plugins = conway
            .config()
            .plugins
            .subprocess
            .iter()
            .map(|entry| crate::tui::state::ConfiguredPluginEntry {
                id: entry.id.clone(),
                command: entry.command.clone(),
            })
            .collect();
        state.mcp_plugins = conway
            .config()
            .plugins
            .mcp
            .iter()
            .map(|entry| crate::tui::state::ConfiguredPluginEntry {
                id: entry.id.clone(),
                command: entry.command.clone(),
            })
            .collect();
        // Board item `01M0VR89FB1F3Q4FQ8852K2A5E`: the fourth `/plugin`
        // source. Unlike `subprocess_plugins`/`mcp_plugins` above (a
        // straight field copy out of already-loaded config), this one
        // re-runs `conway_plugin_claude::discover` against each
        // configured directory -- there is no live report to copy,
        // because `claude_compat_plugins::install` (earlier in this same
        // startup path) attaches translated MCP plugins to `conway`
        // WITHOUT keeping their own `ClaudeCompatReport` around
        // afterward. The re-run is cheap (local JSON/directory reads
        // only, no MCP handshake -- `discover` itself never spawns
        // anything) and, since `install` already succeeded against this
        // exact directory moments before, is not expected to fail here;
        // a failure at this point (the directory vanishing mid-startup)
        // degrades that one entry out of the listing with a `tracing::
        // warn!` rather than panicking the TUI over a display-only
        // re-read (P-10: never panic on untrusted/racy filesystem state).
        state.claude_compat_plugins = conway
            .config()
            .plugins
            .claude_compat
            .iter()
            .filter_map(|entry| match conway_plugin_claude::discover(&entry.dir) {
                Ok(report) => Some(crate::tui::state::ClaudeCompatPluginEntry {
                    id: entry.id.clone(),
                    source_dir: entry.dir.clone(),
                    mcp_server_count: report.mcp_servers.len(),
                    mapped_hook_count: report.mapped_hook_count(),
                    // Board item `01M0XRD8VMWD273W0W51T8ECCM`, acceptance 4:
                    // classifies against `conway::DENY_CAPABLE_EVENTS` --
                    // the SAME canonical set `claude_compat_plugins::
                    // report_hook_registrations` reads on stderr, not a
                    // third, independently-drifting list for this row.
                    deny_capable_hook_count: report
                        .hooks
                        .iter()
                        .filter(|h| match &h.outcome {
                            conway_plugin_claude::HookMapOutcome::Mapped {
                                conway_event, ..
                            } => conway::DENY_CAPABLE_EVENTS.contains(conway_event),
                            conway_plugin_claude::HookMapOutcome::Unmapped { .. } => false,
                        })
                        .count(),
                    unmapped_hook_names: report
                        .hooks
                        .iter()
                        .filter(|h| {
                            matches!(h.outcome, conway_plugin_claude::HookMapOutcome::Unmapped { .. })
                        })
                        .map(|h| h.claude_event.clone())
                        .collect(),
                    unsupported_names: report
                        .unsupported
                        .iter()
                        .map(|u| u.name.clone())
                        .collect(),
                }),
                Err(err) => {
                    tracing::warn!(
                        entry_id = %entry.id,
                        dir = %entry.dir.display(),
                        %err,
                        "re-reading a [plugins].claude_compat directory for the /plugin listing failed; \
                         omitting its row (the plugin itself, attached earlier in this same startup \
                         path, is unaffected)"
                    );
                    None
                }
            })
            .collect();
        let (modal_ask_tx, modal_ask_rx) = mpsc::unbounded_channel();
        let (plugin_cmd_tx, plugin_cmd_rx) = mpsc::unbounded_channel();
        let (provider_status_tx, provider_status_rx) = mpsc::unbounded_channel();
        let (await_tx, await_rx) = mpsc::unbounded_channel();
        Ok(Self {
            handle,
            state,
            conway: conway.clone(),
            theme,
            modal_ask_tx,
            modal_ask_rx: Some(modal_ask_rx),
            command_registry,
            plugin_cmd_tx,
            plugin_cmd_rx: Some(plugin_cmd_rx),
            await_tx,
            await_rx: Some(await_rx),
            provider_status_tx,
            provider_status_rx: Some(provider_status_rx),
            history_path,
            env: env_vars,
            cwd,
        })
    }

    /// Parks this process's `conway.names` store on the app's own state
    /// (board item `01M0TV5BSE98S16SFYECG9G9WP`) -- see
    /// `crate::tui::state::AppState::agent_names` for what reads it, and
    /// `crate::tui::run` (this method's ONE caller) for why the store
    /// arrives here as a post-construction setter rather than as another
    /// [`App::new`] parameter: `new` has ~40 call sites in this crate's own
    /// tests, none of which exercises naming, and every one of them would
    /// otherwise have to name a store it never touches.
    ///
    /// Consuming `self` rather than taking `&mut self` so the one caller
    /// can chain it onto `App::new`'s own `Ok` arm and never hold an `App`
    /// that is constructed but still unwired.
    pub fn with_agent_names(
        mut self,
        agent_names: std::sync::Arc<dyn conway_plugin_names::AgentNames>,
    ) -> Self {
        self.state.agent_names = Some(agent_names);
        self
    }

    /// Board item 01M1YVP3FDPHY4WZ72SXMWAN2D: resolves the mode a new
    /// session STARTS in, and the transcript notice (if any) explaining
    /// why a project-scope contribution was skipped. Returns `(mode,
    /// notice)`.
    ///
    /// **Precedence**: `cli_flag` (`--default-permission-mode`, one
    /// launch) > project `permissions.default_mode` (ONLY when
    /// `project_trusted`) > user `permissions.default_mode` >
    /// `merged_default_mode` (the config's own five-source-merged value,
    /// used verbatim whenever project scope did not itself set the key,
    /// or did and IS trusted -- in both cases the merge already resolved
    /// the correct precedence between user/project/env/CLI, so this
    /// function has nothing to add).
    ///
    /// **Fail-closed on trust, the item's own load-bearing line**: a
    /// PROJECT-scope `default_mode` (e.g. a cloned repo's own
    /// `.conway/settings.json` claiming `auto_allow`) must not take
    /// effect until the operator has run `/trust permissions` for THIS
    /// project. `project_trusted` is the caller's ONE input for that --
    /// `App::new` passes `PermissionLoadReport::project_permissions_
    /// trusted`, the IDENTICAL trust decision `permissions.json`'s own
    /// `allow` half already requires (reused, not duplicated -- see that
    /// field's own doc for why one trust subject, not two).
    ///
    /// **Pure and synchronous, unlike its one caller**: every filesystem
    /// fact this needs is an explicit parameter (`project_settings_path`/
    /// `user_settings_path`, already-resolved paths) rather than read from
    /// `std::env::vars()` internally -- the same hermetic-testing idiom
    /// `conway::config::merge::LoadOptions::env`/`App::apply_plugin_toggle`
    /// already establish workspace-wide (see that method's own module doc
    /// for why an in-process unit test must never itself mutate real
    /// process env: `crates/conway/tests/config_isolation_guard.rs` exists
    /// because that exact hazard already broke a test suite once). This is
    /// what lets `mod tests` below drive every precedence/trust branch
    /// directly against a tempdir, with no real `CONWAY_CONFIG_DIR`/
    /// `TrustStore` involved at all.
    fn resolve_default_mode(
        cli_flag: Option<crate::cli::TuiPermissionMode>,
        project_settings_path: Option<&std::path::Path>,
        project_trusted: bool,
        user_settings_path: Option<&std::path::Path>,
        merged_default_mode: conway::PermissionMode,
    ) -> (conway::PermissionMode, Option<String>) {
        if let Some(flag_mode) = cli_flag {
            return (flag_mode.into(), None);
        }
        let project_default_mode =
            project_settings_path.and_then(conway::config::schema::read_default_mode);
        if project_default_mode.is_some() && !project_trusted {
            // Ignore the project's contribution; fall back to whatever
            // user scope alone resolves to -- mirroring exactly how an
            // untrusted project permissions file's `allow` rules are
            // skipped with a notice rather than silently applied
            // (`App::new`'s own `report.notices` handling).
            let user_default_mode = user_settings_path
                .and_then(conway::config::schema::read_default_mode)
                .unwrap_or_default();
            let notice = format!(
                "{} sets permissions.default_mode, but this project is not trusted -- run \
                 `/trust permissions` to apply it; starting in {} mode instead",
                project_settings_path
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "the project's settings.json".to_string()),
                user_default_mode.label()
            );
            return (user_default_mode, Some(notice));
        }
        (merged_default_mode, None)
    }
}

#[cfg(test)]
mod resolve_default_mode_tests {
    use conway::PermissionMode;

    use super::App;
    use crate::cli::TuiPermissionMode;

    fn write_default_mode(dir: &std::path::Path, mode: &str) -> std::path::PathBuf {
        let path = dir.join("settings.json");
        std::fs::write(
            &path,
            format!(r#"{{ "permissions": {{ "default_mode": "{mode}" }} }}"#),
        )
        .expect("write settings.json");
        path
    }

    /// Untrusted project scope: `default_mode = "auto_allow"` must NOT
    /// take effect, and the session must fall back to user scope's own
    /// value -- not to `PermissionMode::default()` -- proving the
    /// fallback genuinely reads user scope rather than merely refusing
    /// the project's.
    ///
    /// ACCEPTANCE 2's own load-bearing check: a wrong implementation that
    /// ignores project scope UNCONDITIONALLY (never even looking at
    /// `project_trusted`) would also pass a bare "untrusted -> ignored"
    /// test -- this one is paired with `trusted_project_default_mode_
    /// applies_once_trusted` below (same fixture, only `project_trusted`
    /// flipped) specifically so that wrong implementation fails THAT one
    /// instead.
    #[test]
    fn untrusted_project_default_mode_is_ignored_with_a_notice() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project_path = write_default_mode(dir.path(), "auto_allow");
        let user_dir = tempfile::tempdir().expect("tempdir");
        let user_path = write_default_mode(user_dir.path(), "plan");

        let (mode, notice) = App::resolve_default_mode(
            None,
            Some(&project_path),
            false, // untrusted
            Some(&user_path),
            PermissionMode::Prompt, // what a real merge would have produced
        );

        assert_eq!(
            mode,
            PermissionMode::Plan,
            "must fall back to USER scope's own value, not a bare default"
        );
        let notice = notice.expect("an untrusted project override must produce a notice");
        assert!(
            notice.contains("not trusted"),
            "notice must explain why: {notice}"
        );
        assert!(
            notice.contains("/trust permissions"),
            "notice must name the remedy: {notice}"
        );
    }

    /// The trusted pairing of the test above: identical fixture, only
    /// `project_trusted` flipped to `true` -- and now the project's OWN
    /// `auto_allow` applies, with no notice. Proves the trust flag is
    /// genuinely load-bearing (not merely plumbed and ignored) and that
    /// project scope, once trusted, is not silently capped at whatever
    /// user scope says either.
    #[test]
    fn trusted_project_default_mode_applies_once_trusted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project_path = write_default_mode(dir.path(), "auto_allow");
        let user_dir = tempfile::tempdir().expect("tempdir");
        let user_path = write_default_mode(user_dir.path(), "plan");

        let (mode, notice) = App::resolve_default_mode(
            None,
            Some(&project_path),
            true, // trusted
            Some(&user_path),
            // A real five-source merge, once the project is trusted, would
            // already have resolved `default_mode` to the project's own
            // "auto_allow" (project outranks user) -- this stands in for
            // that already-merged value, exactly as `App::new`'s real
            // caller passes `conway.config().permissions.default_mode`.
            PermissionMode::AutoAllow,
        );

        assert_eq!(
            mode,
            PermissionMode::AutoAllow,
            "a TRUSTED project's own default_mode must apply"
        );
        assert!(
            notice.is_none(),
            "a trusted project's contribution must not print a notice: {notice:?}"
        );
    }

    /// No project-scope contribution at all (a project with no
    /// `settings.json`/no `default_mode` key): trust is irrelevant, and
    /// the merged value (here standing in for "user scope, or the
    /// built-in default") passes through unchanged, with no notice --
    /// proves this function does not manufacture a notice for a project
    /// that never touched the key.
    #[test]
    fn no_project_contribution_passes_the_merged_value_through_untouched() {
        let (mode, notice) = App::resolve_default_mode(
            None,
            None, // no project settings.json at all
            false,
            None,
            PermissionMode::Plan,
        );
        assert_eq!(mode, PermissionMode::Plan);
        assert!(notice.is_none());
    }

    /// `--default-permission-mode` outranks everything, including a
    /// TRUSTED project's own contradictory `default_mode` -- an explicit,
    /// one-launch operator choice is never second-guessed by a config
    /// file, trusted or not.
    #[test]
    fn cli_flag_outranks_a_trusted_project_default_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project_path = write_default_mode(dir.path(), "auto_allow");

        let (mode, notice) = App::resolve_default_mode(
            Some(TuiPermissionMode::Plan),
            Some(&project_path),
            true, // trusted -- would otherwise win with auto_allow
            None,
            PermissionMode::AutoAllow,
        );

        assert_eq!(mode, PermissionMode::Plan, "the CLI flag must win outright");
        assert!(notice.is_none());
    }
}

#[cfg(test)]
mod continue_tests {
    //! Board item `01M1YS4FMJH004D1Y619MTBY7A`: [`App::most_recently_
    //! written`]'s own hermetic tests -- a plain tempdir standing in for
    //! `session_names::session_root`, no `Conway`/session store involved at
    //! all (matching `resolve_default_mode_tests`'s own idiom, immediately
    //! above).

    use conway::{SessionId, SessionMeta};

    use super::App;

    /// A minimal, otherwise-irrelevant `SessionMeta` for a given id and
    /// `created` timestamp -- every OTHER field is a placeholder, since
    /// [`App::most_recently_written`] reads only `id` off this struct (the
    /// mtime comes from the file this fixture also writes, not from
    /// anything on `SessionMeta` itself).
    fn meta(id: SessionId, created: chrono::DateTime<chrono::Utc>) -> SessionMeta {
        SessionMeta {
            id,
            agent_id: conway::AgentId::new(),
            origin: None,
            agent_def: None,
            role: None,
            created,
            cwd: std::path::PathBuf::from("/tmp"),
            labels: Vec::new(),
            ephemeral: false,
            ask_origin: None,
            root: None,
            plugin_config: Default::default(),
        }
    }

    fn touch_with_mtime(dir: &std::path::Path, id: SessionId, mtime: std::time::SystemTime) {
        let path = dir.join(format!("{id}.jsonl"));
        std::fs::write(&path, b"").expect("write session file");
        let file = std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("reopen session file");
        file.set_modified(mtime).expect("set mtime");
    }

    /// **The load-bearing check**: ranks by the candidate's own file's
    /// mtime, NOT `SessionMeta::created` -- an older-created session
    /// written to MORE RECENTLY must win. `older_but_written_to` is given
    /// an EARLIER `created` than `newer_but_idle` (so a wrong
    /// implementation sorting by `created` picks `newer_but_idle`) but a
    /// LATER file mtime (so the correct answer is `older_but_written_to`).
    /// A wrong implementation that ranks by `SessionMeta::created` instead
    /// of file mtime fails this test outright, picking the wrong session.
    #[test]
    fn continue_picks_the_most_recently_written_session_not_the_most_recently_created() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = std::time::SystemTime::now();
        let an_hour_ago = now - std::time::Duration::from_secs(3600);

        let older_but_written_to = SessionId::new();
        let newer_but_idle = SessionId::new();

        touch_with_mtime(dir.path(), older_but_written_to, now);
        touch_with_mtime(dir.path(), newer_but_idle, an_hour_ago);

        let candidates = vec![
            meta(
                older_but_written_to,
                chrono::DateTime::<chrono::Utc>::from(an_hour_ago),
            ),
            meta(newer_but_idle, chrono::DateTime::<chrono::Utc>::from(now)),
        ];

        let got = App::most_recently_written(dir.path(), &candidates);

        assert_eq!(
            got,
            Some(older_but_written_to),
            "must rank by the file's own last-write time, not SessionMeta::created"
        );
    }

    /// No candidates at all (or none whose file can be stat'd) -> `None`,
    /// not a panic and not an arbitrary pick.
    #[test]
    fn no_candidates_yields_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(App::most_recently_written(dir.path(), &[]), None);
    }

    /// A candidate whose `.jsonl` file is missing (removed mid-race, or
    /// this convention drifting from the real store's own) is skipped
    /// rather than crashing the whole ranking -- the remaining, real
    /// candidate still wins.
    #[test]
    fn a_candidate_with_no_matching_file_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = std::time::SystemTime::now();

        let missing_file = SessionId::new();
        let real = SessionId::new();
        touch_with_mtime(dir.path(), real, now);

        let candidates = vec![
            meta(missing_file, chrono::DateTime::<chrono::Utc>::from(now)),
            meta(real, chrono::DateTime::<chrono::Utc>::from(now)),
        ];

        assert_eq!(
            App::most_recently_written(dir.path(), &candidates),
            Some(real)
        );
    }
}

/// T3: best-effort one-shot `git rev-parse --abbrev-ref HEAD` at startup,
/// returning the current branch name. `None` on any failure -- not a git
/// repo, `git` not on `PATH`, non-zero exit, non-UTF8 output, or a spawn
/// error. Never panics, never blocks startup on a hung `git`: the command
/// runs on the blocking pool and its output is bounded by `Command::output`
/// (which reads stdout into a buffer and waits for the child). No new
/// deps -- `std::process::Command` only.
async fn read_git_branch() -> Option<String> {
    tokio::task::spawn_blocking(|| {
        let output = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let branch = String::from_utf8(output.stdout).ok()?;
        let trimmed = branch.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    //! `App::new`'s own construction path: startup permission-loading,
    //! config-warning surfacing, and the initial `session_head_seq` fetch.

    use std::sync::Arc;

    use async_trait::async_trait;
    use conway::config::{CliOverrides, LoadOptions};
    use conway::plugin::{Plugin, PluginManifest, PluginStatusContribution, Tool};
    use conway::test_support::test_builder;
    use conway::{ConwayBuilder, PermissionGate, ResultStatus};
    use conway_core::agent::PermissionDecision;
    use conway_core::content::{
        ContentBlock, PermissionClass, StopReason, ToolCall, ToolCategory, ToolSpec,
        TruncationPolicy, Usage,
    };
    use conway_core::error::ToolError;
    use conway_core::ids::{BackendId, ModelId, ToolName};
    use conway_core::log::PermissionDecisionSource;
    use conway_core::ports::{GenerateResponse, ToolCtx, ToolOutput};
    use conway_testkit::{text_response, FakeBackend, FakeGate, FakeRouter, FakeStore};
    use conway_testkit::{ScriptedBackend, ScriptedTurn};
    use futures::StreamExt as _;

    use super::super::fixtures::{
        base_config, echo_conway, echo_conway_and_store, echo_conway_over, minimal_cli,
    };
    use super::App;
    use crate::tui::state::Entry;

    /// Loads `config_path` through the REAL `config::load` merge pipeline
    /// (default < project < env < CLI -- three sources, `include_user_config:
    /// No`), exactly what both this module's own callers below actually
    /// need to prove ("reached through `config::load`, not `from_parts`");
    /// neither cares about the operator's REAL `~/.conway/settings.json`
    /// content. **Deliberately not `ConwayBuilder::from_config`/
    /// `from_config_only`**: both always build their own `LoadOptions` via
    /// `LoadOptions::default()` -- this TEST PROCESS's real `std::env::
    /// vars()` and real `std::env::current_dir()` (this crate's manifest
    /// directory, not `cwd`) -- with no seam to inject an isolated `env`/
    /// `cwd` of this fixture's own. Before board item
    /// `01M0QK9GRM8HSNWRAR414TCX42`, that didn't matter here: every path
    /// either caller's assembled `ConwayConfig` needed (`agents.dir`,
    /// `models.metadata_path`) resolves at `build()` time, after `cwd` is
    /// already known. `[session].root`'s central-default resolution
    /// happens INSIDE `config::load` itself, using `LoadOptions.cwd`/`.env`
    /// directly -- against this test process's real ambient values, that
    /// would (harmlessly, since both callers inject a `FakeStore`, so
    /// `build()` never opens it -- but NOT harmlessly for the legacy-
    /// directory check `config::load` also runs, which depends on whatever
    /// this test process's REAL cwd's `.conway/sessions` happens to
    /// contain on the machine running it) resolve against real ambient
    /// state instead of this fixture's own isolated tempdir. Passing `cwd`/
    /// an isolated `env` explicitly here removes that dependency entirely.
    fn isolated_from_config(
        config_path: &std::path::Path,
        cwd: &std::path::Path,
    ) -> conway::Result<ConwayBuilder> {
        let mut env = std::collections::HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            cwd.to_string_lossy().into_owned(),
        );
        ConwayBuilder::from_options_ignoring_user_config(LoadOptions {
            cwd: cwd.to_path_buf(),
            explicit_path: Some(config_path.to_path_buf()),
            env,
            cli_overrides: CliOverrides {
                cwd: Some(cwd.to_path_buf()),
                ..Default::default()
            },
            model_metadata_refresh: false,
        })
    }

    /// `App::new`'s own initial
    /// `AppState::session_head_seq` fetch -- a fresh session's head is
    /// `LogSeq(0)`, read authoritatively via `Conway::session_head` rather
    /// than assumed. See `AppState::session_head_seq`'s own doc for why
    /// this is the FIRST of several refresh points, not the only one (the
    /// others live inside `Self::run`'s own `select!` loop, which -- like
    /// its pre-existing `refresh_focused_usage` sibling one field over --
    /// this crate's test suite does not drive end to end: no `TestBackend`-
    /// backed `run()` call exists here today).
    #[tokio::test]
    async fn app_new_populates_the_initial_session_head_seq() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        assert_eq!(app.state.session_head_seq, Some(conway::LogSeq(0)));
    }

    /// Board item `01M1YS4FMJH004D1Y619MTBY7A`, acceptance 1: `--resume`
    /// backfills a resumed session's REAL history into the transcript at
    /// TUI startup -- proven end to end through the real `App::new`, not
    /// just `backfill_entries` in isolation (`state/transcript.rs`'s own
    /// unit tests cover the mapping itself with hand-built records; this
    /// proves the WIRING: `resolve_handle` actually reaches it). Same "two
    /// `Conway`s sharing one `FakeStore`, simulated restart" shape
    /// `resuming_a_session_refreshes_its_own_head_seq` (`app.rs`) already
    /// establishes for `/resume`'s OWN in-TUI resumption -- this is that
    /// shape's twin for `--resume` at STARTUP instead. `App::new` once
    /// refused `--resume` outright (see `tests/tui_model_pin.rs`'s
    /// now-superseded `session_flags_are_rejected_by_app_new_not_silently_
    /// ignored`), so a passing case here could not have been written at
    /// all until that refusal became support.
    #[tokio::test]
    async fn resolve_handle_resume_backfills_history_into_the_transcript() {
        let (conway, store) = echo_conway_and_store();

        let other_sid = {
            let other_conway = echo_conway_over(store.clone());
            let other = other_conway
                .new_session(conway::SessionSpec::default())
                .await
                .expect("new_session should succeed");
            let sid = other.id();
            other
                .prompt("hello there")
                .await
                .expect("prompt should not error")
                .text()
                .await
                .expect("turn should complete");
            sid
            // `other_conway`/`other` drop here -- their in-memory
            // `Runtime`/tree go with them, leaving only the persisted log
            // in the SHARED `store` behind for `--resume` to read back.
        };

        let mut cli = minimal_cli();
        cli.resume = Some(other_sid.to_string());

        let app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new must accept --resume now, not refuse it");

        assert!(
            app.state
                .transcript
                .iter()
                .any(|e| matches!(e, Entry::User(text) if text == "hello there")),
            "the resumed session's own user turn must be backfilled into the transcript, got: \
             {:?}",
            app.state.transcript
        );
        // The fixture backend echoes the last user segment's text back
        // verbatim (`FakeBackend::echo`'s own doc) -- so the persisted
        // `Assistant` record's text is deterministically the same string,
        // letting this assert on exact content rather than mere presence.
        assert!(
            app.state
                .transcript
                .iter()
                .any(|e| matches!(e, Entry::Assistant { text, .. } if text == "hello there")),
            "the resumed session's own assistant reply must be backfilled too, got: {:?}",
            app.state.transcript
        );
    }

    /// Board item `01M0KARX71A64NTSYTDBVANVPF`: `App::new` populates
    /// `state.plugin_browser` from EVERY compiled-in first-party plugin
    /// candidate (`first_party_plugins::all_bundle_plugins`), not only
    /// the ones actually selected -- a fresh build's `[plugins].install`
    /// is empty (`base_config`'s own `PluginsConfig::default()`), so every
    /// candidate must appear with `installed: false` and a non-empty
    /// description, never silently absent from the browser just because
    /// nothing is on yet.
    #[tokio::test]
    async fn app_new_populates_the_plugin_browser_with_every_candidate_off_by_default() {
        let conway = echo_conway();
        let cli = minimal_cli();
        let app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        for expected_id in [
            conway_plugin_skeleton::PLUGIN_ID,
            conway_plugin_history::PLUGIN_ID,
            conway_plugin_stepguard::PLUGIN_ID,
            conway_plugin_skills::PLUGIN_ID,
            conway_plugin_memory::PLUGIN_ID,
            conway_plugin_path::PLUGIN_ID,
        ] {
            let entry = app
                .state
                .plugin_browser
                .iter()
                .find(|e| e.id == expected_id)
                .unwrap_or_else(|| {
                    panic!(
                        "{expected_id} missing from the browser: {:?}",
                        app.state.plugin_browser
                    )
                });
            assert!(
                !entry.installed,
                "{expected_id} must be OFF on a fresh install with no [plugins].install entry"
            );
            assert!(
                !entry.description.summary.is_empty(),
                "{expected_id} must carry a real description, not the trait's empty default"
            );
        }
    }

    /// The counterpart: a plugin named in `[plugins].install` shows up
    /// `installed: true` in the browser.
    #[tokio::test]
    async fn app_new_marks_a_configured_plugin_as_installed_in_the_browser() {
        let mut config = super::super::fixtures::base_config();
        config.plugins.install = vec![conway_plugin_memory::PLUGIN_ID.to_string()];
        let conway = super::super::fixtures::conway_over_config(config);
        let cli = minimal_cli();
        let app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let entry = app
            .state
            .plugin_browser
            .iter()
            .find(|e| e.id == conway_plugin_memory::PLUGIN_ID)
            .expect("conway.memory must be present in the browser");
        assert!(entry.installed);

        let off_entry = app
            .state
            .plugin_browser
            .iter()
            .find(|e| e.id == conway_plugin_skills::PLUGIN_ID)
            .expect("conway.skills must be present in the browser");
        assert!(
            !off_entry.installed,
            "an unselected plugin must stay OFF in the browser"
        );
    }

    /// A permission rule that fails registration is
    /// OPERATOR-VISIBLE at load time. The assertion is on the observable
    /// transcript/rendered screen -- what the operator actually reads --
    /// NOT on `report.registration_errors` (the field the producer writes;
    /// a unit test on that field is a liveness trap: it exercises the
    /// mapping, not the render path the operator actually reads).
    /// The fixture is a `command_prefix` rule against `read`
    /// (Structured render -- can never match reliably) written as a `deny`
    /// rule, because deny rules are validated and refused BEFORE any trust
    /// gating (deny applies from every file, trusted or not), so the test
    /// needs no recorded trust decision and no user config env isolation.
    #[tokio::test]
    async fn registration_error_surfaces_as_a_transcript_error() {
        let project = tempfile::TempDir::new().expect("tempdir");
        let conway_dir = project.path().join(".conway");
        std::fs::create_dir_all(&conway_dir).expect("mkdir .conway");
        // Pin project discovery to the tempdir (an empty `settings.json` is
        // all `discover` checks for) so no ancestor `.conway/` can
        // redirect the permissions-file path.
        std::fs::write(conway_dir.join("settings.json"), "").expect("write settings.json");
        std::fs::write(
            conway_dir.join("permissions.json"),
            r#"{"rules":[{"select":{"tools":["read"]},"when":{"command_prefix":"read"},"then":"deny"}]}"#,
        )
        .expect("write permissions.json");

        let conway = echo_conway();
        let mut cli = minimal_cli();
        cli.cwd = Some(project.path().to_path_buf());
        let app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let errors: Vec<&str> = app
            .state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Error { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let surfacing = errors.iter().find(|text| {
            text.contains("not installed")
                && text.contains("`read` commands starting with `read`")
                && text.contains("command_prefix")
        });
        assert!(
            surfacing.is_some(),
            "the refused rule must surface as a transcript Error carrying the rule and the \
             reason; transcript errors were: {errors:?}"
        );

        // Buffer-asserting half (this crate's binding TUI test convention):
        // render the REAL `AppState` through the REAL `view::draw` and
        // confirm the operator can actually READ the surfacing on screen.
        let text = crate::tui::test_support::render_text(&app.state, 120, 40);
        assert!(
            text.contains("not installed") && text.contains("command_prefix"),
            "the registration-error Error entry must render on screen: {text}"
        );
    }

    /// Proven end to end through the real
    /// startup loader -- the sibling of `registration_error_surfaces_as_a_
    /// transcript_error` just above, same shape: a real `.conway/
    /// permissions.json` on a real filesystem, loaded by the real
    /// `App::new`, asserted on the OBSERVABLE transcript AND rendered
    /// screen, not on `report.parse_errors` (the loader's own return value
    /// -- already covered by unit tests in `conway-core`/`conway`; a test
    /// that only re-checks that field would be the exact liveness trap
    /// `registration_error_surfaces_as_a_transcript_error`'s own doc
    /// describes). A misspelled `"denys"` key must surface loudly at
    /// startup, naming the offending key, through the same `Entry::Error`
    /// channel a registration error uses -- never merely logged or
    /// dropped.
    #[tokio::test]
    async fn unknown_permission_key_surfaces_as_a_transcript_error_at_startup() {
        let project = tempfile::TempDir::new().expect("tempdir");
        let conway_dir = project.path().join(".conway");
        std::fs::create_dir_all(&conway_dir).expect("mkdir .conway");
        // Pin project discovery to the tempdir, same as the registration-
        // error sibling above.
        std::fs::write(conway_dir.join("settings.json"), "").expect("write settings.json");
        std::fs::write(
            conway_dir.join("permissions.json"),
            r#"{"denys": ["bash:curl"]}"#,
        )
        .expect("write permissions.json");

        let conway = echo_conway();
        let mut cli = minimal_cli();
        cli.cwd = Some(project.path().to_path_buf());
        let app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let errors: Vec<&str> = app
            .state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Error { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let surfacing = errors
            .iter()
            .find(|text| text.contains("denys") && text.contains("was not loaded"));
        assert!(
            surfacing.is_some(),
            "the misspelled key must surface as a transcript Error naming it; \
             transcript errors were: {errors:?}"
        );

        // Buffer-asserting half (this crate's binding TUI test convention):
        // render the REAL `AppState` through the REAL `view::draw` and
        // confirm the operator can actually READ the surfacing on screen.
        let text = crate::tui::test_support::render_text(&app.state, 120, 40);
        assert!(
            text.contains("denys") && text.contains("was not loaded"),
            "the unknown-field Error entry must render on screen: {text}"
        );
    }

    /// `Conway::warnings()` (real,
    /// populated by `config::merge::validate` -- `WarningCode::
    /// HeadroomExceedsContext` when a role's effective headroom is `>=` the
    /// smallest context window reachable through its chain) had zero
    /// callers workspace-wide before this. `main.rs` deliberately does NOT
    /// print these to stderr for the TUI target (a stray write would land
    /// on top of the drawn UI once the terminal is in raw/alternate-screen
    /// mode); `App::new` is the TUI's own surface instead. This asserts the
    /// OBSERVABLE OUTCOME -- the rendered transcript TEXT `App::new`
    /// produces for a REAL misconfigured fixture -- not the return value of
    /// `conway.warnings()`, which is checked only as a sanity precondition
    /// below, never as a substitute (the standing warning against asserting
    /// the intermediate signal).
    ///
    /// Reached through the REAL `config::load` path
    /// (`ConwayBuilder::from_config` against a fixture written to a real
    /// temp dir), not `from_parts`/`base_config` -- `Conway::warnings()`'s
    /// own doc: "Empty when this `Conway` was built via
    /// `ConwayBuilder::from_parts`, which bypasses `load` entirely", so
    /// this is the one test in this module that cannot reuse
    /// `echo_conway`. `models.metadata_path` is written
    /// as an ABSOLUTE path so its resolution never depends on this test
    /// process's current directory (`config::merge::resolve_metadata_path`
    /// only joins a RELATIVE path onto the load's own `cwd`). The
    /// `backends.fake` entry's placeholder `api_key` is never dialed --
    /// `with_backend` below overwrites it, same id, last insert wins
    /// (`ConwayBuilder::build`'s own step 3+4), the identical pattern
    /// `tests/routes_explain_injected_router.rs` already uses.
    #[tokio::test]
    async fn misconfigured_headroom_lands_in_the_tui_transcript_at_startup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let models_path = dir.path().join("models.json");
        std::fs::write(
            &models_path,
            serde_json::json!({
                "models": {
                    "fake/echo-model": {
                        "max_context_tokens": 32_768,
                        "tool_calling": "streaming",
                        "reasoning": false,
                        "reliability_tier": "verified",
                    }
                }
            })
            .to_string(),
        )
        .expect("write models.json");

        let config_path = dir.path().join("conway.json");
        std::fs::write(
            &config_path,
            serde_json::json!({
                "default_role": "coder",
                "roles": {
                    "coder": { "chain": ["fake/echo-model"], "headroom_tokens": 200_000 }
                },
                "backends": {
                    "fake": { "kind": "anthropic", "api_key": "unused-placeholder-key" }
                },
                "models": { "metadata_path": models_path }
            })
            .to_string(),
        )
        .expect("write conway.json");

        let backend: Arc<dyn conway::Backend> = Arc::new(FakeBackend::echo(BackendId::new("fake")));
        let gate: Arc<dyn PermissionGate> = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
        let router: Arc<dyn conway::Router> = Arc::new(FakeRouter::single(conway::ModelRef {
            backend: BackendId::new("fake"),
            model: ModelId::new("echo-model"),
        }));
        let conway = isolated_from_config(&config_path, dir.path())
            .expect("from_config should load the fixture and compute its headroom warning")
            .with_backend(backend)
            .with_session_store(Arc::new(FakeStore::new()))
            .with_permission_gate(gate)
            .with_router(router)
            // `conway` no longer
            // compiles either dialect in -- `from_config` also layers in
            // whatever the live user config-global `settings.json` declares (this
            // function's own doc), so both factories are registered here,
            // matching the real binary's own always-both default.
            .with_backend_factory(Arc::new(conway_plugin_backends::AnthropicBackendFactory))
            .with_backend_factory(Arc::new(conway_plugin_backends::OpenAiCompatBackendFactory))
            .build()
            .expect("build should succeed with every I/O port injected");

        // Sanity precondition, not the assertion this test exists for (see
        // this test's own doc).
        assert_eq!(conway.warnings().len(), 1);

        let mut cli = minimal_cli();
        cli.config = Some(config_path);
        let app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        let rendered: Vec<&str> = app
            .state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Error { text, fatal: false } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            rendered.iter().any(|text| {
                text.contains("headroom for role 'coder'") && text.contains("200000")
            }),
            "expected the transcript to carry the headroom warning as a non-fatal \
             error entry, got: {rendered:?}"
        );
    }

    /// **Board item 01KZVYYWZ85D1SYMCSRRZ7RAM3, verification anchor,
    /// second half.** `crates/conway/tests/architecture_invariants.rs`'s
    /// `t7_facade_has_no_presentation_types` proves the first half (no
    /// ratatui-shaped type is reachable from `conway`'s config schema at
    /// all) -- proving that alone would pass even if the four types had
    /// simply been DELETED rather than moved, so it is paired here with a
    /// real end-to-end CLI run: a real `settings.json`, on disk, carrying a
    /// FULL `[tui.theme]` block plus a custom `[tui.status_line]`, driven
    /// through `ConwayBuilder::from_config` (unchanged: it still succeeds
    /// against a config with a `[tui]` block, since `config::load` strips
    /// and warns rather than hard-erroring) and then `App::new` (this
    /// crate's own separate `crate::tui::config::load`), reaching a real,
    /// rendered TUI session -- not a unit test of either parser in
    /// isolation.
    #[tokio::test]
    async fn a_real_settings_json_with_a_full_theme_block_reaches_a_rendered_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("conway.json");
        std::fs::write(
            &config_path,
            serde_json::json!({
                "default_role": "coder",
                "roles": { "coder": { "chain": [] } },
                "backends": {
                    "fake": { "kind": "anthropic", "api_key": "unused-placeholder-key" }
                },
                "tui": {
                    "theme": {
                        "user": { "fg": "magenta", "modifiers": ["bold", "italic"] }
                    },
                    "status_line": { "fields": ["session", "hint"] }
                }
            })
            .to_string(),
        )
        .expect("write conway.json carrying a full [tui] block");

        let backend: Arc<dyn conway::Backend> = Arc::new(FakeBackend::echo(BackendId::new("fake")));
        let gate: Arc<dyn PermissionGate> = Arc::new(FakeGate::new(PermissionDecision::AllowOnce));
        let router: Arc<dyn conway::Router> = Arc::new(FakeRouter::single(conway::ModelRef {
            backend: BackendId::new("fake"),
            model: ModelId::new("echo-model"),
        }));
        // The facade half: a settings.json with a real [tui] block must
        // still load successfully through the unmodified builder entry
        // point every dispatch target shares -- this would hard-fail here
        // if [tui] were still handed to ConwayConfig's
        // #[serde(deny_unknown_fields)] deserialize unstripped.
        let conway = isolated_from_config(&config_path, dir.path())
            .expect("a settings.json with a [tui] block must still load through the facade")
            .with_backend(backend)
            .with_session_store(Arc::new(FakeStore::new()))
            .with_permission_gate(gate)
            .with_router(router)
            .with_backend_factory(Arc::new(conway_plugin_backends::AnthropicBackendFactory))
            .with_backend_factory(Arc::new(conway_plugin_backends::OpenAiCompatBackendFactory))
            .build()
            .expect("build should succeed with every I/O port injected");

        let mut cli = minimal_cli();
        cli.config = Some(config_path);
        // The CLI half: this crate's OWN separate load of [tui] (`crate::
        // tui::config::load`, called inside `App::new`) must actually wire
        // the configured theme and status line into a real, live App.
        let app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new must succeed against a real settings.json carrying [tui]");

        assert_eq!(
            app.theme.user,
            ratatui::style::Style::default()
                .fg(ratatui::style::Color::Magenta)
                .add_modifier(ratatui::style::Modifier::BOLD | ratatui::style::Modifier::ITALIC),
            "the configured [tui.theme.user] override must reach the built Theme, not just \
             parse: got {:?}",
            app.theme.user
        );
        assert_eq!(
            app.state.status_line_config.fields,
            vec!["session".to_string(), "hint".to_string()],
            "the configured [tui.status_line.fields] must reach AppState"
        );

        // "Reaching a rendered session": render the REAL AppState through
        // the REAL view::draw (this crate's own binding TUI test
        // convention, also used by the headroom test above) and confirm
        // the session is actually live and renders something an operator
        // would see, not an inert struct nobody drew.
        let text = crate::tui::test_support::render_text(&app.state, 120, 40);
        assert!(
            !text.trim().is_empty(),
            "a session built from a config carrying a full [tui] block must still render"
        );
    }

    /// A plugin with no `status_contributions()` override (every fixture
    /// in this module up to here, and every first-party plugin `App::new`
    /// otherwise installs) contributes nothing -- the trait's own
    /// zero-cost default. Only a plugin that overrides it produces a
    /// contribution, which is why this fixture exists as its own type
    /// rather than reusing `install_selected.rs::FakePlugin`-shaped
    /// zero-dependency plugins already scattered across this crate's test
    /// suites: none of them override this one method.
    struct ContributingPlugin;

    impl Plugin for ContributingPlugin {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                id: "test.guard".to_string(),
                version: "0.0.0".to_string(),
                tools: vec![],
                required_host_caps: vec![],
                optional_host_caps: vec![],
                requires: vec![],
                optional: vec![],
            }
        }

        fn tools(&self) -> Vec<Arc<dyn Tool>> {
            vec![]
        }

        fn status_contributions(&self) -> Vec<PluginStatusContribution> {
            vec![PluginStatusContribution {
                key: "guard".to_string(),
                status: ResultStatus::Completed,
                value: "qwen2.5-3b".to_string(),
            }]
        }
    }

    /// Implemented by board item `01M0XC1GF73Z9GTE7TN65TRW4A`. The render
    /// path (`view::status::status_line_spans`'s `plugins` field,
    /// `view/status.rs`'s own `a_plugin_contribution_appears_in_the_status_
    /// line`) was already real and tested -- what it lacked was
    /// live data: `AppState::plugin_status_contributions` was set only by
    /// hand, in tests, never by `App::new` from a running `Conway`. This
    /// proves the missing link, end to end: a plugin installed through the
    /// REAL `ConwayBuilder::with_plugin` (never `AppState` set directly)
    /// whose `Plugin::status_contributions()` returns one contribution
    /// reaches BOTH `app.state.plugin_status_contributions` AND the
    /// actually rendered status line through the real `App::new` +
    /// `view::draw` -- the same "assert on the observable, rendered
    /// outcome, not the intermediate field" idiom every other startup test
    /// in this module already uses (see
    /// `registration_error_surfaces_as_a_transcript_error`'s own doc).
    ///
    /// **Also proves this is a snapshot, not a live poll.** `App::new`
    /// copies `conway.plugin_status_contributions()` -- itself the
    /// build-time value `ConwayBuilder::build` collected from
    /// `ContributingPlugin::status_contributions()` at `test_builder(..)
    /// .build()` time above, BEFORE any session exists -- so a value
    /// reaching the screen here says nothing about a value pushed by a
    /// `status/1` notification during a live turn (`Conway::
    /// plugin_status_contributions()`'s own doc; this crate's test suite
    /// has no harness for driving that wire path at all).
    #[tokio::test]
    async fn app_new_populates_plugin_status_contributions_from_a_real_plugin() {
        let conway = test_builder(base_config())
            .with_backend(Arc::new(FakeBackend::echo(BackendId::new("fake"))))
            .with_plugin(Arc::new(ContributingPlugin))
            .build()
            .expect("build should succeed with a status-contributing plugin installed");
        let mut cli = minimal_cli();
        // `plugins` is NOT in `StatusLineConfig::default`'s Lean line
        // (`session,lineage,mode,model,ctx,tokens,activity,hint`) -- an
        // operator has to opt in, matching every other status-line field
        // this module's own render tests configure explicitly. `cli.config`
        // drives `crate::tui::config::load` (`App::new`'s OWN separate
        // `[tui]` load, entirely independent of the `test_builder`-built
        // `conway` above, which never reads this file at all), so this
        // settings.json need only carry the one key this test cares about.
        let tui_config_dir = tempfile::tempdir().expect("tempdir");
        let tui_config_path = tui_config_dir.path().join("settings.json");
        std::fs::write(
            &tui_config_path,
            serde_json::json!({"tui": {"status_line": {"fields": ["plugins"]}}}).to_string(),
        )
        .expect("write settings.json carrying [tui.status_line.fields]");
        cli.config = Some(tui_config_path);
        let app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");

        assert_eq!(
            app.state.plugin_status_contributions,
            vec![PluginStatusContribution {
                key: "guard".to_string(),
                status: ResultStatus::Completed,
                value: "qwen2.5-3b".to_string(),
            }],
            "App::new must copy Conway::plugin_status_contributions() into AppState, not \
             leave it at AppState::new's empty default"
        );

        // Buffer-asserting half (this crate's binding TUI test convention,
        // used by every other startup test in this module): render the
        // REAL AppState through the REAL view::draw and confirm the
        // contribution is actually READABLE on screen, not merely present
        // on the struct -- the exact "renders but nothing feeds it" defect
        // this test exists to catch.
        let text = crate::tui::test_support::render_text(&app.state, 120, 40);
        assert!(
            text.contains("guard: qwen2.5-3b"),
            "the plugin's status contribution must reach the rendered status line: {text}"
        );
    }

    /// An Execute-category tool -- Plan mode denies this category
    /// outright, regardless of what the injected `PermissionGate` would
    /// have answered. Mirrors `app::ask::tests::MarkerTool`'s shape
    /// (a separate module's private fixture, not reusable from here),
    /// `ToolCategory::Execute` instead of `Read`.
    struct ExecuteMarkerTool;

    #[async_trait]
    impl Tool for ExecuteMarkerTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: ToolName::new("marker_exec"),
                description: "test-only Execute-category marker tool".into(),
                schema: serde_json::from_value(serde_json::json!({"type": "object"})).unwrap(),
                category: ToolCategory::Execute,
                permission: PermissionClass::Safe,
            }
        }

        async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput {
                blocks: vec![ContentBlock::Text {
                    text: "should never run under plan mode".into(),
                }],
                is_error: false,
                truncation: TruncationPolicy::None,
                artifacts: vec![],
            })
        }
    }

    struct ExecuteMarkerPlugin;

    impl Plugin for ExecuteMarkerPlugin {
        fn manifest(&self) -> PluginManifest {
            PluginManifest {
                id: "test.marker_exec".to_string(),
                version: "0.0.0".to_string(),
                tools: vec![ToolName::new("marker_exec")],
                required_host_caps: vec![],
                optional_host_caps: vec![],
                requires: vec![],
                optional: vec![],
            }
        }

        fn tools(&self) -> Vec<Arc<dyn Tool>> {
            vec![Arc::new(ExecuteMarkerTool)]
        }
    }

    fn tool_call_response(call_id: &str, tool: &str) -> GenerateResponse {
        GenerateResponse {
            content: vec![],
            tool_calls: vec![ToolCall {
                call_id: call_id.to_string(),
                name: ToolName::new(tool),
                arguments: serde_json::json!({}),
            }],
            stop: StopReason::ToolUse,
            usage: Usage::default(),
        }
    }

    /// ACCEPTANCE (the project rule that "a check is not established until it
    /// has been shown to fail" line, half (a)): `permissions.default_mode
    /// = "plan"` does not merely PARSE -- it actually GATES a first real
    /// tool call, the identical way `/settings` cycling into `Plan`
    /// mid-session already does.
    ///
    /// **The gate injected here is `test_builder`'s own default:
    /// always-allow (`allow_once_gate()`), deliberately NOT overridden.**
    /// If `default_mode` failed to reach `PermissionBroker::set_mode` at
    /// all (a config field that parses and does nothing -- precisely the
    /// defect this whole item exists to close), the always-allow gate
    /// would let this Execute-category call straight through and the
    /// tool's own "should never run" text would appear in the turn's
    /// reply. A pass here can only be explained by the mode itself having
    /// denied the call BEFORE the gate was ever consulted --
    /// `conway_runtime::permission::PermissionBroker::decide`'s own
    /// `PermissionDecisionSource::Mode` (see that type's own doc: "Plan
    /// refusing a category it does not permit") is the one event source
    /// that can ONLY mean the mode decided, never the gate.
    #[tokio::test]
    async fn config_default_mode_plan_gates_a_real_execute_tool_call() {
        let mut config = base_config();
        config.permissions.default_mode = conway::PermissionMode::Plan;

        let backend = Arc::new(
            ScriptedBackend::new(vec![
                ScriptedTurn::Respond(tool_call_response("call_1", "marker_exec")),
                ScriptedTurn::Respond(text_response("done")),
            ])
            .with_id(BackendId::new("fake")),
        );
        let conway = test_builder(config)
            .with_backend(backend)
            .with_plugin(Arc::new(ExecuteMarkerPlugin))
            .build()
            .expect("build should succeed with every port injected");

        // Exercises the code under test: `App::new` must read
        // `config.permissions.default_mode` and apply it via
        // `Conway::set_permission_mode` before returning.
        let cli = minimal_cli();
        let app = App::new(&cli, &conway, &[])
            .await
            .expect("App::new should succeed");
        assert_eq!(
            app.state.permission_mode,
            conway::PermissionMode::Plan,
            "App::new must have applied config.permissions.default_mode before the first turn"
        );

        // Now drive an actual turn, straight against `conway` (the SAME
        // instance `App::new` just configured the mode on) -- the mode
        // lives on the broker, shared by the whole `Conway`/`Runtime`, not
        // scoped to whichever session issues the call.
        let session = conway
            .new_session(conway::SessionSpec::default())
            .await
            .expect("new_session should succeed");
        let mut events = session.events();
        let _turn = session
            .prompt("please use marker_exec")
            .await
            .expect("prompt should be accepted");

        // Bounded so a genuine hang (a scripting mistake, not the property
        // under test) fails legibly rather than blocking the suite --
        // mirrors `app::ask::tests::HANG_TIMEOUT`'s own reasoning, one
        // module over.
        let mut saw_mode_denial = false;
        let drain = async {
            while let Some(envelope) = events.next().await {
                if let conway::Event::PermissionDecision { source, tool, .. } = &envelope.event {
                    if *tool == ToolName::new("marker_exec") {
                        assert_eq!(
                            *source,
                            PermissionDecisionSource::Mode,
                            "the Execute call must be resolved by MODE, never by reaching the \
                             always-allow gate (source: Operator would mean plan mode was \
                             never consulted at all)"
                        );
                        saw_mode_denial = true;
                    }
                }
                if let conway::Event::AgentFinished { result, .. } = &envelope.event {
                    if result.agent_id == session.root() {
                        break;
                    }
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(30), drain)
            .await
            .expect(
                "turn must finish within the bound (a hang here is a scripting defect, \
                     not the property under test)",
            );
        assert!(
            saw_mode_denial,
            "expected a PermissionDecision event for the marker_exec call, resolved by mode"
        );
    }
}
