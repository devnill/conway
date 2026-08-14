//! Integration coverage for `FsPlugin`'s search tools (`glob`, `grep`) and
//! the `FsPlugin` assembly (criteria).
//!
//! Requires the `test-fakes` feature (for `conway_tools::testing::test_ctx`).
//! Declared with `required-features = ["test-fakes"]` in Cargo.toml, so a
//! plain `cargo test -p conway-tools` skips (not fails) this file.

#![cfg(feature = "test-fakes")]

use std::time::{Duration, SystemTime};

use conway_core::content::{ContentBlock, ToolCall, ToolCategory};
use conway_core::error::ToolError;
use conway_core::ids::ToolName;
use conway_core::ports::{Plugin, Tool};
use conway_tools::fs::{EditTool, FsPlugin, GlobTool, GrepTool, ReadTool, WriteTool};
use conway_tools::testing::test_ctx;
use tempfile::TempDir;

fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        call_id: "tc_1".into(),
        name: ToolName::new(name),
        arguments,
    }
}

fn text_of(out: &conway_core::ports::ToolOutput) -> &str {
    match &out.blocks[0] {
        ContentBlock::Text { text } => text,
        other => panic!("expected a text block, got {other:?}"),
    }
}

fn set_mtime(path: &std::path::Path, offset_secs: u64) {
    let file = std::fs::File::options().write(true).open(path).unwrap();
    let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + offset_secs);
    file.set_modified(time).unwrap();
}

// ------------------------------------------------------------- FsPlugin ---

#[test]
fn fs_plugin_manifest_and_tools() {
    let plugin = FsPlugin::new();
    let manifest = plugin.manifest();
    assert_eq!(manifest.id, "conway.fs");

    let mut names: Vec<String> = plugin
        .tools()
        .iter()
        .map(|t| t.spec().name.as_str().to_string())
        .collect();
    names.sort();
    assert_eq!(names, vec!["cd", "edit", "glob", "grep", "read", "write"]);
}

#[test]
fn tool_names_and_categories_are_as_specified() {
    assert_eq!(GlobTool::new().spec().name.as_str(), "glob");
    assert_eq!(GlobTool::new().spec().category, ToolCategory::Search);
    assert_eq!(GrepTool::new().spec().name.as_str(), "grep");
    assert_eq!(GrepTool::new().spec().category, ToolCategory::Search);
    // The core three still round-trip through the same plugin surface.
    assert_eq!(ReadTool::new().spec().name.as_str(), "read");
    assert_eq!(WriteTool::new().spec().name.as_str(), "write");
    assert_eq!(EditTool::new().spec().name.as_str(), "edit");
}

// ---------------------------------------------------------------- glob ----

#[tokio::test]
async fn glob_matches_nested_rust_files_relative_to_root() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn a() {}").unwrap();
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/b.rs"), "fn b() {}").unwrap();
    std::fs::write(dir.path().join("sub/c.txt"), "not rust").unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = GlobTool::new()
        .invoke(call("glob", serde_json::json!({"pattern": "**/*.rs"})), ctx)
        .await
        .unwrap();

    assert!(!out.is_error);
    let mut lines: Vec<&str> = text_of(&out).lines().collect();
    lines.sort();
    assert_eq!(lines, vec!["a.rs", "sub/b.rs"]);
}

#[tokio::test]
async fn glob_excludes_git_dir_and_gitignored_paths() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
    std::fs::create_dir_all(dir.path().join("target")).unwrap();
    std::fs::write(dir.path().join("target/x.rs"), "fn x() {}").unwrap();
    std::fs::create_dir_all(dir.path().join(".git/objects")).unwrap();
    std::fs::write(dir.path().join(".git/objects/pack.rs"), "not real").unwrap();
    std::fs::write(dir.path().join("kept.rs"), "fn kept() {}").unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = GlobTool::new()
        .invoke(call("glob", serde_json::json!({"pattern": "**/*.rs"})), ctx)
        .await
        .unwrap();

    assert!(!out.is_error);
    assert_eq!(text_of(&out), "kept.rs");
}

#[tokio::test]
async fn glob_orders_by_mtime_descending_then_lexicographic() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("older.rs"), "1").unwrap();
    std::fs::write(dir.path().join("b_tie.rs"), "2").unwrap();
    std::fs::write(dir.path().join("a_tie.rs"), "3").unwrap();
    set_mtime(&dir.path().join("older.rs"), 0);
    set_mtime(&dir.path().join("b_tie.rs"), 100);
    set_mtime(&dir.path().join("a_tie.rs"), 100);
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = GlobTool::new()
        .invoke(call("glob", serde_json::json!({"pattern": "*.rs"})), ctx)
        .await
        .unwrap();

    assert!(!out.is_error);
    let lines: Vec<&str> = text_of(&out).lines().collect();
    assert_eq!(lines, vec!["a_tie.rs", "b_tie.rs", "older.rs"]);
}

