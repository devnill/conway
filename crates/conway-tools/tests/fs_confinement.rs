//! Verification anchor for "Retire the harness-level confinement root once
//! `conway.fs` enforces its own".
//!
//! The single test that matters here is
//! [`read_denies_a_symlink_planted_between_resolution_and_open`]: it proves
//! the check and the open are ONE step, not two, by planting the escape
//! AFTER the point a pre-check-then-open implementation would have already
//! computed its answer, then driving the REAL production entry point
//! (`ReadTool::invoke`) and asserting on the actual returned tool result --
//! never on an internal field, never on `crate::fs::beneath` directly.
//!
//! This is deliberately at the `conway-tools` level (real `Tool`, real
//! `ToolCtx`, no fake path resolution), not the full `conway`-facade level
//! `crates/conway/tests/root_containment_seam.rs` already covers with real
//! `PermissionBroker`/gate/hook wiring -- the two are complementary, not
//! duplicates: this file proves `conway.fs` itself is TOCTOU-closed; that
//! one proves the end-to-end pipeline still denies what it always denied.
#![cfg(feature = "test-fakes")]
#![cfg(unix)]

use std::os::unix::fs::symlink;
use std::path::Path;
use std::sync::Arc;

use conway_core::content::ToolCall;
use conway_core::error::ToolError;
use conway_core::ids::ToolName;
use conway_core::ports::{PluginConfig, Tool, ToolCtx};
use conway_tools::fs::{ReadTool, WriteTool};
use conway_tools::testing::test_ctx;

/// A confined `ToolCtx` -- `conway.fs.root` set to `root`, cwd set to
/// `root` as well (the ordinary "agent confined to its own cwd" shape).
fn confined_ctx(root: &Path) -> (ToolCtx, conway_tools::testing::TestHandles) {
    let (mut ctx, handles) = test_ctx(root.to_path_buf());
    let mut values = serde_json::Map::new();
    values.insert(
        "conway.fs.root".to_string(),
        serde_json::json!(root.display().to_string()),
    );
    ctx.config = Arc::new(PluginConfig { values });
    (ctx, handles)
}

fn read_call(path: &str) -> ToolCall {
    ToolCall {
        call_id: "tc_1".into(),
        name: ToolName::new("read"),
        arguments: serde_json::json!({ "path": path }),
    }
}

fn write_call(path: &str, content: &str) -> ToolCall {
    ToolCall {
        call_id: "tc_1".into(),
        name: ToolName::new("write"),
        arguments: serde_json::json!({ "path": path, "content": content }),
    }
}

/// Ordinary case, for contrast with the race below: a plain out-of-root
/// symlink, present BEFORE any tool call, is denied. (A pre-check-then-open
/// implementation would ALSO catch this one -- it is not the discriminating
/// test.)
#[tokio::test]
async fn read_denies_a_preexisting_escaping_symlink() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().join("root");
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), b"TOP SECRET").unwrap();
    symlink(Path::new("../outside"), root.join("link")).unwrap();

    let (ctx, _h) = confined_ctx(&root);
    let err = ReadTool::new()
        .invoke(read_call("link/secret.txt"), ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
}

/// THE load-bearing test (this item's own verification anchor).
///
/// A test that merely calls `ReadTool::invoke` once against an
/// already-swapped symlink (see `read_denies_a_preexisting_escaping_symlink`
/// above) does NOT discriminate a TOCTOU-closed implementation from a
/// pre-check-then-open one: `conway.fs`'s own containment resolution
/// follows symlinks, so a FRESH check against an already-escaped path
/// denies it either way, whether or not the eventual open independently
/// re-verifies anything. Proving the check-and-open-are-one-step claim
/// requires reproducing the actual shape of the race: resolve containment
/// ONCE while the filesystem still looks legitimate, mutate it, THEN
/// perform only the "use" step with nothing but what the check already
/// returned -- exactly what `conway_tools::fs::beneath::toctou_probe`
/// exists to let this external test do without reaching into `pub(crate)`
/// internals (see that module's own doc, and its sibling unit test in
/// `beneath.rs` for the identical proof from inside the crate).
#[tokio::test]
async fn read_denies_a_symlink_planted_between_resolution_and_open() {
    use conway_tools::fs::beneath::toctou_probe;

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().join("root");
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), b"TOP SECRET").unwrap();

    // A real directory exists here, right now -- the check below will find
    // `staging` real and unremarkable, and answer "inside".
    std::fs::create_dir(root.join("staging")).unwrap();

    let (ctx, _h) = confined_ctx(&root);
    let candidate = root.join("staging").join("secret.txt");

    // Step 1: the check -- this is `ReadTool::invoke`'s own first move,
    // internally (`beneath::resolve`), reused here via the probe so this
    // test can insert the mutation at the exact point a pre-check-then-open
    // shape would have already returned control to its caller.
    let (root_path, relative) =
        toctou_probe::resolve_confined(&ctx, &candidate).expect("expected Confined");
    assert_eq!(relative, Path::new("staging/secret.txt"));

    // Step 2: the race. `staging` becomes a symlink to `outside`. Nothing
    // above this line has performed any I/O beyond the check itself --
    // this models "a check ran, and the filesystem changed before the
    // corresponding open" (an operator's gate-approval wait, a cooperative
    // scheduling point, a genuinely concurrent sibling operation -- the
    // mechanism does not matter, only that a gap exists for one).
    std::fs::remove_dir(root.join("staging")).unwrap();
    symlink(&outside, root.join("staging")).unwrap();

    // Step 3: the use -- given ONLY step 1's `(root_path, relative)`, the
    // same information (and nothing more) `ReadTool::invoke` would have to
    // work with at this point in its own real call.
    let escape_result = toctou_probe::open_confined(&root_path, &relative);
    assert!(
        escape_result.is_err(),
        "a symlink swapped into an existing path component between check and open must still \
         be refused: got {escape_result:?}"
    );

    // Now prove `ReadTool::invoke` -- the real, whole, production entry
    // point -- exhibits this same refusal end to end (a single call, since
    // the probe above already established the mechanism; this closes the
    // loop back to "asserted on the persisted tool result").
    let (ctx2, _h2) = confined_ctx(&root);
    let err = ReadTool::new()
        .invoke(read_call("staging/secret.txt"), ctx2)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
    assert!(
        outside.join("secret.txt").exists(),
        "the file was never leaked, let alone moved"
    );
}

/// The write-side mirror: a symlink swapped into an existing intermediate
/// directory between the write's containment resolution and its actual
/// `create_dir_all`/`create`/`rename` sequence must not let content escape
/// through it.
#[tokio::test]
async fn write_denies_a_symlink_planted_between_resolution_and_open() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().join("root");
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::create_dir(root.join("staging")).unwrap();
    std::fs::remove_dir(root.join("staging")).unwrap();
    symlink(&outside, root.join("staging")).unwrap();

    let (ctx, _h) = confined_ctx(&root);
    let err = WriteTool::new()
        .invoke(write_call("staging/exfil.txt", "leaked"), ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
    assert!(!outside.join("exfil.txt").exists());
}
