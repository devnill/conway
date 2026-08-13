//! conway-runtime: the agent engine.
//!
//! Agent loop, agent tree + supervisor, mailboxes, context assembly,
//! provenance tracking, plugin registry, permission brokering, tool
//! execution, backend attempt/fallback sequencing, the event bus, budgets,
//! and the `SubagentHost` implementation (architecture §7, `## Module:
//! conway-runtime`).
//!
//! This item (WI-076) establishes the crate and its module layout so no
//! later work item ever contends on this file, plus the [`events`] module's
//! [`events::EventBus`] — the bus every other component emits through.
//! Every other module listed here is a placeholder until its owning work
//! item lands.

pub mod agent_loop;
pub mod artifact_store;
pub mod attempt;
pub mod context;
pub mod error;
pub mod events;
pub mod mailbox;
pub mod observation;
pub mod permission;
pub mod result;
pub mod runtime;
pub mod step_digest;
pub mod subagent;
pub mod supervisor;
pub mod tools;
pub mod tree;
