//! Context assembly (WI-077): `ContextBuilder` assembles an agent's request
//! context in the fixed architecture §5.3 order, with complete provenance
//! and cache hints. `report` (WI-087) persists and reads back the
//! assembled `ContextReport`.

pub mod builder;
pub mod prefix;
pub mod report;

pub use builder::{
    ContextBuilder, ContextInput, HeadSegment, InheritedPrefix, SkillFragment, SystemPromptSpec,
    TOKEN_ESTIMATOR,
};
pub use prefix::prefix_key;
