//! Board item 01KZHF3E1ZG3AZ7F7HHVY324T9's "not trapped in one mode" half:
//! `src/bin/thirdparty_backend_demo.rs` run as the REAL compiled binary
//! `cargo test` already built for this crate, via `assert_cmd` -- the same
//! mechanism `crates/conway-cli/tests/common/mod.rs`'s own
//! `command`/`run_conway` use against the real `conway` binary, one crate
//! over (that file's own module doc explains why a third party's
//! compiled-binary proof is necessarily their OWN binary, never `conway`'s
//! -- see `src/bin/thirdparty_backend_demo.rs`'s doc comment for the fuller
//! statement).

use assert_cmd::Command;

#[test]
fn compiled_binary_serves_a_real_turn_and_prints_its_text() {
    let mut cmd = Command::cargo_bin("thirdparty_backend_demo").expect("locate compiled binary");
    let output = cmd.output().expect("run compiled binary");

    assert!(
        output.status.success(),
        "binary must exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout is utf8");
    assert_eq!(
        stdout.trim_end(),
        conway_thirdparty_backend::REPLY_TEXT,
        "the compiled binary's own stdout must be the third-party backend's real reply, the \
         same text the library-embedder test (tests/end_to_end.rs) asserts through \
         conway::SessionHandle::prompt directly -- proving the identical capability from a \
         genuinely separate compiled process, not merely a function call inside this crate's \
         own test binary"
    );
}
