//! `scripts/check-fast-gates.sh`'s ARGUMENT HANDLING is under test, not its
//! gates.
//!
//! Five CI jobs call that script (`.github/workflows/ci.yml`), so its CLI is
//! load-bearing infrastructure with a blast radius of every fast gate at once.
//! Nothing exercised it until a review found `--gate` with no value spinning
//! at 100% CPU indefinitely: `shift 2` with one positional left does not
//! shift and returns non-zero, and with `set -u` but no `set -e` that failure
//! was silent, so `$1` stayed `--gate` and the case arm re-entered forever.
//!
//! That defect was found by a human running the command, not by a test, in a
//! script whose whole purpose is to be run cheaply and defensively by people
//! and agents who will not be watching it. These tests exist so the next one
//! is found by CI.
//!
//! WHY THE BOUND IS SO GENEROUS. The failure mode is a HANG, and a bound's
//! job is to turn a hang into a legible failure — never to measure how long
//! something takes. It therefore sits far above any plausible slow run: these
//! invocations parse arguments and exit without compiling anything, so they
//! finish in milliseconds, and a bound of thirty seconds cannot flake under
//! load while still converting an infinite loop into a named failure. Tuning
//! a timeout near the range it measures is how a hang-detector becomes a
//! flake, which this project has already paid for once.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Far above any plausible run of an argument-parse-and-exit invocation; see
/// the module doc for why it is not tuned tighter.
const HANG_TIMEOUT: Duration = Duration::from_secs(30);

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<name> is always two levels below the repo root")
        .to_path_buf()
}

/// Runs the script with `args` and returns its exit code, converting a hang
/// into a panic naming the arguments rather than stalling the suite forever.
fn run_script(args: &[&str]) -> i32 {
    let root = repo_root();
    let mut child = Command::new("bash")
        .arg(root.join("scripts/check-fast-gates.sh"))
        .args(args)
        .current_dir(&root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn check-fast-gates.sh");

    let started = Instant::now();
    loop {
        match child.try_wait().expect("poll check-fast-gates.sh") {
            Some(status) => {
                return status.code().unwrap_or_else(|| {
                    panic!("check-fast-gates.sh {args:?} was killed by a signal")
                });
            }
            None => {
                if started.elapsed() > HANG_TIMEOUT {
                    let _ = child.kill();
                    panic!(
                        "check-fast-gates.sh {args:?} did not exit within {HANG_TIMEOUT:?} -- \
                         it is hanging. This is the exact defect this file exists to catch: \
                         an argument form that re-enters its own parse loop instead of \
                         erroring."
                    );
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// The regression. `--gate` as the final token, with no name after it.
///
/// Before the fix this did not fail — it never returned at all, burning a
/// core with no output. Asserting on the exit code is what distinguishes a
/// correct refusal from a hang; asserting only "did not succeed" would pass
/// against a process that never finishes.
#[test]
fn gate_with_no_name_is_a_usage_error_rather_than_a_hang() {
    assert_eq!(
        run_script(&["--gate"]),
        2,
        "`--gate` with no name must exit 2 (usage error), not loop"
    );
}

/// The neighbouring forms, so a future fix to the above cannot quietly turn a
/// real gate name into a usage error or vice versa.
#[test]
fn an_unknown_gate_name_is_refused_rather_than_silently_passing() {
    assert_eq!(
        run_script(&["--gate", "cargo fmt"]),
        2,
        "a near-miss gate name must be refused by name, not treated as a pass -- \
         a mistyped --gate in ci.yml that exited 0 would silently disable that gate"
    );
    assert_eq!(
        run_script(&["--gate", ""]),
        2,
        "an empty gate name must be refused"
    );
}

#[test]
fn an_unrecognized_flag_is_a_usage_error() {
    assert_eq!(run_script(&["--nope"]), 2);
}

/// `--list` and `--help` are the two forms that must succeed without running
/// a single gate, so they stay cheap enough to call from anywhere.
#[test]
fn list_and_help_succeed_without_running_any_gate() {
    assert_eq!(run_script(&["--list"]), 0);
    assert_eq!(run_script(&["--help"]), 0);
}

/// `--help` must reach the invocation syntax, not stop inside the rationale.
///
/// It previously printed a hardcoded line range (`sed -n '2,30p'`) that the
/// header outgrew, so `--help` showed only prose and never told the reader
/// how to invoke `--gate` — the one thing it exists for. Anchoring on content
/// rather than line numbers is the fix; this asserts the outcome of that.
#[test]
fn help_output_contains_the_actual_invocation_syntax() {
    let root = repo_root();
    let out = Command::new("bash")
        .arg(root.join("scripts/check-fast-gates.sh"))
        .arg("--help")
        .current_dir(&root)
        .output()
        .expect("run check-fast-gates.sh --help");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("--gate NAME"),
        "--help must show how to invoke --gate; got:\n{text}"
    );
    assert!(
        text.contains("--list"),
        "--help must show --list; got:\n{text}"
    );
}

/// Every gate name `ci.yml` passes must be one the script actually knows.
///
/// This is the coupling the fast-gates change introduced: five CI jobs now
/// depend on one script, and they address it by NAME. A rename on either side
/// alone turns five green jobs into five usage errors, which is loud — but a
/// rename that lands on only one side and still matches something would not
/// be, so pin the correspondence rather than trusting it.
#[test]
fn every_gate_name_ci_invokes_is_one_the_script_knows() {
    let root = repo_root();
    let ci = std::fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("read ci.yml");

    let known = Command::new("bash")
        .arg(root.join("scripts/check-fast-gates.sh"))
        .arg("--list")
        .current_dir(&root)
        .output()
        .expect("run check-fast-gates.sh --list");
    let known: Vec<String> = String::from_utf8_lossy(&known.stdout)
        .lines()
        .map(str::to_string)
        .collect();

    // Only actual `run:` steps, never comments. ci.yml's own prose explains
    // the mechanism using a `--gate "<name>"` placeholder, and a scan that
    // reads it finds a gate called `<name>` and reports a defect that does
    // not exist. This project has already shipped one spec whose diagnostic
    // grep matched the wrong thing and manufactured a finding; a scan is a
    // claim about the source and has to be as careful as one.
    let invoked: Vec<String> = ci
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("run:"))
        .filter_map(|line| {
            let (_, rest) = line.split_once("check-fast-gates.sh --gate \"")?;
            let (name, _) = rest.split_once('"')?;
            Some(name.to_string())
        })
        .collect();

    assert!(
        !invoked.is_empty(),
        "found no `check-fast-gates.sh --gate \"...\"` invocations in ci.yml -- \
         either CI stopped using the script or this scan broke; fix the scan \
         before trusting a pass"
    );

    for name in &invoked {
        assert!(
            known.contains(name),
            "ci.yml invokes gate {name:?}, which the script does not know. \
             Known gates: {known:?}"
        );
    }
}
