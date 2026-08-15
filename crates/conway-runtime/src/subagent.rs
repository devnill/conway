//! `impl SubagentHost for Runtime` (architecture §4.6, §5.1, §5.2): the
//! cycle-breaking fork/spawn entry point every tool call and developer API goes
//! through (decision 2). Fork and spawn are both, mechanically, "create a child
//! session, resolve its starting context, attach it to the tree, and launch its
//! `AgentLoop`" — the only real difference between them is *how* the starting
//! context is resolved: a fork's `InheritedPrefix` is the parent's own
//! effective transcript up to the fork point (a fork inherits the WHOLE
//! context, never part of it: the ENTIRE context up to the fork point, not a
//! truncated slice); a spawn's context has no inherited prefix at all, by
//! design.
//!
//! ## `InheritedPrefix` and sibling sharing
//!
//! [`conway_core::transcript::TranscriptResolver`] resolves a *session's own*
//! effective transcript (ancestors' prefix, in full, concatenated with that
//! session's own records up to its current head) — it has no public method
//! that resolves an arbitrary ancestor at an arbitrary bound directly. This
//! module gets exactly that (the parent's prefix at `at_seq`, and nothing
//! of the child's own) by exploiting timing: right after `store.fork`
//! creates the child (and *before* this module appends the child's own
//! `ForkDirective` record — i.e. while the child has zero own records),
//! `resolver.resolve(store, &child)` necessarily walks up to the parent,
//! computes the parent's prefix at `at_seq`, and — because the child itself
//! owns nothing yet — returns exactly that prefix *as its own return
//! value* (the resolver's own short-circuit for `level_upto ==
//! LogSeq::ZERO` is a plain `Arc::clone`, not a fresh allocation, so this
//! is the very same `Arc` `resolve` just memoized under `(parent,
//! at_seq)`, not a second copy of it). `start` below uses that return
//! value directly as `InheritedPrefix::records`; it does not re-fetch
//! through `TranscriptResolver::peek_prefix` (a `#[doc(hidden)] pub`,
//! TEST-ONLY seam per that method's own doc — re-fetching through it here,
//! after already having the answer, would only add a theoretical race
//! against the shared LRU evicting the just-written entry before the
//! second lookup, for no benefit). Three siblings forked at the same
//! `(parent, at_seq)` each trigger the same cache key, so all three
//! `resolve` calls return `Arc::clone`s of the identical backing
//! allocation (`Arc::ptr_eq`) — sibling sharing falls out of
//! `conway-session`'s own memoization, with no second cache added here
//! (per this item's binding notes). Tests assert that sharing via
//! `peek_prefix` directly (a legitimate test-only use of the seam); this
//! module's own production path never calls it.
//!
//! ## `InheritedPrefix::from` at fork depth >= 2
//!
//! A grandchild's (or deeper descendant's) `InheritedPrefix.records` is the
//! WHOLE effective transcript up to the fork point — a fork inherits all of it
//! or none, never a slice — the root's own records, then every intermediate
//! ancestor's own records in turn, up to and including the immediate parent's —
//! concatenated in order, per `TranscriptResolver`'s "local units everywhere,
//! the inherited prefix always flows through in full" contract (that module's
//! own docs). The bundle is nonetheless stamped with a SINGLE
//! `InheritedPrefix.from`: the immediate parent's session id. That field means
//! "who handed me this context" — not "who originally authored each record" —
//! and `ContextBuilder` (`context/builder.rs`) carries that same single `from`
//! onto every `Provenance::Inherited` segment it produces from `records`,
//! regardless of which ancestor a given record actually originated in. This is
//! a deliberate, coordinator-ruled semantic (rework), not an oversight:
//! recovering true per-record authorship at arbitrary depth would require
//! per-record session tracking that does not exist upstream — neither
//! `conway_core::log::LogRecord` nor `conway_core::transcript`'s resolver carries an
//! originating-session field per record — which is out of this item's scope. It
//! is queued as a refinement question rather than attempted here.
//!
//! Once resolved, the `InheritedPrefix` is stored once on the child's
//! `AgentLoop` (`agent_loop::AgentLoop::inherited`) and never recomputed —
//! see that field's own doc for why later parent appends can never change
//! it (the fork is a snapshot; `conway-session`'s `fork.rs` enforces this by
//! construction, and `conway-session`'s memoized cache entries are
//! themselves immutable once written).
//!
//! ## `RuntimeError::InvalidSpec` (rejected specs)
//!
//! `conway_core::error::RuntimeError` carries an `InvalidSpec { detail }`
//! variant for exactly this: a caller-supplied `SubagentSpec` (or
//! `ResumeSpec`) that fails a runtime-side consistency check
//! `SubagentSpec::validate` cannot perform itself (that method does no I/O).
//! `invalid_spec` below is the one place this crate constructs it, reused
//! by both `start` (this file) and `runtime.rs`'s `resume_root` (the
//! resumed-`cwd` x persisted-`root` check) so every spec-shaped rejection in
//! this crate goes through the same helper. `conway_core::ports::subagent::
//! translate` maps it to `SubagentError::InvalidSpec`, which `conway-tools`
//! in turn surfaces to the model as `ToolError::InvalidArguments` -- a
//! mistake in the caller's own spec, not `Internal` infrastructure noise.
//!
//! Two OTHER gaps in this file remain "closest fit" `Tool(Internal)`
//! mappings, deliberately NOT routed through `InvalidSpec`, because neither
//! is a rejection of caller-supplied spec data: `already_attached`
//! (`tree.rs`) is a duplicate-`AgentId` invariant violation (a bug
//! elsewhere, never a normal spec mistake), and `WeakRuntimeHost::upgrade`'s
//! failure means the runtime itself has already been dropped (nothing about
//! the spec is wrong at all).
//!
//! Relatedly, the spec's "every child has a budget, by construction"
//! criterion describes a runtime check this item cannot perform: committed
//! `SubagentSpec::budget` is a non-`Option<Budget>` `Budget` value, and
//! `Budget::max_steps` is a required `u32` (default 40) with no "unset"
//! sentinel — there is no way for a spec to arrive here with an absent
//! budget or an absent `max_steps`. The property holds vacuously, by the
//! type, rather than by a runtime check added here.
//!
//! ## `steer` (supersedes this item's stub)
//!
//! Real mailbox delivery now backs `steer`.
//! added the trait's `caller` parameter and
//! changed `from`/`at_parent_seq` to derive from it directly -- see that
//! method's own doc.
//!
//! ## `CacheMode` is hardcoded, not caller-supplied
//!
//! Below, `AgentSpec::cache_mode` is still hardcoded `CacheMode::None` for
//! every fork/spawn child, same as `runtime.rs`'s `start_root`/resume-root —
//! this is deliberate, not a gap: see the prompt-caching item's resolution
//! (`attempt.rs`'s `attach_route_cache_hints`) for why. `ContextBuilder::
//! build` runs *before* routing resolves a concrete model, so `AgentSpec::
//! cache_mode` can only ever be a pre-routing placeholder here — the REAL
//! cache-hint attachment for every turn (root, fork, and spawn alike,
//! unconditionally, not gated on any caller-supplied `cache_mode`) happens
//! as a post-pass in `AttemptEngine::execute`, keyed on the ACTUALLY
//! resolved model's declared `Capabilities::cache` for each candidate in
//! the fallback chain. (`SubagentSpec::cache_hint`, the caller-intent field
//! this section used to describe as unconsumed here, was deleted outright
//! rather than wired to anything, since nothing anywhere ever read it
//! either -- the same conclusion `await_result` reached before it.)

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use chrono::Utc;
use conway_core::agent::{
    AgentDefRef, AgentMessage, AgentResult, AgentTreeSnapshot, AskOutcome, CancelMode,
    ResultStatus, SubagentMode, SubagentSpec,
};
use conway_core::capabilities::CacheMode;
use conway_core::config::DEFAULT_MAX_PARALLEL_TOOLS;
use conway_core::containment::{CanonicalRoot, Containment};
use conway_core::content::Usage;
use conway_core::error::{ConwayError, RuntimeError, ToolError};
use conway_core::event::Event;
use conway_core::ids::{AgentId, LogSeq, RoleAlias, SeqRange, SessionId};
use conway_core::log::{ForkOrigin, LogRecord, SessionMeta};
use conway_core::ports::SubagentHost;
use conway_core::provenance::Provenance;
use conway_core::segment::CacheTtl;
use futures::StreamExt;

