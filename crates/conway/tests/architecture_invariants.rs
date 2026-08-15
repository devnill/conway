//! The nine architecture invariants, as guards pinned at TODAY's values.
//!
//! This file is the ratchet for the Stage 0-5 migration recorded in
//! the staged migration plan.
//!
//! # Why the false ones are asserted as false
//!
//! Two of the nine invariants do not hold yet (T7 flipped to enforcing when
//! Stage 2a landed). Each of those guards asserts
//! **the current, wrong state** and names the stage that will flip it. That is
//! deliberate and it is the part most likely to look like a mistake.
//!
//! The alternative -- writing the target assertion now and letting it fail, or
//! marking it `#[ignore]` -- produces a suite with known-red or known-skipped
//! entries. Within a week nobody can tell those apart from a genuinely broken
//! test, and a suite people have learned to skim is worth nothing. Worse, it
//! would give no protection at all in the meantime.
//!
//! Asserting the false state protects the migration in the direction it can
//! actually be harmed: **it fails if the problem gets bigger.** A second
//! forbidden dependency edge, a fifth presentation type, another runtime
//! re-export, a third crate doing I/O in the contract layer -- each of those
//! breaks a guard here, today, before it becomes something a later stage has
//! to unpick. When a stage lands, its guard converts to the target assertion
//! by changing one line, and the failure message says which line.
//!
//! # Placement
//!
//! All nine live in one file, in the facade's test suite, rather than beside
//! the crate each governs. The nine are one artifact -- the architecture
//! table -- and the point of pinning them is that a reader can see the whole
//! ratchet state at once; splitting them across five crates would mean no
//! single place shows it. `conway` hosts them because four of the nine (T5-T8)
//! are about the facade itself, and because every check here reads a manifest
//! or source text by path, so hosting does not limit what is checkable.
//!
//! Precedent exists for both conventions. `conway-cli`'s `no_forbidden_deps`
//! guards its own crate's manifest and rightly lives there;
//! `enum_variant_construction_guard.rs` is a cross-cutting structural guard in
//! this same directory, and is the closer parallel.
//!
//! # Environment-freedom
//!
//! Every guard reads files from the repository. No credential, no network, no
//! local server, no compiled binary. They run anywhere the source does.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The repository root, derived from this crate's manifest directory
/// (`crates/conway` -> up two).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<name> is always two levels below the repo root")
        .to_path_buf()
}

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn manifest(crate_name: &str) -> toml::Value {
    read(&format!("crates/{crate_name}/Cargo.toml"))
        .parse()
        .unwrap_or_else(|err| panic!("parse {crate_name}/Cargo.toml: {err}"))
}

