//! `Conway::classify_agent_intent` (board item C1): natural-language intent
//! classification for the TUI's `/fork` and `/spawn` commands, run as an
//! EPHEMERAL one-turn session routed under the declarative `intent` role.
//!
//! The facade capability lives here (P-8); rendering the result is the
//! TUI's concern (C2), and classification is deliberately NOT a new
//! subagent primitive (P-1) — it is one ordinary ephemeral spawn, one turn,
//! then a purge.
//!
//! ## Configuration (purely declarative — ZERO router code, no models.json)
//!
//! The classifier routes through the ordinary role machinery under the
//! alias `intent`. The repo has no bundled/default config file
//! (`config::schema`'s `roles` map defaults to empty, and `docs/crates/
//! conway.md` only documents the `settings.json` schema), so — per this
//! item's own fallback — the snippet is documented here instead of added
//! to a defaults file. In `settings.json`:
//!
//! ```json
//! {
//!   "roles": {
//!     "intent": { "chain": ["<backend>/<a-cheap-fast-model>"] }
//!   }
//! }
//! ```
//!
//! When no `[roles.intent]` entry is configured, classification degrades to
//! a VERBATIM passthrough (see the fallback section below) and no session
//! is ever created.
//!
//! ## Mechanism choices (the item's "READ the spawn path first" instruction)
//!
//! - **Session shape:** `SubagentHost::start(parent, SubagentSpec { mode:
//!   Spawn, ephemeral: true, keep_alive: false, role: Some("intent"), .. })`
//!   — the same machinery and the same shape B2's `SessionHandle::ask` uses
//!   for its ephemeral one-turn child, with two deliberate differences:
//!   the mode is **Spawn, not Fork** (classification must NOT inherit the
//!   parent's conversation — a fork would ship the whole session context to
//!   a cheap model for no benefit and would taint the classification with
//!   it), and `ask_origin` is **None** (see below).
//! - **System prompt — the prompt-prefix mechanism:** the spawn path
//!   resolves a system prompt ONLY from a registered `AgentDef`
//!   (`conway-runtime`'s `subagent.rs`: `agent_def -> SystemPromptSpec`),
//!   and the def registry is fixed at `Runtime::new` (loaded from
//!   `agents.dir` at build time; no inline/temp def registration API exists
//!   anywhere in the workspace). Requiring users to author an
//!   intent-classifier def file was rejected as far MORE invasive than the
//!   alternative the codebase already supports: the classification
//!   instructions are PREPENDED to the user's text in the spawn's single
//!   `UserTurn` head record ([`classification_prompt`]). A `result_contract`
//!   (structured-output enforcement) was also considered and rejected: its
//!   one-retry-then-`Rejected` semantics would turn a malformed classifier
//!   reply into a hard error, where this item's settled behavior is a
//!   graceful passthrough (see the validation policy below).
//! - **Tools:** `Some(ToolSelector::Only(vec![]))` — the classifier gets
//!   ZERO tools. It must answer from the prompt alone, cannot wander into
//!   tool calls, and (since `has_tools` is false) imposes no tool-calling
//!   capability requirement on the routed model.
//! - **`AskOrigin: None`:** the enum is ask-specific (`ModalAsk` |
//!   `ToolAsk`); an intent session is neither. Confirmed against
//!   `Conway::sweep_stale_modal_asks`: it purges ONLY `ModalAsk`-tagged
//!   sessions, so untagged intent sessions can never be swept — and they
//!   never NEED sweeping, because this module purges the session inline on
//!   every exit path (below). Disclosed residual: a process crash in the
//!   narrow window between `start` and `remove` leaks one untagged
//!   ephemeral session that no sweep will ever reclaim (the same shape as
//!   any pre-`AskOrigin` leftover); it stays hidden from default listings.
//! - **`budget`:** `max_steps: 2`, no deadline/token caps. With zero tools
//!   the classifier can only answer in one step; the slack step is
//!   belt-and-braces. `cache_hint: false` matches `SubagentSpec::spawn`'s
//!   own constructor (the hint is never correctness-bearing).
//!
//! ## Unconfigured-role fallback (the item's `UnknownRole` catch)
//!
//! `config.roles` IS the exact role set the builder-compiled
//! `DeclarativeRouter` was built from (`ConwayBuilder::build` step 6-7), so
//! an absent `intent` alias is precisely the case
//! `conway-routing/src/router.rs:232` would raise
//! `RoutingError::UnknownRole` for — except the raise would happen INSIDE
//! the agent loop, which folds routing failures into a `Failed`
//! `AgentResult` (`agent_loop`'s `finish_error`), where no typed
//! `RoutingError` is recoverable. The catch therefore happens PRE-FLIGHT:
//! an absent alias returns a verbatim passthrough `AgentIntent` (`recipe =
//! default_recipe`, `agent_def = None`, `prompt = text` unchanged) WITHOUT
//! creating any session. This is also the only role knowledge reachable
//! from the facade when the router was INJECTED (`ConwayBuilder::
//! with_router` leaves `router_explain: None`), keeping behavior identical
//! across both builder paths. Disclosed residual: with an injected router
//! that DISAGREES with the config (config has `intent`, the injected router
//! does not), the intent turn itself fails and surfaces as
//! [`ConwayError::IntentClassification`] rather than a passthrough — an
//! injected-router/config mismatch is a build-time integration error, not
//! an "unconfigured role". Every OTHER error (store I/O, def loading,
//! backend/routing failure inside the turn) propagates unchanged.
//!
//! ## P-10 validation policy (model output is untrusted)
//!
//! The reply is parsed STRICTLY and every field is validated before it can
//! reach the caller ([`parse_reply`]):
//!
//! 1. **Unparseable reply** (not one JSON object after trimming and
//!    stripping at most one ``` ``` ``` code fence) → verbatim passthrough.
//!    Classification is an assist; a confused cheap model must never break
//!    `/fork`/`/spawn`.
//! 2. **`recipe` missing or not `fork`/`spawn`** (compared
//!    case-insensitively after trimming) → verbatim passthrough with the
//!    CALLER's `default_recipe`. The recipe is the primary output; a
//!    classifier that cannot get the enum right is not trusted with the
//!    prompt rewrite either.
//! 3. **`prompt` missing or empty after trimming** → verbatim passthrough
//!    (the classifier returned nothing usable; the raw text is the honest
//!    prompt).
//! 4. **`agent_def` naming a def that is not configured** → STRIPPED to
//!    `None`, recipe and prompt kept (reject-vs-strip, decided: strip). A
//!    hallucinated def name must never reach the caller as a valid
//!    `AgentIntent`, but the def is an OPTIONAL garnish — `null` is the
//!    common valid answer — so an otherwise-valid classification degrades
//!    field-locally instead of failing wholesale. The configured set is
//!    re-read from `agents.dir` at classify time via
//!    [`crate::agents::load_agent_defs`], the same loader the builder used
//!    (disclosed residual: the dir is re-resolved against `config.cwd` at
//!    classify time, so a relative `cwd` combined with a process `chdir`
//!    between build and classify could read a different directory; the TUI
//!    never chdirs). Unknown JSON fields are ignored (lenient); the three
//!    fields this module reads are the strict part.
//!
//! ## Deferred (explicitly out of scope per the item)
//!
//! Oneshot (`-p`) NL intent classification is NOT in this epic — this
//! facade capability is built for the interactive `/fork`/`/spawn` path
//! only.

