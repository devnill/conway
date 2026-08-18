//! `conway.memory`: a mutable [`MemoryStore`] injected into context by a
//! [`ContextHook`] (board item `01M09P2T8E5M292WMSMS64CVC4`).
//!
//! # This is a REWORK, not an extension
//!
//! This crate used to ship a [`Curator`](conway::plugin::Curator): a whole
//! SESSION was marked recallable by a `SessionMeta.labels` entry, and
//! `MemoryCurator::curate` recalled its records verbatim via
//! `ValidatedPath::derive_with`. That design was built by treating
//! DESIGN-context-path §11.7's "memory needs no storage of its own, no
//! retrieval semantics of its own, and no new port" as a REQUIREMENT rather
//! than the hypothesis it actually was, and the hypothesis failed on five
//! counts, in production:
//!
//! - **Marking was mistimed** -- a label is write-once at session creation,
//!   the one moment nobody can yet judge whether the conversation mattered.
//! - **No removal** -- nothing could un-remember.
//! - **Unbounded growth "solved" with an 8-record / 8192-byte cap** -- the
//!   real tell: the cap truncated a whole SESSION to 8 arbitrary records
//!   because a session was the wrong UNIT, and the old module doc filed
//!   that cap under "bounded by construction" -- a virtue's clothing on a
//!   growth problem.
//! - **Wrong granularity** -- marking a 200-turn session to recall one fact
//!   was a bookmark with a token budget.
//! - **Could not distil** -- path selection can only reference bytes that
//!   already exist as records; freeform text (a summary, a hand-typed note)
//!   was structurally unrepresentable.
//!
//! The type system itself confirms this rather than merely permitting the
//! rework: `CurateOutcome` is `{ Unchanged, Derived(Derivation), Failed }`,
//! and a `Derivation` can only be built from `ValidatedPath::derive`, which
//! can only reference nodes already on a resolved path. There is no
//! variant that can carry authored text with no backing record.
//!
//! # Why this is a `ContextHook`, not a `Curator` -- and why that is NOT a
//! contradiction of `conway_core::ports::curator`'s own module doc
//!
//! That doc (DESIGN §11.3) argues `ContextHook` is the WRONG seam *for
//! curation* -- and it still is. Curation edits WHICH records end up on the
//! resolved path; the mechanism it needs (byte-identical records, a
//! validated `Derivation`, refusal instead of silent repair) lives entirely
//! at the selection layer. Memory is not curation: it INJECTS content that
//! never was a record anywhere, and the only seam that can hand a hook
//! authored text with no backing record is exactly the one §11.3 rules out
//! for curation -- `ContextHook::before_request`, which runs POST-assembly
//! over already-rendered `PromptSegment`s and can add one carrying whatever
//! text the hook likes. This is the same move `Provenance::AgentDef`/
//! `Provenance::Skill` already make for a system prompt / skill fragment
//! that also never came from a logged record.
//!
//! **The curator seam stays in the tree, untouched and unused by this
//! plugin.** It is not wasted -- it is the right seam for
//! `conway.compaction` and any future record-granular selection policy;
//! this item does not remove it.
//!
//! # The store: `MemoryStore`, not `PathStore`
//!
//! `PathStore` (`conway-core`/`conway-session`) is the LAYERING inspiration
//! -- a port here, an implementation crate there -- but not the type
//! itself: `PathStore` is write-once and content-addressed over an
//! EXPANDED node list, and a memory is neither. `MemoryStore` (`conway::
//! plugin::MemoryStore`) is mutable, caller-id-addressed, and
//! removal-first-class -- see that trait's own doc (`crates/conway-core/
//! src/ports/memory_store.rs`) for the full port shape and the R1-R3
//! reasoning it carries.
//!
//! # R1 -- freeform text, no opinion, no summarisation
//!
//! `Memory::text` is an opaque `String`. This crate performs NO
//! summarisation of its own -- no model call, no imposed structure. The
//! `remember` tool below stores exactly the text the model (or whichever
//! caller invokes it) supplies.
//!
//! # R2 -- provenance is optional, and honestly attached where it is free
//!
//! `RememberTool::invoke` always attaches `MemoryProvenance { session:
//! ctx.session_id, range: None }` -- not because provenance is required
//! (the port's own `Option` says it is not: a hand-authored memory written
//! directly through `MemoryStore::put`, bypassing this tool, carries
//! `None`), but because `ToolCtx::session_id` makes the calling session
//! genuinely free to record, and omitting a fact this cheap to attach would
//! be pointless. `range: None` because a MODEL SYNTHESIZING "remember X" is
//! not citing one specific logged record -- attaching a `SeqRange` here
//! would be honesty theater, a specific-looking reference to nothing in
//! particular.
//!
//! # R3 -- mutable: put/get/list/remove, removal first-class
//!
//! `RememberTool` (put), `ListMemoriesTool` (list, so a caller can find
//! an id to forget), and `ForgetTool` (remove) are three separate tools,
//! deliberately: `list_memories` closes the loop `remember`/`forget` alone
//! would leave open (a caller cannot forget an id it cannot see), making
//! "mutable: add, remove, list" (R3) usable conversationally, not merely
//! satisfiable at the port level.
//!
//! # Open questions from the board item, decided here (reasoning restated
//! from `crates/conway-core/src/ports/memory_store.rs`'s own doc, which is
//! authoritative for #1)
//!
//! 1. **Scoping: GLOBAL.** See `MemoryStore`'s own module doc for the full
//!    reasoning; this crate reads one shared store with no per-session/
//!    per-project partition.
//! 2. **Injection point and budget.** See `MemoryInjectHook::before_request`:
//!    memories are inserted immediately before the first `ToolRegistry`
//!    segment (falling back to the front of the list if none is found --
//!    see that method's own doc), sorted oldest-first, and the walk stops
//!    the moment either [`MemoryConfig::max_memories`] or
//!    [`MemoryConfig::max_bytes`] would be exceeded -- the SAME
//!    stop-not-skip discipline the old curator's R4 cap used, restated at
//!    the injection layer since there is no `derive_with` here to enforce
//!    it beforehand.
//! 3. **Cache impact: memory segments join the STATIC tier, at the front.**
//!    `Provenance::Memory::is_static()`/`::tier()` both say `Static` --
//!    see that impl's own doc. A global, rarely-changing memory set is
//!    byte-identical across every sibling agent at any instant, exactly the
//!    property `AgentDef`/`Skill`/`ToolRegistry` already have, so placing it
//!    as early as possible in the assembled segment list maximises how much
//!    of the request stays a stable, cacheable prefix across turns -- it
//!    changes only when a memory is actually added/removed, never merely
//!    because a turn happened. This hook does NOT set `PromptSegment::
//!    cache_hint` on the injected segments: no shipped `ContextHook`
//!    (checked -- `conway-plugin-skills`' `SkillIndexHook` does not either)
//!    computes the internal, `PrefixKey`-consistent breakpoint a correct
//!    `cache_hint` would need, and a `ContextHook` running strictly after
//!    `ContextBuilder::build` has no way to reach that computation without
//!    a new, out-of-scope port surface. Position-based cache-friendliness
//!    is the answer THIS item ships; a `cache_hint`-aware one is a
//!    reportable follow-up, not a silent gap (see this crate's own
//!    completion report).
//! 4. **The write surface: a tool.** `RememberTool`/`ForgetTool`/
//!    `ListMemoriesTool`, all ordinary `Tool` implementations that
//!    capture their own `Arc<dyn MemoryStore>` at construction time (the
//!    SAME pattern `conway-plugin-skills`' `ReadSkillTool` uses for its
//!    shared skills map) rather than reaching for anything on `ToolCtx`:
//!    checked, and `ToolCtx` (`conway-core/src/ports/plugin.rs`) carries no
//!    `SessionStore`/store-shaped field of any kind, only `agent_id`,
//!    `session_id`, `cwd`, `chdir`, `cancel`, `events`, `subagents`,
//!    `plugin_events`, `config` -- so a design that needed `ToolCtx` to
//!    reach a store WOULD have been a reportable facade gap. It is not
//!    needed: constructor-captured `Arc` state is exactly how a plugin's
//!    tool and hook already share the skills map precedent above, and it
//!    is how [`MemoryPlugin`] wires its OWN tools/hook to the SAME store
//!    instance here.
//! 5. **Migration: replace internals, do not carry a compatibility path.**
//!    Nothing in production depended on the old plugin (never installed by
//!    default). Its 18 tests (label matching, ancestry exclusion, the R4
//!    caps, R5's "both halves or neither" tool-pairing filter) are DELETED,
//!    not ported: every one of them exercised policy specific to path
//!    selection (`SessionFilter { label }`, `base.nodes()` ancestry,
//!    `derive_with`'s orphan rules) that has no analog once memory is
//!    injected text with no backing record. Verified safe to delete, not
//!    merely assumed:
//!    - **Ancestry exclusion** existed because path selection could
//!      re-select a record already reachable from the calling session's
//!      own tree (a same-tree selection masquerading as memory,
//!      INTENT.md §5e). An injected `Provenance::Memory` segment is never a
//!      `PathOp::Include` of any node at all -- there is no tree for it to
//!      already be part of, so the failure mode this logic guarded against
//!      cannot occur here.
//!    - **"Both halves or neither"** (R5 of the old design) existed
//!      because `derive_with` can only refuse an orphan it can NAME, so a
//!      recalled `ToolUse` whose `ToolResultBlock` half was not also
//!      recalled would ship an unanswered tool call. An injected memory
//!      segment is plain `ContentBlock::Text`, never a `ToolUse`/
//!      `ToolResultBlock` -- the class of content this restriction existed
//!      to keep out of a derived path cannot appear here at all, and the
//!      hook guard (`crates/conway-runtime/src/context/hook_guard.rs`,
//!      `check_tool_call_coherence`) independently confirms it: a text-only
//!      segment has no `ToolUse`/`ToolResultBlock` block to orphan, so it
//!      trivially passes that check regardless -- proved, not merely
//!      asserted, by the guard's own test named two lines above.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use conway::plugin::{
    async_trait, ContentBlock, ContextHook, ContextHookCtx, ContextPayload, Memory,
    MemoryProvenance, MemoryStore, MemoryStoreError, PathArgs, PermissionClass, Plugin,
    PluginManifest, PromptSegment, Provenance, RenderKind, Role, Tool, ToolCall, ToolCategory,
    ToolCtx, ToolError, ToolOutput, ToolSpec, TruncationPolicy,
};
use conway::{MemoryId, ToolName};

