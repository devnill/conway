//! `TranscriptResolver` re-export.
//!
//! The resolver moved to `conway_core::transcript` (board item
//! 01KZVYVTVWRH20R6VJ6G3SWTJ6, "Stage 1a"): it is pure logic over the
//! `SessionStore` *port*, not over `JsonlSessionStore` specifically, so it
//! belongs beside the contract rather than inside this one adapter. This
//! module re-exports the type unchanged so existing callers of
//! `conway_session::TranscriptResolver` (and `conway_session::resolver::
//! TranscriptResolver`) keep compiling without edits.

pub use conway_core::transcript::TranscriptResolver;