use crate::agent_loop::{AgentLoop, AgentSpec};
use crate::context::{InheritedPrefix, SystemPromptSpec};
use crate::mailbox::{self, Mailbox};
use crate::runtime::Runtime;
use crate::tree::AgentNode;

#[async_trait]
impl SubagentHost for Runtime {
    /// Fork or spawn `spec` under `parent`, per architecture §5.1/§5.2:
    ///
    /// 1. Validate `spec` (mode/`agent_def` pairing — see the module doc on
    ///    why nothing further is checked).
    /// 2. Resolve `parent`'s session and its current head (`at_seq`), the
    ///    freeze point.
    /// 3. Fork: `store.fork(parent_session, at_seq, meta)` (exactly once,
    ///    copies zero records) then resolve the `InheritedPrefix` (see the
    ///    module doc). Spawn: `store.create(meta)`, recording
    ///    `ForkOrigin{parent, at_seq, mode: Spawn}` in the header purely so
    ///    the tree is reconstructible from headers alone — context
    ///    assembly ignores it (`inherited` stays `None`; `AgentLoop` always
    ///    reads a session's *own* records straight from the store,
    ///    regardless of what its header's `origin` says).
    /// 4. Append the head record: `LogRecord::ForkDirective` (fork) or
    ///    `LogRecord::UserTurn` (spawn) — `agent_loop::split_head` (///    unmodified) already turns either into the right `HeadSegment`.
    ///    **Skipped** when `spec.keep_alive` is set AND `spec.prompt` is
    ///    empty (the interactive keep-alive case, this item's addition): no
    ///    placeholder record is written and the child's `resume_gate` starts
    ///    `awaiting_prompt: true` instead, so it idles until the caller's
    ///    first real message arrives via `Runtime::prompt` — mirrors
    ///    `Runtime::start_root`'s own handling of a prompt-less root.
    /// 5. Attach to the tree (`Runtime::launch_agent` -> `AgentTree::attach`
    ///    emits `Event::AgentSpawned` for us — see the module doc's carried
    ///    note on why this code must not emit it a second time) and launch
    ///    the child's `AgentLoop` under the supervisor.
    async fn start(
        &self,
        caller: AgentId,
        parent: AgentId,
        mut spec: SubagentSpec,
    ) -> Result<AgentId, RuntimeError> {
        // `caller` must own
        // `parent` -- checked BEFORE anything else runs, exactly like
        // `steer`/`await_result`/`cancel`'s own `ensure_own_subtree` call --
        // so no cwd/root resolution, store I/O, or child attach happens for
        // a caller attempting to start a child under an agent outside its
        // own subtree. `caller == parent` (an agent starting a child of
        // itself, the ordinary case for every model-invoked tool call and
        // for a fresh root's own first fork/spawn) always passes trivially:
        // `ensure_own_subtree`'s `path(target).contains(caller)` includes
        // `target` itself.
        self.ensure_own_subtree(caller, parent)?;
        spec.validate().map_err(invalid_spec)?;

        let parent_session = self.agent_session(parent)?;
        let parent_meta = self.loop_deps().store.meta(&parent_session).await?;
        let at_seq = self.loop_deps().store.head(&parent_session).await?;

        // C1: the child's own cwd, resolved and validated ONCE here, then
        // used at BOTH inheritance sites below (`SessionMeta.cwd` and
        // `AgentLoop.cwd`) -- they must never diverge, or a session whose
        // header says one directory while its tools actually resolve
        // relative paths against another would be incoherent. `spec.cwd:
        // None` (still what `SubagentSpec::fork`/`::spawn` produce) means
        // "inherit the parent's cwd", exactly the behavior from before this
        // field existed. `Some(path)`: an absolute path is used as-is; a
        // relative path is resolved against the PARENT's cwd -- the child
        // has no cwd of its own yet to resolve against, and re-resolving a
        // relative override against the child's own (not-yet-existent) cwd
        // would be circular. See `conway_core::agent::SubagentSpec::cwd`'s
        // own doc for the full semantics this implements, including the "no
        // sandbox claim" caveat (this governs relative-path resolution
        // only; an absolute path, or a `..` that walks back out, still
        // escapes it -- the permission gate remains the real enforcement
        // layer). A nonexistent resolved path fails the spawn fast, below,
        // rather than starting a child whose tools would silently fail on
        // every relative path.
        let child_cwd = match spec.cwd.clone() {
            Some(cwd) if cwd.is_absolute() => cwd,
            Some(cwd) => parent_meta.cwd.join(cwd),
            None => parent_meta.cwd.clone(),
        };
        if tokio::fs::metadata(&child_cwd).await.is_err() {
            return Err(invalid_spec(ConwayError::Config {
                detail: format!("subagent cwd {} does not exist", child_cwd.display()),
            }));
        }

        // S3: the child's confinement root, resolved and validated ONCE
        // here (mirroring `child_cwd` immediately above), then persisted
        // verbatim onto `SessionMeta.root` -- the one and only inheritance
        // site this slice wires (see `conway_core::agent::SubagentSpec::
        // root`'s own doc: `AgentLoop` gains no root field yet, since
        // nothing enforces it against a tool call until a later slice).
        //
        // `spec.root: None` (still what `SubagentSpec::fork`/`::spawn`
        // produce, and the ONLY shape the facade's `ForkSpec` can express --
        // fork always inherits, never overrides) means "inherit the
        // parent's root, unchanged", including an unconfined parent, which
        // stays unconfined. `Some(requested)` resolves relative paths
        // against the PARENT's cwd (same base `child_cwd` above uses) and
        // canonicalizes the result; a root that does not canonicalize fails
        // the spawn, the same "fail fast" shape as the cwd check above.
        //
        // The inheritance algebra (spawn only reaches this branch, since a
        // fork's `SubagentSpec.root` is always `None` by construction on
        // every reachable path -- see the module doc's cwd precedent for why
        // this is enforced by facade encapsulation, not a mode branch here):
        // a `requested` root contained in the parent's own root (or a parent
        // with no root at all, i.e. nothing to narrow against yet) narrows
        // the child and is accepted; a `requested` root that is WIDER than,
        // or disjoint (sideways) from, the parent's own root FAILS the spawn
        // with a typed error naming both roots -- never silently clamped to
        // the parent's root (silent narrowing would turn an operator's
        // mistake into a working-but-not-what-was-asked-for configuration,
        // the same bug shape 0.5.0 fixed for pattern grants).
        // `Containment::Undecidable` ("can't check") is treated identically
        // to `Outside` at every decision point below -- see that type's own
        // doc for why "can't check" must never mean "allow".
        let effective_root: Option<CanonicalRoot> = match spec.root.clone() {
            None => match parent_meta.root.as_deref() {
                Some(parent_root_path) => {
                    Some(CanonicalRoot::new(parent_root_path).map_err(|err| {
                        invalid_spec(ConwayError::Config {
                            detail: format!(
                                "inherited root {} does not canonicalize: {err}",
                                parent_root_path.display()
                            ),
                        })
                    })?)
                }
                None => None,
            },
            Some(requested) => {
                // Min-1: resolve via the SHARED rule, the single implementation
                // every root check calls (absolute -> as-is, relative -> join
                // base, NUL -> None) instead of inlining two-thirds of it and
                // silently dropping the NUL guard. A relative root resolves
                // against the parent's cwd, exactly as before. A non-UTF-8 or
                // NUL-carrying root is a typed config rejection -- untrusted
                // input, never a panic.
                let requested_str = requested.to_str().ok_or_else(|| {
                    invalid_spec(ConwayError::Config {
                        detail: format!("subagent root {} is not valid UTF-8", requested.display()),
                    })
                })?;
                let resolved =
                    crate::permission::resolve_like_the_tool_will(&parent_meta.cwd, requested_str)
                        .ok_or_else(|| {
                            invalid_spec(ConwayError::Config {
                                detail: "subagent root contains a NUL byte the OS cannot resolve"
                                    .to_string(),
                            })
                        })?;
                let canonical_requested = CanonicalRoot::new(&resolved).map_err(|err| {
                    invalid_spec(ConwayError::Config {
                        detail: format!(
                            "subagent root {} does not canonicalize: {err}",
                            resolved.display()
                        ),
                    })
                })?;
                if let Some(parent_root_path) = parent_meta.root.as_deref() {
                    let parent_root = CanonicalRoot::new(parent_root_path).map_err(|err| {
                        invalid_spec(ConwayError::Config {
                            detail: format!(
                                "parent root {} does not canonicalize: {err}",
                                parent_root_path.display()
                            ),
                        })
                    })?;
                    match parent_root.contains(canonical_requested.as_path()) {
                        Containment::Inside => {}
                        Containment::Outside | Containment::Undecidable => {
                            return Err(invalid_spec(ConwayError::Config {
                                detail: format!(
                                    "subagent root {} is not contained within parent root {} \
                                     (a spawn's root may only narrow, never widen or move \
                                     sideways relative to its parent's)",
                                    canonical_requested.as_path().display(),
                                    parent_root.as_path().display(),
                                ),
                            }));
                        }
                    }
                }
                Some(canonical_requested)
            }
        };

        // C1+S3: cwd subset-of root, always. `child_cwd` above may have been
        // inherited from the parent OR overridden by `spec.cwd`; either way,
        // if this spawn ends up confined (`effective_root: Some`), the
        // child's cwd must not already fall outside it -- most concretely, a
        // spawn that narrows `root` without also narrowing `cwd` would
        // otherwise start a child whose own working directory sits outside
        // its own confinement before a single tool call ever runs.
        if let Some(root) = &effective_root {
            match root.contains(&child_cwd) {
                Containment::Inside => {}
                Containment::Outside | Containment::Undecidable => {
                    // Report the cwd in canonical form when it HAS one, so
                    // both operands of this comparison are displayed on the
                    // same footing -- a symlinked cwd rendered raw next to a
                    // canonical root reads as a mismatch between unrelated
                    // paths, which is exactly the wrong hint when the root
                    // check is what rejected it. The raw path is kept
                    // alongside when it differs, since that is the string the
                    // caller actually passed and has to correct. A cwd that
                    // does not canonicalize (the `Undecidable` arm can be
                    // reached that way) falls back to the raw path -- there is
                    // nothing better to show, and failing here to produce a
                    // prettier error would be absurd.
                    let canonical_cwd = child_cwd.canonicalize().ok();
                    let shown = match &canonical_cwd {
                        Some(c) if c != &child_cwd => {
                            format!("{} (resolved: {})", child_cwd.display(), c.display())
                        }
                        _ => child_cwd.display().to_string(),
                    };
                    return Err(invalid_spec(ConwayError::Config {
                        detail: format!(
                            "subagent cwd {} is outside its own root {}",
                            shown,
                            root.as_path().display()
                        ),
                    }));
                }
            }
        }
        let effective_root: Option<PathBuf> =
            effective_root.map(|root| root.as_path().to_path_buf());

        // [S1.5]: the child's own EFFECTIVE per-agent plugin config,
        // resolved and validated ONCE here (mirroring `effective_root`
        // immediately above) -- the PARENT's own LIVE effective value
        // (`Runtime::agent_plugin_config`, an in-memory `AgentHandle`
        // lookup, not a re-read of the parent's *persisted* `SessionMeta.
        // plugin_config`) narrowed by `spec.plugin_config`
        // (01M0321414SVRD60HEP074AFHG: `SessionMeta.plugin_config` now
        // exists and is what a RESUMED parent's live value is re-derived
        // from -- see `runtime/root.rs`'s `resume_root`; a LIVE, still-
        // running parent's `AgentHandle.plugin_config` is always exactly
        // the same value that was (or will be) persisted onto its own
        // header at construction, so reading the live copy here rather than
        // re-fetching the header is equivalent, and cheaper)
        // (`None` means "inherit unchanged", exactly `PluginConfig::
        // narrow`'s own contract) against every installed plugin's declared
        // narrowing rules (`PluginRegistry::narrowing_rules`). A key not
        // declared narrowable by any installed plugin, or a requested value
        // that would WIDEN what the parent already carries, fails the
        // spawn outright with a typed error naming the key -- never
        // silently clamped to the parent's value and never silently
        // honored, the same "fail the whole operation, don't guess"
        // discipline `effective_root`'s own widening check just above
        // already established.
        let parent_plugin_config = self.agent_plugin_config(parent)?;
        let child_plugin_config = Arc::new(
            parent_plugin_config
                .narrow(
                    spec.plugin_config.as_ref(),
                    self.loop_deps().registry.narrowing_rules(),
                )
                .map_err(|err| {
                    invalid_spec(ConwayError::Config {
                        detail: format!("subagent plugin_config: {err}"),
                    })
                })?,
        );

        let agent_id = AgentId::new();
        let mut agent_path = self.tree_ref().path(parent);
        agent_path.push(agent_id);

        // Fork-only inheritance fill: a
        // forked child inherits the PARENT's own `agent_def` when the call
        // site named none itself -- `conway_fork`, `ForkSpec::from`, and the
        // TUI's `bare_fork` all build `SubagentSpec::agent_def: None`
        // unconditionally today, so without this fill a fork off a
        // def-carrying agent silently dropped that def (system prompt, tool
        // selector, model pin) rather than continuing under it. Gated on
        // `spec.mode == Fork`: spawn is explicitly out of scope for this
        // ruling and must not inherit a def this way (a spawn with no
        // `agent_def` already has its own documented "inherit the parent's
        // role/model, but not a def" behavior -- see `SpawnSpec`'s doc --
        // and is left alone).
        //
        // This supersedes the near-identical fill `ask` (below) used to
        // perform on the parent's `agent_def` before calling this method:
        // `ask` is fork-only (the `spec.mode != Fork` guard at the top
        // of that method), so this Fork arm now covers every path that used
        // to need it, and the second copy was deleted rather than kept in
        // sync by hand.
        //
        // ORDERING, load-bearing for `result_contract` below: `def_was_inherited`
        // is captured from `spec.agent_def` BEFORE this fill mutates it, so
        // the `result_contract` computation further down can tell "the call
        // site named this def" apart from "this def only got here via
        // inheritance" and never source a contract from the latter. A result
        // contract is always declared at a call site (a tool's own
        // argument, a `ForkSpec`/`SpawnSpec` builder field, or by naming a
        // def) and is NEVER inherited -- see `AgentDef::result_contract`'s
        // own doc and this file's `result_contract` computation below for
        // the full reasoning (the contract chain is exactly two-deep;
        // sourcing from an inherited def would be a third, undocumented
        // step).
        let def_was_inherited = spec.mode == SubagentMode::Fork && spec.agent_def.is_none();
        if def_was_inherited {
            spec.agent_def = parent_meta.agent_def.clone().map(AgentDefRef);
        }

        let agent_def = spec
            .agent_def
            .as_ref()
            .and_then(|r| self.agent_defs().get(r.0.as_str()));
        let role = spec
            .role
            .clone()
            .or_else(|| agent_def.and_then(|d| d.role.clone()))
            // inherit the PARENT's role before any hardcoded fallback.
            // A fork inherits the parent's context, so it must route the same
            // way; the literal `"default"` below is not a configured role
            // alias in a normal config (config names its default via
            // `default_role`, e.g. "coder"), so falling back to it made every
            // roleless fork fail routing with `unknown role alias: default`.
            // `parent_meta.role` carries the parent's effective role (the root
            // got it from `config.default_role`; it propagates transitively),
            // so this reaches the literal only if the parent itself has none.
            .or_else(|| parent_meta.role.clone())
            .unwrap_or_else(|| RoleAlias::new("default"));
        let system_prompt = agent_def.map(|d| SystemPromptSpec {
            agent_def: d.name.clone(),
            text: d.system_prompt.clone(),
        });
        let tools = spec
            .tools
            .clone()
            .or_else(|| agent_def.map(|d| d.tools.clone()));
        let pin = agent_def.and_then(|d| d.model.clone());
        // Precedence: the explicit call-site contract (`spec.result_contract`
        // -- the model's `conway_fork`/`conway_spawn` `result_contract` arg, or an
        // embedder's `ForkSpec`/`SpawnSpec::result_contract` builder) wins
        // over the def's; the def supplies only the DEFAULT applied when the
        // call site left its own contract unset. This mirrors `tools` just
        // above (`spec.tools` shadows `agent_def.tools`) rather than `role`,
        // which additionally falls back to the parent -- a subagent's result
        // contract has no such "inherit from parent" step, so the fallback
        // chain here is exactly two-deep.
        //
        // **`def_was_inherited` carve-out:**
        // a fork's `agent_def` may now be the fork-only inheritance fill just
        // above rather than something the call site itself named. A result
        // contract is always declared AT a call site -- a tool argument, a
        // `ForkSpec`/`SpawnSpec` builder field, or by NAMING a def -- and is
        // never inherited merely because the def that happens to carry it
        // was. `def_was_inherited` guards exactly that: when true, `agent_def`
        // here is the SAME `AgentDef` the parent itself is running under, and
        // its `result_contract` is skipped entirely, never consulted, even
        // though `agent_def.tools`/`.model`/the system prompt (all resolved
        // above, from the very same `agent_def`) DO apply to this child. This
        // is deliberately NOT the same rule as the `ask_origin` carve-out
        // just below: that one exists because `AskOutcome` has no
        // `structured` field for a contract to validate at all; THIS one
        // applies to an ordinary fork child, whose `AgentResult.structured`
        // is both satisfiable and readable by the forker -- the contract is
        // skipped purely because "the def only got here by inheritance" is
        // not "the call site declared a contract".
        //
        // **`ask_origin.is_some()` carve-out** (/
        //): `spec.ask_origin` is `Some` for
        // exactly the two ask entry points -- `conway`'s `SessionHandle::ask`
        // (`AskOrigin::ModalAsk`) and the `conway_ask` tool
        // (`AskOrigin::ToolAsk`) -- and `None` for every `conway_fork`/
        // `conway_spawn` call, model-invoked or embedder-invoked, that
        // reaches this SAME `start` (`ask` composes `start`, it is not
        // a third primitive, so this is the one place both an `ask` and an
        // ordinary fork/spawn spec converge). It is therefore the correct
        // signal to gate on here, RELIABLY, without adding a parallel
        // "is this an ask" flag: it already means exactly that, everywhere
        // a `SubagentSpec` reaches this function.
        //
        // An ask child's result never reaches a caller who can read
        // `structured` at all -- `SubagentHost::ask` returns `AskOutcome`
        // (`text`/`usage`/`status`/`transcript_ref` only, no `structured`
        // field, `conway-core`'s `agent.rs`), and `SessionHandle::ask`'s
        // `TurnHandle` is driven the same way. A def-declared `result_contract`
        // can therefore only ever turn a good prose answer into a validation
        // failure for an ask child -- it can never satisfy anything a caller
        // reads back -- so it is NEVER sourced from the def here, regardless of
        // what the spawning def declares. An EXPLICIT call-site contract on an
        // ask spec (not reachable from either shipped ask entry point today --
        // both always pass `result_contract: None` -- but `SubagentSpec` is a
        // public library type an embedder could construct by hand with
        // `ask_origin: Some` AND `result_contract: Some` set directly) is
        // rejected with a typed error -- untrusted input is never silently
        // ignored -- rather than accepted and then structurally unsatisfiable.
        // Reuses `RuntimeError:: InvalidSpec` via the `invalid_spec` helper
        // already used at every other "reject the caller's own spec" site in
        // this file (see the module doc's "`RuntimeError::InvalidSpec`
        // (rejected specs)" section) rather than minting a new variant
        // paralleling `AskRequiresFork`'s shape one-for-one: `InvalidSpec`
        // already routes identically through `conway_core::ports::subagent::
        // translate` -> `SubagentError::InvalidSpec` ->
        // `ToolError::InvalidArguments`, so a dedicated variant would add a
        // second, structurally identical path for zero behavioral gain.
        let result_contract = if spec.ask_origin.is_some() {
            match spec.result_contract.clone() {
                Some(_) => {
                    return Err(invalid_spec(ConwayError::Config {
                        detail: "an ask spec (ask_origin is set) may not carry its own \
                                 result_contract: AskOutcome/TurnHandle never expose a \
                                 structured field for a caller to read, so a contract on an \
                                 ask child can only ever fail, never succeed"
                            .to_string(),
                    }));
                }
                None => None,
            }
        } else {
            spec.result_contract.clone().or_else(|| {
                if def_was_inherited {
                    None
                } else {
                    agent_def.and_then(|d| d.result_contract.clone())
                }
            })
        };

        let now = Utc::now();
        let mut meta = SessionMeta {
            id: SessionId::new(),
            agent_id,
            origin: None,
            agent_def: agent_def.map(|d| d.name.clone()),
            role: Some(role.clone()),
            created: now,
            cwd: child_cwd.clone(),
            labels: Vec::new(),
            // `ephemeral` flows straight from the caller's `SubagentSpec`: a
            // `conway_ask` fork (item d sets `spec.ephemeral = true`) stamps
            // `AgentSpawned`/`AgentFinished` with `ephemeral: true` via the
            // captured local below; legacy `conway_fork`/`conway_spawn` paths
            // build their `SubagentSpec` with `ephemeral: false`
            // (`SubagentSpec::fork`/`::spawn`'s own constructor default), so
            // they stay non-ephemeral exactly as before. The facade's `/ask`
            // (`conway`'s `SessionHandle::ask` B2) also comes
            // through THIS path with `spec.ephemeral = true` -- only
            // `start_root` (a root is never ephemeral, per spec point 4) and
            // `resume_root` (which re-stamps from the persisted header, for
            // any resumed session that was forked off ephemeral) set this
            // field from anywhere other than the spec.
            ephemeral: spec.ephemeral,
            // B5: the `/ask`-origin tag flows straight from the caller's
            // `SubagentSpec`, exactly like `ephemeral` above: the TUI's
            // modal `/ask` (`SessionHandle::ask`) stamps
            // `Some(AskOrigin::ModalAsk)`, the `conway_ask` tool stamps
            // `Some(AskOrigin::ToolAsk)`, everything else leaves `None`.
            // The TUI's crash-residue sweep (`Conway::
            // sweep_stale_modal_asks`) purges ONLY `ModalAsk`-tagged
            // leftovers -- a `ToolAsk` child's `EphemeralSessionRef`
            // artifact would dangle (see `conway_core::log::AskOrigin`).
            ask_origin: spec.ask_origin,
            // S3: the resolved-once `effective_root` computed above, always
            // already canonical -- see `SessionMeta::root`'s own doc for why
            // persisting the canonical form (rather than the raw, possibly
            // relative `spec.root`) is what lets a resumed session's
            // confinement survive a store round-trip unchanged.
            root: effective_root,
            // (S1.5 resume gap) The full, already-validated EFFECTIVE
            // per-agent plugin config computed above (`child_plugin_config`)
            // -- persisted verbatim, mirroring `root` immediately above, so
            // this child's own narrowing survives a resumed store
            // round-trip instead of silently reverting to the global
            // default (`SessionMeta::plugin_config`'s own doc).
            plugin_config: child_plugin_config.as_ref().clone(),
        };

        // Capture before `meta` is moved into `store.fork`/`store.create` below
        // -- the child's `ephemeral` flag is stamped into `AgentNode` (and thus
        // `Event::AgentSpawned`/`Event::AgentFinished`) verbatim from it.
        let ephemeral = meta.ephemeral;
        // S5: likewise captured before the move, so `AgentLoop::root` below
        // gets the exact same already-canonical (or `None`) value just
        // persisted onto `meta.root` -- no independent recomputation.
        let agent_loop_root = meta.root.clone();

        let (session_id, inherited, inherited_upto) = match spec.mode {
            SubagentMode::Fork => {
                // `meta.origin` is left `None`: `store.fork` sets it itself
                // from its own `parent`/`at` arguments (defaulting `mode` to
                // `Fork` when the caller's `meta.origin` was `None`) — see
                // `conway-session`'s `fork.rs`.
                let sid = self
                    .loop_deps()
                    .store
                    .fork(&parent_session, at_seq, meta)
                    .await?;

                // Resolving the (still record-empty) child forces
                // `(parent_session, at_seq)` into the resolver's cache and
                // hands the result straight back as `resolve`'s own return
                // value (`Arc::clone`, not a fresh allocation — see the
                // module doc): the child's own zero records mean the
                // resolver's `level_upto == LogSeq::ZERO` short-circuit
                // returns exactly the parent's memoized prefix. No
                // re-fetch through the `#[doc(hidden)]` `peek_prefix` test
                // seam is needed (or wanted — see the module doc for why a
                // second cache lookup here could theoretically race an LRU
                // eviction under concurrent forks).
                let records = self
                    .resolver()
                    .resolve(self.loop_deps().store.as_ref(), &sid)
                    .await?;
                let inherited = InheritedPrefix {
                    from: parent_session,
                    seq_range: SeqRange::new(LogSeq::ZERO, Some(at_seq)),
                    records,
                };
                (sid, Some(inherited), Some(at_seq))
            }
            SubagentMode::Spawn => {
                // Recorded for tree reconstructability only (see the
                // module doc) — context assembly never reads it.
                meta.origin = Some(ForkOrigin {
                    parent: parent_session,
                    at_seq,
                    mode: SubagentMode::Spawn,
                });
                let sid = self.loop_deps().store.create(meta).await?;
                (sid, None, None)
            }
        };

        // Interactive keep-alive children (bare TUI `/spawn`/`/fork`, WI's
        // "open an interactive session" item) are built with `keep_alive:
        // true` and an EMPTY prompt/directive -- the caller's first real
        // message arrives later, via `Runtime::prompt`. For exactly that
        // case this mirrors `start_root`'s own "no placeholder record, gate
        // the first iteration" handling for a prompt-less root (see that
        // method's doc): no empty head record is appended here, so the
        // child never runs a turn against blank input. A `keep_alive` spec
        // with a NON-empty prompt (a library consumer could construct one,
        // even though the TUI never does) still appends its head record and
        // runs its first turn immediately, same as before -- only
        // `keep_alive` alone means "idle after this turn ends", per
        // `AgentSpec::keep_alive`'s own doc; `keep_alive` PLUS an empty
        // prompt is what additionally means "idle from the very start".
        let starts_idle = spec.keep_alive && spec.prompt.is_empty();
        if !starts_idle {
            let head_record = match spec.mode {
                SubagentMode::Fork => LogRecord::ForkDirective {
                    seq: LogSeq::ZERO,
                    ts: now,
                    text: spec.prompt.clone(),
                    by: parent,
                    prov: Provenance::ForkDirective { by: parent },
                },
                SubagentMode::Spawn => LogRecord::UserTurn {
                    seq: LogSeq::ZERO,
                    ts: now,
                    text: spec.prompt.clone(),
                    prov: Provenance::UserPrompt,
                },
            };
            self.loop_deps()
                .store
                .append(&session_id, head_record)
                .await?;
        }

        let cancel = self.tree_ref().child_cancel_token(parent)?;
        let last_report = Arc::new(Mutex::new(None));
        let agent_spec = AgentSpec {
            system_prompt,
            skills: Vec::new(),
            tools,
            role: role.clone(),
            pin,
            budget: spec.budget.clone(),
            // Pre-routing placeholder -- see this module's doc, "`CacheMode`
            // is hardcoded, not caller-supplied".
            cache_mode: CacheMode::None,
            cache_ttl: CacheTtl::FiveMinutes,
            headroom_override: None,
            max_parallel_tools: DEFAULT_MAX_PARALLEL_TOOLS,
            report_slot: Some(last_report.clone()),
            // carried `spec.result_contract` straight through as a
            // plain value handoff. This item adds the def as a second,
            // lower-precedence source: `result_contract` (computed above)
            // is the call site's contract when the caller supplied one,
            // else the spawning `AgentDef`'s own `result_contract` -- see
            // that computation's own comment for the full precedence rule.
            result_contract,
            // Threaded straight from the spec (WI keep-alive item): a
            // fork/spawn child that `await_result`s (`AgentTree::
            // await_result`) still depends on `keep_alive: false`
            // (`SubagentSpec::fork`/`::spawn`'s own constructor default,
            // unchanged) actually terminating on `Completed`; keep-alive is
            // an explicit opt-in only an interactive-session caller (the
            // TUI's bare `/spawn`/`/fork`, via `conway`'s `SpawnSpec::
            // keep_alive`/`ForkSpec::keep_alive`) sets.
            keep_alive: spec.keep_alive,
            // threaded straight
            // through, unread by this loop -- see `AgentSpec::tag`'s own
            // doc for the "conway never interprets this" guarantee.
            tag: spec.tag.clone(),
        };

        // this child's own mailbox, plus the already-attached
        // parent's mailbox sender, so `AgentLoop::finish` can deliver this
        // child's terminal `Result` upward (architecture §3.2: "child
        // terminates -> AgentResult -> parent mailbox").
        let (mailbox_tx, mailbox_rx) = Mailbox::new(mailbox::RUNTIME_CAPACITY);
        let mailbox_tx = mailbox_tx.with_events(
            self.loop_deps().bus.clone(),
            session_id,
            agent_id,
            cancel.clone(),
        );
        let parent_mailbox = self.agent_mailbox(parent)?;

        let agent_loop = AgentLoop {
            agent_id,
            session: session_id,
            parent: Some(parent),
            agent_path,
            cwd: child_cwd,
            root: agent_loop_root,
            plugin_config: child_plugin_config,
            deps: self.loop_deps().clone(),
            spec: agent_spec,
            cancel: cancel.clone(),
            inherited,
            inbox: mailbox_rx,
            parent_mailbox: Some(parent_mailbox),
            pending_cancel: None,
            // only `Runtime::resume_root` and (this item's
            // addition) an interactive keep-alive child with no initial
            // prompt gate a loop's first iteration -- every other fork/spawn
            // child still starts ungated (`Default`), exactly as before.
            // `launch_agent` clones this SAME `notify` out before spawning
            // the task (see that method's own comment), so
            // `Runtime::prompt`/`SessionHandle::prompt_agent` wake this
            // child's gated first iteration precisely as they already wake
            // a resumed root's.
            resume_gate: if starts_idle {
                crate::agent_loop::ResumeGate {
                    awaiting_prompt: true,
                    ..Default::default()
                }
            } else {
                crate::agent_loop::ResumeGate::default()
            },
        };

        let node = AgentNode {
            id: agent_id,
            parent: Some(parent),
            session: session_id,
            kind: Some(spec.mode),
            agent_def: agent_def.map(|d| d.name.clone()),
            role: Some(role),
            budget: spec.budget,
            cancel,
            inherited_upto,
            // `meta.ephemeral` is `spec.ephemeral` (see the literal above): a
            // `conway_ask` fork carries `ephemeral: true` end-to-end through
            // `AgentNode` and thus `Event::AgentSpawned`/`Event::AgentFinished`;
            // a legacy `conway_fork`/`conway_spawn` (`SubagentSpec::fork`/
            // `::spawn`, `ephemeral: false` by construction) keeps `false` all
            // the way through, exactly as before this field existed.
            ephemeral,
        };

        self.launch_agent(node, agent_loop, last_report, mailbox_tx)?;

        // The live `Event::UserTurn` twin (this item) of the `LogRecord::
        // UserTurn` head record appended above for a `Spawn` with a
        // non-empty initial prompt (a library caller's own `SpawnSpec`, or
        // -- the common production case -- the model-invoked
        // `conway_fork`/`conway_spawn` tool, which always supplies a
        // real prompt and never sets `keep_alive`). Emitted AFTER
        // `launch_agent` (which calls `AgentTree::attach`, and thus, since
        // `node.kind` is `Some(spec.mode)` here, already emitted
        // `Event::AgentSpawned` for `agent_id` above) rather than inline
        // with the append a few lines up: that append happens BEFORE this
        // child is attached to the tree, so emitting the live event at the
        // append site (mirroring `Runtime::prompt`'s own placement, which
        // is always ordering-safe because ITS target is already attached)
        // would invert the "`AgentSpawned` precedes every event for its
        // agent" guarantee for exactly this one path -- this is the
        // pre-spawn-ordering hazard this item's completion notes disclose.
        // A `Fork`'s own head record (`ForkDirective`) has no `Event`
        // counterpart yet (see `record_to_event`'s doc for why that's this
        // item's own deliberate, disclosed decision) so nothing is emitted
        // here for that branch.
        if !starts_idle && spec.mode == SubagentMode::Spawn {
            self.loop_deps().bus.emit(
                session_id,
                agent_id,
                Event::UserTurn {
                    text: spec.prompt.clone(),
                    prov: Provenance::UserPrompt,
                },
            );
        }

        // `child_spawned`, fired after
        // the child exists and is attached, for BOTH modes -- this method is
        // the single entry point for fork and spawn alike, which is why the
        // event is wired here rather than at either caller.
        //
        // STRICTLY OBSERVE-ONLY, AND WHETHER IT MAY EVER DENY IS AN OPEN
        // QUESTION, DELIBERATELY DEFERRED. Unlike `post_tool_use` -- where the
        // call has already run, so there is nothing left to refuse -- nothing
        // structurally prevents a spawn from being refused. It is not refusable
        // here because refusing raises questions that item did not scope: what
        // does the parent agent see when its own spawn is denied, a tool error
        // or a silent no-op? Does the caller need new error handling? Those are
        // not answered by giving this dispatch a denial-shaped return type and
        // leaving the semantics to whoever meets it first. `dispatch` returns
        // `()`, so a failing hook cannot fail the spawn, and the deferral is
        // recorded rather than made by omission -- the same reasoning that
        // keeps `PluginManifest` free of an unwired `on_init`.
        if self
            .observation_dispatcher()
            .will_dispatch(crate::hook_dispatch::CHILD_SPAWNED)
        {
            self.observation_dispatcher()
                .dispatch(
                    crate::hook_dispatch::CHILD_SPAWNED,
                    serde_json::json!({
                        "child_id": agent_id,
                        "parent": parent,
                        "caller": caller,
                        "mode": spec.mode,
                        "session": session_id,
                    }),
                )
                .await;
        }

        Ok(agent_id)
    }

