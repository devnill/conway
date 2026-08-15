//! A host consuming the live event stream instead of waiting for
//! `TurnHandle::text`/`result` to resolve -- the shape a streaming chat UI,
//! a progress indicator, or a tool-call activity log actually needs.
//!
//! ```console
//! cargo run -p conway --example event_stream_consumer
//! ```
//!
//! [`conway::SessionHandle::events`] (and [`conway::TurnHandle::events`] for
//! one turn only) return [`conway::EventStream`], which implements
//! `futures_core::Stream<Item = conway::Envelope>` -- the facade itself
//! depends on `futures-core` only, so a consumer adds `futures` (or
//! `futures-util`) to get `.next()` and drive it in a loop, exactly like
//! `conway-cli`'s own one-shot renderer does
//! (`crates/conway-cli/src/oneshot.rs`).
//!
//! **Subscribe before you act.** `events()` is called BEFORE `prompt()`
//! below, not after -- `SessionHandle::prompt`'s own doc: it takes out the
//! broadcast subscription first, then appends the prompt, so the turn's own
//! first events can never be missed by a subscribe-after-append race.
//! Reversing the two lines below would occasionally lose the very first
//! `TextDelta`.
//!
//! Runs fully offline via `conway_testkit::FakeBackend`, whose `stream`
//! implementation chunks its canned response into several `StreamChunk`s
//! (`conway_testkit`'s own `decompose_to_chunks`) specifically so an example
//! like this one has more than one `TextDelta` to observe, not a single
//! chunk that would make "consuming a STREAM" indistinguishable from
//! `TurnHandle::text().await` collecting it whole.

use std::sync::Arc;

use conway::backend::{BackendId, ModelId};
use conway::{ConwayBuilder, Event, ModelRef, PermissionDecision, SessionSpec};
use conway_testkit::{FakeBackend, FakeGate, FakeRouter, FakeStore};
use futures::StreamExt;

/// See `discover_getting_started.rs`'s own doc/copy of this same helper:
/// isolates `ConwayBuilder::discover()` below from whatever happens to be
/// configured on the machine running this example, purely for
/// reproducibility. A real host application does not do this.
fn isolate_ambient_config_for_this_example() {
    let scratch = std::env::temp_dir().join(format!(
        "conway-event-stream-consumer-example-{}",
        std::process::id()
    ));
    let xdg = scratch.join("xdg-config-home");
    let cwd = scratch.join("cwd");
    std::fs::create_dir_all(&xdg).expect("create scratch XDG_CONFIG_HOME");
    std::fs::create_dir_all(&cwd).expect("create scratch cwd");
    std::env::set_var("XDG_CONFIG_HOME", &xdg);
    std::env::set_current_dir(&cwd).expect("set scratch cwd");
}

#[tokio::main]
async fn main() -> conway::Result<()> {
    isolate_ambient_config_for_this_example();

    let backend = Arc::new(FakeBackend::echo(BackendId::new("fake")));
    let route = ModelRef {
        backend: BackendId::new("fake"),
        model: ModelId::new("echo-model"),
    };

    let conway = ConwayBuilder::discover()?
        .with_backend(backend)
        .with_router(Arc::new(FakeRouter::single(route)))
        .with_permission_gate(Arc::new(FakeGate::new(PermissionDecision::AllowOnce)))
        .with_session_store(Arc::new(FakeStore::new()))
        .build()?;

    let session = conway.new_session(SessionSpec::default()).await?;

    // Subscribe first (see this module's own doc on why the order matters),
    // then prompt -- `prompt` returns once the runtime has accepted the
    // turn, not once it has finished, so the loop below is racing the
    // agent's own turn live, not replaying it after the fact.
    let mut events = session.events();
    let _turn = session.prompt("Hello, conway!").await?;

    let mut reply = String::new();
    while let Some(envelope) = events.next().await {
        match envelope.event {
            Event::TextDelta { text } => {
                reply.push_str(&text);
                print!("{text}");
            }
            Event::ToolCallProposed { tool, .. } => {
                println!("\n[tool call proposed: {tool}]");
            }
            // `AgentFinished` bypasses this stream's own session/agent
            // filter (tree lifecycle is a global concern -- see
            // `docs/embedding.md`'s own "Consuming the event stream"
            // section), so a consumer scoped to one agent -- this example's
            // whole point -- must itself check which agent finished before
            // treating it as ITS OWN turn's terminal event.
            Event::AgentFinished { result, .. } if result.agent_id == session.root() => {
                println!("\n[turn finished: {:?}]", result.status);
                break;
            }
            _ => {}
        }
    }

    println!("full reply, assembled from the stream alone -> {reply:?}");

    Ok(())
}
