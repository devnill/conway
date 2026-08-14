//! `RoutingRouterFactory`: this crate's [`conway_core::ports::RouterFactory`]
//! -- names [`ROUTER_ID`] up front so
//! `[plugins].install` (or a direct `ConwayBuilder::with_router_factory`
//! call) can select this crate before backends exist, then builds the same
//! `DeclarativeRouter` + `BreakerRegistry` pairing `conway`'s own
//! `builder.rs` used to compile in unconditionally before this crate became
//! an installable plugin. Absent this factory (no `[plugins].install` entry
//! naming [`ROUTER_ID`]), `ConwayBuilder::build` falls through to
//! `conway_core::routing::MinimalRouter` -- the honest, config-only core
//! resolver -- never to this crate, which `conway` no longer links at all.

use std::sync::Arc;

use conway_core::error::ConwayError;
use conway_core::ports::{
    HealthRegistry, Router, RouterBuildContext, RouterBundle, RouterFactory, RoutingExplainer,
};

use crate::breaker::BreakerRegistry;
use crate::router::DeclarativeRouter;

/// This crate's published router id -- the name `[plugins].install` (or a
/// direct `ConwayBuilder::with_router_factory` call) uses to select it.
/// Stable across every `Router` this factory constructs (`RouterFactory::
/// id`'s own contract): a KIND's identity, not a configured instance's.
pub const ROUTER_ID: &str = "conway.routing";

/// This crate's [`RouterFactory`]. Zero-sized -- every input it needs
/// arrives through [`RouterBuildContext`] at `build()` time, nothing is
/// carried on `self`.
#[derive(Debug, Default, Clone, Copy)]
pub struct RoutingRouterFactory;

impl RouterFactory for RoutingRouterFactory {
    fn id(&self) -> &str {
        ROUTER_ID
    }