    /// Delivers `text` into `target`'s mailbox as an `AgentMessage::Steer`,
    /// landing at `target`'s next turn boundary (architecture
    /// §6.2). `caller` must be
    /// `target` itself or one of its ancestors (`ensure_own_subtree`,
    /// below) -- a sibling (or any other unrelated agent) is rejected with
    /// `RuntimeError::AgentNotInSubtree` before the mailbox is even
    /// resolved. `from`/`at_parent_seq` are now derived from `caller`
    /// DIRECTLY (never from `target`'s own tree parent, the pre-fix
    /// behavior this item replaces): deriving attribution from `target`
    /// side-stepped the very check above and made a forged steer
    /// indistinguishable, from the recipient's side, from a genuine parent
    /// instruction. A child steering ITS OWN parent (the other direction
    /// bidirectionality requires) does not go through this method at all:
    /// it already holds its own `agent_id` and its `parent_mailbox` sender
    /// directly (`AgentLoop`), and can send correctly-attributed messages
    /// without needing this trait's help -- see `tests/steering.rs` for
    /// both directions exercised at the mailbox level, and this module's
    /// own doc's "`steer` (supersedes this item's stub)" section for
    /// the prior gap this replaces.
    async fn steer(
        &self,
        caller: AgentId,
        target: AgentId,
        text: String,
    ) -> Result<(), RuntimeError> {
        self.ensure_own_subtree(caller, target)?;
        let mailbox = self.agent_mailbox(target)?;
        let at_parent_seq = self
            .loop_deps()
            .store
            .head(&self.agent_session(caller)?)
            .await?;
        mailbox.send(AgentMessage::Steer {
            from: caller,
            text,
            at_parent_seq,
        });
        Ok(())
    }

