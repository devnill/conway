//! `PromptSegment`: one piece of assembled context, always tagged with the
//! [`Provenance`] that explains why it is present, plus the cache-hint types
//! that make caching an economics-only concern, never correctness-bearing.

use serde::{Deserialize, Serialize};

use crate::content::{ContentBlock, Role};
use crate::ids::{PrefixKey, SegmentId};
use crate::provenance::Provenance;

// `CacheTtl` lives in `capabilities.rs` (WI-004 depends only on WI-001 and
// needed this type before this module existed). Re-export it rather than
// redefining it — see the doc comment on `capabilities::CacheTtl`.
pub use crate::capabilities::CacheTtl;

/// One piece of assembled context.
///
/// There is deliberately no `Default` impl and `provenance` is not
/// `Option<Provenance>`: a segment cannot be constructed without stating why
/// it exists. The only public constructor, [`PromptSegment::new`], takes
/// `Provenance` as a required argument, so this is a structural (not just
/// documented) guarantee. The absence of `Default` is enforced at compile
/// time by a `static_assertions::assert_not_impl_any!(PromptSegment:
/// Default)` guard in this module's tests.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromptSegment {
    pub id: SegmentId,
    pub role: Role,
    pub content: Vec<ContentBlock>,
    /// Required: every segment must say why it exists. No `Default`.
    pub provenance: Provenance,
    /// Caching is never correctness-bearing: stripping every
    /// `cache_hint` from a `Vec<PromptSegment>` must not change the
    /// assembled request content bytes. See [`strip_cache_hints`].
    pub cache_hint: Option<CacheHint>,
    pub tokens_est: Option<u32>,
}

impl PromptSegment {
    /// Construct a new segment. Generates a fresh [`SegmentId`], with
    /// `cache_hint: None` and `tokens_est: None`.
    pub fn new(role: Role, content: Vec<ContentBlock>, provenance: Provenance) -> Self {
        Self {
            id: SegmentId::new(),
            role,
            content,
            provenance,
            cache_hint: None,
            tokens_est: None,
        }
    }

    pub fn with_cache_hint(mut self, cache_hint: CacheHint) -> Self {
        self.cache_hint = Some(cache_hint);
        self
    }

    pub fn with_tokens_est(mut self, tokens_est: u32) -> Self {
        self.tokens_est = Some(tokens_est);
        self
    }
}

/// A hint to the backend about where and how long to cache a shared prefix.
/// Never correctness-bearing: see [`PromptSegment::cache_hint`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheHint {
    pub breakpoint: bool,
    pub ttl: CacheTtl,
    pub prefix_key: PrefixKey,
}

/// Strip every `cache_hint` from `segments` in place, so downstream tests
/// can mechanically assert the invariant: doing this must never change
/// the assembled request's content bytes.
pub fn strip_cache_hints(segments: &mut [PromptSegment]) {
    for segment in segments {
        segment.cache_hint = None;
    }
}

/// A lightweight classification of a segment's role, used by the CLI
/// `/context` renderer.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentKind {
    SystemPrompt,
    SkillFragment,
    ToolSchemas,
    InheritedPrefix,
    Directive,
    Turn,
}

