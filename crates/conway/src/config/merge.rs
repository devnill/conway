//! Five-source precedence merge (default < user < project < env < CLI),
//! `CONWAY_*` environment mapping, and `ConwayConfig` semantic validation
//! (the headroom checks and structural consistency checks).
//!
//! [`load_ignoring_user_config`] is the one seam that opts out of a source entirely
//! — see its own doc for why user config
//! alone, not `env` too, and why it is a sibling function rather than a new
//! `LoadOptions` field.
//!
//! Merge happens on `serde_json::Value` (tables union by key, arrays and
//! scalars replace wholesale), and only the final merged document is
//! deserialized into [`ConwayConfig`] — this is what makes
//! `#[serde(deny_unknown_fields)]` a meaningful fail-loud check on the
//! *result* of layering five sources, rather than on each source
//! individually (a source may legitimately omit almost everything).
//!
//! **One named exception (Stage 2a):** a top-level `tui` key is stripped
//! out of the merged document before that deserialize, rather than tripping
//! `deny_unknown_fields`, because `[tui]` is `conway-cli`'s presentation
//! config (`TuiSection` and its siblings no longer live in this schema at
//! all) and an existing `settings.json` naming it must still load
//! successfully. [`load`]/[`load_ignoring_user_config`] record the strip as a
//! [`crate::config::ConfigWarning`] rather than dropping it with no trace;
//! [`merged_document`] is the escape hatch a caller that DOES understand
//! `[tui]` (`conway-cli`) uses to read it back out of the same layered
//! document.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use conway_core::event_name::validate_event_name;
use conway_core::ids::{ModelRef, RoleAlias};
use conway_runtime::hook_dispatch::EVENTS_WITHOUT_TOOL_NAME;
use serde_json::{Map, Value};

use crate::config::model_metadata::ModelMetadata;
use crate::config::schema::ConwayConfig;
use crate::config::{discovery, model_metadata, ConfigWarning, LoadOutcome, WarningCode};
use crate::error::{FacadeError, Result};

/// The five-source `load` input. `env` stands in for the process
/// environment: `load` with a default `LoadOptions` reads `std::env::vars()`
/// into it, but tests construct their own map so precedence tests never
/// mutate real process env (and stay parallel-safe).
#[derive(Debug, Clone)]
pub struct LoadOptions {
    pub cwd: PathBuf,
    pub explicit_path: Option<PathBuf>,
    pub env: HashMap<String, String>,
    pub cli_overrides: CliOverrides,
    /// The only supported default is `false`: `load` performs no network
    /// I/O regardless of this flag's value (there is currently no code path
    /// that reads it — it exists so a future opt-in refresh call site has
    /// somewhere to receive caller intent without changing this struct's
    /// shape).
    pub model_metadata_refresh: bool,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            explicit_path: None,
            env: std::env::vars().collect(),
            cli_overrides: CliOverrides::default(),
            model_metadata_refresh: false,
        }
    }
}

/// An embedder-facing override struct: [`load`]'s fifth (highest-precedence)
/// layer, and [`apply_cli`]'s re-application point. Fully wired and tested
/// *as a library API* — an embedder that constructs one and passes it to
/// `LoadOptions::cli_overrides` or `ConwayBuilder::with_cli_overrides` gets
/// exactly the precedence and validation this module promises.
///
/// **What it is not (corrected):** despite its name and this
/// struct's original doc comment, `conway-cli` does not construct or pass
/// one of these. `grep -rn "with_cli_overrides" crates/` finds exactly three
/// hits: this struct's definition, and two test files
/// (`crates/conway-cli/tests/oneshot_ask.rs`,
/// `crates/conway-cli/tests/continuity.rs`) — zero production call sites.
/// `conway-cli`'s actual flag-to-config wiring is separate, bespoke code in
/// that crate, not this struct. The previous wording ("mirrored here (not
/// in `conway-cli`) so the library is the source of truth") read as
/// a claim that CLI flag values flow through this exact struct in
/// production; they do not, and the next person to add a field here on the
/// strength of that claim would reasonably expect it to reach a real `conway`
/// invocation when it would not. Whether/how to reconcile the bespoke
/// `conway-cli` wiring with this struct is the open architectural question
/// filed as — not decided here.
///
/// Reconciliation disclosed here: this field list is
/// exactly the amendment's enumerated set. It has no per-backend override
/// (e.g. no `backends.<id>.base_url` field) — the precedence-test criterion
/// names `backends.<id>.base_url` as one of the keys to prove all five
/// sources against, but no CLI path exists for that leaf under this
/// documented shape. `tests/config_precedence.rs` covers that key across
/// default/user/project/env (four sources) and notes the gap rather than
/// inventing an undocumented field here.
///
/// **`permission_mode`/`allowed_tools`/`denied_tools` removed, `default_mode`
/// added** (board item 01M1YVP3FDPHY4WZ72SXMWAN2D): the first three used to
/// translate into `permissions.mode`/`permissions.allowed_tools`/
/// `permissions.denied_tools` in the merge document, keys `PermissionsConfig`
/// no longer has at all (`gates::GateConfig` replaced them, an explicit
/// Rust value an embedder now passes to `ConwayBuilder::with_gate_config`
/// directly rather than through this override struct's merge-document
/// detour). `default_mode` is its replacement's own CLI-precedence path,
/// translating into `permissions.default_mode`.
#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub default_role: Option<RoleAlias>,
    pub default_mode: Option<conway_core::permission_mode::PermissionMode>,
    pub max_steps: Option<u32>,
    pub session_root: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    /// Amendment addition: sets `routing.default_headroom_tokens`. There is
    /// no CLI form for a per-role override — a CLI-supplied value is a
    /// session-wide floor.
    ///
    /// Not a mode-reachability violation despite no `conway-cli` flag
    /// setting it through this struct (see the struct doc comment):
    /// headroom is independently reachable today via `settings.json`
    /// (`[routing].default_headroom_tokens`, `[roles.<alias>].headroom_tokens`)
    /// and the `CONWAY_ROUTING__DEFAULT_HEADROOM_TOKENS` /
    /// `CONWAY_ROLES__<ALIAS>__HEADROOM_TOKENS` env vars. Left in place
    /// rather than removed (a settled design decision): unlike the deleted `model` field —
    /// which had no `ConwayConfig` key to land on and was skipped by this
    /// struct's own `cli_overrides_to_value` — this field IS translated
    /// into the merge document (`routing.default_headroom_tokens`) and is
    /// exercised end to end by `tests/config_headroom.rs`. Its only gap is
    /// that no `conway-cli` flag ever constructs a `CliOverrides` with it
    /// set, which is now true of every field here: `conway-cli` wires its
    /// own flags directly rather than through this struct (see the struct
    /// doc comment), so this field's reachability from a real CLI
    /// invocation is exactly as good, and exactly as absent, as its five
    /// remaining siblings'.
    pub headroom_tokens: Option<u32>,
}

/// The full five-source load: default < user < project < env < CLI.
pub fn load(options: LoadOptions) -> Result<LoadOutcome> {
    load_impl(options, IncludeUserLayer::Yes)
}

