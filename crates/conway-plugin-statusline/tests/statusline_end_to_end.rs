//! This crate's own real-facade proof, written the way a library embedder
//! would write it -- `ConwayBuilder`, `ScriptedBackend`/`FakeStore` (the
//! credential-free fakes family, no network), and
//! `conway_plugin_statusline::StatusLinePlugin` attached the same way any
//! third-party plugin would be, via `ConwayBuilder::with_plugin` -- the
//! identical shape `conway-plugin-skeleton`'s own
//! `tests/skeleton_end_to_end.rs` established.
//!
//! **This file is also acceptance criterion 8's evidence, not only
//! acceptance criterion 1's.** `positive_a_fast_commands_output_reaches_the_
//! real_facade_snapshot` proves the push path CAN reach
//! `Conway::plugin_status_contributions()` -- when the plugin's own
//! background loop wins the race against `ConwayBuilder::build`'s
//! synchronous, one-time read of it.
//! `negative_the_hosts_one_shot_snapshot_never_sees_a_refresh_that_completes_
//! after_build` proves the other half of that same finding: the exact same
//! plugin, with the exact same command, produces NOTHING in the facade's
//! snapshot when `build()` is called before the first refresh completes --
//! and, critically, the plugin's OWN `status_contributions()` (read
//! directly, bypassing the facade's frozen copy) shows the value arriving
//! moments later, proving the gap is the HOST's one-shot read, not this
//! plugin failing to produce anything. See this crate's own `src/lib.rs`
//! module doc, "Why this crate is worth more than the migration", for the
//! full §7c argument these two tests are the executable half of.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use conway::config::schema::{
    AgentsConfig, ConwayConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
    PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig, ToolsConfig,
};
use conway::plugin::Plugin as _;
use conway::test_support::test_builder;
use conway_core::ids::{BackendId, RoleAlias};
use conway_testkit::{text_response, ScriptedBackend, ScriptedTurn};

use conway_plugin_statusline::{
    StatusLinePlugin, StatusLineSpec, DEFAULT_KEY, MIN_REFRESH_INTERVAL_MS,
};

/// Mirrors `conway-plugin-skeleton`'s own `base_config` helper exactly --
/// see that crate's `tests/skeleton_end_to_end.rs` for why every field is
/// spelled out rather than deriving `Default` (this facade config has no
/// blanket `Default`, by design: an embedder must make every choice, not
/// inherit an implicit one -- see `ConwayConfig`'s own doc).
fn base_config() -> ConwayConfig {
    let mut roles = BTreeMap::new();
    roles.insert(
        "default".to_string(),
        RoleEntry {
            chain: vec![],
            headroom_tokens: None,
            ..Default::default()
        },
    );
    ConwayConfig {
        default_role: RoleAlias::new("default"),
        cwd: std::path::PathBuf::from("."),
        session: SessionConfig::default(),
        limits: LimitsConfig::default(),
        permissions: PermissionsConfig::default(),
        backends: BTreeMap::new(),
        routing: RoutingSection::default(),
        roles,
        health: HealthSection::default(),
        agents: AgentsConfig::default(),
        models: ModelsConfig::default(),
        tools: ToolsConfig::default(),
        // `[plugins].install`/`.statusline` are read by whatever
        // BINARY links this crate (`conway-cli`'s own
        // `first_party_plugins.rs`) -- a library embedder instead attaches
        // the plugin directly via `with_plugin`, which is what this test
        // does. Left at its default here on purpose, mirroring
        // `conway-plugin-skeleton`'s own identical note.
        plugins: PluginsConfig::default(),
        hooks: HooksConfig::default(),
    }
}

fn fake_backend() -> Arc<ScriptedBackend> {
    Arc::new(
        ScriptedBackend::new(vec![ScriptedTurn::Respond(text_response("hi"))])
            .with_id(BackendId::new("fake")),
    )
}

/// The manifest id matches the published constant -- the same "wiring
/// anchor" check `conway-plugin-skeleton`'s own test suite makes for its
/// own id.
#[test]
fn manifest_id_matches_the_published_constant() {
    let plugin = StatusLinePlugin::new(StatusLineSpec::default());
    assert_eq!(plugin.manifest().id, conway_plugin_statusline::PLUGIN_ID);
}

