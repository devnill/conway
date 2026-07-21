//! `FsPlugin`: the file tools (`read`, `write`, `edit`, `glob`, `grep`).
//!
//! `read`/`write`/`edit` land in WI-062; `glob`/`grep` and the `FsPlugin`
//! assembly land in WI-063.

pub mod edit;
pub mod read;
pub mod write;

pub use edit::EditTool;
pub use read::ReadTool;
pub use write::WriteTool;