/// Identical to [`load`], except the user layer
/// (`$CONWAY_CONFIG_DIR/settings.json`, or `~/.conway/settings.json`)
/// is never read — the merge becomes `default < project < env < CLI`, four
/// sources instead of five.
///
/// [`load`] reads the user layer unconditionally, *before* the
/// `explicit_path`/discovered project layer, regardless of whether
/// `options.explicit_path` is set — so a caller who wants isolation (a test
/// fixture, or an embedder that wants to use its own configuration rather
/// than whatever is in the invoking user's home directory) has had no way
/// to get it. This function is that seam. See
/// [`crate::builder::ConwayBuilder::from_config_only`] for the builder-level
/// entry point most callers want instead of calling this directly.
///
/// **A sibling function, not a new `LoadOptions` field, deliberately:**
/// `LoadOptions` is constructed via full struct-literal syntax (naming
/// every field, no `..LoadOptions::default()` base) at call sites in other
/// crates across this workspace (`crates/conway-cli/src/tui/view/status.rs`,
/// `crates/conway-thirdparty-backend/src/lib.rs`) — outside this seam's own
/// file lane. Growing `LoadOptions`'s field set would silently break every
/// one of those at compile time for a capability they have no reason to
/// opt into; a same-shaped sibling function costs them nothing.
///
/// **user config only, not `env` too — decided, not left implicit:** `CONWAY_*`
/// environment variables differ from the user layer in the one way that
/// matters here — they are how a *caller* (CI, a container entrypoint, an
/// embedder's own process supervisor) explicitly hands *this* invocation
/// its credentials and overrides, at the moment `options.env` is
/// constructed and passed in. The user layer, by contrast, is a *file on
/// disk*, written independently of any particular invocation and
/// discovered by walking the filesystem rather than supplied by the
/// caller — exactly the ambient "invoking user's home directory" state this
/// function exists to bypass. Suppressing `env` here would break the
/// CI/embedder credential-passing use case this seam serves, for no
/// isolation benefit: a caller that also wants an env-free load already has
/// the tool for that — pass a hand-built (possibly empty) `env` map, the
/// same mechanism every hermetic test in this workspace already uses (see
/// `crates/conway/tests/support/mod.rs::isolated_env`).
pub fn load_ignoring_user_config(options: LoadOptions) -> Result<LoadOutcome> {
    load_impl(options, IncludeUserLayer::No)
}

/// Whether [`load_impl`] reads the user layer — a private, two-variant
/// enum rather than a bare `bool` so `load`/`load_ignoring_user_config`'s own call
/// sites stay self-documenting at the call site, not `load_impl(options,
/// true)`/`load_impl(options, false)` with no indication of which way
/// `true` goes.
enum IncludeUserLayer {
    Yes,
    No,
}

/// The fully layered document (the same five-source precedence [`load`]
/// uses -- default < user < project < env < CLI), as raw JSON, BEFORE the
/// final `ConwayConfig` deserialize.
///
/// **The one sanctioned escape hatch for a section this facade's schema
/// deliberately does not define.** Stage 2a moved `TuiSection`/
/// `ThemeConfig`/`StatusLineConfig`/`ThemeStyleConfig` out of
/// `ConwayConfig` entirely -- `[tui]` is `conway-cli`'s presentation
/// config, and a headless host linking only this facade has no business
/// parsing or validating a theme it can never render. `load`/
/// `load_ignoring_user_config` strip a top-level `tui` key out of the merged
/// document before deserializing (see their own doc for why: otherwise
/// EVERY existing `settings.json` with a `[tui.theme]`/`[tui.status_line]`
/// block would hard-fail to load through this crate at all), so `[tui]`'s
/// actual value is not reachable through [`LoadOutcome`] at all any more.
/// `conway-cli` calls this function directly instead, to read `[tui]`'s
/// raw value back out of the SAME layered document and deserialize it into
/// its own, locally-owned `TuiSection` (`crates/conway-cli/src/tui/
/// config.rs`) -- the one caller today.
///
/// Every other caller should prefer [`load`]/[`load_ignoring_user_config`]
/// instead: this bypasses `ConwayConfig`'s `#[serde(deny_unknown_fields)]`
/// validation entirely, so a typo anywhere in the document is not caught
/// here.
pub fn merged_document(options: &LoadOptions) -> Result<Value> {
    merged_document_impl(options, IncludeUserLayer::Yes)
}

fn merged_document_impl(
    options: &LoadOptions,
    include_user_config: IncludeUserLayer,
) -> Result<Value> {
    let mut merged = default_document();

    if matches!(include_user_config, IncludeUserLayer::Yes) {
        if let Some(path) = discovery::user_config_path(&options.env) {
            if let Some(layer) = read_json_layer(&path)? {
                merge_values(&mut merged, layer);
            }
        }
    }

    // The exclusion list closes the "one file, two roles" collision
    // `discovery::discover`'s own module doc describes: without it, an
    // unbounded upward walk from `options.cwd` can reach
    // `~/.conway/settings.json` and return it as the *project* layer even
    // when `CONWAY_CONFIG_DIR` (above) has relocated the *user* layer
    // elsewhere -- silently defeating the isolation that variable
    // advertises (board item `01M0VV6CVSZM4XH8J4G6EBV5E3`).
    let project_path = options.explicit_path.clone().or_else(|| {
        discovery::discover(
            &options.cwd,
            &discovery::project_discovery_exclusions(&options.env),
        )
    });
    if let Some(path) = project_path {
        if let Some(layer) = read_json_layer(&path)? {
            merge_values(&mut merged, layer);
        }
    }

    let env_layer = env_to_value(&options.env, &merged);
    merge_values(&mut merged, env_layer);

    let cli_layer = cli_overrides_to_value(&options.cli_overrides);
    merge_values(&mut merged, cli_layer);

    Ok(merged)
}

