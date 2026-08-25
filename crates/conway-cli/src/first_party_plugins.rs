//! The first-party plugin tier's install mechanism for the CLI binary
//!: every first-party plugin crate
//! this BINARY happens to link, resolved against `[plugins].install`
//! (`conway::config::schema::PluginsConfig`) before `ConwayBuilder::build`.
//!
//! `conway` (the facade) does not, and must never, depend on any of these
//! crates -- see that field's own doc for why. This module is the one
//! place a first-party plugin crate IS linked into a shipped binary: behind
//! this file, never inside the facade itself. A library embedder wanting
//! one of these plugins depends on its crate directly and calls
//! `ConwayBuilder::with_plugin`, exactly as this module does internally --
//! `conway-plugin-skeleton`'s own `tests/skeleton_end_to_end.rs` is that
//! embedder-shaped usage, written against the identical crate this module
//! links.
//!
//! **This bundle is a worked example, not a commitment to any of its
//! members individually.** Today it contains ten plugin entries --
//! `conway-plugin-skeleton`, a skeleton proving nothing beyond the install
//! mechanism (see that crate's own module doc); `conway-plugin-history`,
//! `/conway.history.rewind`/`/conway.history.mask`/`/conway.history.checkout`
//! -- so `/checkout` and a reachable `ContextMask` are built too, not only
//! `/rewind` (see that crate's own module doc); `conway-plugin-stepguard`,
//! repeated-tool-call detection moved out of the agent loop; `conway-plugin-
//! skills`, progressive skill disclosure; `conway-plugin-memory`, a mutable
//! `MemoryStore`-backed context hook; `conway-plugin-path`, the
//! `compose_context_path` tool; `conway-plugin-discover`, the
//! `search_sessions` tool that feeds it; `conway-plugin-idiom`, which
//! prepends a short conway-idioms instruction fragment to a session (board
//! item `01M0VR3BKW5N3V3WS28H7FV8ZK`) -- the one entry in this list
//! contributing no tool at all; `conway-plugin-trim`, a
//! `Curator` that omits tool call/result round-trips older than a
//! configurable turn window (board item `01M0TV447NAJ1R06S455DZPP54` --
//! this crate did not depend on it at all before that item, so naming
//! `"conway.trim"` in `[plugins].install` used to reach nothing; see that
//! crate's own module doc for the rest of the history); and
//! `conway-plugin-names`, which lets an operator name an agent and then
//! steer it by that name, and which is the tier's worked proof that a real
//! capability needs no core change at all (board item
//! `01M0TV5BSE98S16SFYECG9G9WP`). Dynamic routing is
//! built too (`conway-plugin-routing`, resolved through `router_bundle`
//! below, not this list), and so is MCP client support -- through a
//! separate mechanism entirely, `[plugins].mcp` wired by this crate's own
//! `mcp_plugins` module, never through `bundle` here (see that module's own
//! doc). Context compaction is the one first-party-plugin-tier capability
//! still unbuilt (`scripts/board-claims.md`'s `absent: conway\.compaction`
//! predicate pins this so the claim goes stale loudly, not silently, the
//! moment that changes) -- it adds its own entry here when it lands,
//! through `ConwayBuilder::with_plugin`, `with_backend_factory`, or
//! `with_router_factory`, whichever channel fits it, since nothing about
//! `[plugins].install` itself is tool-specific -- `router_bundle` and
//! `backend_bundle` below are exactly the other two of those three
//! channels.
//!
//! Resolution below matches an id against each candidate's own identity.
//! `Backend` carries an `id()` of its own (`conway_core::ports::backend`),
//! but that is a CONFIGURED INSTANCE's identity, not a KIND's -- the same
//! reason `Router` has none at all (see the paragraph below): a
//! `BackendFactory`'s own `id()` is what `backend_bundle` resolves
//! against, mirroring `router_bundle` one line over. `Router`
//! (`conway_core::ports::routing`) has NO id-bearing method at all --
//! answered this, settling that a
//! router's identity lives on a separate `RouterFactory` trait instead
//! (`RouterFactory::id`), never on `Router` itself: router SELECTION
//! (naming a kind) must precede router CONSTRUCTION, which needs backends
//! and a capability picture that do not exist until much later in
//! startup, well after `[plugins].install` is read. `router_bundle`/
//! `backend_bundle` below are this binary's linked `RouterFactory`/
//! `BackendFactory` lists, resolved in the SAME pass as `bundle` by
//! [`ConwayBuilder::install_selected`] -- an id may name a plugin, a router
//! factory, or a backend factory, never more than one of the three, and
//! naming more than one router factory is rejected (a build has exactly
//! one router).
//!
//! ## What this module used to do, and does not any more
//!
//! Before, this file resolved
//! `[plugins].install` UNIONED with `[plugins].default_backends` against
//! `bundle`/`router_bundle`/`backend_bundle` itself, in a ~70-line
//! hand-rolled loop -- the exact resolution logic every OTHER embedder had
//! to rebuild from scratch, since it lived only here. That resolution is
//! now [`ConwayBuilder::install_selected`], a facade method taking the same
//! three caller-supplied bundles this module still constructs -- so what
//! remains here is exactly what is genuinely CLI-specific: which plugin,
//! router-factory, and backend-factory crates THIS BINARY links at all.
//! [`install`] below is now three `Vec` constructions and one call.
//!
//! ## What makes the two backend kinds attach with no `[plugins].install` entry
//!
//! Every other candidate in this file is opt-in: absent from the resolved
//! id set (`[plugins].install`), it is simply never attached, and `conway`
//! keeps working with whatever it does have (no extra tool, `MinimalRouter`
//! instead of `DeclarativeRouter`). A `[backends.<id>]` entry with no
//! matching `BackendFactory` has no such fallback -- `ConwayBuilder::build`
//! hard-errors ("no backends configured") when the backend map ends up
//! empty, and even a single unresolvable entry fails the whole build. So
//! the id set `ConwayBuilder::install_selected` resolves against is not
//! `[plugins].install` alone: it is `[plugins].install` UNIONED with
//! `[plugins].default_backends` (`conway::config::schema::PluginsConfig`'s
//! own doc -- default `["anthropic", "openai-compat"]`, owner decision
//!) -- computed inside `install_selected` itself
//! now, from whatever `ConwayBuilder` it is called on, so "came from
//! `install`" and "came from `default_backends`" are indistinguishable by
//! the time an id is resolved, exactly as before this item. This is what
//! makes `conway_plugin_backends`'s two factories attach on an ordinary
//! `settings.json` with no `[plugins]` section at all: `default_backends`
//! defaults to naming both, unioned in regardless.

use std::sync::Arc;

use conway::plugin::{MemoryStore, Plugin};
use conway::{BackendFactory, ConwayBuilder, FacadeError, RouterFactory};
use conway_plugin_names::AgentNames;