/// **Acceptance criterion 1's real-facade proof, and acceptance criterion
/// 8's POSITIVE half.** A fast, near-instant command's output reaches
/// `Conway::plugin_status_contributions()` -- the facade's own build-time
/// snapshot, `ConwayBuilder::build`'s exact collection code path -- when
/// this plugin's background loop is given enough of a head start to win
/// the race. The sleep between constructing the plugin and calling
/// `.build()` stands in for whatever real work `ConwayBuilder::build` does
/// between attaching a plugin and reading its `status_contributions()` in
/// the real CLI binary (config validation, capability-index construction,
/// ...) -- see this crate's own `src/lib.rs` module doc for why that gap's
/// actual size is what decides whether a real session ever observes this.
#[tokio::test]
async fn positive_a_fast_commands_output_reaches_the_real_facade_snapshot() {
    let spec = StatusLineSpec {
        command: vec!["echo".to_string(), "hello-from-statusline".to_string()],
        refresh_interval_ms: MIN_REFRESH_INTERVAL_MS,
        ..StatusLineSpec::default()
    };
    let plugin = Arc::new(StatusLinePlugin::new(spec));

    // Head start: poll the plugin's OWN state (not the facade, which does
    // not exist yet) until its first run has actually finished, rather
    // than a fixed sleep -- robust against a slow CI box instead of
    // gambling that `echo` beats an arbitrary fixed delay. `build()` is
    // called only once this condition holds, so the race described in
    // this test's own doc is deliberately won, not merely likely.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while plugin.status_contributions().is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the plugin's background loop never produced a result within 5s"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let conway = test_builder(base_config())
        .with_backend(fake_backend())
        .with_plugin(plugin)
        .build()
        .expect("build should succeed with every port injected");

    let contributions = conway.plugin_status_contributions();
    assert_eq!(
        contributions.len(),
        1,
        "the facade's own build-time snapshot must carry this plugin's contribution once its \
         background loop has actually produced one: {contributions:?}"
    );
    assert_eq!(contributions[0].key, DEFAULT_KEY);
    assert_eq!(contributions[0].value, "hello-from-statusline");
}

/// **Acceptance criterion 8's NEGATIVE half -- the concrete finding for
/// `DESIGN-plugin-dependencies.md` §7c.** The identical plugin and command
/// as the positive test above, but `build()` is called IMMEDIATELY, with
/// no head start at all. `Conway::plugin_status_contributions()` comes
/// back empty -- not because this plugin failed to produce anything (the
/// second half of this test proves it did, moments later, by reading the
/// PLUGIN directly, bypassing the facade's frozen copy entirely), but
/// because the host reads the snapshot exactly once, before the race was
/// ever winnable. This is what `conway::Conway::plugin_status_contributions`'s
/// own doc means by "a build-time snapshot... frozen thereafter for the
/// life of the process": this test is that sentence, made executable.
#[tokio::test]
async fn negative_the_hosts_one_shot_snapshot_never_sees_a_refresh_that_completes_after_build() {
    let spec = StatusLineSpec {
        command: vec!["echo".to_string(), "hello-from-statusline".to_string()],
        refresh_interval_ms: MIN_REFRESH_INTERVAL_MS,
        ..StatusLineSpec::default()
    };
    let plugin = Arc::new(StatusLinePlugin::new(spec));

    // No head start at all -- `build()` runs before the background loop's
    // first `echo` has any realistic chance to have completed.
    let conway = test_builder(base_config())
        .with_backend(fake_backend())
        .with_plugin(plugin.clone())
        .build()
        .expect("build should succeed with every port injected");

    assert!(
        conway.plugin_status_contributions().is_empty(),
        "the facade's frozen snapshot must be empty when build() races ahead of the plugin's \
         own first refresh -- if this ever starts passing, the host has grown the live poll \
         `DESIGN-plugin-dependencies.md` §7c calls for, and this test (and this crate's own \
         §7c finding) should be revisited, not just deleted"
    );

    // The plugin itself is NOT broken -- reading it directly (not through
    // the frozen facade copy) shows the value arrives shortly after,
    // proving the gap above is the host's one-shot read, not this
    // plugin's own mechanism.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let live = plugin.status_contributions();
        if !live.is_empty() {
            assert_eq!(live[0].value, "hello-from-statusline");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the plugin's own background loop never produced a result within 5s -- that WOULD \
             be a defect in this crate, unlike the empty facade snapshot above"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