fn load_impl(options: LoadOptions, include_user_config: IncludeUserLayer) -> Result<LoadOutcome> {
    let mut merged = merged_document_impl(&options, include_user_config)?;

    // `[tui]` (or a `CONWAY_TUI__*` env var) is a presentation-only
    // section this facade deliberately does not define a type for any
    // more (Stage 2a; see `merged_document`'s own doc). Extracted and
    // DROPPED here, rather than handed to `ConwayConfig`'s
    // `#[serde(deny_unknown_fields)]` deserialize below, which would
    // otherwise hard-fail loading any EXISTING settings.json that
    // configures a TUI theme or status line -- `conway-cli` genuinely
    // still needs those to load successfully (it re-reads `[tui]` itself
    // via `merged_document`, see that function's own doc). Silently
    // dropping it with no trace at all would be the worst option (board
    // item's own framing): a `ConfigWarning` is pushed onto this outcome
    // instead, so a caller that does NOT separately re-parse `[tui]`
    // itself is told its presence was seen and ignored, not left to
    // wonder why nothing happened.
    let had_tui = merged
        .as_object_mut()
        .is_some_and(|obj| obj.remove("tui").is_some());

    // Board item 01M1YVP3FDPHY4WZ72SXMWAN2D: a settings file naming one of
    // the three removed `[permissions]` keys gets a message that POINTS
    // somewhere (`permissions.json`/`default_mode`), not `serde_json`'s
    // bare "unknown field" -- checked ahead of the generic deserialize
    // below, which would otherwise be what an operator actually sees.
    if let Some(message) = crate::config::schema::permissions_removed_key_error(&merged) {
        return Err(FacadeError::Config {
            path: None,
            message,
        });
    }

    let mut config: ConwayConfig =
        serde_json::from_value(merged).map_err(|e| FacadeError::Config {
            path: None,
            message: format!("failed to parse merged configuration: {e}"),
        })?;

    config.cwd = resolve_cwd(&config.cwd, &options.cwd);

    let metadata_path = resolve_metadata_path(&config.models.metadata_path, &options.cwd);
    let metadata = model_metadata::load(&metadata_path)?;

    // ADAPTIVE HEADROOM IS NO LONGER COMPUTED HERE (board item
    // `01M2TVEWVMPP69TZ17XSGWEW82`). This is the removal that item names as
    // its blocking prerequisite, and it is worth recording why, because the
    // code that used to sit here looked entirely reasonable.
    //
    // It derived ONE headroom per role, as a fraction of the SMALLEST
    // context window it could find among that role's chain entries, and
    // wrote the result into `roles.<alias>.headroom_tokens`. Two defects,
    // and the second is the one that cost an operator a working model:
    //
    //  1. A chain entry absent from `models.json` was invisible to the
    //     scan. A chain of [32k-model-not-in-metadata, 1M-model] therefore
    //     had "smallest" = 1,000,000, and at the default fraction of 10
    //     derived 100,000 tokens of headroom -- which made the 32k model
    //     permanently unreachable. The warning that should have caught that
    //     used the identical scan, compared 100,000 against 1,000,000, and
    //     stayed silent.
    //  2. Writing the derived value INTO the role's own override field made
    //     a number conway invented indistinguishable, downstream, from one
    //     the operator typed. That is what made a correct precedence
    //     impossible: nothing after this point could tell level 2
    //     (operator, per role) from level 3 (derived).
    //
    // The fraction now travels to `conway_core::routing::RoutingConfig::
    // headroom_fraction` (via `ConwayConfig::routing`) and is applied by
    // the ROUTER, per candidate, against that candidate's OWN window --
    // which is the only window the number is ever true about. Nothing
    // rewrites the operator's document.
    //
    // GP-14 ("the computed value is visible") is still met, and better:
    // `ConwayConfig::headroom_for_model` answers it for any
    // (role, model) pair, `routes explain` reports it per candidate
    // (`ExplainEntry::headroom_tokens`), and check 6 below now names the
    // specific chain entry rather than a role-wide aggregate.

    let mut warnings = validate(&config, &metadata, &options.env)?;
    if had_tui {
        warnings.push(ConfigWarning {
            code: WarningCode::PresentationConfigIgnored,
            message: "a [tui] section (or a CONWAY_TUI__* environment variable) is present, \
                      but conway's own config schema no longer defines [tui] -- it is \
                      conway-cli's presentation config (theme/status-line/tool-preview-lines/\
                      history-size), not the facade's, as of Stage 2a. This load accepted the \
                      rest of the document and discarded [tui] entirely; a caller that is not \
                      conway-cli (which re-reads [tui] itself through a separate, un-stripped \
                      merge) will not find its value anywhere."
                .to_string(),
        });
    }

    // `[session].root` central-default resolution (board item
    // `01M0QK9GRM8HSNWRAR414TCX42`, decision `01M0QK8J757ZH6R06WYJ0PQGEM`).
    // `root` is still `None` here whenever no source (user/project/env/CLI)
    // set it -- `default_document`'s own comment on why it never bakes one
    // in. Resolved HERE, once, using `options.cwd`/`options.env` (the same
    // load-scoped, already-isolated inputs `resolve_metadata_path` just
    // used two lines up), because `ConwayBuilder::build` has no seam of its
    // own to receive them -- see `SessionConfig`'s own doc for the full
    // disclosure of what a config that bypasses `load` entirely
    // (`ConwayBuilder::from_parts`) falls back to instead.
    //
    // **No warning when an old, unmigrated `<cwd>/.conway/sessions`
    // exists** -- removed by decision `01M0RW05G3Y81AZW96NVKTY1RV`, which
    // reversed board item `01M0QK9GRM8HSNWRAR414TCX42`'s own "leave and
    // point" choice: a ~90-word notice repeating on every single run to
    // describe a one-time, already-CHANGELOG'd fact was a permanent tax on
    // a pre-1.0 tree with no users who didn't choose to be early. Nothing
    // about the resolution itself changed -- an old directory is still
    // never read, moved, or deleted here, only never pointed at again; see
    // `docs/sessions.md`'s "If you already have a project-local
    // `.conway/sessions`" section for where the explanation now lives
    // instead of in-product.
    if config.session.root.is_none() {
        config.session.root = Some(discovery::session_root(&options.cwd, None, &options.env));
    }

    Ok(LoadOutcome { config, warnings })
}

/// Re-applies `cli_overrides` to an already-loaded config (used by
/// `ConwayBuilder::build`, so an
/// invalid override is caught even when `from_parts`/`from_config` bypassed
/// `load`'s own CLI layer). Runs every validation check unconditionally --
/// board item 01M1YVP3FDPHY4WZ72SXMWAN2D removed the one check
/// (`permissions.mode = "allowlist"` requiring non-empty `allowed_tools`,
/// formerly check 3) that used to run here on a narrower footing than
/// [`validate`]'s own STRICT entry point; every check that remains has
/// always run identically at both call sites.
pub fn apply_cli(config: &ConwayConfig, cli: &CliOverrides) -> Result<ConwayConfig> {
    let mut value = serde_json::to_value(config).map_err(|e| FacadeError::Config {
        path: None,
        message: format!("failed to serialize config for CLI override merge: {e}"),
    })?;
    merge_values(&mut value, cli_overrides_to_value(cli));
    let merged: ConwayConfig = serde_json::from_value(value).map_err(|e| FacadeError::Config {
        path: None,
        message: format!("failed to parse config after CLI override merge: {e}"),
    })?;

    let metadata_path = resolve_metadata_path(&merged.models.metadata_path, &merged.cwd);
    let metadata = model_metadata::load(&metadata_path).unwrap_or_else(|_| ModelMetadata::empty());
    validate_impl(&merged, &metadata, &HashMap::new())?;

    Ok(merged)
}

/// Resolves the merged `cwd` VALUE (`configured`) against the load's own
/// invocation directory (`invocation`, i.e. `LoadOptions::cwd`, which
/// `LoadOptions::default` fills from `std::env::current_dir()`): returned
/// unchanged when already absolute, otherwise joined onto `invocation` and
/// lexically normalized ([`discovery::normalize_lexically`] -- no
/// filesystem I/O, no symlink resolution, so this stays as pure and
/// deterministic as the rest of [`load`]).
///
/// **Why this exists at all** (board item
/// `01M2V6JHRWWEPN690C954R0PYS`): `ConwayConfig::cwd`'s schema default is
/// the literal `"."` (`schema::default_cwd`) and must STAY that way --
/// `default_document`'s own doc records why a real `std::env::current_dir()`
/// baked into the default would reintroduce the "`default_document` and
/// `ConwayConfig::baseline()` silently disagree" drift a previous item
/// eliminated. But nothing used to replace that placeholder afterwards, so
/// the shipped binary handed a literal `"."` to every downstream consumer
/// that does path ARITHMETIC on it -- most visibly
/// `conway_plugin_idiom::project_instructions_path`, whose walk up to the
/// enclosing git root is completely inert on `"."` (`Path::new(".")
/// .parent()` is `Some("")`, whose `.parent()` is `None`: the walk gives
/// up after two relative misses). Launching conway from a SUBDIRECTORY of
/// a repository therefore never found the repo-root `AGENTS.md` or
/// `.conway/instructions.md`, silently; from the repository root it only
/// appeared to work because `.` itself held the file. The environment
/// block that same plugin sends the model was the second symptom, telling
/// it `cwd .`.
///
/// Fixed HERE, at the one place the load's own invocation directory and
/// the merged `cwd` value are both in hand, rather than inside any
/// individual consumer: canonicalizing at a leaf (the git-root walk, say)
/// would leave every OTHER consumer reading `"."` and would not touch the
/// environment block at all. The walk is not wrong; its input was.
///
/// An operator who writes a genuinely relative `"cwd"` into a settings
/// file (or passes one through [`CliOverrides::cwd`]) gets it interpreted
/// relative to the invocation directory, which is the only reading that
/// matches how `std::fs` would have resolved it anyway -- so this changes
/// the SPELLING every consumer sees, not which directory it names.
fn resolve_cwd(configured: &std::path::Path, invocation: &std::path::Path) -> PathBuf {
    if configured.is_absolute() {
        return configured.to_path_buf();
    }
    discovery::normalize_lexically(&invocation.join(configured))
}