/// Every first-party plugin this binary links, in no particular order.
/// `Vec<Arc<dyn Plugin>>` rather than a `HashMap` keyed by id: the bundle
/// is tiny, and resolving by a linear scan over each candidate's own
/// `PluginManifest::id` is the same style `conway`'s own
/// `presets::builtin_plugins()` uses for the built-in bundle -- no second
/// registry idiom introduced for a one-plugin list.
///
/// `cwd` is the same `ConwayConfig::cwd` the facade's own `build()` resolves
/// `.conway/skills` against, passed in so `conway.skills` can load its own
/// copy of the on-disk skill table via the SAME public
/// `conway::skills::load_skill_defs` the builder uses (no privileged
/// channel -- see `conway_plugin_skills`'s own module doc). A missing
/// skills directory yields an empty-skills plugin (narrows nothing, serves
/// "no such skill" for every call); a MALFORMED skill file falls back to
/// the same empty shape HERE rather than failing the bundle construction,
/// because `ConwayBuilder::build` independently loads the same directory
/// and fails LOUDLY on a malformed `SKILL.md` -- so a genuinely broken
/// skill never reaches a turn with a silently-empty plugin, it fails the
/// build first. The fallback only ever triggers for a directory this
/// binary can read but the timing of `bundle()` happens to race with --
/// never observed in practice, and safe by construction if it did.
///
/// `memory_store` backs the `conway.memory` entry below -- ALREADY
/// resolved (durable vs in-memory, board item `01M09V3S2AQYB2VK6MANFRH1JM`)
/// by the caller via `resolve_memory_store`, never opened here. `bundle`
/// itself stays synchronous and side-effect-free: both of its callers
/// (`install`, `installed_plugins`) hand it whichever already-constructed
/// `Arc<dyn MemoryStore>` they are carrying, so calling `bundle` twice in
/// one process (exactly what happens today) constructs two `Plugin`
/// candidate lists that share the SAME underlying store, never two stores.
///
/// `agent_names` backs the `conway.names` entry below on exactly the same
/// footing (board item `01M0TV5BSE98S16SFYECG9G9WP`): resolved once by the
/// caller via `resolve_agent_names`, never opened here, so two `bundle`
/// calls in one process share ONE store rather than opening two views of
/// one file. Unlike `memory_store`, this `Arc` is ALSO read directly by the
/// TUI (`tui::state::AppState::agent_names`) -- see `install`'s own doc for
/// why that makes it a compiled interface rather than a shared file format.
fn bundle(
    cwd: &std::path::Path,
    memory_store: Arc<dyn MemoryStore>,
    agent_names: Arc<dyn AgentNames>,
) -> Vec<Arc<dyn Plugin>> {
    let skills_plugin =
        conway_plugin_skills::SkillsPlugin::from_dir(&cwd.join(".conway").join("skills"))
            .unwrap_or_else(|_| {
                conway_plugin_skills::SkillsPlugin::new(Arc::new(std::collections::HashMap::new()))
            });
    vec![
        Arc::new(conway_plugin_skeleton::SkeletonPlugin),
        // `/conway.history.rewind`
        // -- the answer to "is /rewind a plugin", per the owner's ruling
        // that session-history features belong in the plugin tier, not
        // core. Resolved through this SAME `[plugins].install` mechanism;
        // absent from `main.rs`'s default install set (and from every
        // README/getting-started example's default snippet), exactly like
        // `conway_plugin_skeleton` above -- first-party still means
        // opt-in.
        Arc::new(conway_plugin_history::HistoryPlugin),
        // `conway.stepguard` -- repeated-tool-call detection, which the agent
        // loop used to carry unconditionally. `PHILOSOPHY.md` §6 leaves loop
        // intervention to the operator ("including writing none"), which is
        // only true if declining it is possible; moving it here is what makes
        // it so. Opt-in like every other member of this bundle, so a default
        // build observes nothing.
        Arc::new(conway_plugin_stepguard::StepGuardPlugin::new()),
        // `conway.skills` -- progressive skill disclosure (board item
        // `01M03GMNB3P048G72M158XPDG2`): narrows full-body
        // `Provenance::Skill` context segments to a one-line index and
        // offers `read_skill` for the full body on demand. Opt-in like
        // every other member of this bundle; its `ContextHook` installs
        // through `Plugin::context_hooks` (the SAME `with_plugin`/
        // `install_selected` surface its tool uses), so `[plugins].install
        // = ["conway.skills"]` is the whole of the wiring -- no separate
        // `with_context_hook` call.
        Arc::new(skills_plugin),
        // `conway.memory` -- a mutable `MemoryStore` injected into context
        // by a `ContextHook` (board item `01M09P2T8E5M292WMSMS64CVC4`, a
        // REWORK of the label-based curator this bundle used to install --
        // see `conway_plugin_memory`'s own module doc for why). Installs
        // through the SAME `Plugin::tools`/`Plugin::context_hooks`/
        // `with_plugin` surface every other plugin capability uses (GP-03)
        // -- so `[plugins].install = ["conway.memory"]` is the whole of the
        // wiring, exactly like the skills plugin above. Opt-in like every
        // other member of this bundle.
        //
        // Board item `01M09V3S2AQYB2VK6MANFRH1JM`: the durable
        // `conway::memory::FsMemoryStore`, not `InMemoryMemoryStore`, when
        // `conway.memory` is actually selected (`memory_store`, resolved by
        // [`resolve_memory_store`], is threaded in by both of `bundle`'s
        // callers rather than opened here -- see that function's own doc for
        // the single-open-site guarantee this depends on, and `install`'s
        // doc for why `InMemoryMemoryStore` is still what an UNSELECTED
        // build passes here).
        Arc::new(conway_plugin_memory::MemoryPlugin::new(
            memory_store,
            conway_plugin_memory::MemoryConfig::default(),
        )),
        // `conway.path` -- the tool a model calls to compose a session's
        // context path (board item `01M0PEFMG96SVBBD5D2E06H34A`, decision
        // `01M0K4QT6MBXPD6PXMBBBD2P7B`): the first production caller of
        // `write_head`/`ValidatedPath::derive_with`, which existed with no
        // caller anywhere in a running build before this entry. Needs no
        // constructor argument (unlike `conway.memory` above) -- every
        // dispatched tool's `ToolCtx::context_path` is already populated by
        // the runtime itself, regardless of which plugin owns the tool
        // reading it. Opt-in like every other member of this bundle: this
        // entry is what makes `[plugins].install = ["conway.path"]` reach
        // real code at all -- this is exactly the shape a missing
        // dependency line here breaks (see `conway.trim`'s own entry below
        // for the board item that closed that exact defect for THAT
        // plugin).
        Arc::new(conway_plugin_path::PathPlugin),
        // `conway.discover` -- the tool a model calls to find a session or
        // record it does not already hold a reference to (board item
        // `01M0PS8J3AK7Z7253Z3E3RD3GY`), feeding `conway.path`'s
        // `compose_context_path` immediately above -- the two are meant to
        // be installed together, though nothing here enforces that (an
        // operator who installs `conway.discover` alone gets a tool that
        // can find but not compose; see this crate's own module doc for
        // why every candidate here stays independently opt-in). Needs no
        // constructor argument, exactly like `conway.path` -- every
        // dispatched tool's `ToolCtx::session_discovery` is already
        // populated by the runtime itself.
        Arc::new(conway_plugin_discover::DiscoverPlugin),
        // `conway.idiom` -- a plugin that prepends a short conway-idioms
        // instruction fragment to a session (board item
        // `01M0VR3BKW5N3V3WS28H7FV8ZK`): fork vs. spawn, how an agent ends,
        // configuration-dependent tools, context scarcity, permissions,
        // budgets, steering. Contributes no tool -- unlike `conway.path`/
        // `conway.discover` immediately above, this candidate needs no
        // constructor argument and no host capability at all; the fragment
        // is `include_str!`-loaded at compile time
        // (`conway-plugin-idiom`'s own `fragments/idiom.md`). Opt-in like
        // every other member of this bundle: naming `"conway.idiom"` in
        // `[plugins].install` is what makes the interactive TUI's
        // otherwise-empty `[0] SystemPrompt` step carry ANY harness
        // orientation at all (`App::session_spec` sets no `agent_def`/
        // `system_prompt_override` -- see `conway-plugin-idiom`'s own
        // module doc for the re-verified premise). Reaches root agents
        // only -- see that crate's own doc and `PluginDescription::
        // you_lose` for the disclosed subagent gap.
        Arc::new(conway_plugin_idiom::IdiomPlugin),
        // `conway.trim` -- a `Curator` that omits tool call/result
        // round-trips older than a configurable turn window (board item
        // `01M0TV447NAJ1R06S455DZPP54`; `conway-plugin-trim`'s own module
        // doc for the full "smallest honest curator" reasoning). Installs
        // through `Plugin::curators` -- a DIFFERENT `Plugin` surface than
        // every other candidate in this bundle, none of which contributes
        // one -- composed with any embedder `with_curator` injection and
        // every other plugin's own curators by `ConwayBuilder::build`
        // (`compose_curators`), so `[plugins].install = ["conway.trim"]`
        // is still the whole of the wiring, exactly like the tool-
        // contributing entries above. History: this crate did not depend
        // on `conway-plugin-trim` at all before the board item above --
        // naming `"conway.trim"` in `[plugins].install` resolved to an
        // unknown-id error no matter what, the exact defect this entry
        // closes (`tests/first_party_plugins.rs`'s
        // `conway_trim_curator_omits_old_tool_round_trips_once_installed`
        // is the reachability proof). Needs no constructor argument --
        // `TrimPlugin::new()` is `TrimOldToolResults::default()`
        // (`DEFAULT_KEEP_TURNS`), the same default an embedder gets from
        // `TrimPlugin::default()`; `TrimPlugin::with_keep_turns` exists for
        // a caller that wants a different window, but this bundle has no
        // config surface to thread a per-operator value through yet, so it
        // is not reached from here.
        Arc::new(conway_plugin_trim::TrimPlugin::new()),
        // `conway.names` -- operator-chosen, renameable names for agents
        // (board item `01M0TV5BSE98S16SFYECG9G9WP`, decision
        // `01M0TV3ZZBDKSSV7MD0FW3FSY7`). Three commands
        // (`/conway.names.rename`/`.unname`/`.list`) over the
        // `conway_plugin_names::AgentNames` store `agent_names` already
        // holds -- the SAME instance `main.rs` hands the TUI, so a rename
        // typed here is visible to `resolve_agent`/the `/agents` panel
        // immediately, with no reload and no second reader of a file.
        //
        // Constructed with its store rather than opening one, exactly like
        // `conway.memory` above; see [`resolve_agent_names`] for why an
        // operator who never opted in gets a throwaway in-memory store and
        // no file. Opt-in like every other member of this bundle -- see
        // this crate's own module doc and board item
        // `01M0TV5BSE98S16SFYECG9G9WP`'s report for why "should
        // `conway.names` be on by default" was deliberately left to the
        // operator rather than answered here: every candidate in this
        // bundle is off by default, and making this one the exception
        // would be a change to the BUNDLE's policy, not to this plugin.
        Arc::new(conway_plugin_names::NamesPlugin::new(agent_names)),
    ]
}