    /// Builds a [`DeclarativeRouter`] over `ctx.capability_index` -- the
    /// SAME index `ConwayBuilder::build` itself computed from
    /// `.conway/models.json` (optionally overlaid by the startup capability
    /// probe), never an independently re-derived one (see
    /// `RouterBuildContext::capability_index`'s own doc for why a factory
    /// cannot correctly reconstruct that set from `ctx.backends` alone) --
    /// plus a fresh [`BreakerRegistry`] seeded from `ctx.routing.health`.
    /// This is exactly what `conway`'s own `builder.rs` used to assemble
    /// directly (step 5-7 of `ConwayBuilder::build`, before this crate
    /// existed as a plugin) -- relocated here, not reimplemented.
    fn build(&self, ctx: RouterBuildContext<'_>) -> Result<RouterBundle, ConwayError> {
        let health = BreakerRegistry::new(ctx.routing.health);

        let router = Arc::new(
            DeclarativeRouter::new(
                ctx.routing,
                ctx.headroom,
                health.clone() as Arc<dyn HealthRegistry>,
                ctx.capability_index,
            )
            .map_err(|issues| ConwayError::Config {
                detail: format!("routing config invalid: {issues:?}"),
            })?,
        );

        Ok(RouterBundle {
            router: router.clone() as Arc<dyn Router>,
            health: health as Arc<dyn HealthRegistry>,
            explain: Some(router as Arc<dyn RoutingExplainer>),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway_core::capabilities::{
        CacheMode, Capabilities, HeadroomPolicy, ReliabilityTier, StructuredOutput, ToolCallSupport,
    };
    use conway_core::error::BackendError;
    use conway_core::ids::{AgentId, BackendId, ModelId, ModelRef, RoleAlias};
    use conway_core::ports::{
        Backend, BoxStream, CapabilityIndex, GenerateRequest, GenerateResponse, StreamChunk,
    };
    use conway_core::routing::{
        HealthConfig, RoleConfig, RouteRequest, RoutingConfig, RoutingReason,
    };
    use std::collections::BTreeMap;

    fn routing_config(roles: BTreeMap<String, RoleConfig>) -> RoutingConfig {
        RoutingConfig {
            roles,
            health: HealthConfig::default(),
            default_headroom_tokens: HeadroomPolicy::default().default_headroom_tokens,
        }
    }

    fn caps() -> Capabilities {
        Capabilities {
            tool_calling: ToolCallSupport::Streaming { validated: true },
            cache: CacheMode::None,
            parallel_tool_calls: true,
            structured_output: StructuredOutput::Grammar,
            max_context_tokens: 100_000,
            reasoning: true,
            reliability_tier: ReliabilityTier::Verified,
        }
    }

    struct StubBackend {
        id: BackendId,
    }

    #[async_trait::async_trait]
    impl Backend for StubBackend {
        fn id(&self) -> BackendId {
            self.id.clone()
        }
        fn capabilities(&self, _model: &ModelId) -> Capabilities {
            caps()
        }
        async fn generate(&self, _req: GenerateRequest) -> Result<GenerateResponse, BackendError> {
            unimplemented!("not exercised by this test")
        }
        async fn stream(
            &self,
            _req: GenerateRequest,
        ) -> Result<BoxStream<'static, Result<StreamChunk, BackendError>>, BackendError> {
            unimplemented!("not exercised by this test")
        }
        async fn probe(&self) -> Result<conway_core::capabilities::ProbeReport, BackendError> {
            unimplemented!("not exercised by this test")
        }
    }

    #[test]
    fn id_is_the_published_router_id() {
        assert_eq!(RoutingRouterFactory.id(), ROUTER_ID);
    }

    /// End-to-end proof that `build()` produces a real, working
    /// `DeclarativeRouter`: capability filtering, a shared `BreakerRegistry`,
    /// and a matching `RoutingExplainer` all come back live, not stubbed.
    #[test]
    fn build_produces_a_working_capability_filtered_router() {
        let backend: Arc<dyn Backend> = Arc::new(StubBackend {
            id: BackendId::new("local"),
        });
        let mut roles = BTreeMap::new();
        roles.insert(
            "coder".to_string(),
            RoleConfig {
                chain: vec![ModelRef {
                    backend: BackendId::new("local"),
                    model: ModelId::new("m1"),
                }],
                ..Default::default()
            },
        );
        let routing = routing_config(roles);
        let capability_index = CapabilityIndex::builder()
            .insert(BackendId::new("local"), ModelId::new("m1"), caps())
            .build();
        let ctx = RouterBuildContext {
            routing: routing.clone(),
            headroom: HeadroomPolicy::from_routing_config(&routing),
            backends: std::slice::from_ref(&backend),
            capability_index,
        };

        let bundle = RoutingRouterFactory
            .build(ctx)
            .expect("valid config builds");
        let req = RouteRequest {
            role: RoleAlias::new("coder"),
            pin: None,
            required: Default::default(),
            est_tokens: 10,
            agent_id: AgentId::new(),
        };
        let routes = bundle
            .router
            .resolve(&req)
            .expect("configured role resolves");
        assert_eq!(routes.len(), 1);
        assert!(matches!(
            routes[0].reason,
            RoutingReason::AliasPrimary { .. }
        ));
        assert!(bundle.explain.is_some(), "explain must be wired");
    }

    /// An invalid config (an empty chain) surfaces as `ConwayError::Config`,
    /// not a panic and not a silent empty router.
    #[test]
    fn build_surfaces_an_invalid_config_as_a_typed_error() {
        let mut roles = BTreeMap::new();
        roles.insert("coder".to_string(), RoleConfig::default());
        let routing = routing_config(roles);
        let ctx = RouterBuildContext {
            routing: routing.clone(),
            headroom: HeadroomPolicy::from_routing_config(&routing),
            backends: &[],
            capability_index: CapabilityIndex::builder().build(),
        };

        let err = RoutingRouterFactory
            .build(ctx)
            .expect_err("an empty chain must be rejected");
        assert!(matches!(err, ConwayError::Config { .. }));
    }
}