#[tokio::test]
async fn glob_zero_matches_is_recoverable_error_with_pattern() {
    let dir = TempDir::new().unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = GlobTool::new()
        .invoke(
            call("glob", serde_json::json!({"pattern": "*.nonexistent"})),
            ctx,
        )
        .await
        .unwrap();

    assert!(out.is_error);
    assert_eq!(text_of(&out), "no files matched *.nonexistent");
}

#[tokio::test]
async fn glob_over_limit_truncates_with_more_matches_suffix() {
    let dir = TempDir::new().unwrap();
    for i in 0..5 {
        std::fs::write(dir.path().join(format!("f{i}.rs")), "x").unwrap();
    }
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = GlobTool::new()
        .invoke(
            call("glob", serde_json::json!({"pattern": "*.rs", "limit": 2})),
            ctx,
        )
        .await
        .unwrap();

    assert!(!out.is_error);
    let text = text_of(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[2], "… (3 more matches)");
}

// ---------------------------------------------------------------- grep ----

#[tokio::test]
async fn grep_finds_matching_lines_grouped_by_path_in_walk_order() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn a() {}\nlet x = 1;\n").unwrap();
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/b.rs"), "fn b() {}\n").unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = GrepTool::new()
        .invoke(call("grep", serde_json::json!({"pattern": r"fn \w+"})), ctx)
        .await
        .unwrap();

    assert!(!out.is_error);
    let text = text_of(&out);
    assert!(!text.contains("let x"));
    // Grouped by path IN WALK ORDER: every line for one
    // path is contiguous, and the multi-file ordering is asserted, not just
    // membership. Root files walk before subdirectory files here.
    let lines: Vec<&str> = text.lines().filter(|l| l.contains(':')).collect();
    assert_eq!(lines, vec!["a.rs:1:fn a() {}", "sub/b.rs:1:fn b() {}"]);
}

#[tokio::test]
async fn grep_case_insensitive_matches_differing_case() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("f.txt"), "Hello World\n").unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = GrepTool::new()
        .invoke(
            call(
                "grep",
                serde_json::json!({"pattern": "hello", "case_insensitive": true}),
            ),
            ctx,
        )
        .await
        .unwrap();

    assert!(!out.is_error);
    assert!(text_of(&out).contains("f.txt:1:Hello World"));
}

#[tokio::test]
async fn grep_invalid_regex_is_error_with_parse_error_text() {
    let dir = TempDir::new().unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = GrepTool::new()
        .invoke(call("grep", serde_json::json!({"pattern": "("})), ctx)
        .await
        .unwrap();

    assert!(out.is_error);
    // regex's own parse error text for an unclosed `(` is literally
    // "unclosed group" — assert the underlying parse error is embedded.
    assert!(text_of(&out).contains("unclosed group"));
}

#[tokio::test]
async fn grep_skips_binary_files_silently() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("bin.dat"), [b'f', b'n', 0u8, b' ', b'x']).unwrap();
    std::fs::write(dir.path().join("text.txt"), "fn x() {}\n").unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = GrepTool::new()
        .invoke(call("grep", serde_json::json!({"pattern": "fn"})), ctx)
        .await
        .unwrap();

    assert!(!out.is_error);
    let text = text_of(&out);
    assert!(text.contains("text.txt"));
    assert!(!text.contains("bin.dat"));
}

#[tokio::test]
async fn grep_glob_argument_filters_candidate_files() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(dir.path().join("a.txt"), "fn a() {}\n").unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = GrepTool::new()
        .invoke(
            call("grep", serde_json::json!({"pattern": "fn", "glob": "*.rs"})),
            ctx,
        )
        .await
        .unwrap();

    assert!(!out.is_error);
    let text = text_of(&out);
    assert!(text.contains("a.rs"));
    assert!(!text.contains("a.txt"));
}

#[tokio::test]
async fn grep_zero_matches_is_recoverable_error_with_pattern() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("f.txt"), "hello\n").unwrap();
    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = GrepTool::new()
        .invoke(call("grep", serde_json::json!({"pattern": "nomatch"})), ctx)
        .await
        .unwrap();

    assert!(out.is_error);
    assert_eq!(text_of(&out), "no matches for nomatch");
}

// ------------------------------------------------------------- cancellation

