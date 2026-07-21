//! Crate-internal error plumbing.
//!
//! `conway_core::error::RuntimeError` is the runtime's one public error
//! type (architecture §4, §8); this module holds the crate's `Result` alias
//! and conversions, and does not define a second public error enum.

/// Shorthand for `Result<T, conway_core::error::RuntimeError>`, used
/// throughout the crate's public API.
pub type RuntimeResult<T> = Result<T, conway_core::error::RuntimeError>;