    /// Delegates to [`crate::tree::AgentTree::await_result`], which already
    /// provides every guarantee this method needs (unknown agent ->
    /// `AgentNotFound`; a finished agent's result returned immediately; no
    /// tree lock held across the await). `caller` must own `target`
    /// (`ensure_own_subtree`, below) -- checked BEFORE delegating, so a
    /// sibling can never block on, or read the result of, another branch's
    /// run.
    async fn await_result(
        &self,
        caller: AgentId,
        target: AgentId,
    ) -> Result<AgentResult, RuntimeError> {
        self.ensure_own_subtree(caller, target)?;
        self.tree_ref().await_result(target).await
    }

    /// `mode: CancelMode::Immediate`
    /// delegates to the existing `Runtime::cancel`, whose
    /// signature already matches this trait method's immediate path
    /// exactly. `mode: CancelMode::Graceful` instead enqueues
    /// `AgentMessage::Cancel { hard: false, .. }` into `target`'s own
    /// mailbox -- the same three-step shape `steer` (above) already uses
    /// (`ensure_own_subtree` -> `agent_mailbox` -> `mailbox.send`), just
    /// with a `Cancel` message instead of a `Steer` one. `caller` must
    /// own `target` (`ensure_own_subtree`, below) -- checked BEFORE either
    /// path, so a sibling can never destroy another branch's work, hard or
    /// soft.
    async fn cancel(
        &self,
        caller: AgentId,
        target: AgentId,
        reason: String,
        mode: CancelMode,
    ) -> Result<(), RuntimeError> {
        self.ensure_own_subtree(caller, target)?;
        match mode {
            CancelMode::Immediate => Runtime::cancel(self, target, reason),
            CancelMode::Graceful => {
                let mailbox = self.agent_mailbox(target)?;
                mailbox.send(AgentMessage::Cancel {
                    from: caller,
                    reason,
                    // `CancelMode::hard()` rather than a `false` literal: the
                    // mode-to-flag mapping has one home, so a third variant
                    // cannot be added without this callsite following it.
                    hard: mode.hard(),
                });
                Ok(())
            }
        }
    }

