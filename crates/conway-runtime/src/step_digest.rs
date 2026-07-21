//! `StepDigest`: repeated-tool-call detection (WI-086) -- one of the three
//! MAST mitigations this crate owns. A per-agent, in-memory, bounded ring
//! of the last-seen `(tool, canonicalized-args)` digests; the 3rd identical
//! call notices exactly once, so the model can be told to stop looping
//! without the runtime ever refusing to run the call itself (detection,
//! not enforcement).
//!
//! Turn-loop-local state (`AgentLoop::run_inner`'s stack), not a field on
//! `AgentLoop`/`AgentSpec` -- see `result.rs`'s module doc for why.

use std::num::NonZeroUsize;

use conway_core::ids::{LogSeq, ToolName};
use lru::LruCache;
use serde_json::Value;

use crate::context::prefix::canonical_json_bytes;

/// The default bound on [`StepDigest`]'s ring -- the spec's own number.
pub const DEFAULT_RING_CAPACITY: usize = 64;

/// One tracked digest's bookkeeping: how many times it has been seen, the
/// `LogSeq` of its first occurrence (surfaced in the notice so the model
/// can reference the earlier result instead of re-running the call), and
/// whether a notice has already fired for it.
#[derive(Clone, Copy, Debug)]
struct DigestEntry {
    count: u8,
    first_seq: LogSeq,
    noticed: bool,
}

/// A repeated-call notice: the tool whose call repeated, and the `LogSeq`
/// of that tool's first result -- exactly the payload `Event::RepeatedStep`
/// and the injected `SystemNote` need.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepeatedStep {
    pub tool: ToolName,
    pub prior_seq: LogSeq,
}

/// Bounded LRU ring of `(tool, canonicalized-args)` digests, one instance
/// per agent run.
pub struct StepDigest {
    ring: LruCache<[u8; 32], DigestEntry>,
}

impl StepDigest {
    /// Builds a ring bounded to `capacity` entries (LRU-evicted beyond
    /// that). `capacity == 0` falls back to [`DEFAULT_RING_CAPACITY`]
    /// rather than panicking -- a caller-supplied `0` is a configuration
    /// mistake, not a reason to crash the agent.
    pub fn new(capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity)
            .unwrap_or_else(|| NonZeroUsize::new(DEFAULT_RING_CAPACITY).unwrap());
        Self {
            ring: LruCache::new(cap),
        }
    }

    /// The digest for one call: `blake3(tool_name ‖ canonical_normalized_args)`.
    /// Canonicalization (recursively sorted object keys, rendered via
    /// `serde_json::to_vec`) is shared with the permission broker's own
    /// cache key, not duplicated: both this function and
    /// `crate::permission::CacheKey::for_call` call
    /// [`crate::context::prefix::canonical_json_bytes`].
    ///
    /// Reconciliation against the spec's prose: the spec's own notes
    /// additionally say the shared canonicalization "drops nulls"; the
    /// committed `canonical_json_bytes` (WI-077) does not drop them -- it
    /// only sorts object keys and serializes without insignificant
    /// whitespace. This function uses the committed behavior as-is (the
    /// binding "share it, do not duplicate" instruction wins over the
    /// prose detail); no criterion here depends on null-dropping.
    fn digest(tool: &ToolName, args: &Value) -> [u8; 32] {
        let mut bytes = tool.as_str().as_bytes().to_vec();
        bytes.extend_from_slice(&canonical_json_bytes(args));
        *blake3::hash(&bytes).as_bytes()
    }

    /// Observes one call. Returns `Some(RepeatedStep)` the instant a
    /// digest's count reaches 3 for the first time (`noticed` then flips
    /// permanently `true`, so the 4th, 5th, ... occurrence of the same
    /// digest never notices again); two different digests are tracked, and
    /// can each notice, independently. `seq` should be the `LogSeq` this
    /// call's own result was (or will be) persisted at; only the *first*
    /// occurrence's `seq` is retained and surfaced.
    pub fn observe(&mut self, tool: &ToolName, args: &Value, seq: LogSeq) -> Option<RepeatedStep> {
        let key = Self::digest(tool, args);
        match self.ring.get_mut(&key) {
            Some(entry) => {
                entry.count = entry.count.saturating_add(1);
                if entry.count == 3 && !entry.noticed {
                    entry.noticed = true;
                    Some(RepeatedStep {
                        tool: tool.clone(),
                        prior_seq: entry.first_seq,
                    })
                } else {
                    None
                }
            }
            None => {
                self.ring.put(
                    key,
                    DigestEntry {
                        count: 1,
                        first_seq: seq,
                        noticed: false,
                    },
                );
                None
            }
        }
    }
}