use std::collections::{HashMap, HashSet};
use std::future::poll_fn;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use conway_core::agent::{
    AgentResult, Budget, ResultStatus, SubagentMode, SubagentSpec, ToolSelector,
};
use conway_core::config::AgentDef;
use conway_core::error::RuntimeError;
use conway_core::event::Event;
use conway_core::ids::{AgentId, RoleAlias};
use conway_core::ports::{SessionStore, SubagentHost};
use conway_runtime::runtime::Runtime;
use futures_core::Stream;

use crate::config::ConwayConfig;
use crate::error::{ConwayError, Result};
use crate::event_stream::EventStream;

/// The declarative role alias the classifier routes under — see the module
/// doc for the `settings.json` snippet and the unconfigured-role fallback.
const INTENT_ROLE: &str = "intent";

/// The classifier's budget: one answer step plus slack, nothing else. With
/// zero tools it cannot legitimately spend more; the caps stay unset so the
/// cheap model is never cut off mid-answer by an arbitrary token ceiling.
const INTENT_BUDGET: Budget = Budget {
    max_steps: 2,
    deadline: None,
    max_tokens: None,
    max_tool_calls: None,
};

/// The classification instructions, prepended to the user's text in the
/// intent session's single `UserTurn` (the prompt-prefix mechanism — see
/// the module doc for why this, not an `AgentDef` system prompt).
const INSTRUCTIONS: &str = "\
You are an intent classifier inside an agent harness. The user typed the \
natural-language request quoted at the end of this message. Decide how to \
realize it and reply with EXACTLY one JSON object and nothing else — no \
prose, no markdown code fences:

{\"recipe\": \"fork\" | \"spawn\", \"agent_def\": \"name\" | null, \"prompt\": \"...\"}

- \"recipe\": \"fork\" when the request depends on the current \
conversation's context (a forked agent inherits the entire conversation); \
\"spawn\" when it is self-contained (a spawned agent starts with a clean \
slate).
- \"agent_def\": the name of one of the configured agent definitions listed \
below that clearly matches the request, or null when none does. NEVER \
invent a name that is not listed.
- \"prompt\": the user's request rewritten as a complete, self-contained \
instruction for the new agent.";

/// The settled output of `Conway::classify_agent_intent` (C1): how to
/// realize the user's natural-language `/fork`/`/spawn` request.
///
/// `recipe` reuses [`SubagentMode`] (already `Fork | Spawn`, already
/// re-exported from this crate's root) rather than a parallel intent-only
/// enum, so there is no second Fork/Spawn type to drift out of sync with
/// the command layer that consumes this.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentIntent {
    /// How to realize the request: `Fork` (inherit the conversation) or
    /// `Spawn` (clean slate). On every degraded path this is the CALLER's
    /// command default (the `default_recipe` argument), never a value this
    /// module invents.
    pub recipe: SubagentMode,
    /// The configured agent def the new agent should run under, validated
    /// against `agents.dir` (P-10): a name the model hallucinated is
    /// stripped to `None` here, never passed through.
    pub agent_def: Option<String>,
    /// The prompt for the new agent — the classifier's self-contained
    /// rewrite of the user's text on the classified path, or the user's
    /// RAW text, unchanged, on every passthrough path.
    pub prompt: String,
}

/// The verbatim passthrough: today's command behavior, used for the
/// unconfigured-role fallback and every untrusted-output degradation (see
/// the module doc's P-10 policy).
fn passthrough(default_recipe: SubagentMode, raw_text: &str) -> AgentIntent {
    AgentIntent {
        recipe: default_recipe,
        agent_def: None,
        prompt: raw_text.to_string(),
    }
}