    /// `caller`'s own subtree, projected from `Runtime::tree`'s whole
    /// snapshot (adds the
    /// scoping). Deliberately built from the FULL snapshot rather than
    /// walking `tree_ref().path(..)` once per node: `path` and `snapshot`
    /// both take the same tree read lock, and a single `snapshot()` plus an
    /// in-memory parent-chain walk over its own (already-collected) nodes
    /// avoids re-acquiring that lock per node. Unknown `caller` (not
    /// attached to this runtime at all) yields an empty subtree with `root:
    /// caller` -- mirrors `AgentTree::path`'s own "empty for unknown"
    /// convention rather than erroring, since this method returns no
    /// `Result` (unchanged trait shape).
    fn tree(&self, caller: AgentId) -> AgentTreeSnapshot {
        let full = Runtime::tree(self);
        let parent_of: std::collections::HashMap<AgentId, Option<AgentId>> = full
            .nodes
            .iter()
            .map(|node| (node.agent_id, node.parent))
            .collect();
        let in_callers_subtree = |agent: AgentId| -> bool {
            let mut cursor = agent;
            loop {
                if cursor == caller {
                    return true;
                }
                match parent_of.get(&cursor) {
                    Some(Some(parent)) => cursor = *parent,
                    _ => return false,
                }
            }
        };
        let nodes = full
            .nodes
            .into_iter()
            .filter(|node| in_callers_subtree(node.agent_id))
            .collect();
        AgentTreeSnapshot {
            root: caller,
            nodes,
            at: full.at,
        }
    }

