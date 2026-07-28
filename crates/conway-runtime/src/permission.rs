//! `PermissionBroker`: a per-session decision cache layered over the
//! consumer's [`PermissionGate`] (architecture §4.3).
//!
//! The broker normalizes whatever the gate decides into a
//! [`PermissionOutcome`] the tool runner (WI-079) can act on directly, and
//! it owns the `AllowAlways` cache so a consumer answering "allow always"
//! is only ever asked once per scope. It never imposes a timeout on the
//! gate: architecture §8 requires the runtime to hold a pending call open
//! for as long as the gate takes to answer.

use std::collections::HashMap;

use conway_core::permission_mode::PermissionMode;
use conway_core::permission_pattern::PatternRule;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use conway_core::agent::{
    PermissionDecision, PermissionDecisionKind, PermissionRequest, PermissionScope,
};
use conway_core::content::ToolCategory;
use conway_core::event::Event;
use conway_core::ids::{AgentId, SessionId, ToolName};
use conway_core::ports::PermissionGate;

use crate::context::prefix::canonical_json_bytes;
use crate::events::EventBus;

/// The requesting agent's identity and position in the tree, as seen by one
/// [`PermissionBroker::decide`] call.
///
/// `agent_path` is the full root→requester chain (§8 precondition on
/// [`PermissionRequest`]) — it is what makes an `AgentSubtree` grant
/// checkable by prefix membership without walking the live agent tree.
pub struct PermissionCtx {
    pub agent_id: AgentId,
    pub agent_path: Vec<AgentId>,
    pub session: SessionId,
    pub cwd: PathBuf,
}

/// The minimal, already-resolved slice of a proposed tool call the broker
/// needs to authorize it. `category` and `rendered` are supplied by the
/// caller (the tool runner, from the resolved `ToolSpec` and the tool's own
/// renderer) — the broker does not know how to categorize or render a tool
/// call itself.
#[derive(Clone, Debug)]
pub struct AuthorizedCall {
    pub call_id: String,
    pub tool: ToolName,
    pub category: ToolCategory,
    pub arguments: serde_json::Value,
    pub rendered: String,
}

/// The normalized result of [`PermissionBroker::decide`]. There is no
/// "abort" variant: every denial — whether a plain [`PermissionDecision::Deny`]
/// or a [`PermissionDecision::DenyWithFeedback`] — collapses to `Deny`, so a
/// caller matching on this type can only ever turn it into a model-visible
/// tool error, never mistake it for a reason to abort the agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionOutcome {
    Allow,
    Deny { rendered_error: String },
}

impl From<PermissionDecision> for PermissionOutcome {
    fn from(decision: PermissionDecision) -> Self {
        match decision {
            PermissionDecision::AllowOnce | PermissionDecision::AllowAlways { .. } => {
                PermissionOutcome::Allow
            }
            PermissionDecision::Deny { reason } => PermissionOutcome::Deny {
                rendered_error: reason,
            },
            PermissionDecision::DenyWithFeedback { message } => PermissionOutcome::Deny {
                rendered_error: message,
            },
            // `PermissionDecision` is `#[non_exhaustive]`: fail closed on any
            // future variant rather than silently allowing it.
            _ => PermissionOutcome::Deny {
                rendered_error: "permission gate returned an unrecognized decision".into(),
            },
        }
    }
}

/// The cache lookup key: a tool name plus a digest of its canonicalized
/// arguments, so semantically identical argument objects (differing only in
/// key order or insignificant whitespace) hit the same entry.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CacheKey {
    tool: ToolName,
    args_digest: blake3::Hash,
}

impl CacheKey {
    fn for_call(call: &AuthorizedCall) -> Self {
        Self {
            tool: call.tool.clone(),
            args_digest: blake3::hash(&canonical_json_bytes(&call.arguments)),
        }
    }
}

/// One remembered `AllowAlways` grant, scoped per architecture §4.3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GrantScope {
    Session,
    Agent(AgentId),
    Subtree(AgentId),
}

impl GrantScope {
    /// A `Session` grant hits for any requester; an `Agent` grant only for
    /// the exact granting agent; a `Subtree` grant for any requester whose
    /// `agent_path` contains the granting agent (a descendant, or the
    /// granting agent itself).
    fn covers(&self, ctx: &PermissionCtx) -> bool {
        match self {
            GrantScope::Session => true,
            GrantScope::Agent(granter) => *granter == ctx.agent_id,
            GrantScope::Subtree(granter) => ctx.agent_path.contains(granter),
        }
    }
}

