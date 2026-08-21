//! Piped-stdin composability for one-shot mode -- board item
//! 01M0286TWE0F1QDBPDM13HE8P8 ("One-shot cannot be piped into: no
//! streaming stdin, so conway is not a filter").
//!
//! **Verification anchor, honored literally.** Every positive test below
//! drives the REAL, compiled `conway` binary with stdin attached to a REAL
//! OS pipe ([`Stdio::piped`], written to on a background thread) and reads
//! back what the mock backend actually received on the wire
//! (`mock.requests()`) -- never a parsed-flag assertion, and never a temp
//! file whose *path* is handed to some flag instead of piping real bytes: a
//! test built that way would prove nothing about this item, per its own
//! binding notes. `common::command`'s default `Stdio::null()` stdin is
//! what every OTHER suite in this crate uses (none of them pipe anything
//! in); this file is the one that replaces that with a live pipe.
//!
//! **Precedence, stated once, tested by every test below that names it.**
//! `oneshot::read_prompt` treats `-p <text>`'s own value as the DIRECTIVE
//! and piped (non-terminal) stdin as the DATA it operates on -- the same
//! split Unix `grep PATTERN` already makes between its own argv pattern
//! and the corpus it reads from stdin. When both are present they are
//! joined, directive first, separated by a blank line. When only one is
//! present, that one alone is the prompt, unchanged from every version of
//! this flag before this item. See `oneshot.rs`'s own module doc,
//! reconciliation #5, for the full disclosure (including the one
//! behavioral trade-off this precedence accepts: stdin is now probed even
//! when `-p` already has text).
//!
//! This file deliberately does **not** add a test that requires a real
//! terminal -- `crates/conway/tests/interactive_tools.rs` already HANGS
//! without a TTY, and this item's own hazard note is explicit that it must
//! not add a second. [`interactive_dispatch_does_not_read_piped_stdin_as_a_prompt`]
//! proves the interactive path is unaffected by piping real (unread) bytes
//! into stdin using only non-TTY plumbing, bounded by a timeout that fails
//! the test outright rather than ever blocking a test run.

// This suite builds its own `Command` rather than calling
// `common::run_conway`, because `common::command` sets
// `stdin(Stdio::null())` and a real pipe is the entire subject here.
// Same `allow` every other test file in this directory carries, for
// the same reason: the shared module is compiled in full by each
// binary, which uses only the helpers it needs.
#[allow(dead_code)]
mod common;

use std::io::{Read, Write};
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{command, write_fixture, Fixture};

fn ok_script() -> Script {
    Script(vec![vec![Chunk::Text("ok"), Chunk::Finish("stop")]])
}

/// The `content` string of every `role: "user"` message in `request`'s
/// `messages` array, concatenated -- mirrors
/// `oneshot_persona_and_budget.rs`'s own `system_message_text` helper, for
/// the user-role half of the same request shape.
fn user_message_text(request: &serde_json::Value) -> String {
    request["messages"]
        .as_array()
        .expect("request must carry a messages array")
        .iter()
        .filter(|m| m["role"] == "user")
        .map(|m| m["content"].as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Spawns the real `conway` binary (via `common::command`, so `--config`/
/// `CONWAY_CONFIG_DIR`/cwd isolation all still apply) with a REAL OS pipe
/// attached to stdin, writes `stdin_bytes` into it on a background thread,
/// and waits up to `bound` for the process to exit -- draining stdout/
/// stderr concurrently on their own threads throughout, so a payload
/// larger than the OS pipe buffer can never deadlock this harness against
/// the child's own output buffers filling up first (`jsonl_streams_
/// incrementally` in `oneshot.rs` establishes the same "read while it
/// runs" shape for stdout alone; this adds the stdin-writer side).
///
/// Returns `None` if the process did not exit within `bound` -- the
/// process is killed and reaped either way, so no test in this file can
/// leak a hung child, and a caller that expects termination should
/// `.expect(...)` the `Option` rather than let a hang pass silently.
fn run_piped(
    args: &[&str],
    fixture: &Fixture,
    stdin_bytes: &[u8],
    bound: Duration,
) -> Option<Output> {
    let mut cmd = command(args, fixture);
    cmd.stdin(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn conway");
    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");

    let payload = stdin_bytes.to_vec();
    let writer = std::thread::spawn(move || {
        // A write error here (e.g. the child exited, or never read to
        // EOF, before this finished) is not this helper's problem to
        // report -- the timeout/kill path below and the caller's own
        // assertions on the returned `Output` are what a test actually
        // checks.
        let _ = stdin.write_all(&payload);
        // `stdin` drops at the end of this closure either way, closing
        // the pipe's write end -- the child's `read_to_string` sees EOF.
    });
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + bound;
    let status = loop {
        if let Ok(Some(status)) = child.try_wait() {
            break Some(status);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    writer.join().expect("stdin writer thread joins");
    let stdout = stdout_reader.join().expect("stdout reader thread joins");
    let stderr = stderr_reader.join().expect("stderr reader thread joins");

    status.map(|status| Output {
        status,
        stdout,
        stderr,
    })
}

// ---------------------------------------------------------------------
// Bare `-p`: piped stdin alone is the prompt (pre-item behavior,
// re-proven here through a REAL pipe rather than `Stdio::null()`).
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bare_print_reads_piped_stdin_as_the_whole_prompt() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_piped(
        &["-p"],
        &fixture,
        b"PIPED-PROMPT-MARKER",
        Duration::from_secs(20),
    )
    .expect("conway must exit within the bound, not hang reading its own real stdin pipe");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "exactly one /chat/completions request");
    assert_eq!(
        user_message_text(&requests[0]),
        "PIPED-PROMPT-MARKER",
        "the piped bytes must reach the provider request byte-for-byte as the user message"
    );
}

// ---------------------------------------------------------------------
// Precedence: `-p <text>` + piped stdin.
// ---------------------------------------------------------------------

/// The item's own motivating example, driven for real: `-p "what broke?"`
/// with `error.log`'s content on a real pipe. Both reach the model,
/// directive first -- neither is silently dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_prompt_and_piped_stdin_are_joined_directive_first() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_piped(
        &["-p", "what broke?"],
        &fixture,
        b"PIPED-ERROR-LOG-CONTENT",
        Duration::from_secs(20),
    )
    .expect("conway must exit within the bound, not hang reading its own real stdin pipe");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "exactly one /chat/completions request");
    assert_eq!(
        user_message_text(&requests[0]),
        "what broke?\n\nPIPED-ERROR-LOG-CONTENT",
        "an explicit -p prompt plus real piped stdin must reach the provider joined, \
         directive first -- this is the precedence this item picks, states, and pins here"
    );
}

/// The other half of the same precedence: `-p <text>` with a real pipe
/// attached but carrying no bytes at all behaves exactly as it did before
/// this item (argv text alone) -- piping an empty/closed stream must never
/// inject a stray separator or otherwise perturb the prompt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_prompt_with_empty_real_pipe_is_unaffected() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_piped(&["-p", "just this"], &fixture, b"", Duration::from_secs(20))
        .expect("conway must exit within the bound, not hang reading its own real stdin pipe");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "exactly one /chat/completions request");
    assert_eq!(
        user_message_text(&requests[0]),
        "just this",
        "an explicit -p prompt with an empty (but real, piped) stdin must be sent alone, \
         unchanged from this flag's pre-item behavior"
    );
}