/// This plugin's published manifest id -- a config author (or a first-party
/// bundle's own linking module) resolves `[plugins].install` entries
/// against this constant.
pub const PLUGIN_ID: &str = "conway.memory";

/// The bare name `RememberTool` registers under.
pub const REMEMBER_TOOL_NAME: &str = "remember";
/// The bare name `ForgetTool` registers under.
pub const FORGET_TOOL_NAME: &str = "forget";
/// The bare name `ListMemoriesTool` registers under.
pub const LIST_MEMORIES_TOOL_NAME: &str = "list_memories";

/// The default cap on how many memories one turn's hook will inject
/// (open question 2).
pub const DEFAULT_MAX_MEMORIES: usize = 64;
/// The default cap on total injected memory text, in bytes (open question
/// 2), measured as `Memory::text.len()` summed over the injected memories.
pub const DEFAULT_MAX_BYTES: usize = 16_384;

/// Constructor configuration for [`MemoryPlugin`]'s injection budget (open
/// question 2). Unlike the retired label-based design's `MemoryConfig`,
/// there is no selection POLICY left to configure here -- `list()` returns
/// every stored memory (open question 1: global scope) -- only the
/// injection-time budget. A size guard remains, but -- per the board
/// item's own framing -- it is no longer load-bearing for growth control;
/// `ForgetTool` is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryConfig {
    /// The maximum number of memories one turn's hook will inject.
    pub max_memories: usize,
    /// The maximum total (byte-length) of injected memory text in one
    /// turn.
    pub max_bytes: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            max_memories: DEFAULT_MAX_MEMORIES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// The `conway.memory` plugin: contributes one [`ContextHook`] (memory
