//! What THIS host offers to plugins -- the host-side counterpart to
//! [`conway_core::ports::plugin::HostCapability`]'s plugin-declared
//! requirements. Constructed at BUILD time from the configuration (see
//! [`HostCaps::from_config`]) and consulted once per installed plugin at the
//! registration seam in [`crate::ConwayBuilder::build`]: any cap a plugin
//! declares in `PluginManifest::required_host_caps` that the host does NOT
//! offer is a [`conway_core::error::PluginError::MissingHostCapability`] and
//! the plugin is refused -- the NARROWING direction (a plugin declares what
//! it needs; the host refuses to load it if the host can't provide it).
//!
//! **Not a free-form registry.** The cap set is the closed
//! `#[non_exhaustive]` [`HostCapability`] enum in `conway-core`; this type
//! holds a `HashSet<HostCapability>` derived from the config, so a cap the
//! host "offers" is always one the vocabulary knows, not a string the host
//! never validates.

use std::collections::HashSet;

use conway_core::error::PluginError;
use conway_core::ports::{HostCapability, PluginManifest};

/// What THIS host offers: a set of [`HostCapability`] values derived at build
/// time from the configuration, not a free-form registry. A plugin whose
/// `required_host_caps` names a cap absent here is refused at registration
/// with [`PluginError::MissingHostCapability`] -- see [`HostCaps::check_manifest`].
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct HostCaps {
    caps: HashSet<HostCapability>,
}

impl HostCaps {
    /// A host that offers nothing -- every cap a plugin declares will be
    /// reported missing. The test-friendly base case: a test builds this and
    /// inserts (or doesn't) exactly the caps it wants the host to offer,
    /// without standing up the full builder config.
    pub fn empty() -> Self {
        Self::default()
    }

    /// A host offering exactly the given caps. Test-friendly constructor
    /// (the [`HostCaps::from_config`] path is what production builds use).
    pub fn with_capabilities<I>(caps: I) -> Self
    where
        I: IntoIterator<Item = HostCapability>,
    {
        Self {
            caps: caps.into_iter().collect(),
        }
    }

    /// Adds a cap the host offers; returns `&mut Self` for chaining. Used by
    /// tests to build a host that offers some caps but LACKS others, without
    /// standing up the full builder config.
    pub fn offer(&mut self, cap: HostCapability) -> &mut Self {
        self.caps.insert(cap);
        self
    }

    /// Whether this host offers `cap`.
    pub fn offers(&self, cap: HostCapability) -> bool {
        self.caps.contains(&cap)
    }

    /// Derives the host's offered caps from the configuration -- the
    /// production construction path used by
    /// [`crate::ConwayBuilder::build`]. Each cap is derived from something
    /// real in the config/builder, not hardcoded:
    ///
    /// - [`HostCapability::Subagent`] -- the `conway` runtime unconditionally
    ///   provides a `SubagentHost` (`impl SubagentHost for Runtime`). There is
    ///   deliberately no `with_subagent_host` injection point, and none is
    ///   coming: fork and spawn are mechanism with exactly one
    ///   implementation, and the runtime that keeps the log is the only thing
    ///   that may fork it (INTENT.md §7 -- *"if it wants them" means
    ///   uncalled, not replaced*). Unlike every other cap in this file, an
    ///   embedder cannot supply its own `SubagentHost`; it can only decline
    ///   to use the one the runtime always provides, so the host always
    ///   offers this cap. This is still the honest derivation, not a
    ///   hardcoded `true`: the cap is offered because the built runtime
    ///   genuinely provides the host, not because this method declares it by
    ///   fiat.
    /// - [`HostCapability::PersistentTransport`] -- offered iff at least one
    ///   `[plugins].subprocess[]` entry is configured with
    ///   `SubprocessTransport::Persistent` (the operator opts IN to
    ///   persistent transport per entry; a host with no persistent entry is
    ///   one-shot-only, and a plugin requiring this cap against it is
    ///   refused).
    pub fn from_config(config: &crate::config::schema::ConwayConfig) -> Self {
        let mut caps = HostCaps::empty();
        // Subagent: always offered -- the runtime provides a SubagentHost
        // unconditionally. No injection point removes it, by design: see
        // this method's own doc and INTENT.md §7.
        caps.offer(HostCapability::Subagent);
        // PersistentTransport: offered iff at least one subprocess entry is
        // configured persistent.
        let any_persistent = config
            .plugins
            .subprocess
            .iter()
            .any(|e| e.transport == crate::config::schema::SubprocessTransport::Persistent);
        if any_persistent {
            caps.offer(HostCapability::PersistentTransport);
        }
        caps
    }

