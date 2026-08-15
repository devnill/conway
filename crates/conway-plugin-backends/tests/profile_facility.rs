//! S4b: the kind-agnostic profile facility (`conway_plugin_backends::
//! profile_store`) proven at the crate boundary, from outside the module
//! that owns it.
//!
//! Two things this file exists to prove that a unit test inside
//! `src/profile_store.rs` cannot: (1) a STRUCTURAL guard over the crate's
//! own `src/` text that a second profile-store type cannot silently appear
//! beside the first, and (2) the verification anchor the item's own spec
//! names -- one facility, the SAME loaded profile FILE, resolved through
//! BOTH shipped kinds' real `BackendFactory::build`, each producing its own
//! correct wire output.
//!
//! Both tests are credential-free: `AnthropicBackendFactory` and
//! `OpenAiCompatBackendFactory` are driven against a local `wiremock`
//! server, never a real provider.

use std::collections::BTreeMap;
use std::path::Path;

use conway_core::content::{ContentBlock, Role, SamplingParams};
use conway_core::ids::{BackendId, ModelId};
use conway_core::ports::{BackendBuildContext, BackendFactory, GenerateRequest};
use conway_core::provenance::Provenance;
use conway_core::segment::PromptSegment;
use conway_plugin_backends::{AnthropicBackendFactory, OpenAiCompatBackendFactory};
use wiremock::matchers::{method, path as path_matcher};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------------------------------------------------------------------
// (1) Structural guard: exactly one profile-store TYPE exists.
// ---------------------------------------------------------------------

/// The repository root, derived from this crate's manifest directory
/// (`crates/conway-plugin-backends` -> up two) -- the same derivation
/// `crates/conway/tests/architecture_invariants.rs`'s own `repo_root` uses,
/// and the same style of grep-the-source-text structural guard that file's
/// nine `t*` tests already establish as this repo's convention for "asserted
/// by a test over the types rather than a reviewer's reading."
fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<name> is always two levels below the repo root")
        .to_path_buf()
}