/// injection) and three [`Tool`]s (`remember`/`forget`/`list_memories`),
/// all sharing one `Arc<dyn MemoryStore>` captured at construction time.
/// Installs through the SAME `Plugin::tools`/`Plugin::context_hooks`/
/// `with_plugin` surface every other plugin capability uses (GP-03) -- no
/// privileged first-party channel.
pub struct MemoryPlugin {
    store: Arc<dyn MemoryStore>,
    config: MemoryConfig,
}

impl MemoryPlugin {
    /// `store` is caller-constructed: an embedder wanting durable,
    /// filesystem-backed memory passes `conway::memory::FsMemoryStore::
    /// open(root).await` (behind the facade's `jsonl-store` feature); a
    /// caller that only needs process-lifetime memory (e.g. this crate's
    /// own tests, or a CLI bundle not yet wired for durability -- see this
    /// crate's own completion report) passes [`InMemoryMemoryStore`]
    /// instead. Either way this plugin is generic over the port, never the
    /// implementation (mirrors `SkillsPlugin::new` taking an
    /// already-loaded skills map rather than resolving a path itself).
    pub fn new(store: Arc<dyn MemoryStore>, config: MemoryConfig) -> Self {
        Self { store, config }
    }
}

impl Plugin for MemoryPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PLUGIN_ID.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            tools: vec![
                ToolName::new(REMEMBER_TOOL_NAME),
                ToolName::new(FORGET_TOOL_NAME),
                ToolName::new(LIST_MEMORIES_TOOL_NAME),
            ],
            required_host_caps: vec![],
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(RememberTool {
                store: self.store.clone(),
            }),
            Arc::new(ForgetTool {
                store: self.store.clone(),
            }),
            Arc::new(ListMemoriesTool {
                store: self.store.clone(),
            }),
        ]
    }

    fn context_hooks(&self) -> Vec<Arc<dyn ContextHook>> {
        vec![Arc::new(MemoryInjectHook {
            store: self.store.clone(),
            config: self.config.clone(),
        })]
    }
}

/// The injection half (open questions 2/3). See the module doc for the
/// full placement/budget/cache reasoning; this type's own
/// [`before_request`](ContextHook::before_request) doc walks the
/// mechanical steps in order.
struct MemoryInjectHook {
    store: Arc<dyn MemoryStore>,
    config: MemoryConfig,
}

/// Where an injected memory segment lands among an already-assembled
/// segment list: immediately before the FIRST `Provenance::ToolRegistry`
/// segment (architecture §5.3's `[2] ToolRegistry`), so it sits inside the
/// static preamble alongside `AgentDef`/`Skill`, ahead of the
/// per-session `Inherited`/volatile tail (open question 3). Absent a
/// `ToolRegistry` segment at all (a hand-built payload in a test, or a
/// future assembly shape that omits it), falls back to the very front of
/// the list -- still ahead of everything else, never dropped, never
/// appended to the volatile tail by accident.
fn insertion_index(segments: &[PromptSegment]) -> usize {
    segments
        .iter()
        .position(|s| matches!(s.provenance, Provenance::ToolRegistry { .. }))
        .unwrap_or(0)
}