/// Resolves a raw `[models].metadata_path` value (or its schema default,
/// `.conway/models.json`) against `cwd`: returned unchanged if absolute,
/// else joined onto `cwd`. Takes the bare path rather than a whole
/// [`ConwayConfig`] -- the only two production callers were `load_impl`/
/// `apply_cli` (both already hold a full, validated config to read the
/// field off of) until [`metadata_path_for`] joined them: that caller has
/// only peeked at the merged document's own `models.metadata_path` (never
/// run the full validated `load`, which a setup flow with no backend
/// configured yet cannot always satisfy), so it needs to resolve the SAME
/// value the same way without constructing a `ConwayConfig` first.
pub fn resolve_metadata_path(metadata_path: &std::path::Path, cwd: &std::path::Path) -> PathBuf {
    if metadata_path.is_absolute() {
        metadata_path.to_path_buf()
    } else {
        cwd.join(metadata_path)
    }
}

/// The effective `models.json` path a setup-time caller (never running the
/// full [`load`]) should read/write -- the SAME five-source
/// `models.metadata_path` value (default < user < project < env; no CLI
/// layer here, since this runs before any CLI flags are parsed) [`load`]
/// itself resolves via [`resolve_metadata_path`], computed through
/// [`merged_document`] rather than [`load`] so this never fails just
/// because backends/roles/routing are not fully configured yet -- exactly
/// the state a setup flow runs in (`crates/conway-cli/src/first_run.rs`'s
/// guided setup, and `crates/conway-cli/src/tui/app/provider_manage.rs`'s
/// `/settings` → providers → add).
///
/// **This is the setup-time context-window persistence's own file/scope
/// decision** (board item: setup-time context window, ASK + PERSIST): a
/// discovered-or-typed window is written into whichever `models.json` this
/// exact process would ACTUALLY read back for `cwd` right now -- never a
/// new, always-user-scope location invented for this purpose. Two
/// alternatives were considered and rejected; see
/// `crates/conway-cli/src/first_run.rs`'s own module doc for the fuller
/// account:
/// - **Rejected: always write `$CONWAY_CONFIG_DIR/models.json` (or
///   `~/.conway/models.json`), alongside `settings.json`**, and point
///   `[models].metadata_path` at it explicitly. This would make a
///   discovered window follow the operator across every `cwd` -- but it
///   also means the FIRST setup to run this code would silently redirect
///   `[models].metadata_path` away from any project-local `.conway/
///   models.json` an operator already relies on (the default, unqualified
///   resolution this same function reproduces), shadowing a working
///   project override the operator never asked to change. A setup flow
///   must never rewrite a config key it was not asked to change as a side
///   effect of persisting an unrelated value.
/// - **Rejected: `backends.<id>.models.<model>.max_context_tokens`** (a
///   sibling key inside the `settings.json` entry itself) -- this is the
///   channel `crate::builder`'s own module doc and `docs/providers.md`
///   confirm is INERT for `"openai-compat"` (`factory.rs`'s `ctx.extra`
///   precedence overlay is exercised for the `anthropic` kind only); an
///   earlier draft of this same board item wrote there, found the defect,
///   and reverted -- recorded here so it is never rebuilt on.
pub fn metadata_path_for(cwd: &std::path::Path, env: &HashMap<String, String>) -> Result<PathBuf> {
    let options = LoadOptions {
        cwd: cwd.to_path_buf(),
        explicit_path: None,
        env: env.clone(),
        cli_overrides: CliOverrides::default(),
        model_metadata_refresh: false,
    };
    let merged = merged_document(&options)?;
    let raw = merged
        .get("models")
        .and_then(|m| m.get("metadata_path"))
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".conway/models.json"));
    Ok(resolve_metadata_path(&raw, cwd))
}

/// The built-in, lowest-precedence layer -- derived, not hand-maintained.
/// See `routing.default_headroom_tokens`'s treatment in
/// `schema::DEFAULT_HEADROOM_TOKENS`'s own doc comment for why `8_192` (not
/// the headroom amendment's literal `16000`) is what a role with no
/// override resolves to.
///
/// `roles.default` (an empty chain, no headroom override) is baked in so
/// that `default_role = "default"` — itself a baked-in default — passes
/// the "`default_role` exists in `[roles]`" validation check even when
/// every other source is absent (the bare-defaults / "D wins" precedence-
/// test stage). An empty chain trivially satisfies the chain-format/
/// backend-existence check too.
///
/// **Named `"default"`, not `"coder"` (board item
/// 01M02CWXP4846SX97KNW35501S).** This layer used to ship `"coder"` here,
/// which was a coding-agent opinion in a facade that serves a coding agent
/// and a bare inference call equally — the exact asymmetry `docs/
/// embedding.md`'s "Discovery, not a struct literal" section already
/// rejects for `ConwayConfig`'s absent `Default` impl one layer up
/// (settled by board item 01M00QGYR1M8F71HTAA1S3PEKS). Renaming the JSON
/// key instead of dropping it keeps the property that section documents
/// and this crate's tests still prove: an unmodified default routes
/// nowhere and fails loud (`RoutingError::NoCandidate`) rather than
/// silently — dropping `default_role`/`roles` entirely here instead would
/// turn "D wins" into a five-source-precedence failure mode with no
/// baked-in floor, a strictly larger change this finding did not ask for.
/// `"default"` names the role's actual content (an unconfigured
/// placeholder), not a task any particular embedder performs.
///
/// **Every caller that lists roles for a human to choose among must skip
/// this floor entry** ([`is_baked_in_role_floor`], below) -- it is a
/// validation safety net, not a role anyone configured or would
/// deliberately select. Board item `01M18Q7P25DTSKQJDJJCC3E800` found this
/// the hard way: `/settings`' "default role" cycle list read
/// [`merged_document`]'s `roles` map directly and offered this floor
/// alongside every real, operator-declared role.
///
/// **What this floor is for, restated after board item
/// `01M1A2HKMDGNK961ZFV1EGZDQ0`, which found guided setup relying on it by
/// accident:** `conway-cli::first_run::finish_setup` used to write only
/// `backends.<id>` and never touched `default_role`/`roles` at all, so
/// every guided run landed on THIS empty floor and failed loud with
/// "no candidate for role default (0 considered)" the moment a real turn
/// was attempted -- guided setup's own verification passed (it builds an
/// independent, in-memory config with a real chain) while the file it left
/// behind could never route. That was never this floor's job and is fixed
/// at the writer (`conway::config::writer::set_role_chain`/
/// `ensure_default_role`), not here: this floor exists ONLY so that a
/// document with genuinely nothing configured -- an embedding caller that
/// builds a bare `ConwayConfig`, a non-interactive run that declines
/// guided setup, or a fresh checkout before guided setup has ever run --
/// still VALIDATES (passes the "`default_role` exists in `[roles]`"
/// check) instead of failing to even start. Once guided setup (or an
/// operator by hand) writes a real `roles.default.chain`, this floor's own
/// entry is masked by the overlay (`merge_values`, below) and never
/// consulted again for that document -- nothing in this crate reads this
/// floor's *content* as a fallback for a real chain that is merely short
/// or exhausted. Continuing to rely on this floor for "the document
/// parses at all" is still correct and still needed; relying on it for
/// "the document routes anywhere" -- guided setup's old, accidental use --
/// is the defect that board item fixed.
pub(crate) const BASELINE_ROLE_NAME: &str = "default";

