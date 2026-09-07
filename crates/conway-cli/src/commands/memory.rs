//! `conway memory {list,forget}`: a built-in, headless audit/undo surface
//! over `conway::plugin::MemoryStore` -- the same store `conway.memory`'s
//! `remember`/`forget`/`list_memories` tools (and its own
//! `/conway.memory.list`/`remember`/`forget` operator commands, reachable
//! today only via the generic `conway <plugin-id>.<command>` external-
//! subcommand path, `commands::plugin::run`) already read and write.
//!
//! # Why this exists alongside `conway conway.memory.list`
//!
//! `conway conway.memory.list`/`conway.memory.forget <id>` already work --
//! `conway-plugin-memory`'s own `ListCommand`/`ForgetCommand`, resolved
//! through `crate::tui::commands::CommandRegistry` exactly like every other
//! plugin-declared command (`tests/memory_plugin_commands.rs` proves the
//! full round trip). This module does not replace that path or duplicate
//! its storage access; it adds a SECOND, complementary way to reach the
//! same store, for the reasons `sessions`/`routes`/`tools`/`plugin` are
//! their own built-in subcommands rather than plugin-declared ones:
//!
//! - **Discoverable from `conway --help`/`conway memory --help`.** A
//!   plugin-declared command only appears once `conway.memory` is resolved
//!   at runtime (`CommandRegistry::build` over the installed set) -- it is
//!   invisible to clap's own static help tree, and invisible entirely if
//!   `"conway.memory"` is not in `[plugins].install` (`tests/
//!   memory_plugin_commands.rs`'s `unknown_without_the_plugin_installed`).
//!   `conway memory list`/`conway memory forget` are always in `--help`,
//!   matching how `conway sessions`/`conway tools` behave regardless of
//!   which plugins are selected.
//! - **No throwaway session.** `commands::plugin::run` starts a fresh,
//!   prompt-less session purely to build a `CommandCtx` a `Command` impl
//!   can take (that module's own doc) -- overhead a pure store read/write
//!   never needed. This module talks to the `Arc<dyn MemoryStore>` `main.rs`
//!   already resolved directly, the identical `Arc` `commands::plugin::run`/
//!   `run_admin` are handed the same way (see `run`'s own doc below) --
//!   never a second, independently-opened store over the same root
//!   (`FsMemoryStore::put_lock`'s own doc is why that would matter).
//!
//! Both paths are safe to use interchangeably: same store, same on-disk
//! root once `conway.memory` is durable (`docs/plugins/memory.md`), no
//! locking hazard either creates that the other does not already have.
//!
//! # Absent `"conway.memory"` from `[plugins].install`
//!
//! `main.rs`'s `first_party_plugins::resolve_memory_store` hands every
//! caller in this binary -- this one included -- a fresh, empty, entirely
//! unattached `InMemoryMemoryStore` when `conway.memory` was never
//! selected (that function's own doc). `conway memory list` in that case
//! reports "no memories stored" -- truthfully: nothing could have been
//! remembered through a plugin that was never installed, so an empty
//! listing is not a degraded or misleading answer, just the honest one.
//! `conway memory forget <id>` in the same state always reports "no such
//! memory" for any id, for the identical reason.

use std::str::FromStr;
use std::sync::Arc;

use clap::{Args, Subcommand};
use conway::plugin::{MemoryStore, MemoryStoreError};
use conway::MemoryId;

use crate::commands::fmt;
use crate::diag;
use crate::exit::ExitCode;

#[derive(Args, Debug)]
pub struct MemoryArgs {
    #[command(subcommand)]
    pub action: MemoryAction,
}

#[derive(Subcommand, Debug)]
pub enum MemoryAction {
    /// List everything currently remembered: id, when it was written, and a
    /// text preview -- enough to decide what to `forget`.
    List {
        /// Print every field (full, untruncated text included) as a JSON
        /// array instead of the human-readable table.
        #[arg(long)]
        json: bool,
    },
    /// Forget (permanently remove) one remembered item by id, as reported
    /// by `conway memory list`.
    Forget {
        /// The memory's id (see the `ID` column of `conway memory list`).
        id: String,
    },
}

/// `memory_store` is the SAME `Arc<dyn MemoryStore>` `main.rs`'s
/// `dispatch` hands `commands::plugin::run`/`run_admin` -- resolved exactly
/// once, at process startup, by `first_party_plugins::resolve_memory_store`
/// (that function's own doc). This function never opens a second store.
pub async fn run(
    args: &MemoryArgs,
    memory_store: Arc<dyn MemoryStore>,
) -> conway::Result<ExitCode> {
    match &args.action {
        MemoryAction::List { json } => list(memory_store, *json).await,
        MemoryAction::Forget { id } => forget(memory_store, id).await,
    }
}

