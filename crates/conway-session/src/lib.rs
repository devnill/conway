//! `conway-session`: persistence for the conway agent harness (architecture
//! §7, "Module: conway-session").
//!
//! Owns the `JsonlSessionStore` implementation of `conway_core::ports::
//! SessionStore`, session log record (de)serialization (the JSONL line
//! codec, [`codec`]), ancestry resolution ([`resolver`]), fork-by-reference
//! ([`fork`]), and the derived session index ([`index`]). Not responsible
//! for deciding *what* to persist (that's `conway-runtime`), context
//! assembly, or in-memory agent state.
//!
//! This crate is a skeleton as of [`meta`] and [`codec`] are fully
//! implemented (session metadata re-exports and the header/record line
//! codec); [`store`], [`fork`], [`resolver`], [`index`], and [`provenance`]
//! are stubs with signatures fixed here, filled in by earlier work through
//! respectively.

pub mod codec;
pub mod discovery;
pub mod fork;
pub mod index;
pub mod memory_store;
pub mod meta;
pub mod path_store;
pub mod provenance;
pub mod resolver;
pub mod store;

pub use index::SessionIndex;
pub use memory_store::FsMemoryStore;
pub use meta::{ForkOrigin, SessionFilter, SessionMeta};
pub use path_store::{FsPathIndex, FsPathStore};
pub use provenance::ContextReport;
pub use resolver::TranscriptResolver;
pub use store::{FsyncPolicy, JsonlSessionStore, StoreConfig};
