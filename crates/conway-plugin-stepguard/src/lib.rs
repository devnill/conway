//! `conway.stepguard`: notices when an agent calls the same tool with the
//! same arguments over and over, and says so where the agent will read it.
//!
//! # Why this is a plugin
//!
//! This detection used to be compiled into `conway-runtime`'s agent loop,
//! unconditionally, with no way to decline it. That contradicted
//! `PHILOSOPHY.md` §6 in the page's own words — "repeated-step detection,
//! retry ceilings, and circling-agent heuristics are not in the core. The
//! events exist, so the policy is yours to write, including writing none."
//! Writing none was not an option while the core shipped one.
//!
//! It also made the harness author into the model's context on its own
//! initiative, which §6 rules out for the same reason it rules out automatic
//! compaction: a guess about what your work needs, applied invisibly.
//!
//! So the mechanism moved out and the judgment came with it. What the core
//! kept is the seam — `ToolObserver`, which hands an observer a finished call
//! and takes back a description of what to record. Everything below is
//! policy, and every part of it is yours to disagree with: the threshold, the
//! wording, what counts as "the same call".
//!
//! # The policy, stated plainly
//!
//! A bounded LRU ring per agent run holds `blake3(tool ‖ canonical-json(args))`
//! for the last [`DEFAULT_RING_CAPACITY`] distinct calls. The **third**
//! occurrence of a digest produces one note, once; the fourth and fifth
//! produce nothing further. Different arguments are a different digest and
//! never conflate.
//!
//! Three, rather than two, because a legitimate retry after a transient
//! failure is ordinary and telling an agent off for it is worse than saying
//! nothing. Once, rather than every time, because a note that repeats is
//! itself a repeated step.
//!
//! # What it does NOT do
//!
//! It never refuses the call. Detection, not enforcement — by the time an
//! observer runs, the call has already happened, and a policy that wants to
//! *stop* something wants `PermissionGate` or a `pre_tool_use` hook instead.
//! An agent that has genuinely decided to loop is free to keep looping; what
//! changes is that it has been told, and that the log says so.
//!
//! # Installing it
//!
//! ```json
//! { "plugins": { "install": ["conway.stepguard"] } }
//! ```
//!
//! With it uninstalled, nothing observes tool calls and no note is ever
//! written — the runtime's observer pass does not execute at all.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use conway::plugin::{
    EventDecl, ObservedCall, ObserverAnswer, ObserverCtx, ObserverNote, Plugin, PluginDescription,
    PluginManifest, Tool, ToolObserver,
};

/// The install id an operator names in `plugins.install`.
pub const PLUGIN_ID: &str = "conway.stepguard";

/// `SystemNote::reason` on every note this plugin writes, so a reader
/// filtering a session log can select exactly these.
pub const NOTE_REASON: &str = "repeated_step";

/// The bare event name this plugin declares and fires. Reachable in an
/// operator's `hooks.rules[].event` as `conway.stepguard.repeated_step` —
/// the host prefixes it; a plugin never picks its own namespace.
pub const EVENT_REPEATED_STEP: &str = "repeated_step";

/// How many distinct `(tool, arguments)` digests one agent run remembers.
/// Bounded so a long run cannot grow this without limit; the eviction
/// consequence is stated on [`StepGuard`].
pub const DEFAULT_RING_CAPACITY: usize = 64;

/// Occurrences of one digest before a note fires. See the module doc for why
/// this is 3 and not 2.
pub const NOTICE_AT: u8 = 3;

/// Per-digest bookkeeping: how many times seen, where the first one's result
/// landed, and whether its note already fired.
#[derive(Clone, Copy, Debug)]
struct Seen {
    count: u8,
    first_seq: conway::LogSeq,
    noticed: bool,
}

/// The observer itself.
///
/// State is keyed by `AgentId` so sibling agents in a fan-out never pool
/// their calls — ten children each reading the same file once is not a loop,
/// and reporting it as one would be exactly the false positive that makes
/// people turn a check off.
///
/// **Eviction is a deliberate false-negative.** A ring of
/// [`DEFAULT_RING_CAPACITY`] means an agent that cycles through more than
/// that many distinct calls between repeats will not be noticed. That is the
/// right way for this to fail: a missed notice costs nothing, and an
/// unbounded map on a long-running agent costs memory that grows with the
/// session.
pub struct StepGuard {
    rings: Mutex<HashMap<conway::AgentId, lru::LruCache<[u8; 32], Seen>>>,
    capacity: NonZeroUsize,
}