/// Every first-party `Plugin` this binary links, REGARDLESS of
/// `[plugins].install` selection -- the plugin browser's own read surface
/// (board item `01M0KARX71A64NTSYTDBVANVPF`, `crates/conway-cli/src/tui/
/// app/startup.rs`'s `App::new`, the one caller). A thin, same-shaped
/// wrapper over `bundle` (private to this module) rather than a second candidate list: `bundle`
/// already IS "every first-party plugin this binary links", unfiltered --
/// [`installed_plugins`] filters it down to the SELECTED subset for
/// command-registry purposes, and this function is the other read the same
/// unfiltered list needs, for a browser that must show what is
/// AVAILABLE-but-off, not only what is on. Never re-derives the candidate
/// set independently, so the browser's own "N installed of M compiled-in"
/// count can never drift from what `[plugins].install` actually resolves
/// against.
pub fn all_bundle_plugins(
    cwd: &std::path::Path,
    memory_store: Arc<dyn MemoryStore>,
) -> Vec<Arc<dyn Plugin>> {
    // A throwaway `InMemoryAgentNames` backs the `conway.names` candidate
    // for this scan, deliberately -- and, unlike `memory_store` above,
    // constructed HERE rather than threaded in by the caller. This
    // function's one production caller (`tui::app::startup`'s `App::new`)
    // only ever calls `.manifest()`/`.description()` on each candidate,
    // never a method that touches a store, so opening the REAL store again
    // would violate [`resolve_agent_names`]'s "exactly one
    // `FsAgentNames::open` call site" invariant for no benefit. Keeping it
    // inside this function rather than on its signature also keeps that
    // caller's existing call unchanged -- the throwaway is a property of
    // what THIS function does (a read-only capability scan), not a choice
    // its caller should have to make. Mirrors `resolve_agent_names`'s own
    // fallback for an unselected build ("unused, cheap, no I/O").
    let browse_names: Arc<dyn AgentNames> =
        Arc::new(conway_plugin_names::InMemoryAgentNames::new());
    bundle(cwd, memory_store, browse_names)
}