impl From<&Provenance> for SegmentKind {
    /// Mirrors the architecture §5.3 fixed segment order: `[0] AgentDef ->
    /// SystemPrompt`, `[1] Skill -> SkillFragment`, `[2] ToolRegistry ->
    /// ToolSchemas`, `[3] Inherited -> InheritedPrefix`, `[4]
    /// ForkDirective|UserPrompt -> Directive`, `[5..]
    /// ParentSteer/ToolResult/SystemNote/ChildResult -> Turn`.
    fn from(provenance: &Provenance) -> Self {
        match provenance {
            Provenance::AgentDef { .. } => SegmentKind::SystemPrompt,
            Provenance::Skill { .. } => SegmentKind::SkillFragment,
            Provenance::ToolRegistry { .. } => SegmentKind::ToolSchemas,
            Provenance::Inherited { .. } => SegmentKind::InheritedPrefix,
            Provenance::ForkDirective { .. } | Provenance::UserPrompt => SegmentKind::Directive,
            // A merged `/ask` question is user-authored directive text
            // folded into the parent's log (B4) — same `[4]` slot as
            // `UserPrompt`/`ForkDirective`, not a model-turn artifact.
            Provenance::MergedAsk { .. } => SegmentKind::Directive,
            // A child's terminal result, recorded into this agent's own
            // `[5..]` volatile records by `mailbox::classify` -- same slot
            // as a steer or a tool result, not a model-turn artifact either.
            Provenance::ParentSteer { .. }
            | Provenance::ToolResult { .. }
            | Provenance::SystemNote { .. }
            | Provenance::ChildResult { .. } => SegmentKind::Turn,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Compile-time regression guard for the WI-003 criterion: if any future
    // change adds `#[derive(Default)]` (or a manual `Default`) to
    // `PromptSegment`, compilation of the test suite fails here — a segment
    // must never be constructible without stated provenance.
    static_assertions::assert_not_impl_any!(PromptSegment: Default);

    #[test]
    fn constructor_requires_provenance() {
        // There is no way to build a `PromptSegment` without supplying a
        // `Provenance`, and no `Default` impl to fall back on.
        let seg = PromptSegment::new(Role::User, vec![], Provenance::UserPrompt);
        assert_eq!(seg.provenance, Provenance::UserPrompt);
        assert!(seg.cache_hint.is_none());
        assert!(seg.tokens_est.is_none());
    }

    #[test]
    fn builder_methods_set_optional_fields() {
        let hint = CacheHint {
            breakpoint: true,
            ttl: CacheTtl::FiveMinutes,
            prefix_key: PrefixKey::from_blake3(blake3::hash(b"seg")),
        };
        let seg = PromptSegment::new(
            Role::System,
            vec![],
            Provenance::AgentDef { name: "r".into() },
        )
        .with_cache_hint(hint.clone())
        .with_tokens_est(128);
        assert_eq!(seg.cache_hint, Some(hint));
        assert_eq!(seg.tokens_est, Some(128));
    }

    #[test]
    fn strip_cache_hints_clears_every_hint_without_touching_content() {
        let hint = CacheHint {
            breakpoint: true,
            ttl: CacheTtl::OneHour,
            prefix_key: PrefixKey::from_blake3(blake3::hash(b"seg")),
        };
        let mut segments = vec![
            PromptSegment::new(
                Role::System,
                vec![ContentBlock::Text { text: "sys".into() }],
                Provenance::AgentDef { name: "r".into() },
            )
            .with_cache_hint(hint.clone()),
            PromptSegment::new(
                Role::User,
                vec![ContentBlock::Text { text: "hi".into() }],
                Provenance::UserPrompt,
            ),
        ];
        let before: Vec<Vec<ContentBlock>> = segments.iter().map(|s| s.content.clone()).collect();
        strip_cache_hints(&mut segments);
        assert!(segments.iter().all(|s| s.cache_hint.is_none()));
        let after: Vec<Vec<ContentBlock>> = segments.iter().map(|s| s.content.clone()).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn segment_kind_from_provenance() {
        assert_eq!(
            SegmentKind::from(&Provenance::AgentDef { name: "r".into() }),
            SegmentKind::SystemPrompt
        );
        assert_eq!(
            SegmentKind::from(&Provenance::Skill { name: "s".into() }),
            SegmentKind::SkillFragment
        );
        assert_eq!(
            SegmentKind::from(&Provenance::ToolRegistry { hash: "h".into() }),
            SegmentKind::ToolSchemas
        );
        assert_eq!(
            SegmentKind::from(&Provenance::Inherited {
                from: crate::ids::SessionId::new(),
                seq_range: crate::ids::SeqRange::full(),
            }),
            SegmentKind::InheritedPrefix
        );
        assert_eq!(
            SegmentKind::from(&Provenance::UserPrompt),
            SegmentKind::Directive
        );
        assert_eq!(
            SegmentKind::from(&Provenance::ForkDirective {
                by: crate::ids::AgentId::new()
            }),
            SegmentKind::Directive
        );
        assert_eq!(
            SegmentKind::from(&Provenance::SystemNote { reason: "x".into() }),
            SegmentKind::Turn
        );
        assert_eq!(
            SegmentKind::from(&Provenance::ChildResult {
                from: crate::ids::AgentId::new()
            }),
            SegmentKind::Turn
        );
    }
}
