//! One-shot mode's streaming renderers: [`make`] selects one of
//! [`text::TextRenderer`], [`json::JsonRenderer`], [`jsonl::JsonlRenderer`]
//! from `--output-format`. Every renderer writes through the same
//! `Box<dyn Write + Send>` -- `oneshot::run` always hands it a
//! `BufWriter<Stdout>`, flushed by the renderer itself after every write
//! (buffering across events, rather than within one event's own write, is
//! what the module spec forbids -- see each renderer's own doc).

pub mod json;
pub mod jsonl;
pub mod text;

use std::io;

use conway::{AgentId, AgentResult, Envelope};

use crate::cli::OutputFormat;

/// Consumes one session's event stream, incrementally.
///
/// `on_event` is called once per envelope, in stream order, for every
/// envelope up to and including the terminal `Event::AgentFinished`.
/// `finish` is then called exactly once, separately, with that envelope's
/// `AgentResult` -- or `None` if the run ended without ever producing one
/// (e.g. a SIGINT grace-window timeout).
///
/// The subscribed stream carries the whole session, and since
/// `AgentSpawned`/`AgentFinished` now bypass the stream's session/agent
/// filter (a subagent's lifecycle events reach a root subscriber -- see
/// `conway::EventStream`), a renderer that reacts to `AgentFinished` must
/// know which agent's finish is the run's own terminal occasion.
/// [`Renderer::set_root`] supplies that id; the default is a no-op for
/// renderers whose output doesn't depend on it.
pub trait Renderer: Send {
    fn on_event(&mut self, env: &Envelope) -> io::Result<()>;
    fn finish(&mut self, result: Option<&AgentResult>) -> io::Result<()>;

    /// Tells the renderer which agent is the run's root, so it can
    /// distinguish the root's terminal `AgentFinished` from a subagent's
    /// (which now also reaches the stream). Called once, before the first
    /// `on_event`. Default: ignore it.
    fn set_root(&mut self, _root: AgentId) {}
}

/// Selects the `Renderer` for `format`, writing through `out`.
pub fn make(format: OutputFormat, out: Box<dyn io::Write + Send>) -> Box<dyn Renderer> {
    match format {
        OutputFormat::Text => Box::new(text::TextRenderer::new(out)),
        OutputFormat::Json => Box::new(json::JsonRenderer::new(out)),
        OutputFormat::Jsonl => Box::new(jsonl::JsonlRenderer::new(out)),
    }
}

/// Shared test-only fixtures for `text.rs`/`json.rs`/`jsonl.rs`'s unit
/// tests, kept in one place so each renderer's test module isn't
/// re-declaring the same recording writer.
#[cfg(test)]
pub(crate) mod test_support {
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// A `Write` sink that records every byte written and counts `flush`
    /// calls, so a test can assert both the final byte content and exactly
    /// how many times the renderer under test flushed.
    #[derive(Clone, Default)]
    pub struct RecordingWriter {
        buf: Arc<Mutex<Vec<u8>>>,
        flushes: Arc<AtomicUsize>,
    }

    impl RecordingWriter {
        pub fn contents(&self) -> Vec<u8> {
            self.buf.lock().unwrap().clone()
        }

        pub fn flush_count(&self) -> usize {
            self.flushes.load(Ordering::SeqCst)
        }
    }

    impl io::Write for RecordingWriter {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.buf.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
}