/// Every first-party `RouterFactory` this binary links, in no particular
/// order -- the router-side sibling of `bundle`, resolved against the
/// SAME `[plugins].install` list, in the same pass ([`install`]).
///
/// **First occupant:**
/// `conway-plugin-routing`'s `RoutingRouterFactory` -- the capability-/
/// health-filtering `DeclarativeRouter` engine `conway` itself used to
/// compile in unconditionally, now installed by naming its published
/// `ROUTER_ID` (`"conway.routing"`) in `[plugins].install`, exactly the way
/// `bundle`'s skeleton plugin is named. Absent that entry, `build()`
/// falls through to `conway_core::routing::MinimalRouter` (see
/// `docs/routing.md`).
fn router_bundle() -> Vec<Arc<dyn RouterFactory>> {
    vec![Arc::new(conway_plugin_routing::RoutingRouterFactory)]
}

/// Every first-party `BackendFactory` this binary links -- the
/// backend-side sibling of `bundle`/`router_bundle`, resolved against
/// the SAME id list, in the same pass ([`install`]).
///
/// **Both occupants:**
/// `conway_plugin_backends`'s `AnthropicBackendFactory`/
/// `OpenAiCompatBackendFactory` -- the two provider-adapter dialects
/// `conway` itself used to compile in unconditionally, now installed by
/// naming their published kind ids (`conway_plugin_backends::
/// ANTHROPIC_KIND`/`OPENAI_COMPAT_KIND` -- `"anthropic"`/`"openai-compat"`,
/// unchanged from before this item) -- ordinarily with NO
/// `[plugins].install` entry at all, since `ConwayBuilder::install_selected`
/// itself unions `[plugins].default_backends` into the resolved id set (see
/// this module's own doc, "What makes the two backend kinds attach...").
/// Absent BOTH ids (an operator who edited `default_backends` down to `[]`
/// or removed a specific one), a `[backends.<id>]` entry naming that kind
/// fails `build()` -- there is no silent fallback, by design: nothing may
/// claim to be reached that isn't, and the whole point of this pair
/// shipping attached by default is that an operator has to take a
/// deliberate action to lose the capability, not merely omit one.
fn backend_bundle() -> Vec<Arc<dyn BackendFactory>> {
    vec![
        Arc::new(conway_plugin_backends::AnthropicBackendFactory),
        Arc::new(conway_plugin_backends::OpenAiCompatBackendFactory),
    ]
}

/// Resolves the ONE `Arc<dyn MemoryStore>` `bundle`'s `conway.memory` entry
/// is constructed with -- board item `01M09V3S2AQYB2VK6MANFRH1JM`, closing
/// the gap the prior item deferred (see `bundle`'s own former inline note,
/// now removed, and this module's `01M09V3S2AQYB2VK6MANFRH1JM`
/// history for the reasoning this replaces).
///
/// **Called exactly once per process, from [`install`] alone.** This is the
/// ONLY line in this crate that ever calls `FsMemoryStore::open` -- not "the
/// only line that is SUPPOSED to", but literally the only call site the
/// source contains -- so a second, independent, unsynchronized store over
/// the same root is unrepresentable by construction, not merely avoided by
/// discipline: making a second one would require a second call to THIS
/// function to exist somewhere, and none does. [`installed_plugins`] (the
/// TUI's own re-derivation of `bundle`'s selected subset, `commands::
/// plugin::run`'s identical need) never opens a store itself; both receive
/// the SAME `Arc` [`install`] already resolved, threaded through
/// `main.rs`'s `build_conway`/`dispatch` as a plain parameter -- see those
/// functions' own docs. This closes exactly the race `FsMemoryStore::
/// put_lock`'s own doc warns two independent instances over one directory
/// would reopen (`crates/conway-session/src/memory_store.rs`).
///
/// **Only actually opens the durable store when `conway.memory` is named in
/// `install_ids`.** `bundle` unconditionally includes a `conway.memory`
/// candidate regardless of selection (exactly like every other entry --
/// `install_selected` is what actually filters), so constructing ITS
/// dependency unconditionally would mean an operator who has never opted
/// into `conway.memory` at all could still have the CLI refuse to start over
/// a memory directory it will never touch. An unselected build gets a fresh
/// [`conway_plugin_memory::InMemoryMemoryStore`] instead -- unused, cheap,
/// no I/O -- matching this crate's pre-item behavior for anyone who never
/// asked for durability.
///
/// **Failure posture, decided: fail closed, no silent fallback.** When
/// `conway.memory` IS selected and the durable store cannot be opened
/// (permissions, read-only filesystem, the `jsonl-store` feature disabled in
/// this build), this returns `Err` and [`install`] propagates it --
/// `build_conway` surfaces it on stderr and the process exits nonstarting,
/// the same "no silent fallback, by design" posture `backend_bundle`'s own
/// doc already establishes for a `[backends.<id>]` entry naming a declined
/// kind. Falling back to `InMemoryMemoryStore` here instead would recreate,
/// silently, the EXACT invisible-limitation defect this item exists to fix
/// -- a user who explicitly opted into durable memory would get the
/// non-durable kind back with no visible signal that anything degraded.
/// Mirrors `conway::builder::build_default_store`'s own session-store
/// precedent (`ConwayBuilder::build`), which already propagates rather than
/// falling back when `jsonl-store` is off and no store was injected.
///
/// **No `block_on` bridge, unlike `build_default_store`.** That function is
/// reached from `ConwayBuilder::build`'s deliberately *synchronous* public
/// signature (an embedder may not be running inside `tokio` at all), so it
/// has to bridge sync-to-async itself. Every caller of THIS function in this
/// binary is already inside an `async fn` (`main.rs`'s `#[tokio::main]`
/// `main` all the way down through `build_conway`) -- `.await`ing
/// `FsMemoryStore::open` directly is the whole of the bridge needed here;
/// inventing a second `block_on`-style thread-spawn would solve a problem
/// this call site does not have.
async fn resolve_memory_store(
    cwd: &std::path::Path,
    install_ids: &[String],
) -> Result<Arc<dyn MemoryStore>, FacadeError> {
    if !install_ids
        .iter()
        .any(|id| id == conway_plugin_memory::PLUGIN_ID)
    {
        return Ok(Arc::new(conway_plugin_memory::InMemoryMemoryStore::new()));
    }
    #[cfg(feature = "jsonl-store")]
    {
        let root = cwd.join(".conway").join("memory");
        conway::memory::FsMemoryStore::open(root.clone())
            .await
            .map(|store| Arc::new(store) as Arc<dyn MemoryStore>)
            .map_err(|e| FacadeError::Build {
                message: format!(
                    "conway.memory: cannot open the durable memory store at {} ({e}) -- fix the \
                     directory's permissions/filesystem, or remove \"conway.memory\" from \
                     [plugins].install to run without it",
                    root.display()
                ),
            })
    }
    #[cfg(not(feature = "jsonl-store"))]
    {
        let _ = cwd;
        Err(FacadeError::Build {
            message: "conway.memory is named in [plugins].install but this binary was built \
                      without the 'jsonl-store' feature, so no durable memory store is \
                      available"
                .to_string(),
        })
    }
}