/// The whole classification flow, taken out of `Conway` so `conway.rs`
/// carries only the one-method delegation this item is scoped to add
/// there. `rt`/`store`/`config` are the owning `Conway`'s own fields,
/// passed explicitly.
pub(crate) async fn classify(
    rt: &Arc<Runtime>,
    store: &Arc<dyn SessionStore>,
    config: &ConwayConfig,
    parent: AgentId,
    default_recipe: SubagentMode,
    text: &str,
) -> Result<AgentIntent> {
    // Pre-flight `UnknownRole` catch (see the module doc): no session is
    // created on this path at all.
    if !config.roles.contains_key(INTENT_ROLE) {
        return Ok(passthrough(default_recipe, text));
    }

    // The configured agent defs ground BOTH the instructions (the model can
    // only pick from names it is shown) and the P-10 validation below.
    // Loaded with the same loader the builder used; a load failure is an
    // "other error" and propagates.
    let known_defs = crate::agents::load_agent_defs(&resolve_agents_dir(config))?;
    let def_names: HashSet<String> = known_defs.keys().cloned().collect();

    let spec = SubagentSpec {
        mode: SubagentMode::Spawn,
        prompt: classification_prompt(text, &known_defs),
        agent_def: None,
        role: Some(RoleAlias::new(INTENT_ROLE)),
        tools: Some(ToolSelector::Only(Vec::new())),
        budget: INTENT_BUDGET,
        cache_hint: false,
        result_contract: None,
        await_result: true,
        keep_alive: false,
        ephemeral: true,
        // Not an ask of either kind — see the module doc. The sweep only
        // touches `ModalAsk`, and this session is purged inline below.
        ask_origin: None,
    };

    // Subscribe BEFORE `start` so the child's first `TextDelta` cannot race
    // past the drain (the same ordering `SessionHandle::ask` documents).
    let live = rt.subscribe();
    let child = SubagentHost::start(rt.as_ref(), parent, spec)
        .await
        .map_err(ConwayError::Runtime)?;
    // Disclosed residual (shared with B2's ask path): if `start` fails
    // AFTER its internal `store.create` but before launch, the half-created
    // ephemeral session is unreachable from here (its id is never
    // returned) and leaks. `start` is all-but-atomic in practice; closing
    // that window is `conway-runtime`'s concern, not this item's.
    let session = rt
        .tree()
        .nodes
        .iter()
        .find(|n| n.agent_id == child)
        .map(|n| n.session)
        // Tree nodes are never detached, so a just-attached child cannot be
        // absent; this arm exists only to keep the function total.
        .ok_or(ConwayError::Runtime(RuntimeError::AgentNotFound { agent: child }))?;

    // Drive the child's single turn to its terminal event. `keep_alive:
    // false` guarantees exactly one `AgentFinished` follows the turn
    // (architecture §8), so this drain always terminates unless the runtime
    // itself is dropped.
    let mut stream = EventStream::live(session, Some(child), live);
    let (reply, terminal) = drain_turn(&mut stream, child).await;

    // Purge the intent session BEFORE parsing/validation, so EVERY exit
    // path past `start` — success, parse failure, validation failure, a
    // failed turn — leaves no intent session behind. B1's store guards
    // pass: the header is ephemeral, a one-turn classifier has no children,
    // and the child is terminal (we only get here after its
    // `AgentFinished`; terminal status is absorbing, so this cannot race a
    // still-running loop).
    let purge = store.remove(&session).await;

    // The child terminal error wins over a purge error (it is the more
    // informative failure); a purge error surfaces when the turn itself was
    // fine — a classify must never SILENTLY leak its session.
    let result = terminal.ok_or(ConwayError::Runtime(RuntimeError::AgentNotFound {
        agent: child,
    }))?;
    purge?;

    match result.status {
        ResultStatus::Completed => {}
        // Any non-Completed terminal (backend/routing failure folded into
        // `Failed` by the agent loop, budget, cancellation) is an "other
        // error" — it propagates, it does NOT degrade to a passthrough.
        ResultStatus::Failed { error } => {
            return Err(ConwayError::IntentClassification {
                message: format!("the intent agent failed: {error}"),
            });
        }
        other => {
            return Err(ConwayError::IntentClassification {
                message: format!("the intent agent finished with an unexpected status: {other:?}"),
            });
        }
    }

    Ok(parse_reply(&reply, default_recipe, text, &def_names))
}

/// Accumulates the child's reply text until its own terminal
/// `AgentFinished` — agent-id-checked, because `EventStream` passes tree
/// lifecycle events through regardless of the session/agent filter (the
/// same guard `TurnHandle::text`/`result` document). Returns the terminal
/// `AgentResult`, or `None` only if the bus itself ended first (runtime
/// dropped).
async fn drain_turn(stream: &mut EventStream, child: AgentId) -> (String, Option<AgentResult>) {
    let mut text = String::new();
    while let Some(envelope) = poll_fn(|cx| Pin::new(&mut *stream).poll_next(cx)).await {
        match envelope.event {
            Event::TextDelta { text: delta } => text.push_str(&delta),
            Event::AgentFinished { result, .. } if result.agent_id == child => {
                return (text, Some(result));
            }
            _ => {}
        }
    }
    (text, None)
}