    /// The real `Runtime::ask` impl: fork+await-text (`ask` is exactly
    /// the two existing primitives composed, NOT a third one). Mirrors
    /// `conway`'s facade `SessionHandle::ask`/`TurnHandle::text`/`result`
    /// (`crates/conway/src/session_handle.rs:165`, `:985-1050`) but uses the
    /// raw `EventBus` broadcast receiver directly so conway-runtime does not
    /// depend on the `conway` facade crate.
    ///
    /// ## Subscribe BEFORE launch
    ///
    /// `Runtime::subscribe` (which delegates to `self.bus.subscribe()`) is
    /// called BEFORE `self.start(parent, spec)` so the child's first
    /// `Event::TextDelta` cannot be missed (the full text is what the
    /// orchestrator feeds onward; a missed first delta would silently truncate
    /// it). This is the same ordering `SessionHandle::prompt_agent` uses
    /// (subscribe before append/launch).
    ///
    /// ## Agent-id-checked drain
    ///
    /// The drain accumulates every `Event::TextDelta` whose `envelope.agent`
    /// is the child, captures `usage` from the first `Event::TurnFinished`,
    /// and resolves ONLY on an `Event::AgentFinished` whose `result.agent_id`
    /// equals the child -- a SIBLING's (or any other agent's) `AgentFinished`
    /// MUST NOT resolve this drain (mirrors `TurnHandle::text`/`result`'s
    /// agent-id guard at `session_handle.rs:998`/`:1052`). The raw bus delivers
    /// every agent's envelopes unfiltered (unlike the facade's `EventStream`,
    /// which scopes by session/agent), so the `envelope.agent == child_agent`
    /// top-level filter is what keeps a sibling's `TextDelta`/`ThinkingDelta`
    /// out of this `AskOutcome::text`; the `result.agent_id == child_agent`
    /// match guard is the spec-mandated belt-and-braces check on the terminal
    /// event.
    ///
    /// ## Cancellation
    ///
    /// `ask` has no `ctx` parameter, so the drain loop cannot observe a parent
    /// `CancellationToken` directly. It does not need to: `start` already
    /// wires the child's cancel token to the parent's via
    /// `tree.child_cancel_token(parent)` (architecture §3.2), so if the
    /// parent is cancelled the child is cancelled too, the child's
    /// `AgentLoop` emits `Event::AgentFinished { result: AgentResult { status:
    /// Cancelled, .. }, .. }` (via the supervisor's grace-timeout
    /// synthesized finish if the loop itself does not), and the drain
    /// resolves on that -- returning `AskOutcome { status: Cancelled, .. }`.
    /// No special-casing is needed here.
    ///
    /// ## `transcript_ref`
    ///
    /// The child's `SessionId` is resolved via `Runtime::agent_session`, the
    /// same agent-to-session lookup `start` itself uses (via `agents` map,
    /// populated by `launch_agent`). Provenance is mandatory, so it is carried
    /// in `AskOutcome` and the orchestrator's `ToolResultRecord` can name the
    /// ephemeral child session.
    ///
    /// ## `caller`
    ///
    /// `ask` performs no subtree check of its own -- it composes `start`
    /// (`ask` is fork+await-text, not a third subagent primitive), so passing
    /// `caller` straight through to `self.start(caller, parent, spec)` below
    /// is what enforces "`caller` must own `parent`" here too, reusing
    /// `start`'s own `ensure_own_subtree` call rather than duplicating it.
    /// Before this item, `ask` took only `parent` and forked THAT agent's
    /// context directly with no ownership check at all -- the cross-tree
    /// exfiltration this item's own module doc names: `tree()` (itself
    /// unguarded pre-fix) to find a sibling's `AgentId`, then
    /// `ask(sibling, ..)` to fork the sibling's entire context and read the
    /// reply back as plain model output.
    async fn ask(
        &self,
        caller: AgentId,
        parent: AgentId,
        spec: SubagentSpec,
    ) -> Result<AskOutcome, RuntimeError> {
        // `ask` is fork+await-text -- the fork-only invariant is
        // enforced here at the trait boundary (not only at the `conway_ask`
        // tool callsite, which happens to always construct `Fork` itself),
        // so no other caller -- including a future out-of-process plugin
        // supplying JSON, not a trusted in-process Rust type -- can bypass
        // it with a `Spawn` spec. A real (non-`debug_assert!`) check: a
        // `debug_assert!` compiles to nothing in release builds, which is
        // every binary a user runs, so it left this invariant unenforced
        // outside debug. A malformed spec is untrusted input: a typed error, never a
        // panic -- `assert!`/`unwrap`/`expect` are not used here.
        if spec.mode != SubagentMode::Fork {
            return Err(RuntimeError::AskRequiresFork { mode: spec.mode });
        }
        //, MOVED (decision
        //): the `conway_ask` tool
        // (`crates/conway-tools/src/subagent/ask.rs`) always builds its
        // `SubagentSpec` with `agent_def: None` -- it has no
        // `SessionMeta`/`AgentDef` lookup surface of its own, only a
        // `ToolCtx`. This method used to fill that in itself, HERE, from the
        // PARENT's own `SessionMeta::agent_def`; that fill has been deleted
        // in favor of `start`'s own Fork-only inheritance fill (see that
        // method's doc, right before its `agent_def` resolution) -- `ask` is
        // fork-only (the `spec.mode != Fork` guard just above), so passing
        // `spec.agent_def: None` straight through to `self.start` below now
        // reaches the SAME fallback fill an ordinary def-less `conway_fork`
        // does, with no second copy to keep in sync by hand. A caller that
        // DOES supply `spec.agent_def` itself (an embedder's own `ForkSpec`,
        // hypothetically) is unaffected either way -- both this method's old
        // fill and `start`'s new one are fallbacks, never overrides.
        //
        // Before either fill existed, an ask child got NO agent_def at all:
        // no system-prompt segment (it silently read a transcript authored
        // by an agent it is not), and -- since an absent `spec.tools` PLUS
        // an absent `agent_def.tools` resolves to `PluginRegistry::specs`'s
        // `selector.is_none_or(..)` "no selector -> everything" fallback --
        // the FULL tool registry rather than the parent def's own
        // restrictive selector: a capability escalation one `conway_ask`
        // hop away from a def-restricted parent. `spec.tools` (a caller-
        // narrowing arg, e.g. `AskArgs::tools`) still takes precedence over
        // whatever the filled-in `agent_def.tools` supplies -- unchanged,
        // see `start`'s own `tools` precedence.
        //
        // 1. Subscribe BEFORE launch so the first TextDelta is not missed.
        let mut stream = Runtime::subscribe(self);
        // 2. Launch the child (fork per `spec.mode`; `ask` is fork-only --
        // there is no third primitive). `caller` flows straight through:
        // `start`'s own `ensure_own_subtree(caller, parent)` is this method's
        // ONLY ownership check -- see this method's own doc. `spec.ask_origin`
        // is set by the caller (`ToolAsk`/`ModalAsk`) before it reaches here --
        // `start`'s own `ask_origin.is_some()` carve-out (see that method's
        // `result_contract` computation) is what keeps a def-declared
        // `result_contract`, now reachable via `start`'s own Fork-only
        // `agent_def` inheritance fill, from ever governing this child.
        let child_agent = self.start(caller, parent, spec).await?;
        // 3. Resolve the child's SessionId for `transcript_ref`, which is
        //    what keeps the child's provenance resolvable. The
        //    child is already attached (start -> launch_agent -> tree.attach
        //    -> agents map populated), so this lookup cannot miss.
        let child_session = self.agent_session(child_agent)?;

        // 4. Drain events from the live bus until the child's terminal event.
        let mut text = String::new();
        let mut status = ResultStatus::Completed;
        // Cumulative usage across the child's whole run, taken from the
        // terminal `AgentResult` (`agent_loop` accumulates per-turn usage
        // into `state.usage` and builds the result from it). This is more
        // correct than capturing a single `TurnFinished`'s usage (which
        // would be just one turn's slice -- first or last depending on
        // capture policy); `AskOutcome.usage` should account for the whole
        // ephemeral run.
        let mut usage = Usage::default();
        let mut saw_finish = false;
        while let Some(envelope) = stream.next().await {
            // Top-level agent filter: the raw bus delivers every agent's
            // envelopes; keep only this child's. (`AgentFinished` is
            // re-checked by `result.agent_id` below as the spec-mandated
            // terminal guard, but every other event variant is gated only
            // here.)
            if envelope.agent != child_agent {
                continue;
            }
            match envelope.event {
                Event::TextDelta { text: delta } => text.push_str(&delta),
                // Agent-id-checked terminal: a sibling's finish (different
                // `result.agent_id`) is filtered above by `envelope.agent`,
                // and this guard is the spec-required belt-and-braces check
                // (mirrors `TurnHandle::text`/`result`).
                Event::AgentFinished { result, .. } if result.agent_id == child_agent => {
                    status = result.status;
                    usage = result.usage;
                    saw_finish = true;
                    break;
                }
                _ => {}
            }
        }

        if !saw_finish {
            // The bus stream ended without the child's `AgentFinished`: only
            // happens once the runtime itself is dropped. Mirror
            // `TurnHandle::result`'s terminal-error shape.
            return Err(RuntimeError::AgentNotFound { agent: child_agent });
        }

        Ok(AskOutcome {
            text,
            usage,
            status,
            transcript_ref: child_session,
        })
    }
}