impl std::fmt::Debug for StepGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StepGuard")
            .field("capacity", &self.capacity)
            .finish_non_exhaustive()
    }
}

impl Default for StepGuard {
    fn default() -> Self {
        Self::new(DEFAULT_RING_CAPACITY)
    }
}

impl StepGuard {
    /// Builds a guard whose per-agent ring holds `capacity` digests.
    /// `capacity` is clamped to at least 1.
    pub fn new(capacity: usize) -> Self {
        Self {
            rings: Mutex::new(HashMap::new()),
            capacity: NonZeroUsize::new(capacity).unwrap_or(NonZeroUsize::MIN),
        }
    }

    /// `blake3(tool ‖ canonical_json(arguments))`.
    ///
    /// Canonical JSON — object keys sorted recursively, no insignificant
    /// whitespace — so two calls that differ only in the order a model
    /// happened to serialize their arguments hash the same. Without it this
    /// check would be trivially defeated by a model that shuffles keys, which
    /// is not adversarial behavior, just non-determinism.
    fn digest(tool: &conway::ToolName, arguments: &serde_json::Value) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(tool.as_str().as_bytes());
        hasher.update(&conway::canonical_json_bytes(arguments));
        *hasher.finalize().as_bytes()
    }
}

#[async_trait]
impl ToolObserver for StepGuard {
    async fn after_tool_call(&self, ctx: &ObserverCtx, call: &ObservedCall) -> ObserverAnswer {
        let digest = Self::digest(&call.tool, &call.arguments);

        // Scoped so the lock is never held across the `.await` below.
        let fired = {
            let mut rings = self.rings.lock().expect("stepguard ring lock poisoned");
            let ring = rings
                .entry(call.agent_id)
                .or_insert_with(|| lru::LruCache::new(self.capacity));
            match ring.get_mut(&digest) {
                Some(seen) => {
                    seen.count = seen.count.saturating_add(1);
                    if seen.count >= NOTICE_AT && !seen.noticed {
                        seen.noticed = true;
                        Some(seen.first_seq)
                    } else {
                        None
                    }
                }
                None => {
                    ring.put(
                        digest,
                        Seen {
                            count: 1,
                            first_seq: call.result_seq,
                            noticed: false,
                        },
                    );
                    None
                }
            }
        };

        let Some(first_seq) = fired else {
            return ObserverAnswer::default();
        };

        ctx.events
            .emit(
                EVENT_REPEATED_STEP,
                serde_json::json!({
                    "tool": call.tool.as_str(),
                    "agent_id": call.agent_id,
                    "session": call.session,
                    "occurrences": NOTICE_AT,
                    "first_result_seq": first_seq,
                }),
            )
            .await;

        ObserverAnswer {
            notes: vec![ObserverNote {
                text: format!(
                    "tool `{}` was called with identical arguments {} times; \
                     its first result is at seq {}. Read that result rather than \
                     calling again, or change approach.",
                    call.tool, NOTICE_AT, first_seq
                ),
                reason: NOTE_REASON.to_string(),
            }],
        }
    }
}

/// The plugin wrapper. Contributes no tools and no commands — one observer
/// and one declared event is the whole of it.
#[derive(Debug, Default)]
pub struct StepGuardPlugin {
    guard: Arc<StepGuard>,
}

impl StepGuardPlugin {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds the plugin with a non-default ring capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            guard: Arc::new(StepGuard::new(capacity)),
        }
    }
}