#[async_trait]
impl ContextHook for MemoryInjectHook {
    /// 1. `store.list()` -- a store error is FAIL-OPEN: this hook has no
    ///    `Failed` outcome to report through (unlike a `Curator`), so an
    ///    unreadable store simply injects nothing this turn rather than
    ///    failing the request.
    /// 2. Deterministic order: oldest-`created` first, ties broken by
    ///    `MemoryId` (mirrors the retired curator's own R4 ordering, for
    ///    the same reason -- a stable, reproducible prefix of the
    ///    candidate list rather than store-iteration-order-dependent
    ///    selection).
    /// 3. Walk the sorted list, stopping (never skipping) the moment either
    ///    [`MemoryConfig::max_memories`] or [`MemoryConfig::max_bytes`]
    ///    would be exceeded -- open question 2's budget.
    /// 4. One `PromptSegment` per selected memory, `Role::System`,
    ///    `Provenance::Memory { id }` (R2's honest-attribution
    ///    requirement -- never a `UserPrompt`/`Assistant` provenance that
    ///    would masquerade injected content as a recalled record).
    /// 5. Splice the selected segments in at [`insertion_index`] (open
    ///    question 3) and return. Nothing selected -> `payload` returned
    ///    byte-identical to what was passed in.
    async fn before_request(
        &self,
        _ctx: &ContextHookCtx,
        mut payload: ContextPayload,
    ) -> ContextPayload {
        let mut memories = match self.store.list().await {
            Ok(memories) => memories,
            Err(_) => return payload,
        };
        memories.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));

        let mut selected: Vec<PromptSegment> = Vec::new();
        let mut bytes_used: usize = 0;
        for memory in memories {
            if selected.len() >= self.config.max_memories {
                break;
            }
            let size = memory.text.len();
            if bytes_used.saturating_add(size) > self.config.max_bytes {
                break;
            }
            bytes_used += size;
            selected.push(PromptSegment::new(
                Role::System,
                vec![ContentBlock::Text { text: memory.text }],
                Provenance::Memory { id: memory.id },
            ));
        }

        if selected.is_empty() {
            return payload;
        }

        let at = insertion_index(&payload.segments);
        for (offset, segment) in selected.into_iter().enumerate() {
            payload.segments.insert(at + offset, segment);
        }
        payload
    }
}

/// Args for `RememberTool`.
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct RememberArgs {
    /// The freeform text to remember. Stored verbatim -- this tool performs
    /// no summarisation (R1).
    text: String,
}

/// The write half of R3: stores `text` under a fresh [`MemoryId`], with
/// provenance naming the calling session (see the module doc's "R2" for
/// why this is always attached, `range: None`).
struct RememberTool {
    store: Arc<dyn MemoryStore>,
}

#[async_trait]
impl Tool for RememberTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(REMEMBER_TOOL_NAME),
            description: "Remember a piece of freeform text so it is available in later \
                          turns and sessions that share this memory store. Stored verbatim, with no \
                          summarisation."
                .to_string(),
            schema: schemars::schema_for!(RememberArgs),
            category: ToolCategory::Edit,
            permission: PermissionClass::RequiresApproval,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: RememberArgs =
            serde_json::from_value(call.arguments).map_err(|e| ToolError::InvalidArguments {
                detail: e.to_string(),
            })?;
        let memory = Memory {
            id: MemoryId::new(),
            text: args.text,
            created: Utc::now(),
            provenance: Some(MemoryProvenance {
                session: ctx.session_id,
                range: None,
            }),
        };
        let id = memory.id;
        Ok(match self.store.put(memory).await {
            Ok(()) => ToolOutput {
                blocks: vec![ContentBlock::Text {
                    text: format!("remembered (id: {id})"),
                }],
                is_error: false,
                truncation: TruncationPolicy::None,
                artifacts: Vec::new(),
            },
            Err(e) => ToolOutput {
                blocks: vec![ContentBlock::Text {
                    text: format!("could not remember: {e}"),
                }],
                // Model-visible feedback, never a hard Err/crash -- the
                // same fail-safe shape `conway-plugin-skills`' `ReadSkillTool`
                // uses for an unknown skill name.
                is_error: true,
                truncation: TruncationPolicy::None,
                artifacts: Vec::new(),
            },
        })
    }

    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }
}

/// Args for `ForgetTool`.
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ForgetArgs {
    /// The id of the memory to forget, as reported by `remember` or
    /// `list_memories`.
    id: String,
}

/// The removal half of R3, made first-class rather than an afterthought:
/// retires exactly one memory by id. This is the actual answer to the
/// retired design's unbounded-growth problem -- a cap that can only
/// truncate is not a growth answer; a caller that can name and remove
/// exactly the memory it no longer wants is.
struct ForgetTool {
    store: Arc<dyn MemoryStore>,
}