impl Runtime {
    /// The descendancy check
    /// `steer`/`await_result`/`cancel` share -- see `SubagentHost`'s own
    /// doc for the root/operator-exemption mechanism (there is none; the
    /// check below is uniform for every caller, and a root-originated call
    /// passes it because a root's subtree is its whole session by
    /// construction). Checked structurally against the live tree, via the
    /// same `tree_ref().path(..)` walk `start` (above) already uses to
    /// build a new child's `agent_path`: `path(target)` is `[root, ...,
    /// target]` when `target` is known (`caller` passes when it appears
    /// anywhere in that chain -- itself included, so acting on one's own
    /// id is always allowed), and `[]` when `target` is not known to this
    /// runtime at all (`AgentTree::path`'s own doc).
    ///
    /// `target` unknown entirely -> `RuntimeError::AgentNotFound`, matching
    /// what every one of these three methods already returned for an
    /// unknown `target` before this check existed (`steer` via
    /// `agent_mailbox`, `await_result` via `AgentTree::await_result`,
    /// `cancel` via `Runtime::cancel`, all resolve the same lookup this
    /// check performs first, so an unknown id is diagnosed identically
    /// either way). `target` known but outside `caller`'s subtree ->
    /// `RuntimeError::AgentNotInSubtree`, never a panic (both ids are untrusted and may
    /// be model-supplied).
    fn ensure_own_subtree(&self, caller: AgentId, target: AgentId) -> Result<(), RuntimeError> {
        let path = self.tree_ref().path(target);
        if path.is_empty() {
            return Err(RuntimeError::AgentNotFound { agent: target });
        }
        if path.contains(&caller) {
            return Ok(());
        }
        Err(RuntimeError::AgentNotInSubtree { caller, target })
    }
}