/// Resolves the ONE `Arc<dyn AgentNames>` `bundle`'s `conway.names` entry is
/// constructed with, and that the TUI later reads names out of (board item
/// `01M0TV5BSE98S16SFYECG9G9WP`).
///
/// **The direct sibling of `resolve_memory_store`, and deliberately the
/// same shape rather than a second pattern.** Read that function's own doc
/// first; every argument it makes applies here, so only the differences are
/// restated below.
///
/// **Called exactly once per process, from [`install`] alone**, and this is
/// the only line in this crate that ever calls `FsAgentNames::open`. A
/// second, independent, unsynchronized view of the same file is therefore
/// unrepresentable rather than merely avoided: making one would require a
/// second call to THIS function, and none exists. [`installed_plugins`] and
/// `main.rs`'s `dispatch`/`tui::run` receive the SAME `Arc`.
///
/// **Only opens the durable store when `conway.names` is named in
/// `install_ids`.** `bundle` includes a `conway.names` candidate
/// unconditionally (selection is `install_selected`'s job), so an operator
/// who never opted in gets a fresh `InMemoryAgentNames` -- unused, cheap,
/// no I/O, no file created beside their `settings.json`.
///
/// **Fail closed when it IS selected and the store cannot be opened**, the
/// same posture `resolve_memory_store` takes: a corrupt or unreadable
/// `agent-names.json` stops the process with a message naming the file,
/// rather than silently starting with no names and then overwriting the
/// file on the operator's next rename. `conway_plugin_names::
/// AgentNamesError`'s own `Corrupt` variant carries the remedy in its
/// message.
///
/// **One case that is NOT a failure:** `default_store_path` returning
/// `None` -- no home directory discoverable AND `CONWAY_CONFIG_DIR` unset,
/// the same extreme edge `conway::config::discovery::user_config_path`
/// itself returns `None` for. That yields the in-memory store, so naming is
/// still usable for the life of the process, just not persisted. Refusing
/// to start over a missing HOME would be a worse answer than a degraded but
/// honest one, and unlike the corrupt-file case there is nothing here that
/// could be silently destroyed.
///
/// **Synchronous, unlike `resolve_memory_store`.** `FsAgentNames::open`
/// reads one small JSON file once -- see its own doc for why an async
/// bridge would be ceremony. This function stays `fn` so `install` does not
/// `.await` something that never yields.
fn resolve_agent_names(install_ids: &[String]) -> Result<Arc<dyn AgentNames>, FacadeError> {
    if !install_ids
        .iter()
        .any(|id| id == conway_plugin_names::PLUGIN_ID)
    {
        return Ok(Arc::new(conway_plugin_names::InMemoryAgentNames::new()));
    }
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let Some(path) = conway_plugin_names::default_store_path(&env) else {
        return Ok(Arc::new(conway_plugin_names::InMemoryAgentNames::new()));
    };
    conway_plugin_names::FsAgentNames::open(path)
        .map(|store| Arc::new(store) as Arc<dyn AgentNames>)
        .map_err(|e| FacadeError::Build {
            message: format!("conway.names: {e}"),
        })
}

/// Hands this binary's three linked bundles (`bundle`, `router_bundle`,
/// `backend_bundle`) to [`ConwayBuilder::install_selected`] -- the
/// facade's own resolution of `[plugins].install` UNIONED with
/// `[plugins].default_backends` against exactly those three. Every dispatch target (`main.rs`'s
/// `build_conway`) shares this one call, so the TUI, one-shot `-p`,
/// `sessions`, and `routes` all see the same installed set from the same
/// config.
///
/// **This is now three `Vec` constructions and one call** -- the ~70-line
/// hand-rolled resolution this function used to perform (matching each id
/// against a candidate's own identity, the router-factory cardinality
/// check, the unknown-id error, the `with_declined_backend_kinds` call)
/// moved to `install_selected` itself
/// -- see this module's own doc, "What this module used to do, and does
/// not any more". What is left here is exactly the part that is genuinely
/// CLI-specific: which plugin/router-factory/backend-factory CRATES this
/// binary links at all.
///
/// **Now `async fn`, and returns the resolved `Arc<dyn MemoryStore>`
/// alongside the built `ConwayBuilder`** (board item
/// `01M09V3S2AQYB2VK6MANFRH1JM`) -- see `resolve_memory_store`'s own doc
/// for why this is the ONE place that store is ever opened, and `main.rs`'s
/// `build_conway`/`dispatch` for how the returned `Arc` reaches
/// [`installed_plugins`]'s later, independent call, unopened, as a plain
/// parameter.
///
/// **And the resolved `Arc<dyn AgentNames>` alongside it** (board item
/// `01M0TV5BSE98S16SFYECG9G9WP`), for the identical reason and through the
/// identical route -- see `resolve_agent_names`. This one travels one hop
/// further than the memory store does: `main.rs`'s `dispatch` also hands it
/// to `tui::run`, which parks it on `tui::state::AppState::agent_names` so
/// `resolve_agent` and the `/agents` panel can read a name. That is what
/// keeps plugin and host coupled through a COMPILED interface (the
/// `AgentNames` trait, defined in the plugin crate this binary already
/// links) rather than through a file format two readers agree about by
/// convention -- the shape the governing decision rejected, and the one
/// `conway_core::ports::memory_store`'s own module doc records this project
/// abandoning once already.
pub async fn install(
    builder: ConwayBuilder,
) -> Result<(ConwayBuilder, Arc<dyn MemoryStore>, Arc<dyn AgentNames>), FacadeError> {
    let cwd = builder.config().cwd.clone();
    let memory_store = resolve_memory_store(&cwd, &builder.config().plugins.install).await?;
    let agent_names = resolve_agent_names(&builder.config().plugins.install)?;
    let plugins = bundle(&cwd, memory_store.clone(), agent_names.clone());
    let builder = builder.install_selected(plugins, router_bundle(), backend_bundle())?;
    Ok((builder, memory_store, agent_names))
}