/// Every `struct` (or `pub struct`) definition line, anywhere under
/// `crates/conway-plugin-backends/src`, whose name contains `ProfileStore`.
fn profile_store_struct_definitions() -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for line in text.lines() {
                let trimmed = line.trim();
                let rest = trimmed
                    .strip_prefix("pub struct ")
                    .or_else(|| trimmed.strip_prefix("struct "));
                if let Some(rest) = rest {
                    let name = rest
                        .split([' ', '{', '(', '<', ';'])
                        .next()
                        .unwrap_or_default();
                    if name.contains("ProfileStore") {
                        out.push(format!("{}: {trimmed}", path.display()));
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(
        &repo_root().join("crates/conway-plugin-backends/src"),
        &mut out,
    );
    out
}

/// **The acceptance criterion, asserted over the types**: "exactly one
/// profile facility exists... the specific thing to prevent is a second
/// store appearing beside the first."
///
/// Pinned as exactly one definition -- `profile_store::ProfileStore<T>`,
/// generic over which kind's own payload type resolves a name to
/// (`crate::profile::Profile` for `"openai-compat"`,
/// `crate::profile_store::ProfileBundle` for `"anthropic"`). Two
/// INSTANTIATIONS of one generic type are not two stores; a second `struct`
/// DEFINITION -- even an empty, unused stub -- is.
///
/// **Break-the-guard run** (performed by hand, not left in the tree): adding
/// ```ignore
/// struct StubProfileStore;
/// ```
/// anywhere under `src/` turned this test's `assert_eq!` into a two-element
/// vector against an expected one-element vector, i.e. failed exactly as
/// designed, before being reverted.
#[test]
fn exactly_one_profile_store_type_exists() {
    let found = profile_store_struct_definitions();
    assert_eq!(
        found.len(),
        1,
        "expected exactly ONE `struct ...ProfileStore` definition under \
         crates/conway-plugin-backends/src, found {}: {found:#?}\n\
         A second profile-store type is exactly the drift S4b exists to \
         prevent -- a new kind's profile support belongs on the ONE generic \
         `profile_store::ProfileStore<T>` (implement `Profiled` for your \
         payload type), never a second store definition.",
        found.len()
    );
    assert!(
        found[0].contains("profile_store.rs"),
        "the one definition must live in `profile_store.rs` (the facility), \
         found it in {found:?} instead"
    );
}

// ---------------------------------------------------------------------
// (2) Verification anchor: one facility, two dialects, both proven on the
// serialized output.
// ---------------------------------------------------------------------

/// **A finding surfaced by writing this test, recorded rather than papered
/// over (the item's own instruction for exactly this situation):** a single
/// PHYSICAL `[[profile]]` file cannot mix entries meant for different kinds.
/// `Profiled::parse_source` parses (and, for `Profile`,
/// `deny_unknown_fields`-rejects) EVERY entry in a source string as ITS OWN
/// payload type -- `crate::profile::ProfileFile { profile: Vec<Profile> }`
/// has no way to skip an entry meant for a different kind's vocabulary, and
/// weakening that to "tolerate unrecognized fields" would reopen the exact
/// silent-typo defect `deny_unknown_fields` exists to close (`profile.rs`'s
/// own module doc, "Why every field is `#[serde(default)]`"). So the
/// facility being kind-agnostic (it never reads a field beyond `id`) does
/// NOT make one physical file shareable across kinds with different strict
/// schemas -- that would require a DIFFERENT, weaker facility, which is
/// exactly the kind of "the facility ends up knowing something
/// dialect-specific" outcome the item's own spec says to record rather than
/// engineer around. In production this means: an operator who wants both an
/// `"openai-compat"` profile and an `"anthropic"` profile discoverable via
/// `conway::config::discovery::provider_profile_file_paths` needs them in
/// DIFFERENT files (a project-scoped one and a global-scoped one, say) --
/// not a limitation this item introduces (a mixed file already failed this
/// same way for two `"openai-compat"` profiles with colliding vocabulary
/// before this item), but one this item's addition of a second profile-file
/// READER makes reachable for the first time.
///
/// The two profiles below are therefore two files, loaded via the SAME
/// generic `ProfileStore<T>` facility (different `T` per kind -- see
/// `crate::profile_store`'s own module doc) through each factory's
/// `ctx.profile_file_paths` -- proving the mechanism is shared even though
/// the physical file is not.
const OPENAI_COMPAT_PROFILE_TOML: &str = r#"
[[profile]]
id = "acme-openai-compat"
uses_max_completion_tokens = true
"#;

const ANTHROPIC_PROFILE_TOML: &str = r#"
[[profile]]
id = "acme-anthropic"
anthropic_version = "2024-05-01"

[profile.headers]
"x-acme-gateway" = "yes"
"#;

/// Tests in this file run concurrently (the default `cargo test` thread
/// pool) and two calls landing in the SAME nanosecond is not actually rare
/// on a fast machine -- an earlier version of this helper collided that way
/// and produced a flaky "unknown profile" failure when one test's teardown
/// raced another's read of a same-named directory. A per-process atomic
/// counter, alongside the pid, makes every call's directory name unique
/// regardless of clock resolution.
fn write_profile_file(contents: &str) -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "conway-plugin-backends-profile-facility-test-{}-{n}",
        std::process::id(),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("profiles.toml");
    std::fs::write(&path, contents).unwrap();
    path
}

fn user_request() -> GenerateRequest {
    GenerateRequest {
        model: ModelId::new("test-model"),
        segments: vec![PromptSegment::new(
            Role::User,
            vec![ContentBlock::Text { text: "hi".into() }],
            Provenance::UserPrompt,
        )],
        tools: vec![],
        params: SamplingParams::default(),
        prefix_key: None,
    }
}

/// `user_request` plus an explicit `max_tokens` -- `"openai-compat"`'s
/// `uses_max_completion_tokens` only changes which JSON field NAME carries
/// this value (`openai_compat/wire.rs::build_request_body`), so the anchor
/// test needs one set to observe the profile's effect at all.
fn user_request_with_max_tokens(max_tokens: u32) -> GenerateRequest {
    let mut req = user_request();
    req.params.max_tokens = Some(max_tokens);
    req
}

/// The `"openai-compat"` half of the anchor: `acme-openai-compat`'s
/// `uses_max_completion_tokens = true` must reach the real outgoing request
/// body as the `max_completion_tokens` field name (never `max_tokens`) --
/// proof the profile FILE, loaded through the shared facility, genuinely
/// drove this kind's own wire behavior.
#[tokio::test]
async fn the_shared_profile_file_drives_openai_compat_wire_output() {
    let profiles_path = write_profile_file(OPENAI_COMPAT_PROFILE_TOML);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_matcher("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "ok"},
                "finish_reason": "stop"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let ctx = BackendBuildContext {
        id: BackendId::new("acme"),
        base_url: server.uri(),
        api_key: None,
        dialect: Some("acme-openai-compat".to_string()),
        models: BTreeMap::new(),
        profile_file_paths: vec![profiles_path.clone()],
        extra: BTreeMap::new(),
    };
    let backend = OpenAiCompatBackendFactory
        .build(ctx)
        .expect("acme-openai-compat must resolve from the shared profile file");
    backend
        .generate(user_request_with_max_tokens(256))
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert!(
        body.get("max_completion_tokens").is_some(),
        "uses_max_completion_tokens=true from the shared profile file must reach the wire \
         body's field NAME: {body}"
    );
    assert!(body.get("max_tokens").is_none());

    std::fs::remove_dir_all(profiles_path.parent().unwrap()).ok();
}

/// The `"anthropic"` half of the anchor: `acme-anthropic`'s
/// `anthropic_version`/`headers` -- new profile support this item adds to
/// this kind -- must reach the real outgoing request's HTTP headers (never
/// the JSON body; the same discriminating observable `tests/
/// anthropic_extra_config.rs` already established for `extra`). Loaded
/// through the SAME generic facility
/// `the_shared_profile_file_drives_openai_compat_wire_output` loads through
/// (a different file -- see this test module's own doc, above, for why one
/// physical file cannot mix both kinds' entries).
#[tokio::test]
async fn the_shared_profile_file_drives_anthropic_wire_output() {
    let profiles_path = write_profile_file(ANTHROPIC_PROFILE_TOML);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_matcher("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let ctx = BackendBuildContext {
        id: BackendId::new("acme"),
        base_url: server.uri(),
        api_key: Some("sk-ant-api03-test".to_string()),
        dialect: Some("acme-anthropic".to_string()),
        models: BTreeMap::new(),
        profile_file_paths: vec![profiles_path.clone()],
        extra: BTreeMap::new(),
    };
    let backend = AnthropicBackendFactory
        .build(ctx)
        .expect("acme-anthropic must resolve from the shared profile file");
    backend.generate(user_request()).await.unwrap();

    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .headers
            .get("anthropic-version")
            .expect("anthropic-version header must be present")
            .to_str()
            .unwrap(),
        "2024-05-01",
        "the shared profile file's anthropic_version must reach the real wire header"
    );
    assert_eq!(
        requests[0]
            .headers
            .get("x-acme-gateway")
            .expect("the profile's headers entry must reach the real wire request")
            .to_str()
            .unwrap(),
        "yes"
    );

    std::fs::remove_dir_all(profiles_path.parent().unwrap()).ok();
}

/// The ONE precedence rule (`profile_store::apply_precedence`), driven
/// end-to-end through a real `BackendFactory::build` rather than only at the
/// facility's own unit-test level: `acme-anthropic`'s profile sets
/// `anthropic_version = "2024-05-01"`; `[backends.<id>].extra` sets a
/// DIFFERENT value for the same key; a third profile field
/// (`headers`) is set by neither `extra` nor overridden -- all three
/// disagree, and this asserts which wins.
#[tokio::test]
async fn extra_overrides_a_selected_profile_end_to_end_the_profile_still_supplies_what_extra_does_not(
) {
    let profiles_path = write_profile_file(ANTHROPIC_PROFILE_TOML);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_matcher("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut extra = BTreeMap::new();
    extra.insert(
        "anthropic_version".to_string(),
        serde_json::json!("2099-09-09"),
    );

    let ctx = BackendBuildContext {
        id: BackendId::new("acme"),
        base_url: server.uri(),
        api_key: Some("sk-ant-api03-test".to_string()),
        dialect: Some("acme-anthropic".to_string()),
        models: BTreeMap::new(),
        profile_file_paths: vec![profiles_path.clone()],
        extra,
    };
    let backend = AnthropicBackendFactory.build(ctx).expect("must build");
    backend.generate(user_request()).await.unwrap();

    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .headers
            .get("anthropic-version")
            .unwrap()
            .to_str()
            .unwrap(),
        "2099-09-09",
        "explicit extra.anthropic_version must win over the profile's own value"
    );
    assert_eq!(
        requests[0]
            .headers
            .get("x-acme-gateway")
            .expect("extra never mentioned headers -- the profile's own value must still apply")
            .to_str()
            .unwrap(),
        "yes"
    );

    std::fs::remove_dir_all(profiles_path.parent().unwrap()).ok();
}
