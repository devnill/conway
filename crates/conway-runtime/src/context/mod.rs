//! Context assembly: `ContextBuilder` assembles an agent's request
//! context in the fixed architecture §5.3 order, with complete provenance
//! and cache hints. `report` persists and reads back the
//! assembled `ContextReport`.

pub mod builder;
pub mod curator_stage;
pub(crate) mod hook_guard;
pub mod path;
pub mod prefix;
pub mod report;
pub mod script_hook;

pub use builder::{
    ContextBuilder, ContextInput, InheritedPrefix, SkillFragment, SystemPromptSpec, TOKEN_ESTIMATOR,
};
pub use hook_guard::GuardedContextHook;
pub use prefix::prefix_key;
pub use script_hook::{apply_script_deltas, AppliedContextEdit, SkippedAppend};