/// The subset of `bundle` actually selected by `conway`'s own
/// `[plugins].install` config --
/// what `tui::run`/`App::new` need to build the plugin command registry,
/// since neither `Conway` nor `Runtime` exposes the installed `Plugin` list
/// back out once `[install] handed it to `ConwayBuilder::install_selected`.
///
/// **Deliberately re-derives from `bundle` + `conway.config().plugins.
/// install` rather than re-running `install_selected`'s own resolution
/// logic** (which also unions in `[plugins].default_backends` and enforces
/// router-factory cardinality -- neither applicable here, since `bundle`
/// holds `Plugin`s only, never a `RouterFactory`/`BackendFactory`, and
/// `default_backends` names backend kind ids that never collide with a
/// first-party PLUGIN's own id in practice). Re-running that fuller
/// resolution would need `ConwayBuilder` itself, which is already spent by
/// the time `main.rs`'s `build_conway` returns a built `Conway` -- so this
/// reads back the ONE fact that actually decides plugin membership
/// (`[plugins].install`, a plain `Vec<String>` `Conway::config()` already
/// exposes publicly) and filters `bundle` by it directly. Two callers
/// computing "is id X installed" from the SAME public config field can never
/// disagree about a first-party plugin's own install status, even though
/// they are, mechanically, two call sites.
///
/// `memory_store` is the SAME `Arc` [`install`] resolved (via
/// `resolve_memory_store`) for this same process -- passed in rather than
/// re-resolved here, so this function never opens a second `FsMemoryStore`
/// over the same root (see `resolve_memory_store`'s own doc for why that
/// matters). `main.rs`'s `build_conway` returns it precisely so `dispatch`
/// has it in hand to pass to this call and to `commands::plugin::run`'s
/// identical need.
///
/// `agent_names` is the same `Arc` on the same footing, resolved by
/// `resolve_agent_names` in the same pass -- and it matters MORE here
/// than `memory_store` does, because the `conway.names` plugin this
/// function returns is what writes a name that the TUI, holding that very
/// same `Arc`, then reads back to resolve `/steer <name>`. Handing this
/// function a second store would break that loop silently: the rename would
/// succeed and the name would resolve to nothing.
pub fn installed_plugins(
    conway: &conway::Conway,
    memory_store: Arc<dyn MemoryStore>,
    agent_names: Arc<dyn AgentNames>,
) -> Vec<Arc<dyn Plugin>> {
    let install = &conway.config().plugins.install;
    let cwd = conway.config().cwd.clone();
    bundle(&cwd, memory_store, agent_names)
        .into_iter()
        .filter(|plugin| install.contains(&plugin.manifest().id))
        .collect()
}

/// `install` itself is covered end-to-end in `tests/first_party_plugins.rs`,
/// which drives the real compiled binary: the empty case
/// (`skeleton_tool_is_absent_from_the_announced_set_without_plugins_install`),
/// resolution of a known id
/// (`skeleton_tool_is_present_in_the_announced_set_once_installed`), the
/// resulting tool actually running (`skeleton_tool_is_callable_from_one_shot_
/// once_installed`), a fresh install reaching a model with no
/// `[plugins].install` entry at all (`default_backends_attach_with_no_
/// plugins_install_entry_and_complete_a_one_shot_prompt`), and the
/// unknown-id hard error (`unknown_plugins_install_id_is_a_hard_error`,
/// which also pins that the error message lists the linked plugin ids, the
/// linked router factory ids, and the linked backend factory ids). Each
/// asserts on an observable outcome — the announced tool set on the wire,
/// the invoked tool's preview text, the process exit code and stderr —
/// rather than on an intermediate signal. `ConwayBuilder::install_selected`
/// itself is covered directly, against caller-supplied fakes, in
/// `crates/conway/tests/install_selected.rs`; this file's own coverage is
/// therefore the real-binary liveness proof that `install` above wires this
/// binary's three linked bundles into that method correctly, not a
/// restatement of `install_selected`'s own resolution-logic unit coverage.
///
/// The `with_declined_backend_kinds` call `install_selected` makes
/// internally is covered the same
/// way, separately, in `tests/decline_backend_kind.rs`: declining a shipped
/// dialect via `[plugins].default_backends` while a `[backends.<id>]`
/// entry still names it fails the real compiled binary with a message that
/// reads as **declined**, and a kind this binary has never linked at all
/// still fails with the pre-existing **unknown-kind** message — with a
/// third test pinning that the two stderr strings are genuinely different
/// text.
///
/// This module deliberately does NOT restate that coverage as unit tests.
/// Constructing a `ConwayBuilder` here would need a stub config solely to
/// re-check what the integration suite already proves against the real
/// binary, and two earlier attempts at exactly that asserted only on
/// `bundle` while their names promised they exercised `install` — checks
/// that could not fail, which is the defect class CONTRIBUTING's testing
/// discipline exists to catch. The properties below are local to this
/// module and are stated as narrowly as they are checked.
#[cfg(test)]
mod tests {
    use super::*;

    /// The non-durable store every wiring-only check here passes to
    /// `bundle`. Deliberately never `FsAgentNames`: nothing in this module's
    /// own tests calls a method that touches a store, and a test that
    /// opened the real one would write into the operator's own
    /// `~/.conway/` (`crates/conway/tests/config_isolation_guard.rs`).
    fn test_agent_names() -> Arc<dyn AgentNames> {
        Arc::new(conway_plugin_names::InMemoryAgentNames::new())
    }

    /// The bundle is what `install_selected` resolves against, so an empty
    /// or mis-keyed bundle would turn every `[plugins].install` entry into
    /// an unknown-id error. This checks the wiring only; it makes no claim
    /// about `install_selected`'s own behaviour.
    #[test]
    fn bundle_carries_the_skeleton_plugin_under_its_published_id() {
        // `bundle` takes `cwd` only to load `conway.skills`'s on-disk table;
        // a nonexistent dir yields an empty-skills plugin (the module doc's
        // safe fallback), so a temp dir with no `.conway/skills` is fine for
        // this wiring-only check.
        let cwd = std::env::temp_dir().join("conway-first-party-plugins-bundle-test");
        let memory_store = Arc::new(conway_plugin_memory::InMemoryMemoryStore::new());
        let found = bundle(&cwd, memory_store, test_agent_names())
            .iter()
            .any(|p| p.manifest().id == conway_plugin_skeleton::PLUGIN_ID);
        assert!(
            found,
            "the linked bundle must contain the skeleton plugin under its published id, \
             otherwise `[plugins].install = [\"{}\"]` resolves to an unknown-id error",
            conway_plugin_skeleton::PLUGIN_ID
        );
    }

    /// Same wiring-only check, for `conway_plugin_memory`: without its
    /// published id present in `bundle`, `[plugins].install =
    /// ["conway.memory"]` resolves to an unknown-id error.
    #[test]
    fn bundle_carries_the_memory_plugin_under_its_published_id() {
        let cwd = std::env::temp_dir().join("conway-first-party-plugins-bundle-test");
        let memory_store = Arc::new(conway_plugin_memory::InMemoryMemoryStore::new());
        let found = bundle(&cwd, memory_store, test_agent_names())
            .iter()
            .any(|p| p.manifest().id == conway_plugin_memory::PLUGIN_ID);
        assert!(
            found,
            "the linked bundle must contain the memory plugin under its published id, \
             otherwise `[plugins].install = [\"{}\"]` resolves to an unknown-id error",
            conway_plugin_memory::PLUGIN_ID
        );
    }