// ---------------------------------------------------------------------
// Large input: bigger than argv could ever carry.
// ---------------------------------------------------------------------

/// A payload well past 128 KiB -- Linux's real `MAX_ARG_STRLEN`, the
/// kernel's hard per-argument ceiling on `execve` (POSIX itself only
/// guarantees 4096 bytes, `_POSIX_ARG_MAX`) -- passed on stdin, not argv:
/// this content could not have been handed to `-p` as a literal value on a
/// real system at all. Reading it to EOF and sending it through, in full,
/// byte-for-byte, is this item's other reason for existing (alongside the
/// precedence pinned above).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn large_piped_input_bigger_than_argv_would_allow_reaches_the_request() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);

    const MAX_ARG_STRLEN: usize = 128 * 1024; // Linux's real per-argument execve() ceiling.
    let unit = "the quick brown fox jumps over the lazy dog.";
    let mut big = String::with_capacity(MAX_ARG_STRLEN * 2);
    while big.len() < MAX_ARG_STRLEN * 3 / 2 {
        big.push_str(unit);
    }
    assert!(
        big.len() > MAX_ARG_STRLEN,
        "fixture payload must itself exceed the limit this test is proving conway isn't \
         bound by: {} bytes vs a {MAX_ARG_STRLEN}-byte ceiling",
        big.len()
    );

    let out = run_piped(&["-p"], &fixture, big.as_bytes(), Duration::from_secs(30))
        .expect("conway must exit within the bound even for a large piped payload");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), 1, "exactly one /chat/completions request");
    assert_eq!(
        user_message_text(&requests[0]).len(),
        big.len(),
        "the full piped payload's length must reach the provider request, not a truncated \
         prefix"
    );
    assert_eq!(
        user_message_text(&requests[0]),
        big,
        "the full piped payload must reach the provider request byte-for-byte"
    );
}

// ---------------------------------------------------------------------
// The interactive path is unaffected -- proven without a TTY.
// ---------------------------------------------------------------------

/// With no `-p` and no subcommand, `main.rs`'s own dispatch takes the TUI
/// arm, not `oneshot::run` -- `read_prompt`'s new stdin-reading precedence
/// lives entirely inside `oneshot.rs` and is never reachable from here.
/// This test pipes real, substantial, deliberately UNREAD bytes into
/// stdin and asserts the process still exits promptly: this harness's own
/// stdout is a pipe, not a terminal, so `tui::run`'s `enable_raw_mode()`
/// fails immediately -- a real, non-TTY, real-binary proof that the
/// interactive path never blocks trying to consume piped stdin as a
/// prompt (and never silently treats it as one either, since zero
/// requests ever reach the mock). Bounded by `run_piped`'s own timeout, so
/// a regression that made the TUI start reading stdin here would fail
/// this test outright rather than hang it -- the exact hazard this item's
/// own binding notes warn against reproducing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_dispatch_does_not_read_piped_stdin_as_a_prompt() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_piped(
        &[],
        &fixture,
        b"UNREAD-STDIN-MUST-NEVER-BECOME-A-PROMPT",
        Duration::from_secs(20),
    )
    .expect(
        "the interactive dispatch path must exit within the bound -- a hang here would mean \
         piped stdin is somehow being consumed on the TUI path",
    );

    assert!(
        !out.status.success(),
        "no real terminal is attached in this harness, so the TUI is expected to fail to \
         start; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        mock.requests().is_empty(),
        "the interactive dispatch path must never reach a model at all -- piped stdin is not \
         a prompt on this path"
    );
}