impl Default for StepDigest {
    fn default() -> Self {
        Self::new(DEFAULT_RING_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> ToolName {
        ToolName::new(name)
    }

    #[test]
    fn third_identical_call_notices_exactly_once() {
        let mut digest = StepDigest::new(DEFAULT_RING_CAPACITY);
        let t = tool("read");
        let args = serde_json::json!({"path": "a.txt"});

        assert!(digest.observe(&t, &args, LogSeq(1)).is_none());
        assert!(digest.observe(&t, &args, LogSeq(2)).is_none());
        let notice = digest
            .observe(&t, &args, LogSeq(3))
            .expect("3rd identical call must notice");
        assert_eq!(notice.tool, t);
        assert_eq!(
            notice.prior_seq,
            LogSeq(1),
            "must cite the FIRST occurrence's seq"
        );
    }

    #[test]
    fn fourth_and_fifth_identical_calls_notice_nothing_further() {
        let mut digest = StepDigest::new(DEFAULT_RING_CAPACITY);
        let t = tool("read");
        let args = serde_json::json!({"path": "a.txt"});

        digest.observe(&t, &args, LogSeq(1));
        digest.observe(&t, &args, LogSeq(2));
        assert!(digest.observe(&t, &args, LogSeq(3)).is_some());
        assert!(
            digest.observe(&t, &args, LogSeq(4)).is_none(),
            "4th occurrence must not notice again"
        );
        assert!(
            digest.observe(&t, &args, LogSeq(5)).is_none(),
            "5th occurrence must not notice again"
        );
    }

    #[test]
    fn two_different_digests_are_tracked_independently() {
        let mut digest = StepDigest::new(DEFAULT_RING_CAPACITY);
        let read = tool("read");
        let write = tool("write");
        let args = serde_json::json!({"path": "a.txt"});

        // Two calls to `read`, then two calls to a DIFFERENT tool with the
        // same args -- these must not count toward `read`'s digest.
        digest.observe(&read, &args, LogSeq(1));
        digest.observe(&read, &args, LogSeq(2));
        digest.observe(&write, &args, LogSeq(10));
        assert!(
            digest.observe(&write, &args, LogSeq(11)).is_none(),
            "write's own 2nd call must not notice"
        );

        let notice = digest
            .observe(&read, &args, LogSeq(3))
            .expect("read's own 3rd call must still notice");
        assert_eq!(notice.tool, read);
        assert_eq!(notice.prior_seq, LogSeq(1));

        let write_notice = digest
            .observe(&write, &args, LogSeq(12))
            .expect("write's own 3rd call must notice independently");
        assert_eq!(write_notice.tool, write);
        assert_eq!(write_notice.prior_seq, LogSeq(10));
    }

    #[test]
    fn key_order_does_not_change_the_digest_but_value_changes_do() {
        let mut a = StepDigest::new(DEFAULT_RING_CAPACITY);
        let mut b = StepDigest::new(DEFAULT_RING_CAPACITY);
        let t = tool("read");

        let ordered = serde_json::json!({"a": 1, "b": 2});
        let reordered = serde_json::json!({"b": 2, "a": 1});
        let different = serde_json::json!({"a": 2});

        a.observe(&t, &ordered, LogSeq(1));
        a.observe(&t, &ordered, LogSeq(2));
        let notice_a = a.observe(&t, &ordered, LogSeq(3));

        b.observe(&t, &ordered, LogSeq(1));
        b.observe(&t, &reordered, LogSeq(2));
        let notice_b = b
            .observe(&t, &reordered, LogSeq(3))
            .expect("key-reordered args must hit the same digest as the 3rd call");
        assert_eq!(notice_a, Some(notice_b));

        let mut c = StepDigest::new(DEFAULT_RING_CAPACITY);
        c.observe(&t, &ordered, LogSeq(1));
        c.observe(&t, &ordered, LogSeq(2));
        assert!(
            c.observe(&t, &different, LogSeq(3)).is_none(),
            "a differing value must be a distinct digest, not the 3rd occurrence"
        );
    }

    #[test]
    fn ring_is_bounded_and_evicts_without_unbounded_growth() {
        let mut digest = StepDigest::new(DEFAULT_RING_CAPACITY);
        for i in 0..10_000u32 {
            let args = serde_json::json!({"i": i});
            digest.observe(&tool("read"), &args, LogSeq(i as u64));
        }
        assert!(digest.ring.len() <= DEFAULT_RING_CAPACITY);
    }

    #[test]
    fn zero_capacity_falls_back_to_the_default_rather_than_panicking() {
        let digest = StepDigest::new(0);
        assert_eq!(digest.ring.cap().get(), DEFAULT_RING_CAPACITY);
    }
}