    /// `all_bundle_plugins` must return the SAME candidates `bundle` itself
    /// does, unfiltered by `[plugins].install` -- the plugin browser's own
    /// "available but off" rows depend on seeing every linked candidate,
    /// not only a selected subset.
    #[test]
    fn all_bundle_plugins_returns_every_linked_candidate_unfiltered() {
        let cwd = std::env::temp_dir().join("conway-first-party-plugins-bundle-test");
        let memory_store = Arc::new(conway_plugin_memory::InMemoryMemoryStore::new());
        let ids: Vec<String> = all_bundle_plugins(&cwd, memory_store)
            .iter()
            .map(|p| p.manifest().id)
            .collect();
        for expected in [
            conway_plugin_skeleton::PLUGIN_ID,
            conway_plugin_history::PLUGIN_ID,
            conway_plugin_stepguard::PLUGIN_ID,
            conway_plugin_skills::PLUGIN_ID,
            conway_plugin_memory::PLUGIN_ID,
            conway_plugin_path::PLUGIN_ID,
            conway_plugin_discover::PLUGIN_ID,
            conway_plugin_idiom::PLUGIN_ID,
            conway_plugin_trim::PLUGIN_ID,
            conway_plugin_names::PLUGIN_ID,
        ] {
            assert!(
                ids.contains(&expected.to_string()),
                "missing {expected} in {ids:?}"
            );
        }
    }

    /// Same wiring-only check, for `conway_plugin_trim`: without its
    /// published id present in `bundle`, `[plugins].install =
    /// ["conway.trim"]` resolves to an unknown-id error -- the exact defect
    /// board item `01M0TV447NAJ1R06S455DZPP54` closed (this crate did not
    /// even depend on `conway-plugin-trim` before it).
    #[test]
    fn bundle_carries_the_trim_plugin_under_its_published_id() {
        let cwd = std::env::temp_dir().join("conway-first-party-plugins-bundle-test");
        let memory_store = Arc::new(conway_plugin_memory::InMemoryMemoryStore::new());
        let found = bundle(&cwd, memory_store, test_agent_names())
            .iter()
            .any(|p| p.manifest().id == conway_plugin_trim::PLUGIN_ID);
        assert!(
            found,
            "the linked bundle must contain the trim plugin under its published id, otherwise \
             `[plugins].install = [\"{}\"]` resolves to an unknown-id error",
            conway_plugin_trim::PLUGIN_ID
        );
    }

    /// Same wiring-only check, for `conway_plugin_idiom`: without its
    /// published id present in `bundle`, `[plugins].install =
    /// ["conway.idiom"]` resolves to an unknown-id error -- the exact
    /// defect board item `01M0TV447NAJ1R06S455DZPP54` closed for
    /// `conway.trim`.
    #[test]
    fn bundle_carries_the_idiom_plugin_under_its_published_id() {
        let cwd = std::env::temp_dir().join("conway-first-party-plugins-bundle-test");
        let memory_store = Arc::new(conway_plugin_memory::InMemoryMemoryStore::new());
        let found = bundle(&cwd, memory_store, test_agent_names())
            .iter()
            .any(|p| p.manifest().id == conway_plugin_idiom::PLUGIN_ID);
        assert!(
            found,
            "the linked bundle must contain the idiom plugin under its published id, \
             otherwise `[plugins].install = [\"{}\"]` resolves to an unknown-id error",
            conway_plugin_idiom::PLUGIN_ID
        );
    }

    /// Same wiring-only check, for `backend_bundle`: both published kind
    /// ids must be present, otherwise `[plugins].default_backends`'s own
    /// default value resolves to an unknown-id error and a fresh install
    /// cannot reach a model at all.
    #[test]
    fn backend_bundle_carries_both_published_kind_ids() {
        let bundle = backend_bundle();
        let ids: Vec<&str> = bundle.iter().map(|f| f.id()).collect();
        assert!(
            ids.contains(&conway_plugin_backends::ANTHROPIC_KIND),
            "missing '{}' in the linked backend bundle: {ids:?}",
            conway_plugin_backends::ANTHROPIC_KIND
        );
        assert!(
            ids.contains(&conway_plugin_backends::OPENAI_COMPAT_KIND),
            "missing '{}' in the linked backend bundle: {ids:?}",
            conway_plugin_backends::OPENAI_COMPAT_KIND
        );
    }

    /// Same wiring-only check, for `router_bundle`: the routing plugin's
    /// published `ROUTER_ID` must be present, otherwise
    /// `[plugins].install = ["conway.routing"]` resolves to an unknown-id
    /// error and an operator following `docs/routing.md` cannot install it.
    #[test]
    fn router_bundle_carries_the_routing_plugins_published_id() {
        let bundle = router_bundle();
        let ids: Vec<&str> = bundle.iter().map(|f| f.id()).collect();
        assert!(
            ids.contains(&conway_plugin_routing::ROUTER_ID),
            "missing '{}' in the linked router-factory bundle: {ids:?}",
            conway_plugin_routing::ROUTER_ID
        );
    }

    /// Board item `01M09V3S2AQYB2VK6MANFRH1JM`, acceptance criterion 2
    /// ("exactly one `FsMemoryStore` instance per process per root --
    /// demonstrated, not asserted"). `resolve_memory_store`'s own doc claims
    /// its `FsMemoryStore::open` call is the ONLY one in this file --
    /// checked here by grepping this file's own source rather than trusting
    /// the doc comment, so a future edit that reintroduced a second call
    /// site (e.g. `installed_plugins` opening its own store again instead of
    /// receiving one) fails THIS test rather than silently reopening the
    /// TOCTOU race `FsMemoryStore::put_lock`'s own doc
    /// (`conway-session/src/memory_store.rs`) describes.
    #[test]
    fn fs_memory_store_is_opened_from_exactly_one_call_site() {
        let source = include_str!("first_party_plugins.rs");
        // Scoped to the PRODUCTION portion of this file (everything before
        // this very `mod tests` block) -- `resolve_memory_store_opens_the_
        // durable_store_when_selected_and_it_persists`, below, deliberately
        // opens a SECOND, independent `FsMemoryStore` over the same root to
        // prove durability, which is legitimate TEST code proving a
        // different claim, not a production call site this invariant is
        // about (see that test's own doc).
        let production_source = source
            .split("#[cfg(test)]\nmod tests {")
            .next()
            .expect("split always yields at least the text before the delimiter");
        let occurrences = production_source.matches("FsMemoryStore::open(").count();
        assert_eq!(
            occurrences, 1,
            "expected exactly one `FsMemoryStore::open(` call site in this file's production \
             code (inside `resolve_memory_store`) -- found {occurrences}; a second call site \
             over the same root reintroduces the two-independent-stores race `put_lock`'s own \
             doc exists to close"
        );
    }

