//! `App::session_spec`/`App::new` -- the interactive session's construction
//! path, extracted out of `app.rs` (this item, board) verbatim. `run`, the
//! dispatch loop `new` hands off to, lives in [`super::run`]; the four
//! pre-parser slash-command interceptions stay in `app.rs` itself (T9's own
//! guard, `crates/conway/tests/architecture_invariants.rs`, greps that exact
//! file's source text for them).

use conway::{Conway, RoleAlias, SessionSpec, ToolSelector};
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
    /// `--session`/`--resume`/`--fork-from` (the caller's session-continuity
    /// flags) are a decided non-goal for the TUI, not an oversight: one-
    /// shot's `resolve_session` has real per-flag logic with no equivalent
    /// shape here (an existence probe ahead of `--session`, `--cwd`
    /// rejected alongside `--fork-from`, a local-head lookup for a seq-less
    /// fork ref) -- building a second, TUI-flavored version of that logic is
    /// out of this item's scope. Rather than leave the three flags
    /// accepted-and-ignored (the exact defect this item exists to close for
    /// `--model`), the TUI refuses to start when any of them is passed, with
    /// a usage error naming both alternatives: one-shot mode for startup
    /// continuity, or the already-wired `/resume <session-id>` slash
    /// command once the TUI is running. `docs/interactive.md` documents
    /// this as one-shot-only accordingly.
    pub fn session_spec(cli: &Cli) -> conway::Result<SessionSpec> {
        if cli.session.is_some() || cli.resume.is_some() || cli.fork_from.is_some() {
            return Err(crate::model_pin::usage_error(
                "--session/--resume/--fork-from are not supported when starting the \
                 interactive TUI; use one-shot mode (-p) for session continuity at startup, \
                 or the `/resume <session-id>` slash command once the TUI is running",
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
        let spec = Self::session_spec(cli)?;
        let handle = conway.new_session(spec).await?;
        let mut state = AppState::new(handle.root());
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
        // T3 follow-up: read the local model-metadata map
        // (`[models.metadata_path]`) from `Conway::model_metadata` --
        // `ConwayBuilder::build` already loaded and parsed this file once to
        // construct the `CapabilityIndex`; this used to re-read and
        // re-parse the SAME file itself, a second code path that agreed
        // with the builder's only because both happen to implement the
        // identical "missing file -> empty map" fallback. One load, one
        // source of truth: the status line's `ctx%` field looks up the
        // focused model's max context window by `"backend/model"` string
        // from whatever `model_max_context` ends up holding here. Empty
        // when the builder found no metadata (or it named no models) --
        // the renderer then falls back to raw tokens (no percentage), same
        // as before; never an error, never blocks startup.
        state.model_max_context = conway
            .model_metadata()
            .models
            .iter()
            .map(|(k, v)| (k.clone(), v.max_context_tokens))
            .collect();
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
    /// that has been constructed but not yet wired.
    pub fn with_agent_names(
        mut self,
        agent_names: std::sync::Arc<dyn conway_plugin_names::AgentNames>,
    ) -> Self {
        self.state.agent_names = Some(agent_names);
        self
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

    use conway::config::{CliOverrides, LoadOptions};
    use conway::plugin::{Plugin, PluginManifest, PluginStatusContribution, Tool};
    use conway::test_support::test_builder;
    use conway::{ConwayBuilder, PermissionGate, ResultStatus};
    use conway_core::agent::PermissionDecision;
    use conway_core::ids::{BackendId, ModelId};
    use conway_testkit::{FakeBackend, FakeGate, FakeRouter, FakeStore};

    use super::super::fixtures::{base_config, echo_conway, minimal_cli};
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

    /// A1: a permission rule that fails registration is
    /// OPERATOR-VISIBLE at load time. The assertion is on the observable
    /// transcript/rendered screen -- what the operator actually reads --
    /// NOT on `report.registration_errors` (the field the producer writes;
    /// a unit test on that field is the liveness trap this item exists to
    /// close). The fixture is a `command_prefix` rule against `read`
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
                "permissions": { "mode": "deny" },
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
                "permissions": { "mode": "deny" },
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

    /// Board item `01M0XC1GF73Z9GTE7TN65TRW4A`. The render path
    /// (`view::status::status_line_spans`'s `plugins` field,
    /// `view/status.rs`'s own `a_plugin_contribution_appears_in_the_status_
    /// line`) was real and tested before this item -- what it lacked was
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
        // this item exists to close.
        let text = crate::tui::test_support::render_text(&app.state, 120, 40);
        assert!(
            text.contains("guard: qwen2.5-3b"),
            "the plugin's status contribution must reach the rendered status line: {text}"
        );
    }
}