impl Plugin for StepGuardPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PLUGIN_ID.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            // No tools: this plugin's entire surface is one observer and the
            // one event it fires. `tools` names only what `Plugin::tools`
            // actually returns, never a stub.
            tools: vec![],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    fn description(&self) -> PluginDescription {
        PluginDescription {
            summary: "notices when the same tool call repeats".to_string(),
            you_get: "a note in the transcript the THIRD time the same tool call (same tool, \
                      same arguments) repeats in a row -- once, not every time after"
                .to_string(),
            you_lose: "nothing else -- repeated-call detection goes silent, the call itself is \
                       never blocked either way"
                .to_string(),
            costs: "a small per-call digest computed for every tool call".to_string(),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    fn events(&self) -> Vec<EventDecl> {
        vec![EventDecl {
            name: EVENT_REPEATED_STEP.to_string(),
            summary: "An agent called the same tool with identical arguments three times; \
                      payload carries the tool, the agent, and the seq of the first result"
                .to_string(),
            carries_tool_name: true,
        }]
    }

    fn observers(&self) -> Vec<Arc<dyn ToolObserver>> {
        vec![self.guard.clone() as Arc<dyn ToolObserver>]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway::plugin::PluginEventHandle;
    use conway::{AgentId, LogSeq, SessionId, ToolName};

    fn ctx() -> ObserverCtx {
        ObserverCtx {
            events: PluginEventHandle::noop(PLUGIN_ID),
        }
    }

    /// The plugin browser's own read surface (board item
    /// `01M0KARX71A64NTSYTDBVANVPF`): a real description, never the
    /// trait's empty default.
    #[test]
    fn description_is_non_empty() {
        let description = StepGuardPlugin::new().description();
        assert!(!description.summary.is_empty());
        assert!(!description.you_get.is_empty());
        assert!(!description.you_lose.is_empty());
    }

    fn call(agent: AgentId, tool: &str, args: serde_json::Value, seq: u64) -> ObservedCall {
        ObservedCall {
            agent_id: agent,
            session: SessionId::new(),
            call_id: format!("c{seq}"),
            tool: ToolName::new(tool),
            arguments: args,
            is_error: false,
            result_seq: LogSeq(seq),
        }
    }

    #[tokio::test]
    async fn the_third_identical_call_notices_once_and_cites_the_first_result() {
        let guard = StepGuard::default();
        let agent = AgentId::new();
        let args = serde_json::json!({"path": "a.txt"});

        for seq in [1, 2] {
            let answer = guard
                .after_tool_call(&ctx(), &call(agent, "read", args.clone(), seq))
                .await;
            assert!(answer.notes.is_empty(), "call {seq} must not notice");
        }

        let answer = guard
            .after_tool_call(&ctx(), &call(agent, "read", args.clone(), 3))
            .await;
        assert_eq!(answer.notes.len(), 1);
        assert_eq!(answer.notes[0].reason, NOTE_REASON);
        assert!(
            answer.notes[0].text.contains("seq 1"),
            "the note must cite the FIRST result, not the latest: {:?}",
            answer.notes[0].text
        );

        for seq in [4, 5] {
            let answer = guard
                .after_tool_call(&ctx(), &call(agent, "read", args.clone(), seq))
                .await;
            assert!(
                answer.notes.is_empty(),
                "call {seq} must not notice a second time"
            );
        }
    }

    #[tokio::test]
    async fn different_arguments_are_never_conflated() {
        let guard = StepGuard::default();
        let agent = AgentId::new();
        for (seq, path) in [(1, "a.txt"), (2, "b.txt"), (3, "c.txt")] {
            let answer = guard
                .after_tool_call(
                    &ctx(),
                    &call(agent, "read", serde_json::json!({ "path": path }), seq),
                )
                .await;
            assert!(answer.notes.is_empty(), "distinct paths are distinct calls");
        }
    }

    /// A model that serializes the same arguments with keys in a different
    /// order is making the same call. Without canonicalization this check
    /// would be defeated by ordinary non-determinism.
    #[tokio::test]
    async fn key_order_does_not_change_the_digest() {
        let guard = StepGuard::default();
        let agent = AgentId::new();
        let orders = [
            serde_json::json!({"a": 1, "b": 2}),
            serde_json::json!({"b": 2, "a": 1}),
            serde_json::json!({"a": 1, "b": 2}),
        ];
        let mut noticed = 0;
        for (i, args) in orders.into_iter().enumerate() {
            let answer = guard
                .after_tool_call(&ctx(), &call(agent, "read", args, i as u64 + 1))
                .await;
            noticed += answer.notes.len();
        }
        assert_eq!(noticed, 1, "reordered keys must hash as the same call");
    }

    /// A fan-out is not a loop. Ten children each reading the same file once
    /// is normal, and pooling their calls would report it as repetition.
    #[tokio::test]
    async fn sibling_agents_do_not_pool_their_calls() {
        let guard = StepGuard::default();
        let args = serde_json::json!({"path": "shared.txt"});
        for seq in 1..=3 {
            let answer = guard
                .after_tool_call(&ctx(), &call(AgentId::new(), "read", args.clone(), seq))
                .await;
            assert!(
                answer.notes.is_empty(),
                "each call came from a different agent"
            );
        }
    }

    #[test]
    fn the_declared_event_is_the_one_the_observer_fires() {
        let plugin = StepGuardPlugin::new();
        let declared: Vec<String> = plugin.events().into_iter().map(|e| e.name).collect();
        assert_eq!(declared, vec![EVENT_REPEATED_STEP.to_string()]);
        assert_eq!(plugin.observers().len(), 1);
        assert!(plugin.tools().is_empty());
    }
}
