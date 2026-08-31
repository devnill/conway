//! Regression coverage for the `ollama_cloud/glm-5.2` context-window
//! defect: a real session against Ollama Cloud's `glm-5.2` was refused at
//! 28,096 prompt + 8,192 reserved output = 36,288 tokens ("accepts at most
//! 32,768") while the SAME model, on the SAME endpoint, was independently
//! recorded (a separate client, same day) accepting turns up to 61,667
//! input tokens. The root cause was `glm-5.2` having no declared
//! `max_context_tokens` anywhere conway ships, so every request against it
//! silently fell through to the `"ollama"` dialect profile's
//! last-resort 32,768-token floor.
//!
//! This exercises the REAL entry point end to end: a config exactly shaped
//! like what `crates/conway-cli/src/first_run.rs`'s guided Ollama Cloud
//! setup produces (`dialect: "ollama"`, `base_url:
//! "https://ollama.com/v1"`, default model `"glm-5.2"`, no
//! `metadata_path`, no `models` overrides -- a genuinely fresh install has
//! neither) -- `OpenAiCompatBackend::new`, then `Backend::capabilities`
//! and `Backend::admit`'s default implementation
//! (`conway_core::ports::check_admission`), never an internal helper that
//! would let a fix land somewhere this test can't see it.

use std::collections::BTreeMap;

use conway_core::ids::{BackendId, ModelId};
use conway_core::ports::{check_admission, Backend};
use conway_plugin_backends::config::{Dialect, OpenAiCompatConfig};
use conway_plugin_backends::openai_compat::OpenAiCompatBackend;

/// `first_run.rs`'s Ollama Cloud choice, reproduced field-for-field (see
/// that file's `default_model: "glm-5.2"` and the `base_url`/`dialect`
/// pair `docs/providers.md`'s "Ollama Cloud" section documents as the
/// guided flow's exact output) -- no `metadata_path`, no `models`
/// overrides, because a fresh install's `.conway/` has neither file yet.
fn first_run_ollama_cloud_config() -> OpenAiCompatConfig {
    OpenAiCompatConfig {
        id: BackendId::new("ollama_cloud"),
        base_url: "https://ollama.com/v1".parse().unwrap(),
        api_key: None,
        profile: Dialect::Ollama.profile(),
        timeout: None,
        metadata_path: None,
        models: BTreeMap::new(),
    }
}

/// Acceptance criterion 1 and 3: a fresh-install Ollama Cloud config's
/// `glm-5.2` declares a window far past the old 32,768-token floor, with
/// zero `models.json`/`metadata_path` input -- the bundled per-model
/// default alone carries it.
#[test]
fn fresh_install_ollama_cloud_glm_5_2_declares_a_window_past_the_old_floor() {
    let backend = OpenAiCompatBackend::new(first_run_ollama_cloud_config())
        .expect("a valid first-run-shaped config must construct");
    let caps = backend.capabilities(&ModelId::new("glm-5.2"));

    const OLD_OLLAMA_PROFILE_FLOOR: u32 = 32_768;
    assert!(
        caps.max_context_tokens > OLD_OLLAMA_PROFILE_FLOOR,
        "glm-5.2's window ({}) must exceed the dialect floor ({OLD_OLLAMA_PROFILE_FLOOR}) with \
         no models.json/metadata_path at all -- this is what a genuinely fresh install gets",
        caps.max_context_tokens
    );
}

/// The literal regression: `check_admission` -- the exact fits/shortfall
/// arithmetic `Backend::admit`'s default implementation calls, never
/// hand-restated (see `docs/embedding.md`'s "What conway cannot enforce")
/// -- run against the REAL window `OpenAiCompatBackend::capabilities`
/// resolves for this exact pair, at the exact prompt size (61,667 tokens)
/// a real Claude Code session was recorded accepting from the same model
/// on the same endpoint. Before this item's fix this call returns
/// `Err(ContextTooLarge)` (`glm-5.2`'s window resolved to 32,768, and
/// 61,667 does not fit even with zero headroom); after it, `Ok`.
#[test]
fn glm_5_2_admits_the_recorded_accepted_prompt_size_past_the_old_floor() {
    let backend = OpenAiCompatBackend::new(first_run_ollama_cloud_config())
        .expect("a valid first-run-shaped config must construct");
    let model = ModelId::new("glm-5.2");
    let caps = backend.capabilities(&model);

    const RECORDED_ACCEPTED_TOKENS: u32 = 61_667;
    let admission = check_admission(model, RECORDED_ACCEPTED_TOKENS, 0, caps.max_context_tokens);
    assert!(
        admission.is_ok(),
        "glm-5.2 (window {}) must admit the {RECORDED_ACCEPTED_TOKENS}-token prompt a real \
         session was recorded accepting on the same endpoint; got {admission:?}",
        caps.max_context_tokens
    );
}

/// The operator's own failing shape, reproduced exactly: 28,096 prompt +
/// 8,192 reserved output = 36,288 required tokens. Under the old
/// 32,768-token floor this is `Err(ContextTooLarge)` (short by 3,520,
/// matching the operator's own error text verbatim); against `glm-5.2`'s
/// now-declared window it fits comfortably.
#[test]
fn the_exact_operator_failing_prompt_now_admits() {
    let backend = OpenAiCompatBackend::new(first_run_ollama_cloud_config())
        .expect("a valid first-run-shaped config must construct");
    let model = ModelId::new("glm-5.2");
    let caps = backend.capabilities(&model);

    let admission = check_admission(model, 28_096, 8_192, caps.max_context_tokens);
    assert!(
        admission.is_ok(),
        "the operator's exact recorded prompt (28,096 + 8,192 reserved = 36,288) must now be \
         admitted; got {admission:?}"
    );
}