/// Deterministic, oldest-first ordering -- the same order
/// `MemoryInjectHook`/`conway-plugin-memory`'s own `render_memory_listing`
/// use, so an operator comparing this listing against `/context`'s preamble
/// or the model-facing `list_memories` output sees the same relative order
/// either way.
fn sorted(mut memories: Vec<conway::plugin::Memory>) -> Vec<conway::plugin::Memory> {
    memories.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));
    memories
}

fn memory_row(m: &conway::plugin::Memory) -> Vec<String> {
    vec![
        m.id.to_string(),
        fmt::ts(m.created),
        m.provenance
            .as_ref()
            .map(|p| p.session.to_string())
            .unwrap_or_default(),
        m.text.replace('\n', " "),
    ]
}

fn memory_json(m: &conway::plugin::Memory) -> serde_json::Value {
    serde_json::json!({
        "id": m.id.to_string(),
        "created": fmt::ts(m.created),
        "session": m.provenance.as_ref().map(|p| p.session.to_string()),
        "text": m.text,
    })
}

async fn list(memory_store: Arc<dyn MemoryStore>, json: bool) -> conway::Result<ExitCode> {
    let memories = match memory_store.list().await {
        Ok(memories) => sorted(memories),
        Err(e) => {
            diag::error(format!("could not list memories: {e}"));
            return Ok(ExitCode::Usage);
        }
    };

    if json {
        let arr: Vec<_> = memories.iter().map(memory_json).collect();
        println!(
            "{}",
            serde_json::to_string(&arr).expect("memory list always serializes")
        );
        return Ok(ExitCode::Completed);
    }

    if memories.is_empty() {
        println!("no memories stored");
        return Ok(ExitCode::Completed);
    }

    let rows = memories.iter().map(memory_row).collect();
    print!(
        "{}",
        fmt::table(&["ID", "CREATED", "SESSION", "TEXT"], rows)
    );
    Ok(ExitCode::Completed)
}

async fn forget(memory_store: Arc<dyn MemoryStore>, id: &str) -> conway::Result<ExitCode> {
    let parsed = match MemoryId::from_str(id) {
        Ok(id) => id,
        Err(e) => {
            diag::error(format!("not a valid memory id: {id:?} ({e})"));
            return Ok(ExitCode::Usage);
        }
    };
    match memory_store.remove(&parsed).await {
        Ok(()) => {
            println!("forgot memory {parsed}");
            Ok(ExitCode::Completed)
        }
        Err(MemoryStoreError::NotFound { .. }) => {
            diag::error(format!("no such memory: {parsed}"));
            Ok(ExitCode::Usage)
        }
        Err(e) => {
            diag::error(format!("could not forget {parsed}: {e}"));
            Ok(ExitCode::Usage)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway_plugin_memory::InMemoryMemoryStore;

    fn memory(text: &str, offset_secs: i64) -> conway::plugin::Memory {
        conway::plugin::Memory {
            id: MemoryId::new(),
            text: text.to_string(),
            created: chrono::DateTime::UNIX_EPOCH + chrono::Duration::seconds(offset_secs),
            provenance: None,
        }
    }

    #[test]
    fn sorted_is_oldest_first_with_id_tiebreak() {
        let a = memory("newer", 100);
        let b = memory("older", 1);
        let out = sorted(vec![a.clone(), b.clone()]);
        assert_eq!(out[0].id, b.id);
        assert_eq!(out[1].id, a.id);
    }

    #[tokio::test]
    async fn list_and_forget_round_trip_through_the_store() {
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let m = memory("remember this", 0);
        let id = m.id;
        store.put(m).await.unwrap();

        assert_eq!(
            list(store.clone(), false).await.unwrap(),
            ExitCode::Completed
        );

        let code = forget(store.clone(), &id.to_string()).await.unwrap();
        assert_eq!(code, ExitCode::Completed);
        assert!(store.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn forget_an_unknown_id_is_a_usage_error() {
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let code = forget(store, &MemoryId::new().to_string()).await.unwrap();
        assert_eq!(code, ExitCode::Usage);
    }

    #[tokio::test]
    async fn forget_a_malformed_id_is_a_usage_error() {
        let store: Arc<dyn MemoryStore> = Arc::new(InMemoryMemoryStore::new());
        let code = forget(store, "not-a-memory-id").await.unwrap();
        assert_eq!(code, ExitCode::Usage);
    }
}