#[async_trait]
impl Tool for ForgetTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(FORGET_TOOL_NAME),
            description: "Forget (permanently remove) a previously remembered piece of text by \
                          its id. Call list_memories first if the id is not already known."
                .to_string(),
            schema: schemars::schema_for!(ForgetArgs),
            category: ToolCategory::Delete,
            permission: PermissionClass::RequiresApproval,
        }
    }

    async fn invoke(&self, call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: ForgetArgs =
            serde_json::from_value(call.arguments).map_err(|e| ToolError::InvalidArguments {
                detail: e.to_string(),
            })?;
        let id = match MemoryId::from_str(&args.id) {
            Ok(id) => id,
            Err(e) => {
                return Ok(ToolOutput {
                    blocks: vec![ContentBlock::Text {
                        text: format!("not a valid memory id: {e}"),
                    }],
                    is_error: true,
                    truncation: TruncationPolicy::None,
                    artifacts: Vec::new(),
                })
            }
        };
        Ok(match self.store.remove(&id).await {
            Ok(()) => ToolOutput {
                blocks: vec![ContentBlock::Text {
                    text: format!("forgot memory {id}"),
                }],
                is_error: false,
                truncation: TruncationPolicy::None,
                artifacts: Vec::new(),
            },
            Err(MemoryStoreError::NotFound { .. }) => ToolOutput {
                blocks: vec![ContentBlock::Text {
                    text: format!("no such memory: {id}"),
                }],
                is_error: true,
                truncation: TruncationPolicy::None,
                artifacts: Vec::new(),
            },
            Err(e) => ToolOutput {
                blocks: vec![ContentBlock::Text {
                    text: format!("could not forget {id}: {e}"),
                }],
                is_error: true,
                truncation: TruncationPolicy::None,
                artifacts: Vec::new(),
            },
        })
    }

    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }
}

/// Empty args for `ListMemoriesTool` -- a `schemars`-derivable unit
/// struct so the tool still publishes a (trivial) JSON Schema rather than
/// hand-rolling one.
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct ListMemoriesArgs {}

/// The longest memory text `list_memories` renders per line before eliding.
///
/// A LISTING bound, entirely separate from `MemoryConfig::max_bytes` (which
/// bounds what the hook INJECTS). Without it a single `list_memories` call
/// is unbounded: the injection path is budget-capped, but nothing capped a
/// listing, so enough un-forgotten memories would blow past any reasonable
/// single tool output. Review finding.
const LIST_DISPLAY_MAX_CHARS: usize = 240;

/// Elide `text` for DISPLAY only. The stored text is never touched (R1) --
/// this is a rendering decision in one tool, not a transformation of the
/// memory. Char-based, not byte-based, so it cannot split a UTF-8 sequence.
fn truncate_for_display(text: &str) -> String {
    if text.chars().count() <= LIST_DISPLAY_MAX_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(LIST_DISPLAY_MAX_CHARS).collect();
    format!("{head}... (elided in this listing; the stored memory is intact)")
}

/// The read half that closes R3's loop: without this, a caller could
/// `remember` and `forget` but never discover an id to forget by. Renders
/// each memory as one line: id, creation timestamp, and text (truncated in
/// display only if very long -- the STORED text is never touched).
struct ListMemoriesTool {
    store: Arc<dyn MemoryStore>,
}

#[async_trait]
impl Tool for ListMemoriesTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(LIST_MEMORIES_TOOL_NAME),
            description: "List every currently remembered piece of text, with its id and \
                          creation time, so a specific one can be targeted with forget."
                .to_string(),
            schema: schemars::schema_for!(ListMemoriesArgs),
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(match self.store.list().await {
            Ok(mut memories) => {
                memories.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));
                let text = if memories.is_empty() {
                    "no memories stored".to_string()
                } else {
                    memories
                        .iter()
                        .map(|m| {
                            format!(
                                "{} ({}): {}",
                                m.id,
                                m.created.to_rfc3339(),
                                truncate_for_display(&m.text)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                ToolOutput {
                    blocks: vec![ContentBlock::Text { text }],
                    is_error: false,
                    truncation: TruncationPolicy::None,
                    artifacts: Vec::new(),
                }
            }
            Err(e) => ToolOutput {
                blocks: vec![ContentBlock::Text {
                    text: format!("could not list memories: {e}"),
                }],
                is_error: true,
                truncation: TruncationPolicy::None,
                artifacts: Vec::new(),
            },
        })
    }

    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }
    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }
}

/// A process-lifetime, non-durable [`MemoryStore`] -- for a caller that
/// does not want (or, per this crate's own completion report, is not yet
/// wired for) filesystem persistence. Every acceptance property this port
/// defines (mutability, honest optional provenance, removal) holds
/// identically over this implementation; only DURABILITY across a process
/// restart is what it deliberately does not attempt -- that is
/// `conway::memory::FsMemoryStore`'s job (`conway-session`, re-exported
/// through the facade behind its `jsonl-store` feature).
#[derive(Default)]
pub struct InMemoryMemoryStore {
    memories: Mutex<HashMap<MemoryId, Memory>>,
}

impl InMemoryMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl MemoryStore for InMemoryMemoryStore {
    async fn put(&self, memory: Memory) -> Result<(), MemoryStoreError> {
        let mut guard = self.memories.lock().expect("memory store lock poisoned");
        if guard.contains_key(&memory.id) {
            return Err(MemoryStoreError::AlreadyExists { id: memory.id });
        }
        guard.insert(memory.id, memory);
        Ok(())
    }