    /// Checks a plugin's declared `required_host_caps` against what this host
    /// offers. On the first cap the host does NOT offer, returns
    /// [`PluginError::MissingHostCapability`] naming both the plugin's
    /// manifest id and the missing cap's wire string. Used at the
    /// registration seam in [`crate::ConwayBuilder::build`]; also the
    /// test-facing entry point (a test builds a [`HostCaps`] directly and
    /// calls this with a fixture manifest, no full build required).
    pub fn check_manifest(&self, manifest: &PluginManifest) -> Result<(), PluginError> {
        for cap in &manifest.required_host_caps {
            if !self.offers(*cap) {
                return Err(PluginError::MissingHostCapability {
                    plugin: manifest.id.clone(),
                    capability: cap.to_string(),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway_core::ids::ToolName;
    use conway_core::ports::PluginManifest;

    /// A minimal manifest with the given required caps -- the fixture
    /// "plugin" for the check (no `Plugin` impl needed: `check_manifest`
    /// reads only the manifest).
    fn manifest(id: &str, caps: &[HostCapability]) -> PluginManifest {
        PluginManifest {
            id: id.to_string(),
            version: "0.0.0".to_string(),
            tools: Vec::<ToolName>::new(),
            required_host_caps: caps.to_vec(),
            requires: vec![],
            optional: vec![],
        }
    }

    // -----------------------------------------------------------------
    // HostCapability wire-string / Display
    // -----------------------------------------------------------------

    #[test]
    fn host_capability_wire_strings_are_snake_case() {
        assert_eq!(HostCapability::Subagent.as_wire_str(), "subagent");
        assert_eq!(
            HostCapability::PersistentTransport.as_wire_str(),
            "persistent_transport"
        );
        assert_eq!(HostCapability::Subagent.to_string(), "subagent");
        assert_eq!(
            HostCapability::PersistentTransport.to_string(),
            "persistent_transport"
        );
    }

    /// `as_wire_str` is a hand-maintained second source of truth for the wire
    /// name alongside `#[serde(rename_all = "snake_case")]` on the enum. This
    /// pins that the two AGREE, so an irregular variant name (an acronym, a
    /// typo in the match arm) cannot silently diverge from what serde
    /// actually puts on the wire -- the failure mode `as_wire_str` would
    /// otherwise have no check against. `serde_json::to_value` produces a
    /// `Value::String` carrying exactly the tag serde emits (no surrounding
    /// quotes), so this is the wire form, not a debug print.
    #[test]
    fn as_wire_str_matches_serde_rename_for_each_variant() {
        for cap in [
            HostCapability::Subagent,
            HostCapability::PersistentTransport,
        ] {
            let wire = serde_json::to_value(cap)
                .expect("HostCapability serializes")
                .as_str()
                .expect("serde emitted a string tag")
                .to_string();
            assert_eq!(
                cap.as_wire_str(),
                wire,
                "as_wire_str disagrees with serde's rename for {cap:?}",
            );
        }
    }

    // -----------------------------------------------------------------
    // Acceptance 1: a plugin whose required_host_caps names a cap the host
    // HAS loads normally.
    // -----------------------------------------------------------------

    #[test]
    fn plugin_requiring_a_cap_the_host_offers_is_accepted() {
        // A host that offers Subagent (the always-offered cap).
        let host = HostCaps::with_capabilities([HostCapability::Subagent]);
        // A fixture plugin requiring exactly that cap.
        let m = manifest("test.needs-subagent", &[HostCapability::Subagent]);
        assert_eq!(host.check_manifest(&m), Ok(()));
    }

    // -----------------------------------------------------------------
    // Acceptance 2: a plugin whose required_host_caps names a cap the host
    // LACKS is refused with MissingHostCapability naming both.
    // -----------------------------------------------------------------

    #[test]
    fn plugin_requiring_a_cap_the_host_lacks_is_refused_naming_both() {
        // A host that offers nothing -- it lacks PersistentTransport.
        let host = HostCaps::empty();
        let m = manifest(
            "test.needs-persistent",
            &[HostCapability::PersistentTransport],
        );
        let err = host
            .check_manifest(&m)
            .expect_err("host lacks PersistentTransport");
        match err {
            PluginError::MissingHostCapability { plugin, capability } => {
                assert_eq!(plugin, "test.needs-persistent", "names the plugin");
                assert_eq!(
                    capability, "persistent_transport",
                    "names the missing cap (snake_case wire string)"
                );
            }
            other => panic!("expected MissingHostCapability, got {other:?}"),
        }
    }

    /// A plugin requiring TWO caps where the host offers one but lacks the
    /// other is refused on the FIRST missing one -- the check does not silently
    /// accept a partially-satisfied requirement.
    #[test]
    fn plugin_requiring_two_caps_with_one_missing_is_refused_on_first_missing() {
        let host = HostCaps::with_capabilities([HostCapability::Subagent]);
        let m = manifest(
            "test.needs-two",
            &[
                HostCapability::Subagent,
                HostCapability::PersistentTransport,
            ],
        );
        let err = host
            .check_manifest(&m)
            .expect_err("host lacks PersistentTransport");
        match err {
            PluginError::MissingHostCapability { plugin, capability } => {
                assert_eq!(plugin, "test.needs-two");
                assert_eq!(capability, "persistent_transport");
            }
            other => panic!("expected MissingHostCapability, got {other:?}"),
        }
    }

    /// Empty required_host_caps (the common case -- "needs nothing the host
    /// might lack") is always accepted, even by a host that offers nothing.
    #[test]
    fn empty_required_host_caps_is_always_accepted() {
        let host = HostCaps::empty();
        let m = manifest("test.needs-nothing", &[]);
        assert_eq!(host.check_manifest(&m), Ok(()));
    }

    /// `offer` mutates the host's offered set (used by tests to build a host
    /// that gains a cap).
    #[test]
    fn offer_adds_a_cap() {
        let mut host = HostCaps::empty();
        assert!(!host.offers(HostCapability::Subagent));
        host.offer(HostCapability::Subagent);
        assert!(host.offers(HostCapability::Subagent));
    }
}