/// Builds the intent session's single prompt: the classification
/// instructions, the sorted list of configured agent defs the model may
/// name (sorted for deterministic prompts), and the user's raw text.
fn classification_prompt(text: &str, defs: &HashMap<String, AgentDef>) -> String {
    let mut names: Vec<&String> = defs.keys().collect();
    names.sort();
    let listing = if names.is_empty() {
        "(none configured — always answer null)".to_string()
    } else {
        names
            .iter()
            .map(|name| {
                let description = defs[*name].description.as_deref().unwrap_or("no description");
                format!("- {name}: {description}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "{INSTRUCTIONS}\n\nConfigured agent definitions:\n{listing}\n\n\
         Classify this request:\n\"\"\"\n{text}\n\"\"\"\n"
    )
}

/// The wire shape the classifier is asked to produce. Every field is
/// `Option` so a MISSING field is distinguishable from a present one and
/// handled by the same strict policy; unknown fields are ignored (lenient
/// envelope, strict fields — see the module doc).
#[derive(serde::Deserialize)]
struct RawIntent {
    recipe: Option<String>,
    agent_def: Option<String>,
    prompt: Option<String>,
}

/// Parses and validates the classifier's reply into an [`AgentIntent`],
/// applying the module doc's P-10 policy. Total by construction: every
/// degradation returns the verbatim passthrough (or, for `agent_def` only,
/// strips the untrusted field), so this function never fails.
fn parse_reply(
    reply: &str,
    default_recipe: SubagentMode,
    raw_text: &str,
    def_names: &HashSet<String>,
) -> AgentIntent {
    let raw: RawIntent = match serde_json::from_str(strip_code_fence(reply)) {
        Ok(raw) => raw,
        Err(_) => return passthrough(default_recipe, raw_text),
    };

    let recipe_raw = raw.recipe.as_deref().unwrap_or("").trim().to_ascii_lowercase();
    let recipe = match recipe_raw.as_str() {
        "fork" => SubagentMode::Fork,
        "spawn" => SubagentMode::Spawn,
        _ => return passthrough(default_recipe, raw_text),
    };

    let prompt = match raw.prompt.as_deref().map(str::trim) {
        Some(prompt) if !prompt.is_empty() => prompt.to_string(),
        _ => return passthrough(default_recipe, raw_text),
    };

    // Strip-vs-reject, decided STRIP (see the module doc): a def name the
    // model was not shown is untrusted and is dropped to `None`; the
    // validated recipe and prompt survive.
    let agent_def = raw.agent_def.and_then(|name| {
        let name = name.trim();
        if def_names.contains(name) {
            Some(name.to_string())
        } else {
            None
        }
    });

    AgentIntent {
        recipe,
        agent_def,
        prompt,
    }
}

/// Trims the reply and removes at most one surrounding ``` ``` ``` code
/// fence (with an optional language tag line, e.g. ```json`) — the one
/// concession to real model formatting habits. Anything more exotic
/// (prose around the JSON, multiple fences) is NOT salvaged: strict parse,
/// passthrough on failure.
fn strip_code_fence(reply: &str) -> &str {
    let trimmed = reply.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    // Drop the opening fence's language-tag line, if any.
    let body = match rest.find('\n') {
        Some(i) => &rest[i + 1..],
        None => rest,
    };
    let body = body.trim();
    body.strip_suffix("```").map(str::trim_end).unwrap_or(body)
}

/// Resolves `agents.dir` against `config.cwd` exactly the way
/// `ConwayBuilder::build`'s (private) `resolve_path` did at build time —
/// absolute paths pass through, relative ones join `config.cwd`. See the
/// module doc for the process-`chdir` residual this re-resolution carries.
fn resolve_agents_dir(config: &ConwayConfig) -> PathBuf {
    let dir = &config.agents.dir;
    if dir.is_absolute() {
        dir.clone()
    } else {
        config.cwd.join(dir)
    }
}