/// The workspace-internal (`conway-*`) keys in a crate's `[dependencies]`.
/// Dev-dependencies are deliberately excluded: a test-only edge is not an
/// architectural one, and several crates legitimately dev-depend on plugins
/// they must never link in production.
fn internal_deps(crate_name: &str) -> BTreeSet<String> {
    manifest(crate_name)
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .map(|deps| {
            deps.keys()
                .filter(|k| k.starts_with("conway"))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Files under a crate's `src/` whose text contains `needle`.
fn src_files_containing(crate_name: &str, needle: &str) -> BTreeSet<String> {
    fn walk(dir: &Path, needle: &str, root: &Path, out: &mut BTreeSet<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, needle, root, out);
            } else if path.extension().is_some_and(|e| e == "rs")
                && std::fs::read_to_string(&path).is_ok_and(|t| t.contains(needle))
            {
                out.insert(
                    path.strip_prefix(root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    let src = repo_root().join(format!("crates/{crate_name}/src"));
    let mut out = BTreeSet::new();
    walk(&src, needle, &src, &mut out);
    out
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

// ------------------------------------------------------------------- T1 ----

/// **T1: `conway-core` depends on no workspace crate. HOLDS TODAY.**
///
/// The contract crate is the bottom of the stack; an edge out of it inverts
/// the architecture. This is the one guard with nothing to flip later -- it
/// exists purely to keep a true thing true.
///
/// **Cargo already enforces most of this, which is worth knowing before you
/// trust this guard for more than it does.** Every other crate in the
/// workspace depends on `conway-core`, directly or transitively, so adding any
/// of them here is rejected outright -- `error: cyclic package dependency:
/// package 'conway-core' depends on itself`. Established by attempting it, not
/// assumed.
///
/// What that leaves this guard covering is the case cargo cannot see: a future
/// LEAF crate with no dependencies of its own -- a proc-macro helper, say --
/// could be added to the contract crate without any cycle at all. That is the
/// realistic regression, and it is what the break-the-guard run used (a
/// `conway-`prefixed key aliasing an unrelated published crate, which creates
/// no cycle and fails this assertion cleanly).
#[test]
fn t1_core_depends_on_no_workspace_crate() {
    let deps = internal_deps("conway-core");
    assert!(
        deps.is_empty(),
        "T1 BROKEN: `conway-core` gained workspace dependencies {deps:?}.\n\
         The contract crate must depend on no crate in this workspace -- it is \
         the bottom of the stack, and an edge out of it inverts the \
         architecture. This invariant HOLDS today; you have regressed it. \
         Remove the edge; do not relax this guard."
    );
}

// ------------------------------------------------------------------- T2 ----

/// **T2: `conway-core` performs no I/O in production paths. FALSE TODAY.**
///
/// `containment.rs` calls `canonicalize()`, while the crate's own module doc
/// opens by claiming the crate performs no I/O. Stage 1.5
/// closes this by moving confinement into
/// `conway.fs`, where the check and the open become one step -- specifically
/// its child, "Retire the harness-level
/// confinement root once conway.fs enforces its own", which is the item the
/// forward-declaration labels name and the one that must delete them.
///
/// Pinned as: exactly one file does I/O. Fails if a second one starts.
#[test]
fn t2_core_io_is_confined_to_the_one_known_file() {
    let offenders = src_files_containing("conway-core", "canonicalize(");
    assert_eq!(
        offenders,
        set(&["containment.rs"]),
        "T2 CHANGED: filesystem I/O in `conway-core` is no longer confined to \
         `containment.rs`.\n\
         Expected exactly {{containment.rs}}, found {offenders:?}.\n\
         If a file was ADDED: the contract crate must not do I/O -- put it \
         behind a port. If `containment.rs` was REMOVED because Stage 1.5 \
         landed, this guard has done its \
         job: replace it with `assert!(offenders.is_empty())` and delete the \
         no-I/O forward-declaration label the Stage 0 labelling item added."
    );
}

// ------------------------------------------------------------------- T3 ----

/// **T3: `conway-core` ships no test doubles. TRUE as of board item
/// `01KZVYWNA24EYMPVW3NPGBW51M`.**
///
/// `fakes.rs` (979 lines of doubles) moved out of the contract crate into
/// `conway-testkit`, a crate of its own -- and, unlike the old
/// `[dev-dependencies]`-only `fakes` feature, one `conway`'s facade forwards
/// to third parties (its own `testkit` feature). `conway-core`'s manifest no
/// longer declares a `fakes` feature, and its `src/lib.rs` no longer names
/// one.
///
/// Pinned as: no `fakes`-named feature and no `fakes`-named module anywhere
/// in `conway-core`. Fails if either reappears -- which would mean the
/// doubles moved back into the contract crate, or a vestigial feature name
/// survived the move.
#[test]
fn t3_core_ships_no_test_doubles() {
    let lib = read("crates/conway-core/src/lib.rs");
    assert!(
        !lib.contains("fakes"),
        "T3 BROKEN: `crates/conway-core/src/lib.rs` names `fakes` again -- \
         the contract crate must not declare a test-doubles module or \
         feature. Test doubles for `conway-core`'s ports belong in \
         `conway-testkit`, not here."
    );

    let features = manifest("conway-core");
    let declared = features
        .get("features")
        .and_then(toml::Value::as_table)
        .map(|t| t.contains_key("fakes"))
        .unwrap_or(false);
    assert!(
        !declared,
        "T3 BROKEN: `conway-core`'s manifest declares a `fakes` feature \
         again -- it was removed when the doubles moved to `conway-testkit`; \
         it must not come back."
    );
}

// ------------------------------------------------------------------- T4 ----

/// **T4: `conway-runtime` depends on `conway-core` only. HOLDS as of board
/// item `01KZVYVTVWRH20R6VJ6G3SWTJ6` ("Stage 1a").**
///
/// The runtime used to carry a hard edge to `conway-session`, the JSONL
/// adapter, for two things that were never JSONL-specific:
/// `TranscriptResolver` and the `provenance::*_context_report` helpers. Both
/// moved into `conway-core` (`transcript::TranscriptResolver` and
/// `provenance::{append_context_report, load_context_report,
/// load_all_context_reports}`), and `crates/conway-runtime/Cargo.toml` no
/// longer names `conway-session` in `[dependencies]` -- only in
/// `[dev-dependencies]`, which this guard deliberately does not check (see
/// `internal_deps`'s own doc).
///
/// Pinned as: no workspace dependency but `conway-core`. Fails if the
/// runtime reaches for any adapter again.
#[test]
fn t4_runtime_depends_on_core_only() {
    let deps = internal_deps("conway-runtime");
    assert_eq!(
        deps,
        set(&["conway-core"]),
        "T4 BROKEN: `conway-runtime`'s workspace dependencies are no longer \
         {{conway-core}}; found {deps:?}.\n\
         The runtime is meant to depend on the contract crate alone -- an \
         adapter edge here (e.g. `conway-session`) is exactly the coupling \
         Stage 1a (board item 01KZVYVTVWRH20R6VJ6G3SWTJ6) removed. Move \
         whatever is not JSONL-specific into `conway-core` instead of \
         depending on the adapter that happens to host it."
    );
}

// ------------------------------------------------------------------- T5 ----

/// **T5: the facade depends on core + runtime, adapters only as honest
/// features. HOLDS.**
///
/// `conway-session` and `conway-tools` are both `optional` and gated behind
/// `jsonl-store` and `builtin-tools`. As of board item
/// `01KZVYVTVWRH20R6VJ6G3SWTJ6` ("Stage 1a", which also flipped T4),
/// `jsonl-store` is no longer just default wiring: `conway-runtime` no
/// longer depends on `conway-session` unconditionally, so turning the
/// feature off genuinely unlinks the JSONL crate now.
#[test]
fn t5_facade_gates_its_adapters_behind_features() {
    let m = manifest("conway");
    let deps = m
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .expect("[dependencies]");

    for required in ["conway-core", "conway-runtime"] {
        assert!(
            deps.contains_key(required),
            "T5 BROKEN: the facade must depend on `{required}` unconditionally."
        );
    }

    for (adapter, feature) in [
        ("conway-session", "jsonl-store"),
        ("conway-tools", "builtin-tools"),
    ] {
        let optional = deps
            .get(adapter)
            .and_then(|v| v.get("optional"))
            .and_then(toml::Value::as_bool)
            .unwrap_or(false);
        assert!(
            optional,
            "T5 BROKEN: `{adapter}` must be an OPTIONAL dependency of the \
             facade, gated behind `{feature}`. A non-optional adapter makes \
             the feature a lie in the most direct way possible."
        );
    }

    let features = m
        .get("features")
        .and_then(toml::Value::as_table)
        .expect("[features]");
    assert!(
        features.contains_key("jsonl-store") && features.contains_key("builtin-tools"),
        "T5 BROKEN: the facade no longer declares both adapter features."
    );
}

// ------------------------------------------------------------------- T6 ----

/// **T6: the facade exposes no `conway-runtime` type publicly. HOLDS TODAY.**
///
/// `crates/conway/src/lib.rs` used to re-export `conway_runtime::permission::
/// GrantScope` roughly forty lines below a module doc that denied doing
/// exactly this -- one file contradicting itself. Stage 2b
/// (board item `01KZVYZM7BZRQ54RRB8P814KV9`) closed it the tree's way: the
/// re-export is gone, because an audit resolves a mismatch in the code
/// rather than the page.
///
/// The re-export had a real consumer -- `conway-cli` names `conway::
/// GrantScope` at eight sites to label and revoke a structured-allow rule,
/// and cannot reach it another way (`no_forbidden_deps`) -- so deletion
/// alone would have broken the build. `conway_core::agent::GrantScope`
/// (board item `01KZWRZ4JBAVCRCZ99BFZFF01K`) is the replacement: it carries
/// the same `AgentId` the runtime's own `GrantScope` does, `conway::Conway`'s
/// structured-allow methods convert to/from it at the facade boundary
/// (`crates/conway-runtime/src/permission.rs`'s `From` impls), and all eight
/// `conway-cli` sites moved onto it in the same change.
///
/// Pinned as: empty. Fails if a `conway_runtime` re-export is reintroduced --
/// which would turn a closed contradiction back into an open one.
#[test]
fn t6_facade_re_exports_no_runtime_type() {
    let lib = read("crates/conway/src/lib.rs");
    let reexports: Vec<&str> = lib
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("pub use conway_runtime"))
        .collect();
    assert_eq!(
        reexports,
        Vec::<&str>::new(),
        "T6 BROKEN: the facade publicly re-exports `conway_runtime` type(s) \
         again; found {reexports:?}.\n\
         The module doc in this very file denies exposing runtime types, and \
         Stage 2b (board item `01KZVYZM7BZRQ54RRB8P814KV9`) closed the one \
         known contradiction by deleting the `GrantScope` re-export and \
         replacing it with `conway_core::agent::GrantScope`. Do not \
         reintroduce a `conway-runtime` re-export here -- convert at the \
         facade boundary instead, the way `Conway::active_structured_allow_rules` \
         and `Conway::revoke_structured_allow_rule` do."
    );
}

// ------------------------------------------------------------------- T7 ----

/// **T7: the facade carries no presentation config. TRUE as of Stage 2a.**
///
/// The four ratatui-shaped types that used to sit in the embeddable config
/// schema (`TuiSection`, `ThemeConfig`, `ThemeStyleConfig`,
/// `StatusLineConfig` -- roughly 34 slots of theme and status-line
/// configuration a service or IDE with no terminal used to have to parse
/// and validate) moved to `conway-cli` (`crates/conway-cli/src/tui/
/// config.rs`), the one reader that renders them. This guard now enforces
/// the target state rather than pinning the known-wrong one: it fails the
/// moment ANY presentation-shaped type reappears in this schema, first or
/// fifth.
#[test]
fn t7_facade_has_no_presentation_types() {
    let schema = read("crates/conway/src/config/schema.rs");

    // Every `pub struct` in the schema, then filtered by NAME SHAPE rather
    // than against the four now-removed names -- catches a REINTRODUCED
    // `TuiSection`/`ThemeConfig`/`StatusLineConfig` just as readily as it
    // would catch a brand new presentation type, since both share the same
    // name vocabulary.
    //
    // The filter is a heuristic and worth naming as one: it catches anything
    // called `*Tui*`, `*Theme*` or `*StatusLine*`, which is what every
    // presentation type in this schema was, historically, and what a new one
    // would realistically be called. A presentation type named entirely
    // outside that vocabulary would slip through -- accepted, because the
    // alternative (pinning the schema's TOTAL struct count) breaks on every
    // unrelated config addition and would be turned off within a month.
    let presentation: BTreeSet<String> = schema
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("pub struct "))
        .filter_map(|rest| {
            rest.split([' ', '{', '(', '<', ';'])
                .next()
                .filter(|n| !n.is_empty())
        })
        .filter(|name| {
            name.contains("Tui") || name.contains("Theme") || name.contains("StatusLine")
        })
        .map(str::to_string)
        .collect();

    assert_eq!(
        presentation,
        BTreeSet::new(),
        "T7 REGRESSED: the facade's config schema names a presentation-shaped type again; \
         found {presentation:?}. Stage 2a moved every ratatui-shaped config slot to \
         `conway-cli` (crates/conway-cli/src/tui/config.rs) precisely so a headless host \
         never has to parse terminal vocabulary it can never render -- whatever this found \
         either belongs there instead, or is a real fifth presentation type that should \
         never have landed here at all."
    );
}

// ------------------------------------------------------------------- T8 ----

/// **T8: every adapter is authorable by a crate depending on `conway` alone.
/// HOLDS FOR PLUGINS AND BACKENDS; A PERMANENT, RULED EXCEPTION FOR ROUTERS.**
///
/// This guard asserts an ASYMMETRY, not a gap, and that distinction is the
/// whole point of it existing.
///
/// Operator ruling, 2026-08-12, recorded as:
/// `RouteRequest`/`Route`/`RoutingError` stay unexported from the facade. A
/// router genuinely needs the routing and capability domain that the facade's
/// curated surface deliberately does not carry -- `Router`/`HealthRegistry`
/// are on this crate's own "Deliberately NOT here" list, and
/// `docs/embedding.md`'s "First-party plugin tier" section already recorded
/// the same thing at length. A third party can INSTALL a router; authoring
/// one means depending on `conway-core` directly, on purpose.
///
/// So: a future reader must not be able to mistake this for unfinished work,
/// and equally must not quietly "fix" it by adding the re-export. Both
/// directions fail here.
#[test]
fn t8_router_authoring_exception_is_intact_and_deliberate() {
    let lib = read("crates/conway/src/lib.rs");

    for routing_type in ["RouteRequest", "RoutingError"] {
        let reexported = lib
            .lines()
            .map(str::trim)
            .any(|l| l.starts_with("pub use") && l.contains(routing_type));
        assert!(
            !reexported,
            "T8 CHANGED: the facade now re-exports `{routing_type}`.\n\
             This is a RULED PERMANENT EXCEPTION, not an oversight. The \
             routing domain is deliberately absent from the facade's curated \
             surface; `Router`/`HealthRegistry` are on this crate's own \
             \"Deliberately NOT here\" list. If that ruling has been REVERSED, \
             record the reversal first and then update this guard -- do not \
             let the code and the decision disagree."
        );
    }

    assert!(
        lib.contains("pub mod plugin"),
        "T8 BROKEN: the facade no longer exposes `pub mod plugin`, so a third \
         party cannot author a Plugin/Tool facade-only. That half of T8 HOLDS \
         today and must keep holding -- only the ROUTER half is a permitted \
         exception."
    );
}

// ------------------------------------------------------------------- T9 ----

/// **T9: one command-dispatch path per surface. TRUE as of board item
/// `01KZVZ5XV162XCQR96AQKCCCF7`.**
///
/// The TUI used to intercept `/settings`, `/trust`, `/agents`, `/ask` with
/// direct string comparison in `app.rs::submit`, before `commands::parse`
/// ever ran. All four are now ordinary `commands::SlashCommand` variants,
/// dispatched through the SAME single `commands::parse` -> `commands::
/// execute` call every other command already used -- see `app.rs::submit`'s
/// own doc for the one remaining piece of housekeeping (`/settings`'s
/// pre-render state refresh) that still runs outside `commands::execute`,
/// and why that does not reopen this guard.
///
/// **The count WAS four, not two.** The architecture review, and this
/// guard's own original text, both said two -- `/ask` and `/agents` --
/// because that is what `submit`'s doc comment named at the time.
/// `/settings` and `/trust` were intercepted the same way and the doc
/// mentioned neither; that undercount was itself an instance of the defect
/// class this item closed.
///
/// Pinned as: **empty**. A regression that reintroduces even ONE direct
/// `if text.trim() == "/..."` check ahead of `commands::parse` fails this
/// guard immediately -- the realistic way this gets worse, since a new
/// modal or state-refreshing command is easiest to add as "just one more
/// special case" beside wherever the last one landed.
#[test]
fn t9_tui_has_no_parser_bypasses() {
    let app = read("crates/conway-cli/src/tui/app.rs");
    let bypasses: BTreeSet<String> = app
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("if text.trim() == \"/"))
        .filter_map(|l| {
            let rest = l.strip_prefix("if text.trim() == \"")?;
            rest.split('"').next().map(str::to_string)
        })
        .collect();

    assert_eq!(
        bypasses,
        BTreeSet::new(),
        "T9 CHANGED: the TUI has a pre-parser command interception again -- \
         found {bypasses:?}. Every slash command must reach its handler \
         THROUGH `commands::parse`; a direct `if text.trim() == \"/...\"` \
         check in `app.rs::submit` bypasses it entirely, gets none of the \
         parser's validation, completion or help, and is exactly the defect \
         class board item `01KZVZ5XV162XCQR96AQKCCCF7` removed."
    );
}