    async fn get(&self, id: &MemoryId) -> Result<Memory, MemoryStoreError> {
        self.memories
            .lock()
            .expect("memory store lock poisoned")
            .get(id)
            .cloned()
            .ok_or(MemoryStoreError::NotFound { id: *id })
    }

    async fn list(&self) -> Result<Vec<Memory>, MemoryStoreError> {
        Ok(self
            .memories
            .lock()
            .expect("memory store lock poisoned")
            .values()
            .cloned()
            .collect())
    }

    async fn remove(&self, id: &MemoryId) -> Result<(), MemoryStoreError> {
        let mut guard = self.memories.lock().expect("memory store lock poisoned");
        if guard.remove(id).is_none() {
            return Err(MemoryStoreError::NotFound { id: *id });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    /// R1 holds at the display boundary: `list_memories` may elide for
    /// rendering, but the STORED text is untouched and still injected whole.
    #[tokio::test]
    async fn listing_elides_long_text_for_display_without_touching_the_stored_memory() {
        let long = "x".repeat(LIST_DISPLAY_MAX_CHARS * 3);
        let shown = truncate_for_display(&long);
        assert!(shown.chars().count() < long.chars().count(), "must elide");
        assert!(shown.contains("the stored memory is intact"));

        let short = "a short memory";
        assert_eq!(truncate_for_display(short), short, "short text is verbatim");

        // Multi-byte safety: eliding must never split a UTF-8 sequence.
        let wide = "\u{00e9}".repeat(LIST_DISPLAY_MAX_CHARS * 2);
        let _ = truncate_for_display(&wide); // would panic on a byte split

        // And the store keeps the full text regardless of how it renders.
        let store = std::sync::Arc::new(InMemoryMemoryStore::new());
        let m = Memory {
            id: MemoryId::new(),
            text: long.clone(),
            created: chrono::Utc::now(),
            provenance: None,
        };
        store.put(m.clone()).await.unwrap();
        assert_eq!(store.get(&m.id).await.unwrap().text, long);
    }

    use super::*;

    fn hook_ctx() -> ContextHookCtx {
        let agent_id = conway::AgentId::new();
        ContextHookCtx {
            agent_id,
            agent_path: vec![agent_id],
            session_id: conway::SessionId::new(),
            turn: 1,
            model: None,
            estimated_tokens: 0,
            artifacts: conway::plugin::ArtifactWriteHandle::noop(agent_id),
            tag: None,
        }
    }

    fn tool_registry_segment() -> PromptSegment {
        PromptSegment::new(
            Role::System,
            vec![ContentBlock::Text {
                text: "tools".to_string(),
            }],
            Provenance::ToolRegistry {
                hash: "deadbeef".to_string(),
            },
        )
    }

    fn user_prompt_segment(text: &str) -> PromptSegment {
        PromptSegment::new(
            Role::User,
            vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            Provenance::UserPrompt,
        )
    }

    fn payload(segments: Vec<PromptSegment>) -> ContextPayload {
        ContextPayload {
            segments,
            tools: Vec::new(),
        }
    }

    async fn seed(store: &InMemoryMemoryStore, text: &str, offset_secs: i64) -> MemoryId {
        let memory = Memory {
            id: MemoryId::new(),
            text: text.to_string(),
            created: chrono::DateTime::UNIX_EPOCH + chrono::Duration::seconds(offset_secs),
            provenance: None,
        };
        let id = memory.id;
        store.put(memory).await.unwrap();
        id
    }

    // ------------------------------------------------------------------
    // InMemoryMemoryStore: R3 -- put/get/list/remove.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn in_memory_store_put_get_list_remove() {
        let store = InMemoryMemoryStore::new();
        let id = seed(&store, "hello", 0).await;
        assert_eq!(store.get(&id).await.unwrap().text, "hello");
        assert_eq!(store.list().await.unwrap().len(), 1);
        store.remove(&id).await.unwrap();
        assert!(store.list().await.unwrap().is_empty());
        assert!(matches!(
            store.get(&id).await.unwrap_err(),
            MemoryStoreError::NotFound { .. }
        ));
    }

    #[tokio::test]
    async fn in_memory_store_rejects_a_second_put_under_the_same_id() {
        let store = InMemoryMemoryStore::new();
        let memory = Memory {
            id: MemoryId::new(),
            text: "a".to_string(),
            created: Utc::now(),
            provenance: None,
        };
        store.put(memory.clone()).await.unwrap();
        let err = store.put(memory).await.unwrap_err();
        assert!(matches!(err, MemoryStoreError::AlreadyExists { .. }));
    }

    /// A memory whose provenance names a session id that was never created
    /// anywhere is still fully valid and still listed -- `InMemoryMemoryStore`
    /// never consults any `SessionStore`, so there is nothing to fail even
    /// if the named session is long gone.
    #[tokio::test]
    async fn a_memory_with_a_dangling_session_reference_is_still_valid() {
        let store = InMemoryMemoryStore::new();
        let memory = Memory {
            id: MemoryId::new(),
            text: "orphaned provenance".to_string(),
            created: Utc::now(),
            provenance: Some(MemoryProvenance {
                session: conway::SessionId::new(),
                range: None,
            }),
        };
        store.put(memory.clone()).await.unwrap();
        assert_eq!(store.get(&memory.id).await.unwrap(), memory);
    }

    // ------------------------------------------------------------------
    // MemoryInjectHook: acceptance 1, 5, budget, ordering, insertion point.
    // ------------------------------------------------------------------

    /// Acceptance 1: a memory with NO provenance is injected.
    #[tokio::test]
    async fn a_memory_with_no_provenance_is_injected() {
        let store = Arc::new(InMemoryMemoryStore::new());
        seed(&store, "the deploy secret lives in vault", 0).await;
        let hook = MemoryInjectHook {
            store: store.clone(),
            config: MemoryConfig::default(),
        };
        let out = hook
            .before_request(&hook_ctx(), payload(vec![user_prompt_segment("hi")]))
            .await;
        let texts: Vec<String> = out
            .segments
            .iter()
            .flat_map(|s| s.content.iter())
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(texts.iter().any(|t| t.contains("deploy secret")));
    }

    /// Acceptance 5: an injected segment carries `Provenance::Memory`, never
    /// disguised as a recalled `UserPrompt`/`Assistant` record.
    #[tokio::test]
    async fn injected_segments_carry_honest_memory_provenance() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let id = seed(&store, "text", 0).await;
        let hook = MemoryInjectHook {
            store: store.clone(),
            config: MemoryConfig::default(),
        };
        let out = hook.before_request(&hook_ctx(), payload(vec![])).await;
        assert_eq!(out.segments.len(), 1);
        assert_eq!(out.segments[0].provenance, Provenance::Memory { id });
    }