/// See the module doc. `SubagentSpec::validate()`'s own error type is
/// `ConwayError::Config`; this maps it to `RuntimeError::InvalidSpec`, the
/// one place this crate constructs that variant. `pub(crate)` (not private)
/// so `runtime.rs`'s `resume_root` (S3: the resumed-`cwd` × persisted-`root`
/// check) reuses this exact error surface too, rather than inventing a
/// parallel one.
pub(crate) fn invalid_spec(err: ConwayError) -> RuntimeError {
    RuntimeError::InvalidSpec {
        detail: format!("invalid SubagentSpec: {err}"),
    }
}

/// A thin, non-owning delegate to `Runtime`'s real `SubagentHost` impl
/// (above), used only to break the `Runtime -> LoopDeps -> subagents`
/// reference cycle a literal `Arc<Runtime>` in `LoopDeps::subagents` would
/// create: `Runtime::new` must return the very `Arc<Runtime>` it also hands
/// every agent task (via `LoopDeps`) for tool dispatch, and storing a
/// *strong* copy of that same `Arc` inside the `Arc<LoopDeps>` every one of
/// those tasks also holds would mean `Runtime` never drops, even once every
/// external handle and every agent task is gone.
///
/// `Runtime::new` builds this from the `Weak<Runtime>` `Arc::new_cyclic`
/// hands its constructor closure (see that method's doc), so `upgrade`
/// fails only once every strong `Arc<Runtime>` — including the runtime's
/// own agent tasks' clones — has already been dropped: a runtime that is
/// still doing anything at all still upgrades successfully.
///
/// Deliberately not `pub`: `impl SubagentHost for Runtime` above is this
/// crate's one true implementation (satisfying the criterion that it has
/// "no additional public methods"); this type is construction plumbing, not
/// part of the crate's public surface.
pub(crate) struct WeakRuntimeHost(Weak<Runtime>);

impl WeakRuntimeHost {
    pub(crate) fn new(runtime: Weak<Runtime>) -> Self {
        Self(runtime)
    }

    fn upgrade(&self) -> Result<Arc<Runtime>, RuntimeError> {
        self.0.upgrade().ok_or_else(|| {
            RuntimeError::Tool(ToolError::Internal {
                detail: "subagent host unavailable: the runtime has already been dropped"
                    .to_string(),
            })
        })
    }
}

#[async_trait]
impl SubagentHost for WeakRuntimeHost {
    async fn start(
        &self,
        caller: AgentId,
        parent: AgentId,
        spec: SubagentSpec,
    ) -> Result<AgentId, RuntimeError> {
        self.upgrade()?.start(caller, parent, spec).await
    }

    async fn steer(
        &self,
        caller: AgentId,
        target: AgentId,
        text: String,
    ) -> Result<(), RuntimeError> {
        self.upgrade()?.steer(caller, target, text).await
    }

    async fn await_result(
        &self,
        caller: AgentId,
        target: AgentId,
    ) -> Result<AgentResult, RuntimeError> {
        self.upgrade()?.await_result(caller, target).await
    }

    async fn cancel(
        &self,
        caller: AgentId,
        target: AgentId,
        reason: String,
        mode: CancelMode,
    ) -> Result<(), RuntimeError> {
        // `Runtime` has its own inherent, sync `cancel` method
        // that method resolution prefers over this trait method of the
        // same name -- fully qualified syntax forces the trait impl above.
        SubagentHost::cancel(&*self.upgrade()?, caller, target, reason, mode).await
    }

    /// Delegates to the real `Runtime` impl. The `ask` primitive is
    /// fork+await-text, not a third primitive, surfaced on the same
    /// `SubagentHost` trait every consumer uses -- a built-in gets no
    /// privileged API a third-party plugin lacks.
    async fn ask(
        &self,
        caller: AgentId,
        parent: AgentId,
        spec: SubagentSpec,
    ) -> Result<AskOutcome, RuntimeError> {
        self.upgrade()?.ask(caller, parent, spec).await
    }

    fn tree(&self, caller: AgentId) -> AgentTreeSnapshot {
        match self.upgrade() {
            Ok(runtime) => SubagentHost::tree(&*runtime, caller),
            // Mirrors `runtime.rs`'s (now-removed) `NoSubagentHost::tree`
            // fallback shape for the one case where there is genuinely no
            // runtime left to ask. `root: caller` (not `AgentId::default()`)
            // -- makes `tree()`'s
            // `root` mean "the caller's own subtree root" everywhere else;
            // this one remaining fallback stays consistent with that rather
            // than reverting to a placeholder default.
            Err(_) => AgentTreeSnapshot {
                root: caller,
                nodes: Vec::new(),
                at: Utc::now(),
            },
        }
    }
}