/// The built-in, lowest-precedence layer, as raw JSON --
/// `serde_json::to_value(ConwayConfig::baseline())`, nothing more. There is
/// exactly one place a section's default value is stated: its own `impl
/// Default` in `schema.rs`. In particular, `["limits"]["max_steps"]` here is
/// whatever `LimitsConfig::default().max_steps` says (currently `0`,
/// unlimited) -- NOT `conway_core::agent::Budget::default().max_steps`
/// (`40`), a deliberately different third site for the same-named field;
/// see `LimitsConfig`'s own doc and `Budget`'s own doc for why a root
/// session's default and a bare subagent's default disagree on purpose.
/// This function used to be a second one -- a
/// hand-maintained `serde_json::json!` literal, required to name the exact
/// same values `schema.rs` already named, with nothing enforcing that it
/// did. It drifted more than once (`LimitsConfig`'s `max_tool_calls` and
/// `RoutingSection`'s `headroom_fraction` were both missing here while
/// present in their own `Default` impls; the `max_steps` fix, commit
/// `a0f560d`, had to change both files by hand and its own commit message
/// said so). Deserializing this value back into each section's own
/// type and comparing it against that section's `Default::default()` -- the
/// round-trip `crates/conway/tests/config_defaults_single_source.rs`
/// performs -- can no longer fail: there is only one value to disagree with
/// itself.
///
/// **No `session.root` special case any more.** `SessionConfig::default()`
/// leaves `root: None`, which `Serialize` writes as `"root": null` here;
/// `Option<PathBuf>`'s `Deserialize` reads an explicit `null` identically to
/// the key being absent altogether, so the two are equivalent through this
/// merge (`merge_values`, below, replaces `null` with an overlay's value the
/// same way it replaces an absent key) and `load_impl`'s own
/// `config.session.root.is_none()` check downstream sees the identical
/// `None` either way. An earlier version of this function special-cased
/// `root` out of the literal to avoid asserting a value here; deriving from
/// `ConwayConfig::baseline()` makes that unreachable by construction instead
/// of by a hand-maintained omission -- see `SessionConfig`'s own doc for why
/// `None` and `Some` mean genuinely different things, unaffected by this.
fn default_document() -> Value {
    serde_json::to_value(ConwayConfig::baseline())
        .expect("ConwayConfig::baseline() is plain data with no non-serializable field")
}

/// True when `(name, entry)` is exactly `default_document`'s own baked-in
/// role floor -- `"default"`, an empty chain, no overrides at all -- rather
/// than a role an operator declared. The floor exists only so a config with
/// no `[roles]`/`default_role` of its own still validates (see
/// `default_document`'s own doc); it was never meant to be offered to a
/// human as something to choose. Comparing the VALUE too (not just the
/// name) means an operator who deliberately declares their own role
/// literally named `"default"` is unaffected: the moment it carries a real
/// chain or any other override, `entry != RoleEntry::default()` and this
/// returns `false` -- exactly the layer at which the merge itself lets an
/// overlay replace the floor's own fields (`merge_values`, above).
///
/// Board item `01M18Q7P25DTSKQJDJJCC3E800` needed this: `/settings`'
/// "default role" cycle list (`conway_cli::tui::app::defaults`) read
/// [`merged_document`]'s `roles` map directly and, without this filter,
/// offered the floor alongside every real, operator-declared role.
/// ACCEPTED LIMIT, stated rather than left to be discovered: this matches
/// on the name AND on the entry being byte-identical to `RoleEntry::default()`,
/// so an operator who deliberately names their own role `default` and has not
/// yet given it a chain is indistinguishable from the floor and is filtered
/// too. It reappears the instant that role gains any content, nothing is lost
/// from the config itself, and the alternative -- a marker distinguishing the
/// floor from an identical operator-written entry -- would mean carrying
/// provenance through the whole merge for one self-resolving edge case.
pub fn is_baked_in_role_floor(name: &str, entry: &crate::config::schema::RoleEntry) -> bool {
    name == BASELINE_ROLE_NAME && *entry == crate::config::schema::RoleEntry::default()
}

/// Deep merge: `Object`+`Object` unions by key (recursing); anything else
/// (including array-vs-array) replaces `base` wholesale with `overlay`.
fn merge_values(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Object(base_map), Value::Object(overlay_map)) => {
            for (key, value) in overlay_map {
                match base_map.get_mut(&key) {
                    Some(existing) => merge_values(existing, value),
                    None => {
                        base_map.insert(key, value);
                    }
                }
            }
        }
        (base_slot, overlay_value) => {
            *base_slot = overlay_value;
        }
    }
}

fn read_json_layer(path: &std::path::Path) -> Result<Option<Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let value: Value = serde_json::from_str(&text).map_err(|e| FacadeError::Config {
                path: Some(path.to_path_buf()),
                message: format!("failed to parse JSON at {}: {e}", path.display()),
            })?;
            Ok(Some(value))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(FacadeError::Config {
            path: Some(path.to_path_buf()),
            message: format!("failed to read {}: {e}", path.display()),
        }),
    }
}

/// The top-level `ConwayConfig` field names a `CONWAY_*` env var's first
/// segment must match; anything else is silently ignored ("unknown
/// `CONWAY_*` vars are ignored without error").
const KNOWN_TOP_LEVEL_KEYS: &[&str] = &[
    "default_role",
    "cwd",
    "session",
    "limits",
    "permissions",
    "backends",
    "routing",
    "roles",
    "health",
    "agents",
    "models",
    "tui",
];

/// Array-valued leaf keys, so their env values are comma-split rather than
/// parsed as a single scalar.
const ARRAY_LEAF_KEYS: &[&str] = &["fields"];

/// Builds the env-derived merge layer. `CONWAY_` prefix, `__` as the table
/// separator, uppercase with single `_` preserved within a segment.
/// `known_roles` (the roles table already merged from lower-precedence
/// sources) gates per-role env vars: `CONWAY_ROLES__<ALIAS>__HEADROOM_TOKENS`
/// is applied only when `<ALIAS>` case-insensitively matches an existing
/// role; otherwise it is ignored, per the amendment's "unknown alias is
/// ignored" rule.
fn env_to_value(env: &HashMap<String, String>, merged_so_far: &Value) -> Value {
    let known_roles: BTreeSet<String> = merged_so_far
        .get("roles")
        .and_then(Value::as_object)
        .map(|roles| roles.keys().map(|k| k.to_lowercase()).collect())
        .unwrap_or_default();

    let mut root = Map::new();
    for (key, raw_value) in env {
        let Some(rest) = key.strip_prefix("CONWAY_") else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        let segments: Vec<String> = rest.split("__").map(|s| s.to_lowercase()).collect();
        let Some(top) = segments.first() else {
            continue;
        };
        if !KNOWN_TOP_LEVEL_KEYS.contains(&top.as_str()) {
            continue;
        }
        if top == "roles" {
            // Expect exactly `roles.<alias>.<field>`; only `headroom_tokens`
            // is env-settable per role, and only for a known alias.
            if segments.len() != 3 || segments[2] != "headroom_tokens" {
                continue;
            }
            if !known_roles.contains(&segments[1]) {
                continue;
            }
        }

        let leaf_key = segments.last().unwrap().clone();
        let value = if ARRAY_LEAF_KEYS.contains(&leaf_key.as_str()) {
            Value::Array(
                raw_value
                    .split(',')
                    .map(|s| Value::String(s.trim().to_string()))
                    .collect(),
            )
        } else {
            parse_env_scalar(raw_value)
        };

        set_path(&mut root, &segments, value);
    }
    Value::Object(root)
}

fn parse_env_scalar(raw: &str) -> Value {
    if let Ok(i) = raw.parse::<i64>() {
        return Value::from(i);
    }
    if let Ok(f) = raw.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Value::Number(n);
        }
    }
    match raw {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => Value::String(raw.to_string()),
    }
}

/// Inserts `value` into `root` at the nested path named by `segments`,
/// creating intermediate objects as needed.
fn set_path(root: &mut Map<String, Value>, segments: &[String], value: Value) {
    if segments.len() == 1 {
        root.insert(segments[0].clone(), value);
        return;
    }
    let entry = root
        .entry(segments[0].clone())
        .or_insert_with(|| Value::Object(Map::new()));
    if let Value::Object(map) = entry {
        set_path(map, &segments[1..], value);
    }
}