/// Maps a `PermissionScope` onto the broker's internal `GrantScope`.
///
/// Shared by the `AllowAlways` cache and V2's pattern grants so the two
/// cannot drift on the `#[non_exhaustive]` fallback: an unknown scope
/// narrows to `Agent`, never widens to `Session`.
fn grant_scope_for(scope: PermissionScope, granting_agent: AgentId) -> GrantScope {
    match scope {
        PermissionScope::Session => GrantScope::Session,
        PermissionScope::Agent => GrantScope::Agent(granting_agent),
        PermissionScope::AgentSubtree => GrantScope::Subtree(granting_agent),
        // `PermissionScope` is `#[non_exhaustive]`: fall back to the
        // narrowest known grant rather than silently widening it.
        _ => GrantScope::Agent(granting_agent),
    }
}

/// A per-session decision cache over the consumer's [`PermissionGate`]
/// (architecture §4.3). One broker instance is shared across an agent tree.
pub struct PermissionBroker {
    gate: Arc<dyn PermissionGate>,
    bus: Arc<EventBus>,
    cache: RwLock<HashMap<CacheKey, Vec<GrantScope>>>,
    /// V2: how much the operator is asked. Runtime-mutable via
    /// [`PermissionBroker::set_mode`] so `/settings` can switch out of an
    /// over-broad mode mid-session without a restart.
    mode: RwLock<PermissionMode>,
    /// V2: prefix-pattern grants, paired with the scope they were granted
    /// at. Checked BEFORE the gate, so a matching pattern spares the
    /// operator a prompt -- but only for commands that clear
    /// `PatternRule::matches`'s metacharacter gate.
    patterns: RwLock<Vec<(PatternRule, GrantScope)>>,
}

impl PermissionBroker {
    pub fn new(gate: Arc<dyn PermissionGate>, bus: Arc<EventBus>) -> Self {
        Self {
            gate,
            bus,
            cache: RwLock::new(HashMap::new()),
            mode: RwLock::new(PermissionMode::default()),
            patterns: RwLock::new(Vec::new()),
        }
    }

    /// The current mode. Cheap enough to call per render -- the status
    /// line reads it every frame so the operator can never be uncertain
    /// which mode they are in.
    pub fn mode(&self) -> PermissionMode {
        *self.mode.read().expect("permission mode poisoned")
    }

    /// Switches mode at runtime. This is the escape hatch: an operator who
    /// finds themselves in an over-broad `AutoAllow` returns to `Prompt`
    /// without restarting the session.
    pub fn set_mode(&self, mode: PermissionMode) {
        *self.mode.write().expect("permission mode poisoned") = mode;
    }

    /// Installs a pattern grant at `scope`.
    ///
    /// Note this does NOT pre-validate the rule against metacharacters:
    /// the gate lives in `PatternRule::matches`, applied to the incoming
    /// COMMAND at decision time. Filtering at creation time instead would
    /// be the wrong shape -- it would let a rule created before the gate
    /// existed, or loaded from a file, slip past.
    pub fn remember_pattern(&self, rule: PatternRule, scope: PermissionScope, granting_agent: AgentId) {
        let grant = grant_scope_for(scope, granting_agent);
        self.patterns
            .write()
            .expect("permission patterns poisoned")
            .push((rule, grant));
    }

    /// Every active pattern grant, for the settings menu's review list. An
    /// operator must be able to see what they have granted; a rule set
    /// nobody can inspect is a trap.
    pub fn active_patterns(&self) -> Vec<PatternRule> {
        self.patterns
            .read()
            .expect("permission patterns poisoned")
            .iter()
            .map(|(rule, _)| rule.clone())
            .collect()
    }

    /// Drops every pattern grant and every cached `AllowAlways`, returning
    /// the session to asking. The revocation half of the escape hatch.
    pub fn revoke_all_grants(&self) {
        self.patterns
            .write()
            .expect("permission patterns poisoned")
            .clear();
        self.cache
            .write()
            .expect("permission cache poisoned")
            .clear();
    }

    /// Whether any installed pattern authorizes this call.
    ///
    /// The metacharacter gate is inside `PatternRule::matches`, so a
    /// chained command can never satisfy a pattern here regardless of what
    /// is installed.
    fn pattern_allows(&self, ctx: &PermissionCtx, call: &AuthorizedCall) -> bool {
        self.patterns
            .read()
            .expect("permission patterns poisoned")
            .iter()
            .any(|(rule, grant)| {
                grant.covers(ctx) && rule.matches(call.tool.as_str(), &call.rendered)
            })
    }