/// Builds a tree of `count` files under `dir`, spread across nested
/// subdirectories, each containing a line that would match a trivial
/// pattern — enough walk work that a concurrent cancel lands mid-walk
/// rather than before or after it.
fn build_large_tree(dir: &std::path::Path, count: usize) {
    for i in 0..count {
        let sub = dir.join(format!("d{}", i % 20));
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(format!("f{i}.rs")), "fn marker() {}\n").unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn glob_cancelled_from_another_task_mid_walk_returns_cancelled() {
    let dir = TempDir::new().unwrap();
    build_large_tree(dir.path(), 5000);
    let (ctx, handles) = test_ctx(dir.path().to_path_buf());

    let cancel = handles.cancel.clone();
    let task = tokio::spawn(async move {
        GlobTool::new()
            .invoke(call("glob", serde_json::json!({"pattern": "**/*.rs"})), ctx)
            .await
    });
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_micros(200)).await;
        cancel.cancel();
    });
    let result = task.await.unwrap();

    assert!(matches!(result, Err(ToolError::Cancelled)), "{result:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grep_cancelled_from_another_task_mid_walk_returns_cancelled() {
    let dir = TempDir::new().unwrap();
    build_large_tree(dir.path(), 5000);
    let (ctx, handles) = test_ctx(dir.path().to_path_buf());

    let cancel = handles.cancel.clone();
    let task = tokio::spawn(async move {
        GrepTool::new()
            .invoke(call("grep", serde_json::json!({"pattern": "marker"})), ctx)
            .await
    });
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_micros(200)).await;
        cancel.cancel();
    });
    let result = task.await.unwrap();

    assert!(matches!(result, Err(ToolError::Cancelled)), "{result:?}");
}

// ------------------------------------------------------- walk determinism ----

/// The ordering assertion in
/// `grep_finds_matching_lines_grouped_by_path_in_walk_order` above cannot
/// actually detect an unsorted walk: it creates `a.rs` before `sub/`, so
/// `read_dir` order and sorted order usually agree and it passes either way.
/// That is why an unsorted `walk_files` survived until it failed in CI on a
/// run with no code change.
///
/// This case is built to fail without the sort: every entry is CREATED in the
/// reverse of its sorted order, so a walker yielding creation/inode order
/// produces exactly the reversed list, and only a real sort produces the
/// expected one.
#[tokio::test]
async fn grep_walk_order_is_sorted_not_merely_filesystem_order() {
    let dir = TempDir::new().unwrap();

    // Created last-to-first on purpose. `z_` and `m_` sort after `a_`, and the
    // subdirectory sorts between them, so no accident of creation order
    // reproduces the expected sequence.
    std::fs::create_dir_all(dir.path().join("mid")).unwrap();
    std::fs::write(dir.path().join("z_last.rs"), "fn z() {}\n").unwrap();
    std::fs::write(dir.path().join("mid/m_two.rs"), "fn m2() {}\n").unwrap();
    std::fs::write(dir.path().join("mid/m_one.rs"), "fn m1() {}\n").unwrap();
    std::fs::write(dir.path().join("a_first.rs"), "fn a() {}\n").unwrap();

    let (ctx, _h) = test_ctx(dir.path().to_path_buf());

    let out = GrepTool::new()
        .invoke(call("grep", serde_json::json!({"pattern": r"fn \w+"})), ctx)
        .await
        .unwrap();

    assert!(!out.is_error);
    let text = text_of(&out);
    let lines: Vec<&str> = text.lines().filter(|l| l.contains(':')).collect();

    // Sorted by full path: root entries and the directory interleave by name.
    assert_eq!(
        lines,
        vec![
            "a_first.rs:1:fn a() {}",
            "mid/m_one.rs:1:fn m1() {}",
            "mid/m_two.rs:1:fn m2() {}",
            "z_last.rs:1:fn z() {}",
        ],
        "walk order must be sorted by path, not whatever order the filesystem \
         happens to return -- this list is the exact reverse of creation order"
    );
}

// `glob` deliberately has NO equivalent test. It shares `walk_files` with
// `grep`, but `GlobTool` re-sorts its own results (mtime descending, ties
// broken lexicographically -- see `fs/glob.rs`), so its output order does not
// depend on walk order at all and a test asserting otherwise would pass for
// the wrong reason. An earlier draft of this file HAD such a test: it passed
// with the sort removed from `walk_files`, because the fixture created files
// in reverse-alphabetical order, which made mtime-descending coincide with
// alphabetical. It was asserting glob's own sort while claiming to assert the
// walker's. Removed rather than shipped.