fn cli_overrides_to_value(cli: &CliOverrides) -> Value {
    let mut root = Map::new();
    if let Some(role) = &cli.default_role {
        root.insert(
            "default_role".to_string(),
            Value::String(role.as_str().to_string()),
        );
    }
    if let Some(cwd) = &cli.cwd {
        root.insert(
            "cwd".to_string(),
            Value::String(cwd.to_string_lossy().to_string()),
        );
    }
    if let Some(mode) = cli.default_mode {
        // `PermissionMode` serializes via serde (`rename_all = "snake_case"`)
        // rather than a hand-written match, so this stays correct if a
        // fourth mode is ever added -- `expect` is safe: an enum with no
        // custom `Serialize` impl and no interior data cannot fail to
        // serialize to a `Value`.
        let value = serde_json::to_value(mode).expect("PermissionMode always serializes");
        set_path(
            &mut root,
            &["permissions".to_string(), "default_mode".to_string()],
            value,
        );
    }
    if let Some(max_steps) = cli.max_steps {
        set_path(
            &mut root,
            &["limits".to_string(), "max_steps".to_string()],
            Value::from(max_steps),
        );
    }
    if let Some(session_root) = &cli.session_root {
        set_path(
            &mut root,
            &["session".to_string(), "root".to_string()],
            Value::String(session_root.to_string_lossy().to_string()),
        );
    }
    if let Some(headroom) = cli.headroom_tokens {
        set_path(
            &mut root,
            &["routing".to_string(), "default_headroom_tokens".to_string()],
            Value::from(headroom),
        );
    }
    Value::Object(root)
}

/// Threshold (as `1 / N`) for `WarningCode::HeadroomConsumesLargeFractionOfContext`
/// (check 7, below): a role's effective headroom fires this warning once it
/// reaches `>= 1 / HEADROOM_FRACTION_WARN_DENOM` of the smallest reachable
/// context window in its chain. `4` (25%) is chosen to name the exact ratio
/// that broke board item `01M1AVZPTRSWVE33G4DTJY7Q1B`'s walked scenario
/// (conway's own built-in default, `8192`, against a 32768-token window is
/// precisely 25%) without also firing for that same built-in default against
/// any window a typical hosted model declares (most are hundreds of
/// thousands of tokens, where 8192 is a low single-digit percentage).
const HEADROOM_FRACTION_WARN_DENOM: u32 = 4;

/// Runs every validation step in the documented order, failing on the
/// first hard error and returning accumulated warnings from the last step.
/// The right choice for a config a human might have typed by hand, notably
/// `load_impl`'s own call site (behind [`load`]/
/// [`load_ignoring_user_config`]); [`apply_cli`] calls the identical
/// `validate_impl` -- see that function's own doc for the one check that
/// used to run only here, and no longer exists.
///
/// Note: conway does not validate the *shape* of an API key. Whether a
/// credential is a metered API key, a coding-plan subscription key, or a
/// token for some Anthropic-compatible third-party endpoint is not
/// conway's business to adjudicate -- the provider answers that, and its
/// answer is more accurate than a prefix match here.
pub fn validate(
    config: &ConwayConfig,
    metadata: &ModelMetadata,
    env: &HashMap<String, String>,
) -> Result<Vec<ConfigWarning>> {
    validate_impl(config, metadata, env)
}