    /// No memories stored -> the payload is untouched.
    #[tokio::test]
    async fn no_memories_leaves_the_payload_unchanged() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let hook = MemoryInjectHook {
            store,
            config: MemoryConfig::default(),
        };
        let input = payload(vec![user_prompt_segment("hi")]);
        let out = hook.before_request(&hook_ctx(), input.clone()).await;
        assert_eq!(out.segments.len(), input.segments.len());
    }

    /// Open question 3: memories are inserted before the `ToolRegistry`
    /// segment, not appended to the volatile tail.
    #[tokio::test]
    async fn a_memory_is_inserted_before_the_tool_registry_segment() {
        let store = Arc::new(InMemoryMemoryStore::new());
        seed(&store, "memory text", 0).await;
        let hook = MemoryInjectHook {
            store,
            config: MemoryConfig::default(),
        };
        let out = hook
            .before_request(
                &hook_ctx(),
                payload(vec![tool_registry_segment(), user_prompt_segment("hi")]),
            )
            .await;
        assert_eq!(out.segments.len(), 3);
        assert!(matches!(
            out.segments[0].provenance,
            Provenance::Memory { .. }
        ));
        assert!(matches!(
            out.segments[1].provenance,
            Provenance::ToolRegistry { .. }
        ));
        assert!(matches!(out.segments[2].provenance, Provenance::UserPrompt));
    }

    /// Absent a `ToolRegistry` segment at all, falls back to the front.
    #[tokio::test]
    async fn with_no_tool_registry_segment_a_memory_lands_at_the_front() {
        let store = Arc::new(InMemoryMemoryStore::new());
        seed(&store, "memory text", 0).await;
        let hook = MemoryInjectHook {
            store,
            config: MemoryConfig::default(),
        };
        let out = hook
            .before_request(&hook_ctx(), payload(vec![user_prompt_segment("hi")]))
            .await;
        assert!(matches!(
            out.segments[0].provenance,
            Provenance::Memory { .. }
        ));
    }

    /// Open question 2: `max_memories` caps the injected COUNT.
    #[tokio::test]
    async fn max_memories_caps_the_injected_count() {
        let store = Arc::new(InMemoryMemoryStore::new());
        for i in 0..5 {
            seed(&store, &format!("memory {i}"), i).await;
        }
        let hook = MemoryInjectHook {
            store,
            config: MemoryConfig {
                max_memories: 2,
                ..MemoryConfig::default()
            },
        };
        let out = hook.before_request(&hook_ctx(), payload(vec![])).await;
        assert_eq!(out.segments.len(), 2);
    }

    /// Open question 2: `max_bytes` caps the injected TEXT, stopping the
    /// walk rather than skipping an oversized one.
    #[tokio::test]
    async fn max_bytes_caps_the_injected_text() {
        let store = Arc::new(InMemoryMemoryStore::new());
        seed(&store, "short", 0).await;
        seed(&store, &"x".repeat(1000), 1).await;
        let hook = MemoryInjectHook {
            store,
            config: MemoryConfig {
                max_bytes: 10,
                ..MemoryConfig::default()
            },
        };
        let out = hook.before_request(&hook_ctx(), payload(vec![])).await;
        assert_eq!(out.segments.len(), 1);
    }

    /// Deterministic, oldest-first ordering.
    #[tokio::test]
    async fn memories_are_injected_oldest_first() {
        let store = Arc::new(InMemoryMemoryStore::new());
        seed(&store, "newer", 100).await;
        seed(&store, "older", 1).await;
        let hook = MemoryInjectHook {
            store,
            config: MemoryConfig::default(),
        };
        let out = hook.before_request(&hook_ctx(), payload(vec![])).await;
        let ContentBlock::Text { text } = &out.segments[0].content[0] else {
            panic!("expected text block");
        };
        assert_eq!(text, "older");
    }

    /// A store error (simulated via `remove` on an empty store returning
    /// `NotFound`, not applicable to `list` here -- instead assert the
    /// fail-open CONTRACT directly against a store whose `list` errors).
    struct FailingStore;
    #[async_trait]
    impl MemoryStore for FailingStore {
        async fn put(&self, _memory: Memory) -> Result<(), MemoryStoreError> {
            unreachable!("not exercised by this test")
        }
        async fn get(&self, id: &MemoryId) -> Result<Memory, MemoryStoreError> {
            Err(MemoryStoreError::NotFound { id: *id })
        }
        async fn list(&self) -> Result<Vec<Memory>, MemoryStoreError> {
            Err(MemoryStoreError::Io {
                detail: "simulated failure".to_string(),
            })
        }
        async fn remove(&self, id: &MemoryId) -> Result<(), MemoryStoreError> {
            Err(MemoryStoreError::NotFound { id: *id })
        }
    }

    #[tokio::test]
    async fn a_failing_store_fails_open_leaving_the_payload_unchanged() {
        let hook = MemoryInjectHook {
            store: Arc::new(FailingStore),
            config: MemoryConfig::default(),
        };
        let input = payload(vec![user_prompt_segment("hi")]);
        let out = hook.before_request(&hook_ctx(), input.clone()).await;
        assert_eq!(out.segments.len(), input.segments.len());
    }

    // ------------------------------------------------------------------
    // Tools: remember / forget / list_memories.
    // ------------------------------------------------------------------

    fn tool_ctx() -> ToolCtx {
        let agent_id = conway::AgentId::new();
        ToolCtx::for_test(
            agent_id,
            std::path::PathBuf::from("/tmp"),
            Arc::new(conway_testkit::FakeSubagentHost::new(agent_id)),
            Arc::new(conway_testkit::CollectingEventSink::new()),
        )
    }

    #[tokio::test]
    async fn remember_stores_text_with_the_calling_sessions_provenance() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let tool = RememberTool {
            store: store.clone(),
        };
        let ctx = tool_ctx();
        let session_id = ctx.session_id;
        let out = tool
            .invoke(
                ToolCall {
                    call_id: "c1".to_string(),
                    name: ToolName::new(REMEMBER_TOOL_NAME),
                    arguments: serde_json::json!({"text": "remember this"}),
                },
                ctx,
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        let memories = store.list().await.unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].text, "remember this");
        assert_eq!(
            memories[0].provenance,
            Some(MemoryProvenance {
                session: session_id,
                range: None,
            })
        );
    }

    #[tokio::test]
    async fn forget_removes_a_memory_and_it_stops_appearing() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let id = seed(&store, "gone soon", 0).await;
        let tool = ForgetTool {
            store: store.clone(),
        };
        let out = tool
            .invoke(
                ToolCall {
                    call_id: "c1".to_string(),
                    name: ToolName::new(FORGET_TOOL_NAME),
                    arguments: serde_json::json!({"id": id.to_string()}),
                },
                tool_ctx(),
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(store.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn forget_an_unknown_id_is_a_model_visible_error_not_a_panic() {
        let store = Arc::new(InMemoryMemoryStore::new());
        let tool = ForgetTool {
            store: store.clone(),
        };
        let out = tool
            .invoke(
                ToolCall {
                    call_id: "c1".to_string(),
                    name: ToolName::new(FORGET_TOOL_NAME),
                    arguments: serde_json::json!({"id": MemoryId::new().to_string()}),
                },
                tool_ctx(),
            )
            .await
            .unwrap();
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn list_memories_reports_every_stored_memory() {
        let store = Arc::new(InMemoryMemoryStore::new());
        seed(&store, "one", 0).await;
        seed(&store, "two", 1).await;
        let tool = ListMemoriesTool {
            store: store.clone(),
        };
        let out = tool
            .invoke(
                ToolCall {
                    call_id: "c1".to_string(),
                    name: ToolName::new(LIST_MEMORIES_TOOL_NAME),
                    arguments: serde_json::json!({}),
                },
                tool_ctx(),
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        let ContentBlock::Text { text } = &out.blocks[0] else {
            panic!("expected text block");
        };
        assert!(text.contains("one"));
        assert!(text.contains("two"));
    }

    // ------------------------------------------------------------------
    // Plugin surface.
    // ------------------------------------------------------------------

    #[test]
    fn manifest_id_and_tools_match_the_published_constants() {
        let plugin = MemoryPlugin::new(
            Arc::new(InMemoryMemoryStore::new()),
            MemoryConfig::default(),
        );
        let manifest = plugin.manifest();
        assert_eq!(manifest.id, PLUGIN_ID);
        assert_eq!(manifest.tools.len(), 3);
        assert_eq!(plugin.tools().len(), 3);
        assert_eq!(plugin.context_hooks().len(), 1);
    }
}