    /// Authorize one tool call, consulting the cache first and the gate on a
    /// miss.
    ///
    /// Emission sequence, strictly: `PermissionRequested` → (cache hit, or
    /// await the gate and insert a cache entry on `AllowAlways`) →
    /// `PermissionResolved`. The cache's `RwLock` is never held across the
    /// `await` on `self.gate.check` — every lock acquisition in this method
    /// is a short, synchronous read or write that completes before any
    /// `await` point.
    pub async fn decide(&self, ctx: &PermissionCtx, call: &AuthorizedCall) -> PermissionOutcome {
        let key = CacheKey::for_call(call);

        self.emit(
            ctx,
            Event::PermissionRequested {
                call_id: call.call_id.clone(),
                rendered: call.rendered.clone(),
            },
        );

        if self.cached_grant_covers(&key, ctx) {
            self.emit(
                ctx,
                Event::PermissionResolved {
                    call_id: call.call_id.clone(),
                    decision: PermissionDecisionKind::Cached,
                },
            );
            return PermissionOutcome::Allow;
        }

        // V2 mode gate. Ordered deliberately: PLAN's denial is checked
        // before any allow path, so a plan-mode session cannot be talked
        // out of its denial by a pattern grant or an auto-allow left over
        // from earlier. Plan mode is the mode an operator selects when
        // they want a guarantee, so it behaves like one.
        let mode = self.mode();
        if mode == PermissionMode::Plan && !mode.allows_category(call.category) {
            self.emit(
                ctx,
                Event::PermissionResolved {
                    call_id: call.call_id.clone(),
                    decision: PermissionDecisionKind::Denied,
                },
            );
            return PermissionOutcome::Deny {
                rendered_error: format!(
                    "plan mode: `{}` is a {:?} tool, which plan mode does not permit.                      Switch modes in /settings to run it.",
                    call.tool.as_str(),
                    call.category
                ),
            };
        }

        // A pattern grant spares the operator a prompt -- but only for a
        // command that clears the metacharacter gate inside
        // `PatternRule::matches`. A chained command falls through to the
        // gate below no matter what patterns exist.
        if self.pattern_allows(ctx, call) {
            self.emit(
                ctx,
                Event::PermissionResolved {
                    call_id: call.call_id.clone(),
                    decision: PermissionDecisionKind::Cached,
                },
            );
            return PermissionOutcome::Allow;
        }

        if mode == PermissionMode::AutoAllow {
            self.emit(
                ctx,
                Event::PermissionResolved {
                    call_id: call.call_id.clone(),
                    decision: PermissionDecisionKind::Cached,
                },
            );
            return PermissionOutcome::Allow;
        }

        let request = PermissionRequest {
            agent_id: ctx.agent_id,
            agent_path: ctx.agent_path.clone(),
            tool: call.tool.clone(),
            category: call.category,
            arguments: call.arguments.clone(),
            rendered: call.rendered.clone(),
            call_id: call.call_id.clone(),
        };
        let decision = self.gate.check(request).await;

        if let PermissionDecision::AllowAlways { scope } = &decision {
            self.remember(key, *scope, ctx.agent_id);
        }

        let kind = PermissionDecisionKind::from(&decision);
        self.emit(
            ctx,
            Event::PermissionResolved {
                call_id: call.call_id.clone(),
                decision: kind,
            },
        );

        PermissionOutcome::from(decision)
    }

    fn cached_grant_covers(&self, key: &CacheKey, ctx: &PermissionCtx) -> bool {
        let cache = self.cache.read().expect("permission cache poisoned");
        cache
            .get(key)
            .is_some_and(|grants| grants.iter().any(|grant| grant.covers(ctx)))
    }

    fn remember(&self, key: CacheKey, scope: PermissionScope, granting_agent: AgentId) {
        let grant = grant_scope_for(scope, granting_agent);
        let mut cache = self.cache.write().expect("permission cache poisoned");
        let grants = cache.entry(key).or_default();
        // Dedup on insert: concurrent decide() races on the same key may
        // both reach here; duplicate grants are harmless but accumulate
        // (cycle-1 review M2).
        if !grants.contains(&grant) {
            grants.push(grant);
        }
    }

    fn emit(&self, ctx: &PermissionCtx, event: Event) {
        self.bus.emit(ctx.session, ctx.agent_id, event);
    }
}