fn validate_impl(
    config: &ConwayConfig,
    metadata: &ModelMetadata,
    // Retained in the signature (this function's public wrapper, `validate`,
    // is `pub`) though no current check consults it: the removed key-shape
    // check was its only reader. A future validation that legitimately needs
    // the injected environment has it available without a breaking signature
    // change.
    _env: &HashMap<String, String>,
) -> Result<Vec<ConfigWarning>> {
    // 1. default_role exists in [roles].
    if !config.roles.contains_key(config.default_role.as_str()) {
        let mut known: Vec<&str> = config.roles.keys().map(String::as_str).collect();
        known.sort_unstable();
        return Err(FacadeError::Config {
            path: None,
            message: format!(
                "default_role '{}' is not defined in [roles]; defined roles: [{}]",
                config.default_role,
                known.join(", ")
            ),
        });
    }

    // 2. Every ModelRef in every chain has the form <backend_id>/<model>
    //    and <backend_id> exists in [backends].
    for (name, entry) in &config.roles {
        for raw in &entry.chain {
            let model_ref: ModelRef = raw.parse().map_err(|_| FacadeError::Config {
                path: None,
                message: format!(
                    "role '{name}': chain entry '{raw}' is not a valid 'backend/model' reference"
                ),
            })?;
            if !config.backends.contains_key(model_ref.backend.as_str()) {
                return Err(FacadeError::Config {
                    path: None,
                    message: format!(
                        "role '{name}': chain entry '{raw}' names unknown backend '{}'",
                        model_ref.backend
                    ),
                });
            }
        }
    }

    // 3. fsync = "interval" requires fsync_interval_ms > 0.
    if matches!(
        config.session.fsync,
        crate::config::schema::FsyncMode::Interval
    ) && config.session.fsync_interval_ms == 0
    {
        return Err(FacadeError::Config {
            path: None,
            message: "session.fsync = \"interval\" requires fsync_interval_ms > 0".to_string(),
        });
    }

    // 4. api_key and api_key_env are not both non-empty for the same
    //    backend.
    for (id, backend) in &config.backends {
        if !backend.api_key.is_empty() && !backend.api_key_env.is_empty() {
            return Err(FacadeError::Config {
                path: None,
                message: format!(
                    "backend '{id}': api_key and api_key_env are mutually exclusive but both are set"
                ),
            });
        }
    }

    // 5. Hard error: headroom values must be > 0 (global, every present
    //    per-role override, and -- board item
    //    `01M2TVEWVMPP69TZ17XSGWEW82` -- every present per-model
    //    override). A zero at any level means "reserve nothing for the
    //    model's own output", which is not a setting anyone wants: it
    //    trades a safe pre-flight rejection for a mid-generation overflow
    //    after the prompt tokens are already paid for.
    if config.routing.default_headroom_tokens == 0 {
        return Err(FacadeError::Config {
            path: None,
            message: "routing.default_headroom_tokens must be greater than 0".to_string(),
        });
    }
    let mut role_names: Vec<&String> = config.roles.keys().collect();
    role_names.sort_unstable();
    for name in &role_names {
        if config.roles[*name].headroom_tokens == Some(0) {
            return Err(FacadeError::Config {
                path: None,
                message: format!("roles.{name}.headroom_tokens must be greater than 0"),
            });
        }
    }
    for (model_ref, entry) in &config.routing.models {
        if entry.headroom_tokens == Some(0) {
            return Err(FacadeError::Config {
                path: None,
                message: format!(
                    "routing.models.\"{model_ref}\".headroom_tokens must be greater than 0"
                ),
            });
        }
    }

    // 6. Warning only: headroom >= smallest reachable model context, plus
    //    (board item `01M1AVZPTRSWVE33G4DTJY7Q1B`) a milder warning when
    //    headroom does not literally exceed that window but still consumes
    //    an unreasonably large FRACTION of it -- see
    //    `WarningCode::HeadroomConsumesLargeFractionOfContext`'s own doc
    //    for why this is a second, deliberately non-clamping check rather
    //    than a silent adjustment: shrinking an operator's own configured
    //    headroom trades a safe, pre-flight rejection for the strictly
    //    worse failure of a mid-generation overflow after tokens are
    //    already paid for, so this project surfaces the number instead of
    //    touching it.
    //    **Rewritten per CHAIN ENTRY, board item
    //    `01M2TVEWVMPP69TZ17XSGWEW82`.** This check used to scan a role's
    //    chain for the smallest window IT COULD FIND IN `models.json`,
    //    compare the role-wide headroom against that one window, and move
    //    on. Two things followed from that, and both were silent:
    //
    //      - A chain entry with no `models.json` record was skipped
    //        entirely. It could not trigger a warning however badly sized
    //        the reservation was for it, because it was never looked at.
    //        That is precisely the configuration that motivated this item.
    //      - Even for entries that WERE in metadata, only the smallest was
    //        ever checked. With headroom now resolved per candidate, a
    //        per-model override on a larger sibling can be wrong on its own
    //        terms while the smallest entry is fine.
    //
    //    So: every chain entry of every role, each against its OWN window
    //    and its OWN resolved headroom (`ConwayConfig::headroom_for_model`
    //    -- the same ladder the router applies at route time), and an entry
    //    metadata knows nothing about gets said out loud instead of
    //    skipped.
    let mut warnings = Vec::new();
    if !metadata.models.is_empty() {
        let mut seen = BTreeSet::new();
        for name in &role_names {
            let entry = &config.roles[*name];
            let role_alias = RoleAlias::new((*name).clone());

            for raw in &entry.chain {
                let Some(model_meta) = metadata.models.get(raw.as_str()) else {
                    // No metadata record for this chain entry. conway
                    // cannot size headroom for it, cannot check that
                    // headroom fits, and -- before this item -- said
                    // nothing at all, which is how a 32k model ended up
                    // behind a 100,000-token reservation derived from a 1M
                    // sibling with no diagnostic anywhere.
                    let headroom = config.headroom_for_model(&role_alias, raw, None);
                    let message = format!(
                        "role '{name}': chain entry '{raw}' has no entry in model metadata \
                         (models.json), so conway cannot size its headroom against its real \
                         context window or check that the reservation fits; it falls back to \
                         {headroom} tokens. If that model's window is small, set \
                         routing.models.\"{raw}\".headroom_tokens explicitly, or add the model \
                         to models.json so the adaptive fraction can size it"
                    );
                    if seen.insert(message.clone()) {
                        warnings.push(ConfigWarning {
                            code: WarningCode::ChainEntryContextWindowUnknown,
                            message,
                        });
                    }
                    continue;
                };

                let max_context = model_meta.max_context_tokens;
                let headroom = config.headroom_for_model(&role_alias, raw, Some(max_context));
                // Which knob the reader should edit, most specific first --
                // the same ladder that produced `headroom`, so the message
                // never points at a setting that is not the one in effect.
                let subject = if config
                    .routing
                    .models
                    .get(raw.as_str())
                    .and_then(|m| m.headroom_tokens)
                    .is_some()
                {
                    format!("headroom for model '{raw}'")
                } else if entry.headroom_tokens.is_some() {
                    format!("headroom for role '{name}'")
                } else if config.routing.headroom_fraction.is_some_and(|f| f > 0) {
                    format!("conway's adaptive headroom for role '{name}'")
                } else {
                    "routing.default_headroom_tokens".to_string()
                };

                if headroom >= max_context {
                    let message = format!(
                        "{subject} is {headroom} tokens, which is not less than the context \
                         window of chain entry {model_ref} ({max_context} tokens); every \
                         request routed to that model will be rejected by the context-window \
                         gate",
                        model_ref = raw
                    );
                    if seen.insert(message.clone()) {
                        warnings.push(ConfigWarning {
                            code: WarningCode::HeadroomExceedsContext,
                            message,
                        });
                    }
                } else if headroom.saturating_mul(HEADROOM_FRACTION_WARN_DENOM) >= max_context {
                    // Integer percentage, rounded down; `max_context` is
                    // never 0 on this branch (a 0-token window would already
                    // have tripped the `>=` check above for any headroom
                    // `>= 1`, and every headroom level is validated `> 0` by
                    // check 5, just above).
                    let percent = u64::from(headroom) * 100 / u64::from(max_context);
                    // Names the ROLE as well as the knob. `subject` alone
                    // says which setting to edit, which is right, but when
                    // the global default governs it is
                    // `routing.default_headroom_tokens` -- and this message's
                    // own "its chain" / "for this role" then refer to nothing
                    // the reader can identify. With `seen` deduplicating on
                    // message text, one warning also has to stand for
                    // whichever role actually tripped it.
                    let message = format!(
                        "{subject} is {headroom} tokens, {percent}% of the context \
                         window reachable from role '{name}' ({model_ref} = {max_context} \
                         tokens); a long-running conversation to that model can hit the \
                         context-window gate well before it would with a smaller reservation \
                         -- consider a smaller headroom_tokens for that role, or a \
                         larger-window fallback later in its chain",
                        model_ref = raw
                    );
                    if seen.insert(message.clone()) {
                        warnings.push(ConfigWarning {
                            code: WarningCode::HeadroomConsumesLargeFractionOfContext,
                            message,
                        });
                    }
                }
            }
        }
    }

    // 7. Hard error: every id in tools.builtin_plugins names a real built-in.
    //
    // A hard error rather than a warning, for the same reason check 1 hard-
    // errors on an undefined default_role: the candidate set is closed and
    // known at compile time, so an unrecognized id is unambiguously a typo,
    // never a forward reference. And the failure it prevents is the one
    // This ranks WORST -- user-facing configuration that silently does
    // nothing. `builtin_plugins` is how an operator turns `bash` back on;
    // `"conway.shel"` would leave it off with no signal, and the operator
    // would believe they had enabled it. Silence there is indistinguishable
    // from success, which is precisely the shape this project keeps having
    // to walk back.
    //
    // Note this list is NOT the extension point: a third-party plugin is
    // installed with `ConwayBuilder::with_plugin` and is never filtered by
    // this selection, so naming one here is also a mistake worth catching.
    //
    // Gated on `builtin-tools` alongside the candidate source it validates
    // against: without that feature there are no built-ins to name, and a
    // check with nothing to check against would either reject every id or
    // accept every id -- both worse than not running.
    #[cfg(feature = "builtin-tools")]
    {
        let known = crate::presets::builtin_plugin_ids();
        let mut unknown: Vec<&str> = config
            .tools
            .builtin_plugins
            .iter()
            .map(String::as_str)
            .filter(|id| !known.iter().any(|k| k == id))
            .collect();
        if !unknown.is_empty() {
            unknown.sort_unstable();
            unknown.dedup();
            let mut known_sorted = known.clone();
            known_sorted.sort();
            return Err(FacadeError::Config {
                path: None,
                message: format!(
                    "tools.builtin_plugins names unknown built-in plugin(s): [{}]; known \
                     built-ins: [{}]. A third-party plugin is installed with \
                     ConwayBuilder::with_plugin and is not listed here.",
                    unknown.join(", "),
                    known_sorted.join(", ")
                ),
            });
        }
    }

    // 8. Every [hooks].rules[] entry has a non-empty `id`, and every `id` is
    //    unique across the file. Enforced here (a semantic check on the
    //    parsed value), not by serde, matching how every other "required in
    //    practice" invariant in this function is enforced -- see check 3's
    //    own precedent (`fsync = "interval"` requiring
    //    `fsync_interval_ms > 0`). `id` is load-bearing for the later
    //    operator-visibility item that lists hook rules individually and
    //    revokes one by name (`schema::HookEntry::id`'s own doc comment);
    //    an empty or duplicate id there would make that lookup ambiguous or
    //    silently target the wrong rule.
    //
    // Note: `[hooks]` itself only parses and validates today -- see
    // `schema::HooksConfig`'s own per-event reachability disclosure. This check runs
    // regardless of whether any rule is ever dispatched, exactly like every
    // other structural check in this function runs on config that may
    // never be exercised at runtime.
    {
        let mut seen_ids: BTreeSet<&str> = BTreeSet::new();
        for rule in &config.hooks.rules {
            if rule.id.is_empty() {
                return Err(FacadeError::Config {
                    path: None,
                    message: "hooks.rules[]: every rule must have a non-empty \"id\"".to_string(),
                });
            }
            if !seen_ids.insert(rule.id.as_str()) {
                return Err(FacadeError::Config {
                    path: None,
                    message: format!(
                        "hooks.rules[]: duplicate id '{}' -- every rule's id must be unique",
                        rule.id
                    ),
                });
            }
        }
    }

    // 9. A rule's `match` only
    //     applies to an `event` whose payload actually names a tool --
    //     `"pre_tool_use"`/`"post_tool_use"`. `EVENTS_WITHOUT_TOOL_NAME`
    //     names every OTHER event `conway-runtime` dispatches on this
    //     item's own list (`session_starting`, `child_spawned`,
    //     `request_assembled`, `child_reported`, `prompt_submitted`);
    //     `"pre_tool_use"` itself is added here rather than to that shared
    //     list because `conway-runtime`'s `hook_dispatch` module does not
    //     dispatch it (`crate::permission::PermissionBroker` does) -- see
    //     `EVENTS_WITHOUT_TOOL_NAME`'s own doc.
    //
    //     This is a SURFACED, TYPED error naming the rule's `id` -- per this
    //     item's own ACCEPTANCE, "an error, not silence": a `match` an
    //     operator wrote in good faith that can never fire (because the
    //     event it is paired with never carries a tool name) must not
    //     silently parse into a rule that quietly does nothing extra,
    //     exactly the class of defect check 9's own comment names.
    {
        const PRE_TOOL_USE: &str = "pre_tool_use";
        for rule in &config.hooks.rules {
            if rule.match_tool.is_none() {
                continue;
            }
            if rule.event == PRE_TOOL_USE
                || rule.event == conway_runtime::hook_dispatch::POST_TOOL_USE
            {
                continue;
            }
            if EVENTS_WITHOUT_TOOL_NAME.contains(&rule.event.as_str()) {
                return Err(FacadeError::Config {
                    path: None,
                    message: format!(
                        "hooks.rules[]: rule '{}' sets \"match\" on event \"{}\", which carries \
                         no tool name -- \"match\" only applies to \"pre_tool_use\"/\
                         \"post_tool_use\"",
                        rule.id, rule.event
                    ),
                });
            }
        }
    }

    // 10. Every [hooks].rules[] `event` is a WELL-FORMED name -- bare
    //     (core-shaped) or `plugin_id.event_name` -- per
    //     `conway_core::event_name::validate_event_name`'s subscriber-side
    //     rule (`declaring_plugin: None`).
    //     this closes the FOLLOW-UP `schema::HookEntry::event`'s own doc
    //     comment used to name ("a sibling is deciding that
    //     rule" -- this is that item).
    //
    //     This checks SHAPE only, never membership: whether a well-formed
    //     namespaced `event` names an event some INSTALLED plugin actually
    //     declares needs the resolved plugin set, which this function has
    //     no access to -- that check is `ConwayBuilder::build`'s own (see
    //     `schema::HooksConfig`'s reachability doc for the tolerant
    //     "unknown -- never dispatched, not an error" rule that applies
    //     there, identical to a typo'd core event name today).
    {
        for rule in &config.hooks.rules {
            if let Err(reason) = validate_event_name(&rule.event, None) {
                return Err(FacadeError::Config {
                    path: None,
                    message: format!("hooks.rules[]: rule '{}': {reason}", rule.id),
                });
            }
        }
    }

    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `LoadOptions` with an isolated `CONWAY_CONFIG_DIR` and nothing
    /// else in `env` -- so a test never reads (or is steered by) the
    /// developer's own real user-scope settings.
    fn isolated_options(cwd: &std::path::Path, config_dir: &std::path::Path) -> LoadOptions {
        let mut env = HashMap::new();
        env.insert(
            "CONWAY_CONFIG_DIR".to_string(),
            config_dir.display().to_string(),
        );
        LoadOptions {
            cwd: cwd.to_path_buf(),
            explicit_path: None,
            env,
            cli_overrides: CliOverrides::default(),
            model_metadata_refresh: false,
        }
    }

    /// Board item `01M2V6JHRWWEPN690C954R0PYS`, the regression this
    /// function exists for: with NO settings file anywhere, `cwd` comes
    /// out of the merge as `schema::default_cwd`'s literal relative `"."`
    /// -- the exact value the shipped binary used to hand every downstream
    /// consumer. `load` must replace it with the invocation directory
    /// before anyone downstream does path arithmetic on it.
    #[test]
    fn a_defaulted_relative_cwd_is_resolved_against_the_invocation_directory() {
        let project = tempfile::tempdir().expect("tempdir");
        let config_dir = tempfile::tempdir().expect("tempdir");
        let nested = project.path().join("pkg").join("sub");
        std::fs::create_dir_all(&nested).expect("mkdir nested");

        let outcome = load_ignoring_user_config(isolated_options(&nested, config_dir.path()))
            .expect("an unconfigured project must load");

        assert_eq!(
            outcome.config.cwd, nested,
            "the defaulted `.` must be resolved to the directory conway was actually launched \
             from, not left as a literal `.` for every consumer to mis-resolve"
        );
    }

    /// The same resolution for an EXPLICIT relative `"cwd": "."` written
    /// into a settings file -- the literal spelling board item
    /// `01M2V6JHRWWEPN690C954R0PYS` proved at the wire, exercised through
    /// the real five-source merge rather than only
    /// through the schema default above. An operator's relative value means
    /// "relative to where conway was launched", which is the only reading
    /// `std::fs` would have given it anyway.
    #[test]
    fn an_explicit_relative_dot_cwd_in_a_settings_file_is_resolved_the_same_way() {
        let project = tempfile::tempdir().expect("tempdir");
        let config_dir = tempfile::tempdir().expect("tempdir");
        let dot_conway = project.path().join(".conway");
        std::fs::create_dir_all(&dot_conway).expect("mkdir .conway");
        std::fs::write(dot_conway.join("settings.json"), r#"{ "cwd": "." }"#)
            .expect("write settings.json");
        let nested = project.path().join("pkg").join("sub");
        std::fs::create_dir_all(&nested).expect("mkdir nested");

        let outcome = load_ignoring_user_config(isolated_options(&nested, config_dir.path()))
            .expect("an explicit relative cwd must load");

        assert_eq!(
            outcome.config.cwd, nested,
            "an explicitly written relative `.` must resolve against the invocation directory"
        );
    }

    /// A relative value that is not `.` resolves the same way, and is
    /// lexically normalized on the way through (`..` collapsed without
    /// touching the filesystem) so downstream consumers see one clean
    /// spelling.
    #[test]
    fn a_relative_cwd_with_parent_components_is_joined_and_lexically_normalized() {
        let base = std::path::Path::new("/tmp/conway-invocation/pkg/sub");
        assert_eq!(
            resolve_cwd(std::path::Path::new("../other"), base),
            std::path::PathBuf::from("/tmp/conway-invocation/pkg/other")
        );
        assert_eq!(
            resolve_cwd(std::path::Path::new("."), base),
            std::path::PathBuf::from("/tmp/conway-invocation/pkg/sub")
        );
    }

    /// An operator who writes an ABSOLUTE `cwd` gets exactly that path
    /// back, byte for byte -- this resolution only fills in a relative
    /// value, it never re-interprets an explicit one (the same contract
    /// [`resolve_metadata_path`] already has).
    #[test]
    fn an_absolute_configured_cwd_is_left_exactly_as_written() {
        let elsewhere = tempfile::tempdir().expect("tempdir");
        let project = tempfile::tempdir().expect("tempdir");
        let config_dir = tempfile::tempdir().expect("tempdir");
        let dot_conway = project.path().join(".conway");
        std::fs::create_dir_all(&dot_conway).expect("mkdir .conway");
        std::fs::write(
            dot_conway.join("settings.json"),
            serde_json::to_string(&serde_json::json!({
                "cwd": elsewhere.path().display().to_string(),
            }))
            .expect("serialize settings"),
        )
        .expect("write settings.json");

        let outcome =
            load_ignoring_user_config(isolated_options(project.path(), config_dir.path()))
                .expect("an absolute cwd must load");

        assert_eq!(outcome.config.cwd, elsewhere.path());
    }
}