    /// `resolve_memory_store` must not touch disk at all when `conway.memory`
    /// is absent from `[plugins].install` -- an operator who never opted
    /// into memory must never have the CLI's ability to start depend on a
    /// directory it will never use.
    #[tokio::test]
    async fn resolve_memory_store_skips_disk_io_when_conway_memory_is_not_selected() {
        let cwd = tempfile::tempdir().expect("tempdir");
        let store = resolve_memory_store(cwd.path(), &[])
            .await
            .expect("must not fail when conway.memory is not selected");
        assert!(
            !cwd.path().join(".conway").join("memory").exists(),
            "resolve_memory_store must not create the durable store's directory when \
             conway.memory is unselected"
        );
        // Still a real, usable MemoryStore (InMemoryMemoryStore) -- an
        // unselected build's bundle() entry is unattached, but bundle()
        // itself still needs a concrete value to construct it with.
        let memory = conway::plugin::Memory {
            id: conway::MemoryId::new(),
            text: "unused".to_string(),
            created: chrono::Utc::now(),
            provenance: None,
        };
        store
            .put(memory)
            .await
            .expect("in-memory store accepts a put");
    }

    /// `resolve_memory_store` opens the REAL, durable `FsMemoryStore` at
    /// `<cwd>/.conway/memory` when `conway.memory` IS selected -- and what it
    /// writes is visible to an independently-opened `FsMemoryStore` over the
    /// same root afterward, the actual durability property this item exists
    /// to deliver (the CLI-level, separate-PROCESS version of the same claim
    /// is `tests/durable_memory.rs`, driven against the real compiled
    /// binary; this is the unit-level proof that `resolve_memory_store`
    /// itself wires the right root).
    #[tokio::test]
    async fn resolve_memory_store_opens_the_durable_store_when_selected_and_it_persists() {
        let cwd = tempfile::tempdir().expect("tempdir");
        let install_ids = vec![conway_plugin_memory::PLUGIN_ID.to_string()];
        let store = resolve_memory_store(cwd.path(), &install_ids)
            .await
            .expect("must open the durable store when conway.memory is selected");
        let memory = conway::plugin::Memory {
            id: conway::MemoryId::new(),
            text: "durable across opens".to_string(),
            created: chrono::Utc::now(),
            provenance: None,
        };
        let id = memory.id;
        store
            .put(memory)
            .await
            .expect("durable store accepts a put");

        // A SEPARATE `FsMemoryStore::open` (deliberately not going through
        // `resolve_memory_store` again -- this proves the ROOT is right and
        // durable, not that this crate's call-site count stays at one, which
        // `fs_memory_store_is_opened_from_exactly_one_call_site` already
        // checks on its own) over the identical root sees the write.
        let root = cwd.path().join(".conway").join("memory");
        let reopened = conway::memory::FsMemoryStore::open(root)
            .await
            .expect("reopen the same root");
        let recalled = reopened.get(&id).await.expect("get the remembered id");
        assert_eq!(
            recalled.text, "durable across opens",
            "a memory written through resolve_memory_store's store must be readable by a fresh \
             FsMemoryStore opened over the same root"
        );
    }

    /// Same wiring-only check, for `conway_plugin_names`: without its
    /// published id present in `bundle`, `[plugins].install =
    /// ["conway.names"]` resolves to an unknown-id error and `/conway.names.
    /// rename` reaches nothing -- the exact defect board item
    /// `01M0TV447NAJ1R06S455DZPP54` closed for `conway.trim`.
    #[test]
    fn bundle_carries_the_names_plugin_under_its_published_id() {
        let cwd = std::env::temp_dir().join("conway-first-party-plugins-bundle-test");
        let memory_store = Arc::new(conway_plugin_memory::InMemoryMemoryStore::new());
        let found = bundle(&cwd, memory_store, test_agent_names())
            .iter()
            .any(|p| p.manifest().id == conway_plugin_names::PLUGIN_ID);
        assert!(
            found,
            "the linked bundle must contain the names plugin under its published id, \
             otherwise `[plugins].install = [\"{}\"]` resolves to an unknown-id error",
            conway_plugin_names::PLUGIN_ID
        );
    }

    /// An operator who never named `conway.names` in `[plugins].install`
    /// must not get a file beside their `settings.json` -- the same
    /// "unused, cheap, no I/O" posture `resolve_memory_store` takes for an
    /// unselected build. Checked by observing that resolution succeeds and
    /// stores nothing durable, without ever naming a path: this test
    /// deliberately performs NO filesystem redirection, which is safe only
    /// because the unselected branch reaches no filesystem at all.
    #[test]
    fn an_unselected_names_plugin_resolves_to_a_store_that_touches_no_file() {
        let store = resolve_agent_names(&["conway.memory".to_string()])
            .expect("an unselected conway.names must never fail to resolve");
        let id = conway::AgentId::new();
        store
            .set(&id, "scout")
            .expect("the fallback store still works");
        assert_eq!(store.get(&id).as_deref(), Some("scout"));
        // The durable store would have written `default_store_path`'s file;
        // this one has no path at all, which is the property under test --
        // asserted by the fact that resolution never consulted the
        // environment for one (see `resolve_agent_names`'s early return).
    }

    /// `bundle` must hand the `conway.names` plugin the SAME store its
    /// caller holds, not a private one: a rename typed at
    /// `/conway.names.rename` has to be visible to the `Arc` the TUI reads
    /// through, or `/steer <name>` resolves to nothing. Proven by driving
    /// the plugin's own command and then reading the CALLER's `Arc`.
    #[tokio::test]
    async fn bundle_gives_the_names_plugin_the_callers_own_store() {
        let cwd = std::env::temp_dir().join("conway-first-party-plugins-bundle-test");
        let memory_store = Arc::new(conway_plugin_memory::InMemoryMemoryStore::new());
        let agent_names = test_agent_names();
        let plugins = bundle(&cwd, memory_store, agent_names.clone());
        let names_plugin = plugins
            .iter()
            .find(|p| p.manifest().id == conway_plugin_names::PLUGIN_ID)
            .expect("the names plugin is in the bundle");
        let rename = names_plugin
            .commands()
            .into_iter()
            .find(|c| c.spec().name == conway_plugin_names::COMMAND_NAME_RENAME)
            .expect("the names plugin declares a rename command");
        let agent = conway::AgentId::new();
        rename
            .invoke(conway::plugin::CommandCtx {
                focused_agent: agent,
                root_agent: agent,
                session_id: conway::SessionId::new(),
                args: "scout".to_string(),
            })
            .await;
        assert_eq!(
            agent_names.get(&agent).as_deref(),
            Some("scout"),
            "the plugin wrote into a store the caller cannot see -- the rename/resolve loop \
             would be broken"
        );
    }
}
